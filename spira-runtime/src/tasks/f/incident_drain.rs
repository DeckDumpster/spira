//! `incident.drain`: files what the incident spool holds, one filing per reference. Pattern P1,
//! stateless.
//!
//! # Tick
//!
//! Every `SPIRA_RUNTIME_INCIDENT_DRAIN_SECS`. `incident.drain.observe` counts the spool's
//! entries and reads no store; an empty or unreadable spool ends the tick. Otherwise
//! `incident.drain.drain` runs `incident::intake::drain_spool` over at most
//! `SPIRA_INCIDENT_DRAIN_BATCH` references. An entry it could not file that is older than two
//! cadences is noted in the Concierge inbox, once a day. It runs while the world is halted.
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key |
//! |---|---|---|---|
//! | `incident.drain.observe` (probe) | host | 30 s | tick id |
//! | `incident.drain.drain` | store | 600 s | constant: one drain at a time, however long it runs |

use std::collections::BTreeMap;
use std::path::PathBuf;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use incident::intake::{self, DrainReport, Env};

use super::trigger_groom::{config_input, config_of};
use super::Cycle;

pub const NAME: &str = "incident.drain";
pub const STREAM: &str = "task.incident.drain";
pub const TICK: EventKindTag = "incident.drain.tick";

pub const OBSERVE: &str = "incident.drain.observe";
pub const DRAIN: &str = "incident.drain.drain";
pub const EFFECTS: &[&str] = &[OBSERVE, DRAIN];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_INCIDENT_DRAIN_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, intake::BATCH_KEY];

/// The condition's reference in the inbox and in the alarm state file.
pub const STUCK_REF: &str = "incident:spool-stuck";

const OBSERVE_DEADLINE_MS: u64 = 30_000;
const DRAIN_DEADLINE_MS: u64 = 600_000;
const CYCLE: Cycle = Cycle { stream: STREAM, tick: TICK, observe: OBSERVE, deadline_ms: OBSERVE_DEADLINE_MS };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub count: u64,
    pub oldest_age_s: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub spool: Result<Pending, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Skip(String),
    Drain,
}

/// The pure decision over one snapshot: drain only when something is spooled.
pub fn decide(snap: &Snapshot) -> Decision {
    match &snap.spool {
        Err(_) => Decision::Skip("unreadable".into()),
        Ok(p) if p.count == 0 => Decision::Skip("none".into()),
        Ok(_) => Decision::Drain,
    }
}

/// What the spool at `dir` holds at `now`.
pub fn pending(dir: &std::path::Path, now: i64) -> Result<Pending, String> {
    let entries = incident::spool::entries(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let oldest = entries.iter().filter_map(|p| incident::spool::spooled_at(p)).min();
    Ok(Pending { count: entries.len() as u64, oldest_age_s: oldest.map_or(0, |at| (now - at).max(0) as u64) })
}

fn age(secs: i64) -> String {
    match secs {
        s if s >= 86_400 => format!("{}d {}h", s / 86_400, (s % 86_400) / 3600),
        s if s >= 3600 => format!("{}h {}m", s / 3600, (s % 3600) / 60),
        s if s >= 60 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

/// The standing condition a drain leaves behind: the note's title when a stuck entry is older
/// than two cadences, naming how many are stuck and why the oldest of them is.
pub fn stuck_title(stuck: &[(PathBuf, String)], now: i64, cadence_s: i64) -> Option<String> {
    let (oldest, why) = stuck.iter().filter_map(|(p, why)| Some((incident::spool::spooled_at(p)?, why))).min_by_key(|(at, _)| *at)?;
    let waited = now - oldest;
    (waited > 2 * cadence_s).then(|| format!("{} incident event(s) stuck in the spool, oldest {}: {why}", stuck.len(), age(waited)))
}

fn note_stuck(env: &Env, title: &str) -> Result<(), String> {
    let sink = inbox_rules::Sink::configured("incident")?;
    let state = env.spira_run_path("incident-alarms.tsv");
    intake::with_incident_lock(&env.lock_path, env.lock_wait_s, || incident::alarm::note(&sink, &state, STUCK_REF, title))
        .ok_or_else(|| "another intake held the lock".to_string())?
        .map(|_| ())
}

/// `incident.drain.observe`: the spool's entry count and the oldest one's age.
pub fn run_observe(_e: &crate::Effects, _key: &str, _s: &EffectSpec) -> Result<String, String> {
    let snap = Snapshot { spool: pending(&Env::load().spool_dir, intake::now_epoch()) };
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

/// `incident.drain.drain`: one pass over the spool, its report line as the answer.
pub fn run_drain(e: &crate::Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    let config = config_of(s)?;
    let get = |k: &str| config.get(k).map(String::as_str).unwrap_or_default();
    let batch = intake::parse_batch(get(intake::BATCH_KEY))?;
    let cadence_s: i64 = get(CADENCE_KEY).trim().parse().map_err(|_| format!("{NAME}: {CADENCE_KEY} is not a number"))?;
    let env = Env::load();
    if env.db.is_none() {
        return Err("no SPIRA_DB: the incident spool cannot be drained".into());
    }
    let bd = incident::real::RealBd { lc: Some(spira_store::lc_call::LcClient::socket(&e.world.lc_socket)) };
    let report: DrainReport = intake::drain_spool(&env, &bd, &incident::real::RealMailer, &incident::real::RealClock, batch);
    let line = intake::drain_report(&report);
    if let Some(title) = stuck_title(&report.stuck, intake::now_epoch(), cadence_s) {
        note_stuck(&env, &title).map_err(|why| format!("{line}; the stuck note was not written: {why}"))?;
    }
    Ok(line)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncidentDrain {
    pub interval_ms: u64,
    pub config: BTreeMap<String, String>,
}

impl IncidentDrain {
    pub fn from_settings(settings: &crate::Settings) -> Result<Self, String> {
        Ok(IncidentDrain { interval_ms: super::cadence_ms(settings, CADENCE_KEY)?, config: super::pinned(settings, KEYS)? })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("incident.drain/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for IncidentDrain {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        CYCLE.handle(self.interval_ms, view, ev, String::new, |body| {
            let Ok(snap) = serde_json::from_str::<Snapshot>(body) else { return vec![] };
            match decide(&snap) {
                Decision::Skip(why) => {
                    ctx.event().set("incident_drain.skip", why.as_str());
                    vec![]
                }
                Decision::Drain => vec![Command::Effect(EffectSpec::keyed(DRAIN, NAME.to_string(), config_input(&self.config), DRAIN_DEADLINE_MS))],
            }
        })
    }
}
