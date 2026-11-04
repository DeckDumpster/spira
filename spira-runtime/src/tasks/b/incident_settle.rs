//! # incident.settle — contract
//!
//! Family B of the scheduler consolidation, red-first: `decide` is not ported yet. The
//! sections below are the contract page `wiki/projects/spira/designs/tasks/incident-settle.md`;
//! its Acceptance section is the test file `tests/task_b_incident_settle.rs`.
//!
//! **Pattern (corrections from the contracts).** A P2 machine, not a P1/P4 tick: the state
//! table is `TRANSITIONS`, and a long leg run as a transient unit ends on its `unit.ran`,
//! recognised by `UnitRan::key_for(NAME)`.
//!
//! ## Tick
//!
//! - Cadence 5 min (`OnUnitActiveSec=5min`, `OnBootSec=3min`, `AccuracySec=30s`). Boot tick at +180 s.
//! - Also woken by a lifecycle transition to `LANDED`/`DONE` of a bead that `blocks` an open
//!   incident, and by `release.activated` (unverified: neither event is delivered to handlers today;
//!   without them, cadence alone suffices — the fix waits for the active release anyway).
//! - Single-flight. No `world guard` on the unit; settle runs while halted. Proposed unchanged.
//! - Refuses to arm without a content database: today `SPIRA_DB` unset exits early (`main.rs:872`).
//!
//! ## Observe
//!
//! Probe `incident.settle.observe` returns one row per open incident:
//!
//! ```text
//! Snapshot { incidents: Vec<Inc> }
//! Inc { id, external_ref, labels, fixes: Vec<Fix>, detector: Quiet | Firing(String) | NoProbe | Err(String) }
//! Fix { id, state: String /* lifecycle state */, landing: InForce(sha) | NotInRelease | Err(String) }
//! ```
//!
//! - Candidates: content `list --all --json` by label `SPIRA_INCIDENT_LABEL` (default `incident`),
//!   open only (`settle_real.rs:57-62`), joined to the lifecycle list (`real.rs:136`) → S7 `content`
//!   verb + lifecycle projection.
//! - Fixes: the incident's `blocks` edges (`settle_real.rs:27`, read via `bdq list --id`, `:19`) →
//!   `content` verb.
//! - Fix state: `lc_state::row(fix)` (`settle_real.rs:70`; `spira-config/src/lc_state.rs:241`) →
//!   `LcClient` show. `LANDED`/`DONE` is landed, `DROPPED` is non-blocking (`settle.rs:8,81`).
//! - Landing in force: `git log <base> --grep <id>` then `merge-base --is-ancestor <sha> <release>`
//!   (`settle_real.rs:87,95`); repo from the fix's `repo:` label and the registry. Git stays a child
//!   (class B, allowed). Release sha from `$SPIRA_RELEASE` (`settle_real.rs:135`) → pinned at boot.
//! - Detector: `systemctl --user is-failed <unit>` for `incident:<unit>` refs (`settle_real.rs:102-112`)
//!   → `unit.facts` (S6) read in-process. Non-unit refs are `NoProbe`.
//!
//! The probe stops reading an incident at its first unlanded fix, as today (`settle.rs:85`), so the
//! detector is never run for one.
//!
//! ## Decide
//!
//! `decide(carry, snap) -> (Carry, Vec<Act>)`: `settle_one` with the port's read methods answered
//! from `Inc`. Outcomes `Closed | Persists | Waiting | Skipped` (`settle.rs:52`).
//!
//! - Close when every fix is landed-in-force or dropped and the detector is `Quiet`; evidence names
//!   each fix, its sha, and "detector quiet at <now>" (`settle.rs:98-102`).
//! - Persist (note + label `settle-fired:<hash of sorted fixes>`, `settle.rs:59,112`) once per fix set;
//!   an incident already carrying the label waits (`settle.rs:109`).
//! - Carry: the `Waiting` set as `id\twhy` lines (`settle.rs:124`), today the file
//!   `<incident.log>.settle-waiting` (`main.rs:626-631`). It now lives in the stream's carried state;
//!   a changed line is logged, an unchanged one is not.
//! - Fixtures: the eight tests in `incident/src/settle.rs:209-286` become snapshot fixtures.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `incident.settle.close` (`spira_config::lifecycle_row::close(id, evidence, "incident")`, `settle_real.rs:116`) | store | 5 s | incident id | row already terminal (store answers 3); unreachable store |
//! | `incident.settle.note` (`bdq note <id> --stdin`, `settle_real.rs:120`) | store | 10 s | incident id + fired label | missing bead |
//! | `incident.settle.label` (`bdq label add <id> settle-fired:<h>`, `settle_real.rs:123`) | store | 10 s | incident id + label (set-add) | missing bead |
//! | `incident.settle.log` (append `settle: <outcome>` to `incident.log`, `main.rs:629`) | host | 2 s | event id | none |
//!
//! `note` then `label` are ordered: label only after note succeeds, as today (`settle_real.rs:120-125`).
//! A failed note leaves no label, so the next tick retries.
//!
//! ## Silence
//!
//! Idle: no incident changes outcome. Nothing is emitted beyond the probe; carried state is emitted
//! only when the waiting set changes; `incident.settle.log` fires only for a changed waiting line or a
//! non-waiting outcome. Today `Skipped` outcomes (e.g. "no fix dependency") are logged every pass
//! (`settle.rs:137`, `_ => fresh.push(o)`); proposed: carry them like `Waiting`, so a standing skip is
//! silent.
//!
//! ## Replaces
//!
//! - `spira-incident-settle-prod.service` + `.timer`; `incident settle` stays a CLI for hand use.
//! - File `run/incident.settle-waiting` (carried state instead).
//! - Children per tick: `bdq` 1 + 2·incidents (`real.rs:33`), `spira-lc` list + 1 per fix
//!   (`real.rs:136`, `lc_state.rs:241`), `systemctl` 1 per unit-referenced incident
//!   (`settle_real.rs:107`), `git` 1–2 per landed fix (stays).
//! - Keys pinned: `SPIRA_DB`, `SPIRA_INCIDENT_LABEL`, `SPIRA_HOME_REPO` (default `spira`,
//!   `main.rs:167`), `SPIRA_INCIDENT_LOG`, `SPIRA_RELEASE`, the repo registry (`main.rs:621`),
//!   `BD_TIMEOUT`, `SPIRA_BDQ_CONN_RETRIES` (`real.rs:23-24`).
//!
//! ## Reverse
//!
//! Release rollback restores the unit and timer. Per task: `Observe` mode journals `close`/`note`/
//! `label` as `effect.shadowed`; nothing closes. Running the old unit alongside is safe: close is
//! terminal-refusing and the label is set-add.
//!

