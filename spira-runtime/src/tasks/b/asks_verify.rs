//! # asks.verify — contract
//!
//! Family B of the scheduler consolidation, red-first: `decide` is not ported yet. The
//! sections below are the contract page `wiki/projects/spira/designs/tasks/asks-verify.md`;
//! its Acceptance section is the test file `tests/task_b_asks_verify.rs`.
//!
//! ## Tick
//!
//! - Cadence **15 min** (unit: `OnBootSec=7min`, `OnUnitActiveSec=15min`, `AccuracySec=2min`;
//!   `Persistent=true` inert). First tick 7 min after boot.
//! - No other wake. Single-flight: a pass can take minutes (each check up to 120 s,
//!   `verify-asks.sh:59`), and a tick during one is dropped.
//!
//! ## Observe
//!
//! `asks.verify.observe` returns:
//!
//! ```text
//! Snapshot { asks: Vec<Ask{ id, works: Vec<String>, cmd }>, mislabelled: Vec<(id, kind, title)> }
//! ```
//!
//! - Source: **one** fetch of every bead, `spira-lc content list --all --limit 0 --json`
//!   (`cockpit/db.sh:49-55`, called at `verify-asks.sh:47`), guarded by `.beads` existing
//!   (`db.sh:34-38`, `verify-asks.sh:43-46`). In-process: `content list` over `LcClient` (S7a),
//!   narrowed server-side with `--label-any $SPIRA_ASK_LABEL` (the full list today is wasted;
//!   `db.sh:57-64` measured the cost).
//! - `asks`: ask-labelled, not closed, not epic, carrying `^\s*VERIFY:\s*(.+)$` in the
//!   description; `works` from `work-bead:<id>` labels (`verify-asks.sh:93-113`).
//! - `mislabelled`: ask-labelled open epics (`verify-asks.sh:126-141`).
//! - Unreachable store → snapshot error; decide does nothing (`verify-asks.sh:47-50`).
//!
//! The VERIFY command itself is **not** an observe: it is arbitrary operator shell and runs as
//! an effect (below), its exit code returning as that effect's outcome.
//!
//! ## Decide
//!
//! Two steps, both pure:
//!
//! 1. `decide(&Snapshot) -> Vec<Check{id, cmd}>` — one check per ask; plus one inbox line per
//!    mislabelled epic **only when the set changes** (carried `mislabelled_seen`), replacing
//!    the every-15-min `MISLABELLED` stdout (`verify-asks.sh:124-142`).
//! 2. On a check's outcome: `rc == 0` → `Close{id, reason: "Verified already done — its own
//!    VERIFY check now passes: <cmd> → exit 0. Evidence: <first 400 bytes>"}`
//!    (`verify-asks.sh:59-68`); then for each work bead not terminal (`LANDED|SUPERSEDED|
//!    DROPPED|DONE`, `:79`) a `Withdraw{work, actor: claude}` (`:75-87`). `rc != 0` → nothing.
//!
//! Carried state: `mislabelled_seen`. Fixtures: `spira/test-verify-asks.sh`.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `asks.verify.check` | host | 120 s (`SPIRA_VERIFY_TIMEOUT`, `:59`) | `<ask id>/<hash(cmd)>/<tick>` | an empty command; it is read-only by convention only (`:24-26`) — a child `bash -c`, the one fork that stays |
//! | `asks.verify.close` | store | 15 s | `<ask id>/close` | an already-closed ask (lifecycle exit is quiet); actor fixed `claude` (`:67`) |
//! | `asks.verify.withdraw` | store | 15 s | `<work id>/withdraw/<ask id>` | terminal work bead; exit 1/3 quiet, other exit → warning line (`:81-85`) |
//! | `asks.verify.note` | host | 10 s | `mislabelled/<sorted ids hash>` | unchanged set; becomes `inbox.append` |
//!
//! `check` runs under the S3 `host` class cap; a 120 s check must not hold a `store` permit.
//!
//! ## Silence
//!
//! An idle tick (no ask carries VERIFY, or every check fails, and the mislabelled set is
//! unchanged) emits the observe probe and the check effects' outcomes only — no journaled
//! `Emit`, no inbox line. Today it prints a summary and the `(none yet …)` hint every pass
//! (`verify-asks.sh:144-145`); dropped. Open question: a check per ask per tick is itself
//! traffic — `check` outcomes should be `Probe`-class (unjournaled) when `rc != 0`.
//!
//! ## Replaces
//!
//! - `spira-verify-asks-prod.{service,timer}` (`CPUQuota=25%`, `Nice=10`,
//!   `TimeoutStartSec=600`). No watchd row.
//! - Children per pass: `. conf.sh` via `db.sh:24`; `spira-lc content list`; two `python3`;
//!   per ask `timeout 120 bash -c <cmd>`; on satisfied `spira-lc close`, `spira-lc state`,
//!   `spira-lc withdraw-ask`. **Catalogue lists `lib.sh, spira-lc, bd`: the code sources
//!   `conf.sh` (not `lib.sh`), uses no `bd`, and forks `python3` and `bash -c`.**
//! - Pinned keys: `SPIRA_DB`/`COCKPIT_DB` (`db.sh:27`), `SPIRA_ASK_LABEL`,
//!   `SPIRA_VERIFY_TIMEOUT` (env default 120, not registered; joins `Settings::KEYS`),
//!   `SPIRA_LC_BIN` (test seam, `db.sh:30`).
//!
//! ## Reverse
//!
//! Release rollback restores the unit and the script. `Observe` mode shadows `close` and
//! `withdraw`; `check` still runs (it is the read), matching the script without `--apply`.
//!

