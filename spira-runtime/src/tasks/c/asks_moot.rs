//! # asks.moot — contract
//!
//! Family C of the scheduler consolidation, red-first: `decide` is not ported yet. The
//! sections below are the contract page `wiki/projects/spira/designs/tasks/asks-moot.md`;
//! its Acceptance section is the test file `tests/task_c_asks_moot.rs`.
//!
//! ## Tick
//!
//! - Cadence **15 min** (unit: `OnBootSec=5min`, `OnUnitActiveSec=15min`, `AccuracySec=2min`;
//!   `Persistent=true` inert). First tick 5 min after boot.
//! - No other wake. Single-flight.
//!
//! ## Observe
//!
//! `asks.moot.observe` returns:
//!
//! ```text
//! Snapshot { asks: Vec<Ask{ id, pred }> }
//! ```
//!
//! - Source: `spira-lc content list --all --limit 0 --label-any "insight,$SPIRA_ASK_LABEL,overseer" --json`
//!   (`cockpit/db.sh:71-79`, called at `moot-sweep.sh:36`), guarded by `.beads` existing
//!   (`db.sh:34-38`, `moot-sweep.sh:32-35`). In-process: `content list` over `LcClient` (S7a).
//! - Filter: ask-labelled, status exactly `open`, carrying `^\s*MOOT-WHEN:\s*(.+)$`
//!   (`moot-sweep.sh:70-87`). An ask with no MOOT-WHEN never enters the snapshot (invariant 1,
//!   `:17-19`).
//! - Unreachable store → snapshot error; nothing decided (`moot-sweep.sh:36-39`).
//!
//! ## Decide
//!
//! 1. `decide(&Snapshot) -> Vec<Check{id, pred}>`.
//! 2. On the check outcome: `rc == 0` → `Resolve{id, reason: "auto-resolved: MOOT-WHEN
//!    predicate cleared — <pred> → exit 0. Evidence: <first 300 bytes>"}` (`:52-58`);
//!    `rc == 124` → timeout, ask stays open, counted as error (`:64-66`); any other rc → still
//!    live (`:67-68`). A failed probe never reads as cleared (invariant 2, `:21-22`).
//!
//! No carried state. Fixtures: none in-tree for the script (no `test-moot-sweep*.sh` found);
//! `resolve.rs` unit tests cover the resolve half. T5 fixtures must be captured first.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `asks.moot.check` | host | 30 s (`SPIRA_MOOT_TIMEOUT`, `:50`) | `<ask id>/<hash(pred)>/<tick>` | empty predicate; a child `bash -c`, the one fork that stays |
//! | `asks.moot.resolve` | store | 15 s | `<ask id>/resolve` | an id that is a work bead (has a lifecycle row and is no ask, `resolve.rs:61-70`); cannot-tell on either check (`resolve.rs:56-59,70`); actor fixed `claude` (invariant 3, `:24-25`) |
//! | `asks.moot.withdraw` | store | 15 s | `<work id>/withdraw/<ask id>` | terminal work bead (`resolve_main.rs:61-65`); exit 1/3 quiet (`resolve.rs:97`) |
//!
//! `resolve` folds today's sequence: for an ask on the ask machine, `spira-lc close-ask --exit
//! withdrawn` (`resolve.rs:85-93`); for a legacy ask, `lifecycle_row::close` then
//! `withdraw-ask` per `work-bead:` label (`resolve.rs:72,94-103`). Splitting the per-work-bead
//! withdraw into its own effect makes each one retryable alone.
//!
//! ## Silence
//!
//! An idle tick (no open ask carries MOOT-WHEN, or every predicate is still live) emits the
//! observe and the check outcomes only — no `Emit`, no inbox line. The summary line
//! (`moot-sweep.sh:89`) is dropped. A **timeout** is not silent today in effect (it only
//! prints); proposed: a predicate that times out three ticks running emits one inbox line
//! (new behaviour, flag for Ryan).
//!
//! ## Replaces
//!
//! - `spira-moot-sweep-prod.{service,timer}` (`TimeoutStartSec=300`). No watchd row.
//! - Children per pass: `. conf.sh` via `db.sh:24`; `spira-lc content list`; `python3`; per ask
//!   `timeout 30 bash -c <pred>`; on cleared `resolve`, which forks `timeout 5 spira-lc
//!   content show`, `spira-lc show`, `spira-lc close`, `spira-lc close-ask|withdraw-ask`
//!   (`resolve_main.rs:17-70`). **Catalogue lists `lib.sh, bd`: the code sources `conf.sh`,
//!   uses no `bd`, and forks `python3` and the `resolve` binary.**
//! - Pinned keys: `SPIRA_DB`/`COCKPIT_DB`, `SPIRA_ASK_LABEL`, `SPIRA_MOOT_TIMEOUT` (env default
//!   30, unregistered), `SPIRA_RESOLVE_SH` (test seam, `:44`).
//!
//! ## Reverse
//!
//! Release rollback restores the unit and script. `Observe` mode shadows `resolve` and
//! `withdraw`; `check` still runs, matching the script without `--apply`.
//!

use std::collections::BTreeMap;

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use crate::tasks::b::kit::{Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "asks.moot";
pub const STREAM: &str = "task.asks.moot";
pub const TICK: EventKindTag = "asks.moot.tick";
pub const STATE: EventKindTag = "asks.moot.state";

pub const OBSERVE: &str = "asks.moot.observe";
pub const CHECK: &str = "asks.moot.check";
pub const RESOLVE: &str = "asks.moot.resolve";
pub const WITHDRAW: &str = "asks.moot.withdraw";
pub const EFFECTS: &[&str] = &[OBSERVE, CHECK, RESOLVE, WITHDRAW];

/// The `timeout` exit a predicate that ran out of time returns.
pub const TIMED_OUT_RC: i32 = 124;

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_ASKS_MOOT_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_DB", "SPIRA_ASK_LABEL", "SPIRA_MOOT_TIMEOUT"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 15_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ask {
    pub id: String,
    /// The `MOOT-WHEN:` predicate.
    pub pred: String,
    /// `work-bead:` labels of a legacy ask.
    pub works: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub asks: Vec<Ask>,
}

/// The `check` runner's answer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CheckOutcome {
    pub id: String,
    pub rc: i32,
    pub output: String,
}

/// Per ask (T3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Open,
    Checking,
    Cleared,
    Resolved,
    Withdrawn,
    Live,
    TimedOut,
}

/// `state × event → state` per ask. `resolved` from `timed_out` is refused naming `timed_out`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[
    (Phase::Open, "check", Phase::Checking),
    (Phase::Live, "check", Phase::Checking),
    (Phase::TimedOut, "check", Phase::Checking),
    (Phase::Checking, "rc:0", Phase::Cleared),
    (Phase::Checking, "rc:124", Phase::TimedOut),
    (Phase::Checking, "rc:other", Phase::Live),
    (Phase::Cleared, "resolve:ok", Phase::Resolved),
    (Phase::Resolved, "withdraw:ok", Phase::Withdrawn),
];

/// None in the old script; the per-ask phase and consecutive timeouts (three running emit one
/// inbox line, proposed) are carried.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub asks: BTreeMap<String, Phase>,
    pub timeouts: BTreeMap<String, u32>,
}

pub struct AsksMoot {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// One check per MOOT-WHEN ask; `rc == 0` resolves, 124 is a timeout, any other rc is live.
pub fn decide(_cfg: &Cfg, _carry: &Carry, _input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    unimplemented!("asks.moot: not ported yet")
}

crate::tasks::b::hosted!(AsksMoot);
