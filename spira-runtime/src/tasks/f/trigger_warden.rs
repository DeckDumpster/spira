//! `trigger.warden`: files the warden's scheduled sweep bead. Replaces `spira-warden-prod`
//! (ExecStartPre `world guard`; ExecStart `spira/warden-trigger.sh`). Pattern P1, stateless.
//! Contract: `wiki/projects/spira/designs/tasks/trigger-warden.md`.
//!
//! # Tick
//!
//! Every 2 h (`SPIRA_RUNTIME_TRIGGER_WARDEN_SECS`); the unit's first run is at boot+20 min, which
//! a `Task` row cannot express (the boot tick probes at once and arms one cadence on).
//! Single-flight replaces the `flock`. While the world is halted nothing acts. The script's retry
//! loop goes: a failed count is a skip and the next tick retries; a failed write is retried by the
//! effect runner with S3 backoff. No effect sleeps.
//!
//! # Observe
//!
//! `trigger.warden.observe` returns a [`Snapshot`]: `open_sweeps`, the counter `trigger.groom`
//! uses over `<SPIRA_SCOPE_LABEL>,<SPIRA_WARDEN_LABEL>` (the `content` query plus the `LcClient`
//! list). No lane check and no threshold.
//!
//! # Decide
//!
//! `decide(snap) -> Decision`, pure and stateless: an unreadable count skips and says why; an open
//! sweep skips; otherwise file one sweep. No fixture suite exists; T5 uses fixtures captured from
//! the script's three exits.
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `trigger.warden.observe` (probe) | store | 30 s | tick id | — |
//! | `trigger.warden.file` | store | 600 s | `warden-sweep/<2h period index>` | a re-count showing an open sweep ([`may_file`]) |
//!
//! The bead and its lifecycle row are written in one call: title [`TITLE`], type `task`,
//! priority 3, labels `<scope>,<warden label>,delivers:note:<SPIRA_RUN>/warden.log`, description
//! [`DESCRIPTION`]. No inbox line; the script's stdout lines become wide-event fields.
//!
//! # Silence
//!
//! An idle tick, with a sweep already open, emits only the probe. No carried state, no Emit.
//!
//! # Replaces
//!
//! `spira-warden-prod.service` and `.timer` and their `world guard`; children
//! `mimir-trigger open-trigger-count` (up to 4 per tick), `bdq create`, `spira-lc create-bead`,
//! `timeout`, `flock`, the `lib.sh` bootstrap. Pinned config: [`KEYS`]; `SPIRA_WARDEN_ATTEMPTS`,
//! `SPIRA_WARDEN_BACKOFF` and `SPIRA_WARDEN_CALL_TIMEOUT` retire with the retry loop.
//!
//! # Reverse
//!
//! Release rollback restores the unit and timer; Observe mode shadows `trigger.warden.file`.

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use spira_store::lc_call::LcClient;

use super::trigger_groom::{config_input, config_of, dedup_labels, file_effect, guard, halted, run_file_with, walk};
use super::{Bead, Cycle, Decision, Transition};

pub const NAME: &str = "trigger.warden";
pub const STREAM: &str = "task.trigger.warden";
pub const TICK: EventKindTag = "trigger.warden.tick";

pub const OBSERVE: &str = "trigger.warden.observe";
pub const FILE: &str = "trigger.warden.file";
pub const EFFECTS: &[&str] = &[OBSERVE, FILE];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_TRIGGER_WARDEN_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_SCOPE_LABEL", "SPIRA_WARDEN_LABEL", "SPIRA_RUN"];

pub const TITLE: &str = "Warden sweep — landed work in force, anomalies attributed";
pub const DESCRIPTION: &str = "Scheduled sweep: the warden persona judges whether landed fixes are in force (filing a follow-up for any that are not) and attributes anomalies no probe could. See spira/runes/warden.md.";

const OBSERVE_DEADLINE_MS: u64 = 30_000;
const FILE_DEADLINE_MS: u64 = 600_000;
const CYCLE: Cycle = Cycle { stream: STREAM, tick: TICK, observe: OBSERVE, deadline_ms: OBSERVE_DEADLINE_MS };

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub halted: bool,
    pub open_sweeps: Result<u64, String>,
}

pub use super::trigger_groom::Step;

pub const TRANSITIONS: &[Transition<Step>] = &[
    Transition { from: Step::Observe, on: "unreadable", to: Step::Skip, effects: &[] },
    Transition { from: Step::Observe, on: "open", to: Step::Skip, effects: &[] },
    Transition { from: Step::Observe, on: "file", to: Step::Filed, effects: &[FILE] },
];

/// The tick's next step on `on`, or a refusal naming the step it is in.
pub fn step(from: Step, on: &str) -> Result<Step, String> {
    walk(TRANSITIONS, from, on)
}

/// The file effect's own guard: the open-sweep count it re-reads before writing.
pub fn may_file(recount: &Result<u64, String>) -> Result<(), String> {
    guard(recount).map_err(|e| format!("a sweep is open or unreadable: {e}"))
}

/// The pure decision over one snapshot, with `config` the pinned [`KEYS`].
pub fn decide(snap: &Snapshot, config: &BTreeMap<String, String>) -> Decision {
    if snap.halted {
        return Decision::Skip("halted".into());
    }
    match snap.open_sweeps {
        Err(_) => Decision::Skip("unreadable".into()),
        Ok(n) if n > 0 => Decision::Skip("open".into()),
        Ok(_) => {
            let run = config.get("SPIRA_RUN").cloned().unwrap_or_default();
            let mut labels: Vec<String> = dedup_labels(config, "SPIRA_WARDEN_LABEL").split(',').filter(|l| !l.is_empty()).map(str::to_string).collect();
            labels.push(format!("delivers:note:{run}/warden.log"));
            Decision::File(Bead { title: TITLE.into(), kind: "task".into(), priority: 3, labels, body: DESCRIPTION.into(), persona: None, repo: None })
        }
    }
}

/// `trigger.warden.observe`: whether the world is halted and how many sweeps are open.
pub fn run_observe(e: &crate::Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    let config = config_of(s)?;
    let lc = LcClient::socket(&e.world.lc_socket).within(std::time::Duration::from_millis(OBSERVE_DEADLINE_MS));
    let snap = Snapshot { halted: halted(e), open_sweeps: mimir_trigger::observe::open_trigger_count_on(&lc, &dedup_labels(&config, "SPIRA_WARDEN_LABEL")) };
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

/// `trigger.warden.file`.
pub fn run_file(e: &crate::Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    run_file_with(e, s, may_file)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriggerWarden {
    pub interval_ms: u64,
    pub config: BTreeMap<String, String>,
}

impl TriggerWarden {
    pub fn from_settings(settings: &crate::Settings) -> Result<Self, String> {
        Ok(TriggerWarden { interval_ms: super::cadence_ms(settings, CADENCE_KEY)?, config: super::pinned(settings, KEYS)? })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("trigger.warden/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for TriggerWarden {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        CYCLE.handle(self.interval_ms, view, ev, || config_input(&self.config), |body| {
            let Ok(snap) = serde_json::from_str::<Snapshot>(body) else { return vec![] };
            match decide(&snap, &self.config) {
                Decision::Skip(why) => {
                    ctx.event().set("warden.skip", why.as_str());
                    vec![]
                }
                Decision::File(bead) => vec![file_effect(FILE, format!("warden-sweep/{}", ev.at / self.interval_ms.max(1)), bead, dedup_labels(&self.config, "SPIRA_WARDEN_LABEL"), FILE_DEADLINE_MS)],
            }
        })
    }
}