use std::collections::{BTreeMap, BTreeSet};

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use super::kit::{Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "asks.verify";
pub const STREAM: &str = "task.asks.verify";
pub const TICK: EventKindTag = "asks.verify.tick";
pub const STATE: EventKindTag = "asks.verify.state";

pub const OBSERVE: &str = "asks.verify.observe";
pub const CHECK: &str = "asks.verify.check";
pub const CLOSE: &str = "asks.verify.close";
pub const WITHDRAW: &str = "asks.verify.withdraw";
pub const NOTE: &str = "asks.verify.note";
pub const EFFECTS: &[&str] = &[OBSERVE, CHECK, CLOSE, WITHDRAW, NOTE];

/// Work-bead states a withdraw skips.
pub const TERMINAL: &[&str] = &["LANDED", "SUPERSEDED", "DROPPED", "DONE"];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_ASKS_VERIFY_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_DB", "SPIRA_ASK_LABEL", "SPIRA_VERIFY_TIMEOUT"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 15_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ask {
    pub id: String,
    pub works: Vec<String>,
    /// The `VERIFY:` command.
    pub cmd: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub asks: Vec<Ask>,
    /// (id, kind, title) of ask-labelled open epics.
    pub mislabelled: Vec<(String, String, String)>,
    /// Lifecycle state of each work bead the asks name.
    pub work_states: BTreeMap<String, String>,
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
    Satisfied,
    Closed,
    Withdrawn,
    StillOpen,
}

/// `state × event → state` per ask. `closed` without `satisfied` is refused naming `checking`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[
    (Phase::Open, "check", Phase::Checking),
    (Phase::StillOpen, "check", Phase::Checking),
    (Phase::Checking, "rc:0", Phase::Satisfied),
    (Phase::Checking, "rc:nonzero", Phase::StillOpen),
    (Phase::Satisfied, "close:ok", Phase::Closed),
    (Phase::Closed, "withdraw:ok", Phase::Withdrawn),
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub mislabelled_seen: BTreeSet<String>,
    pub asks: BTreeMap<String, Phase>,
}

pub struct AsksVerify {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// One check per ask; on `rc == 0` a close then a withdraw per non-terminal work bead.
pub fn decide(_cfg: &Cfg, _carry: &Carry, _input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    unimplemented!("asks.verify: not ported yet")
}

super::hosted!(AsksVerify);
