//! `trigger.mimir`: files the Mimir persona's retrospective bead. Replaces `spira-mimir-prod`
//! (ExecStartPre `world guard`; ExecStart `spira/mimir-trigger.sh`, which execs `mimir-trigger`).
//! Pattern P1, stateless. Contract: `wiki/projects/spira/designs/tasks/trigger-mimir.md`. Not
//! ported yet: [`decide`], [`may_file`] and [`step`] are `unimplemented!`, and the row waits in
//! `tasks::f::PENDING`.
//!
//! # Tick
//!
//! Every 15 min (`SPIRA_RUNTIME_TRIGGER_MIMIR_SECS`). Single-flight replaces the `flock` on
//! `mimir-trigger.lock`. While the world is halted nothing acts.
//!
//! # Observe
//!
//! `trigger.mimir.observe` returns a [`Snapshot`]: `open_triggers` (`Real::open_trigger_count`
//! as the `content` query plus the `LcClient` list), `lane_admitted`, `lastpass` and `watermark`
//! (the persona's own files), `landings_since_watermark` (`engine::parse_landing_ids` over
//! `git log` on the home repo and every `SPIRA_REPO_MAP` repo) and `invalid_closed_raw`
//! (`strand::detectors::detect_invalid_closed`, in-process).
//!
//! # Decide
//!
//! `mimir_trigger::engine` already holds the pure core (`time_trigger`, `landing_trigger`,
//! `invalid_closed_rows`, `trigger_labels`, `reason_text`, `description`). In order: a count error
//! or an open trigger skips; an unadmitted lane skips; fire when `now - lastpass >=
//! SPIRA_MIMIR_MAX_GAP_SECONDS`, or `landings >= SPIRA_MIMIR_LANDING_INTERVAL`, or there is any
//! invalid-closed row. No carried state. Fixtures: the 17 engine tests, the 5 `main.rs` tests,
//! `real.rs`'s open-trigger test and `spira/test-mimir-trigger.sh`.
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `trigger.mimir.observe` (probe) | store | 120 s | tick id | — |
//! | `trigger.mimir.file` | store | 120 s | `mimir-sweep/<15m period index>` | an open-trigger re-count above 0 ([`may_file`]) |
//!
//! The bead: title `Mimir pass — <reason>` (`<n>s elapsed`, `<n> landings` and
//! `<n> invalid-closed row(s)` joined with `, `), type `task`, priority 3, labels
//! `<scope>,<mimir label>,delivers:note:<SPIRA_RUN>/mimir.log`, the body of
//! `engine::description`; bead and lifecycle row together. No inbox line.
//!
//! # Silence
//!
//! An idle tick (a trigger already open, or no trigger condition) emits only the probe.
//!
//! # Replaces
//!
//! `spira-mimir-prod.service` and `.timer`, the `world guard` and the shim; children `bdq list`,
//! `bdq create`, `git log` per repo, `date`, the `spira-lc` list. The `mimir-trigger repo-lanes |
//! lane-admitted | open-trigger-count` doors retire with `trigger.groom`. Pinned config: [`KEYS`].
//!
//! # Reverse
//!
//! Release rollback restores the unit, the timer and the shim; Observe mode shadows the file.

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::{Cycle, Decision, Transition};

pub const NAME: &str = "trigger.mimir";
pub const STREAM: &str = "task.trigger.mimir";
pub const TICK: EventKindTag = "trigger.mimir.tick";

pub const OBSERVE: &str = "trigger.mimir.observe";
pub const FILE: &str = "trigger.mimir.file";
pub const EFFECTS: &[&str] = &[OBSERVE, FILE];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_TRIGGER_MIMIR_SECS";
pub const KEYS: &[&str] = &[
    CADENCE_KEY,
    "SPIRA_RUN",
    "SPIRA_REPO_MAP",
    "SPIRA_PLAN_LABEL",
    "SPIRA_INCIDENT_LABEL",
    "SPIRA_GROOMER_LABEL",
    "SPIRA_MIMIR_LABEL",
    "SPIRA_SPIKE_LABEL",
    "SPIRA_CZAR_LABEL",
    "SPIRA_SCOPE_LABEL",
    "SPIRA_MIMIR_MAX_GAP_SECONDS",
    "SPIRA_MIMIR_LANDING_INTERVAL",
    "SPIRA_MIMIR_MAX_BEADS",
    "SPIRA_METRICS_ENDPOINT",
];

const DEADLINE_MS: u64 = 120_000;
const CYCLE: Cycle = Cycle { stream: STREAM, tick: TICK, observe: OBSERVE, deadline_ms: DEADLINE_MS };

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub halted: bool,
    pub open_triggers: Result<u64, String>,
    pub lane_admitted: bool,
    pub now: i64,
    pub lastpass: i64,
    pub watermark: i64,
    pub landings_since_watermark: u64,
    pub invalid_closed_raw: String,
}

pub use super::trigger_groom::Step;

pub const TRANSITIONS: &[Transition<Step>] = &[
    Transition { from: Step::Observe, on: "unreadable", to: Step::Skip, effects: &[] },
    Transition { from: Step::Observe, on: "open", to: Step::Skip, effects: &[] },
    Transition { from: Step::Observe, on: "lane", to: Step::Skip, effects: &[] },
    Transition { from: Step::Observe, on: "no-trigger", to: Step::Skip, effects: &[] },
    Transition { from: Step::Observe, on: "time", to: Step::Filed, effects: &[FILE] },
    Transition { from: Step::Observe, on: "landing", to: Step::Filed, effects: &[FILE] },
    Transition { from: Step::Observe, on: "invalid-closed", to: Step::Filed, effects: &[FILE] },
];

/// The tick's next step on `on`, or a refusal naming the step it is in.
pub fn step(_from: Step, _on: &str) -> Result<Step, String> {
    unimplemented!("trigger.mimir: not ported yet")
}

/// The file effect's own guard: the open-trigger count it re-reads before writing.
pub fn may_file(_recount: &Result<u64, String>) -> Result<(), String> {
    unimplemented!("trigger.mimir: not ported yet")
}

/// The pure decision over one snapshot, with `config` the pinned [`KEYS`].
pub fn decide(_snap: &Snapshot, _config: &BTreeMap<String, String>) -> Decision {
    unimplemented!("trigger.mimir: not ported yet")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriggerMimir {
    pub interval_ms: u64,
    pub config: BTreeMap<String, String>,
}

impl TriggerMimir {
    pub fn from_cfg() -> Result<Self, String> {
        Ok(TriggerMimir { interval_ms: super::cadence_ms(CADENCE_KEY)?, config: super::pinned(KEYS)? })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("trigger.mimir/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for TriggerMimir {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        CYCLE.handle(self.interval_ms, view, ev, String::new, |body| {
            let Ok(snap) = serde_json::from_str::<Snapshot>(body) else { return vec![] };
            match decide(&snap, &self.config) {
                Decision::Skip(why) => {
                    ctx.event().set("mimir.skip", why.as_str());
                    vec![]
                }
                Decision::File(bead) => vec![Command::Effect(EffectSpec { name: FILE.into(), input: serde_json::to_string(&bead).unwrap_or_default(), deadline_ms: DEADLINE_MS })],
            }
        })
    }
}