use std::collections::BTreeMap;

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use super::kit::{Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "incident.settle";
pub const STREAM: &str = "task.incident.settle";
pub const TICK: EventKindTag = "incident.settle.tick";
pub const STATE: EventKindTag = "incident.settle.state";

pub const OBSERVE: &str = "incident.settle.observe";
pub const CLOSE: &str = "incident.settle.close";
pub const NOTE: &str = "incident.settle.note";
pub const LABEL: &str = "incident.settle.label";
pub const LOG: &str = "incident.settle.log";
pub const EFFECTS: &[&str] = &[OBSERVE, CLOSE, NOTE, LABEL, LOG];

/// The label a persisting incident carries per fix set.
pub const FIRED_PREFIX: &str = "settle-fired:";

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_INCIDENT_SETTLE_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_DB", "SPIRA_INCIDENT_LABEL", "SPIRA_HOME_REPO", "SPIRA_INCIDENT_LOG", "SPIRA_RELEASE"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 10_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Landing {
    InForce(String),
    #[default]
    NotInRelease,
    Err(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Fix {
    pub id: String,
    /// The lifecycle state: `LANDED`/`DONE` is landed, `DROPPED` does not block.
    pub state: String,
    pub landing: Landing,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Detector {
    Quiet,
    Firing(String),
    #[default]
    NoProbe,
    Err(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Inc {
    pub id: String,
    pub external_ref: String,
    pub labels: Vec<String>,
    pub fixes: Vec<Fix>,
    pub detector: Detector,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub incidents: Vec<Inc>,
}

/// The P2 machine, per incident (T3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Open,
    Waiting,
    Persists,
    Skipped,
    Closing,
    Closed,
}

/// `state × event → state` per incident, as data. A second `persists` for a fix set already
/// labelled is refused naming the `settle-fired:` label. The long leg, when the check runs as a
/// transient unit, ends on its `unit.ran` (`UnitRan::key_for(NAME)` names the incident).
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[
    (Phase::Open, "fix:unlanded", Phase::Waiting),
    (Phase::Open, "no-fix", Phase::Skipped),
    (Phase::Open, "landed:quiet", Phase::Closing),
    (Phase::Open, "landed:firing", Phase::Persists),
    (Phase::Waiting, "landed:quiet", Phase::Closing),
    (Phase::Waiting, "landed:firing", Phase::Persists),
    (Phase::Persists, "labelled", Phase::Waiting),
    (Phase::Closing, "close:ok", Phase::Closed),
    (Phase::Closing, "unit.ran", Phase::Closed),
    (Phase::Closing, "close:failed", Phase::Open),
    (Phase::Skipped, "fix:unlanded", Phase::Waiting),
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub incidents: BTreeMap<String, Phase>,
    /// `id\twhy` per waiting or skipped incident, so a standing line is silent.
    pub waiting: BTreeMap<String, String>,
}

pub struct IncidentSettle {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// `settle_one` with the port's reads answered from `Inc`.
pub fn decide(_cfg: &Cfg, _carry: &Carry, _input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    unimplemented!("incident.settle: not ported yet")
}

super::hosted!(IncidentSettle);
