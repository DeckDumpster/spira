//! `trigger.groom`: files the groomer's scheduled hygiene bead. Replaces `spira-groom-prod`
//! (ExecStartPre `world guard`; ExecStart `spira/groom-trigger.sh`). Pattern P1, stateless.
//! Contract: `wiki/projects/spira/designs/tasks/trigger-groom.md`. Not ported yet: [`decide`],
//! [`may_file`] and [`step`] are `unimplemented!`, and the row waits in `tasks::f::PENDING`.
//!
//! # Tick
//!
//! Every 6 h (`SPIRA_RUNTIME_TRIGGER_GROOM_SECS`). Single-flight replaces the `flock` on
//! `groom-trigger.lock`. While the world is halted the snapshot carries `halted` and nothing acts.
//!
//! # Observe
//!
//! `trigger.groom.observe` returns a [`Snapshot`]: `open_triggers` (beads labelled
//! `<scope>,<groom label>` whose lifecycle row is neither past the builder nor poisoned: the
//! `content` query plus the `LcClient` list), `lane_admitted` (`lanes::lane_admitted`),
//! `lastpass` (the persona's own `groom.lastpass` file), `live_owing` (READY, WORKING and REWORK
//! rows, from the S7b projection) and `landings_since_lastpass` (distinct bead ids in `git log`
//! on the home repo, parsed by `mimir_trigger::engine::parse_landing_ids`; git stays a child).
//!
//! # Decide
//!
//! `decide(snap, threshold) -> Decision`, pure and stateless, in the script's order: an
//! unreadable count files nothing and says so; an open trigger skips; an unadmitted lane skips;
//! `live_owing + landings < SPIRA_GROOM_THRESHOLD` skips (an unreadable `live_owing` counts as 0);
//! otherwise file. Fixtures port `spira/test-groom-trigger.sh` and
//! `spira/test-groomer-litter-predicate.sh`.
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `trigger.groom.observe` (probe) | store | 60 s | tick id | — |
//! | `trigger.groom.file` | store | 60 s | `groom-trigger/<6h period index>` | an open-trigger re-count above 0 ([`may_file`]) |
//!
//! The bead: title `Groomer pass — scheduled graph hygiene`, for `groomer`, repo the home repo,
//! priority 3, labels `delivers:note:<SPIRA_RUN>/groom.log` and `groom-trigger`, the body
//! `groom-trigger.sh` writes; bead and lifecycle row in one call. No inbox line: the skip reason
//! and the score are wide-event fields (`groom.skip`, `groom.score`).
//!
//! # Silence
//!
//! An idle tick (a trigger already open, or the score below threshold) emits only the probe. No
//! carried state, so never an Emit.
//!
//! # Replaces
//!
//! `spira-groom-prod.service` and `.timer` and their `world guard`; children `mimir-trigger` ×2,
//! `spira-lc list`, `python3`, `git log`, `bead.sh file`, `bdq update`, `flock`, `mktemp`, the
//! `lib.sh` bootstrap. Pinned config: [`KEYS`].
//!
//! # Reverse
//!
//! Release rollback restores the unit and timer; Observe mode shadows `trigger.groom.file`. No
//! state to migrate back.

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::{Cycle, Decision, Transition};

pub const NAME: &str = "trigger.groom";
pub const STREAM: &str = "task.trigger.groom";
pub const TICK: EventKindTag = "trigger.groom.tick";

pub const OBSERVE: &str = "trigger.groom.observe";
pub const FILE: &str = "trigger.groom.file";
pub const EFFECTS: &[&str] = &[OBSERVE, FILE];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_TRIGGER_GROOM_SECS";
pub const THRESHOLD_KEY: &str = "SPIRA_GROOM_THRESHOLD";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_SCOPE_LABEL", "SPIRA_GROOMER_LABEL", THRESHOLD_KEY, "SPIRA_RUN", "SPIRA_DB", "SPIRA_HOME_REPO", "SPIRA_REPO_MAP"];

pub const TITLE: &str = "Groomer pass — scheduled graph hygiene";

const DEADLINE_MS: u64 = 60_000;
const CYCLE: Cycle = Cycle { stream: STREAM, tick: TICK, observe: OBSERVE, deadline_ms: DEADLINE_MS };

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub halted: bool,
    pub open_triggers: Result<u64, String>,
    pub lane_admitted: bool,
    pub lastpass: i64,
    pub live_owing: Result<u64, String>,
    pub landings_since_lastpass: u64,
    pub home_repo: String,
}

/// One tick of a stateless trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Step {
    Observe,
    Skip,
    Filed,
}

pub const TRANSITIONS: &[Transition<Step>] = &[
    Transition { from: Step::Observe, on: "unreadable", to: Step::Skip, effects: &[] },
    Transition { from: Step::Observe, on: "open", to: Step::Skip, effects: &[] },
    Transition { from: Step::Observe, on: "lane", to: Step::Skip, effects: &[] },
    Transition { from: Step::Observe, on: "score", to: Step::Skip, effects: &[] },
    Transition { from: Step::Observe, on: "file", to: Step::Filed, effects: &[FILE] },
];

/// The tick's next step on `on`, or a refusal naming the step it is in.
pub fn step(_from: Step, _on: &str) -> Result<Step, String> {
    unimplemented!("trigger.groom: not ported yet")
}

/// The file effect's own guard: the open-trigger count it re-reads before writing.
pub fn may_file(_recount: &Result<u64, String>) -> Result<(), String> {
    unimplemented!("trigger.groom: not ported yet")
}

/// The pure decision over one snapshot, with `config` the pinned [`KEYS`].
pub fn decide(_snap: &Snapshot, _config: &BTreeMap<String, String>) -> Decision {
    unimplemented!("trigger.groom: not ported yet")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriggerGroom {
    pub interval_ms: u64,
    pub config: BTreeMap<String, String>,
}

impl TriggerGroom {
    pub fn from_cfg() -> Result<Self, String> {
        Ok(TriggerGroom { interval_ms: super::cadence_ms(CADENCE_KEY)?, config: super::pinned(KEYS)? })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("trigger.groom/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for TriggerGroom {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        CYCLE.handle(self.interval_ms, view, ev, String::new, |body| {
            let Ok(snap) = serde_json::from_str::<Snapshot>(body) else { return vec![] };
            match decide(&snap, &self.config) {
                Decision::Skip(why) => {
                    ctx.event().set("groom.skip", why.as_str());
                    vec![]
                }
                Decision::File(bead) => vec![Command::Effect(EffectSpec { name: FILE.into(), input: serde_json::to_string(&bead).unwrap_or_default(), deadline_ms: DEADLINE_MS })],
            }
        })
    }
}
