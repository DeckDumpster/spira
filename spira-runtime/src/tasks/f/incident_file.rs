//! `incident.file`: files the incident rows their raiser left unfiled. Pattern P1, stateless.
//!
//! # Tick
//!
//! At boot, when the store reports a change under `incidents/`, on a re-pin of a key it
//! declares, and when such a key is told again unchanged (`spira-config repo set` tells
//! `SPIRA_REPO_MAP` after rewriting the map's file: how a lanes change reaches it), and at the earliest retry time or
//! claim lapse the last look saw (`Due`). It does not poll. `incident.file.observe` lists the
//! rows from the in-process store view; when [`intake::file_pending`] would file none, the
//! tick ends. Otherwise `incident.file.run` runs it over at most `SPIRA_INCIDENT_DRAIN_BATCH`
//! rows. It has no world-halt guard of its own; a scheduler halt holds the run, a store write,
//! until the resume.
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key |
//! |---|---|---|---|
//! | `incident.file.observe` (probe) | store | 30 s | tick id |
//! | `incident.file.run` | store | 600 s | constant: one pass at a time, however long it runs |
//!
//! A look or a pass the store did not answer is tried again after a minute.

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_store::incident::{Payload, Row};
use spira_store::lc_call::LcClient;

use incident::intake::{self, Env};
use incident::ports::Bd as _;

use super::trigger_groom::{config_input, config_of};
use crate::tasks::d::pattern;
use crate::watchers::answered_effect;

pub const NAME: &str = "incident.file";
pub const STREAM: &str = "task.incident.file";
pub const TICK: EventKindTag = "incident.file.tick";

pub const OBSERVE: &str = "incident.file.observe";
pub const RUN: &str = "incident.file.run";
pub const EFFECTS: &[&str] = &[OBSERVE, RUN];

pub const MAP_KEY: &str = "SPIRA_REPO_MAP";
pub const KEYS: &[&str] = &[MAP_KEY, intake::BATCH_KEY];

/// The shortest time between two ticks.
pub const CADENCE_MS: u64 = 1_000;
const OBSERVE_DEADLINE_MS: u64 = 30_000;
const RUN_DEADLINE_MS: u64 = 600_000;
const AFTER_FAILURE_MS: u64 = 60_000;

/// One unfiled row, its text left behind, with what its repository admits now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiting {
    pub row: Row,
    pub lanes_now: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub rows: Vec<Waiting>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    File,
    /// Nothing to file; look again at this time, if any row's cause moves with the clock.
    Wait(Option<u64>),
}

/// The pure decision over one snapshot at `now_ms`.
pub fn decide(snap: &Snapshot, now_ms: u64) -> Decision {
    let now = (now_ms / 1_000) as i64;
    if snap.rows.iter().any(|w| intake::eligible(&w.row, w.lanes_now.as_deref(), now)) {
        return Decision::File;
    }
    let due = snap.rows.iter().filter_map(|w| match w.row.state {
        spira_store::incident::State::Unread => w.row.retry_at,
        spira_store::incident::State::Filing => w.row.claim.as_ref().map(|c| c.until),
        _ => None,
    });
    Decision::Wait(due.min().map(|at| at.max(0) as u64 * 1_000))
}

/// What a pass answers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Passed {
    pub line: String,
    /// Rows the pass took: each has moved, so another look is worth its cost.
    pub claimed: usize,
    /// Rows it tried to take or file and left unfiled.
    pub failed: usize,
}

/// When to look again after a pass that answered `passed` at `now_ms`; `None` for one that
/// found nothing to take.
pub fn after_pass(passed: &Passed, now_ms: u64) -> Option<u64> {
    if passed.claimed > 0 {
        Some(now_ms + CADENCE_MS)
    } else {
        (passed.failed > 0).then_some(now_ms + AFTER_FAILURE_MS)
    }
}

/// The intake's environment with the repository map this task pins.
fn env_of(config: &BTreeMap<String, String>) -> Env {
    let mut env = Env::load();
    env.repo_map = config.get(MAP_KEY).filter(|p| !p.is_empty()).map(std::path::PathBuf::from);
    env.known_repos = env.repo_map.as_deref().map(intake::repo_names_in).unwrap_or_default();
    env
}

/// `incident.file.observe`: the unfiled rows, from the store view.
pub fn run_observe(e: &crate::Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    let env = env_of(&config_of(s)?);
    let lc = LcClient::socket(&e.world.lc_socket);
    let reply = store_view::call_held(&lc, &["incidents", "list"])?;
    if reply.code != 0 {
        return Err(format!("incidents list: {}", reply.why()));
    }
    let rows = incident::real::rows_listed(&reply.out)?
        .into_iter()
        .map(|(_, row)| {
            let lanes_now = env.lanes_of(&intake::route_repo(&env, &row.payload));
            Waiting { row: Row { payload: Payload { reference: row.payload.reference.clone(), ..Payload::default() }, reason: None, ..row }, lanes_now }
        })
        .collect();
    serde_json::to_string(&Snapshot { rows }).map_err(|e| e.to_string())
}

/// `incident.file.run`: one pass over the rows.
pub fn run_file(e: &crate::Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    let config = config_of(s)?;
    let batch = intake::parse_batch(config.get(intake::BATCH_KEY).map(String::as_str).unwrap_or_default())?;
    let env = env_of(&config);
    if env.db.is_none() {
        return Err("no SPIRA_DB: the incident rows cannot be filed".into());
    }
    let bd = incident::real::RealBd { lc: Some(LcClient::socket(&e.world.lc_socket)) };
    let rows = bd.incident_rows();
    let report = intake::file_pending(&env, &*rows, &bd, &incident::real::RealMailer, &incident::real::RealClock, &|repo| env.lanes_of(repo), intake::now_epoch(), batch);
    if let Some(why) = &report.unreadable {
        return Err(format!("the incident rows cannot be read: {why}"));
    }
    serde_json::to_string(&Passed { line: report.line(), claimed: report.claimed, failed: report.failed.len() }).map_err(|e| e.to_string())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncidentFile {
    pub config: BTreeMap<String, String>,
}

impl IncidentFile {
    pub fn from_settings(settings: &crate::Settings) -> Result<Self, String> {
        Ok(IncidentFile { config: super::pinned(settings, KEYS)? })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("incident.file/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for IncidentFile {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED, EFFECT_FAILED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            return pattern::tick(ev, OBSERVE, config_input(&self.config), OBSERVE_DEADLINE_MS, ev.at + CADENCE_MS);
        }
        let Some(effect) = answered_effect(view, ev).filter(|n| n == OBSERVE || n == RUN) else { return vec![] };
        let again = |at: Option<u64>| at.map(|at| Command::Due { at }).into_iter().collect();
        if ev.kind.tag == EFFECT_FAILED {
            ctx.event().set("incident_file.failed", effect.as_str());
            return again(Some(ev.at + AFTER_FAILURE_MS));
        }
        if effect == RUN {
            let Ok(passed) = serde_json::from_str::<Passed>(&ev.kind.body) else { return vec![] };
            ctx.event().set("incident_file.pass", passed.line.as_str());
            return again(after_pass(&passed, ev.at));
        }
        let Ok(snap) = serde_json::from_str::<Snapshot>(&ev.kind.body) else { return vec![] };
        match decide(&snap, ev.at) {
            Decision::File => vec![Command::Effect(EffectSpec::keyed(RUN, NAME.to_string(), config_input(&self.config), RUN_DEADLINE_MS))],
            Decision::Wait(at) => again(at),
        }
    }
}
