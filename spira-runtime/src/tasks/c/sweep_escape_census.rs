//! # sweep.escape_census — contract
//!
//! Family C of the scheduler consolidation, red-first: `decide` is not ported yet. The
//! sections below are the contract page `wiki/projects/spira/designs/tasks/sweep-escape-census.md`;
//! its Acceptance section is the test file `tests/task_c_sweep_escape_census.rs`.
//!
//! ## Tick
//!
//! - **Calendar:** `OnCalendar=Mon *-*-* 06:15:00`, `AccuracySec=5min`, `Persistent=true`, no
//!   jitter. The sim parser already reads this form (`sim/src/units.rs:80-94`).
//! - **Catch-up:** an overdue tick fires **once**; `last_fired` carried in
//!   `sweep.escape_census.state`.
//! - **Single-flight:** yes. No `world guard`.
//!
//! ## Observe
//!
//! `sweep.escape_census.observe` (probe, store, deadline 30 s) returns:
//!
//! ```text
//! Snapshot { counts: BTreeMap<class, u64>, hours: 168 }
//! ```
//!
//! from `tsd::query::count_by(src, "escape", "class", now, Some(168))` (`tsd/src/query.rs:258`),
//! in-process — the same function `tsd-read count-by` serves (`tsd/src/read_main.rs:92`). The
//! escape family is written by `escape-classify.sh record` (`_tsd_escape`, `:135`); tsd is the
//! component that answers it. Window 168 h is the script default (`:141`, `:189`).
//!
//! ## Decide
//!
//! `decide(snap) -> line`: `"<n> mapping_gap, <n> gate_gap, <n> environment_gap, <n> flake"`, in
//! that fixed order, absent class → 0 (`escape-classify.sh:143-147`). Pure. Carry: `last_fired`.
//! **Fixtures:** an empty family, a family with only `flake`, a family with all four.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `sweep.escape_census.report` | host | 5 s | `escape-census/<ISO week>` | a second report for the same ISO week |
//!
//! Today the line goes to `run/escape-census.log`. The contract keeps the log as the read contract
//! (appended by the effect). **Proposed:** since the header calls MAPPING GAP trending to zero
//! "the convergence metric" (`:13-14`) but nothing reads the log, either route the line to the
//! inbox (`inbox.append`) or retire the unit (P5). Default here: keep the log append, flag for Ryan.
//!
//! ## Silence
//!
//! Between Monday ticks: nothing. On the tick, exactly one `report` effect — the census is the
//! output, so there is no idle variant.
//!
//! ## Replaces
//!
//! - Unit `spira-escape-census-prod.service` / `.timer`; output `run/escape-census.log`.
//! - Children: `bash lib.sh` (`:38`), `tsd-query.sh` → `tsd-read` | `duckdb`, `awk`.
//! - Config keys: none of its own beyond `SPIRA_RUN` (tsd root, via `lib.sh`). The `classify`,
//!   `record` and `rerun-verdict` verbs stay a script, called by the round machinery (out of this task).
//!
//! ## Reverse
//!
//! Release rollback restores unit and timer. `Observe` shadows `report`; a missing weekly line
//! costs nothing (no reader).
//!

// RETIRE-CANDIDATE: no reader of the census line was found (design, "Corrections from the
// contracts": escape-census joins the retire list). Kept as a stub until the ruling lands.

use std::collections::BTreeMap;

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use crate::tasks::b::kit::{Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "sweep.escape_census";
pub const STREAM: &str = "task.sweep.escape_census";
pub const TICK: EventKindTag = "sweep.escape_census.tick";
pub const STATE: EventKindTag = "sweep.escape_census.state";

pub const OBSERVE: &str = "sweep.escape_census.observe";
pub const REPORT: &str = "sweep.escape_census.report";
pub const EFFECTS: &[&str] = &[OBSERVE, REPORT];

/// The classes, in the line's fixed order.
pub const CLASSES: [&str; 4] = ["mapping_gap", "gate_gap", "environment_gap", "flake"];

/// `OnCalendar=Mon *-*-* 06:15:00`, no jitter.
pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_SWEEP_ESCAPE_CENSUS_CALENDAR";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_RUN"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 30_000, cadence_key: CADENCE_KEY, calendar: true, effects: EFFECTS, keys: KEYS, probe_input_key: None };

/// `tsd::query::count_by(src, "escape", "class", now, Some(168))`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub counts: BTreeMap<String, u64>,
    pub hours: u64,
}

/// T3.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Idle,
    Reported,
}

/// `state × event → state`. A second report for a reported ISO week is refused naming
/// `reported`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[(Phase::Idle, "observed", Phase::Reported), (Phase::Reported, "week:new", Phase::Idle)];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub phase: Phase,
    pub last_fired: Option<u64>,
    /// The ISO week last reported, `YYYY-Www`.
    pub week: Option<String>,
}

pub struct SweepEscapeCensus {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// `"<n> mapping_gap, <n> gate_gap, <n> environment_gap, <n> flake"`, absent class → 0.
pub fn decide(_cfg: &Cfg, _carry: &Carry, _input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    unimplemented!("sweep.escape_census: not ported yet")
}

crate::tasks::b::hosted!(SweepEscapeCensus);
