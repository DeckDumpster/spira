//! # reconcile.continuity — contract
//!
//! Family B of the scheduler consolidation, red-first: `decide` is not ported yet. The
//! sections below are the contract page `wiki/projects/spira/designs/tasks/reconcile-continuity.md`;
//! its Acceptance section is the test file `tests/task_b_reconcile_continuity.rs`.
//!
//! ## Tick
//!
//! - `Timer`, every 15 min (`OnUnitActiveSec=15min`, `OnBootSec=5min`, `AccuracySec=1min`).
//!   Boot tick at +5 min. No event wakes it. Single-flight.
//!
//! ## Observe
//!
//! `reconcile.continuity.observe` is a Probe that calls the store verb `event-continuity`, with
//! no flags. The snapshot schema:
//!
//! ```text
//! Snapshot { breaks: Vec<Break> }   // Break { machine, key, seq, event, from_state,
//!                                    //         previous_to_state, acknowledged: Option<String> }
//! ```
//!
//! - **Producer.** `cmd_event_continuity` (`cutover.rs:1721-1779`). It reads
//!   `continuity::current(conn, live.continuity())` (`cutover.rs:1740-1742`), which applies
//!   `SELECT … FROM event WHERE applied = 1 AND seq > upto` (`continuity.rs:56-59`). It joins the
//!   result with the acknowledgement ledger `$SPIRA_RUN/event-continuity.acknowledged`
//!   (`cutover.rs:1722-1739`).
//! - **Exit codes.** 3 means at least one unacknowledged break and is the answer, not a failure
//!   (`cutover.rs:1778`). 2 is "cannot tell".
//!
//! ## Decide
//!
//! ```text
//! decide(carry: Carry { announced: BTreeSet<(machine, key, seq)> }, snap)
//!     -> (Carry, Vec<Line>)
//! ```
//!
//! - A line is announced once per new unacknowledged break.
//! - An acknowledged break is never announced.
//! - A break that vanishes after a daily rescan (`continuity.rs:14,90`) is dropped from the
//!   carried set.
//! - **Fixtures:** clean; one fresh break; the same break on the next tick; a break that is
//!   acknowledged later; a store that cannot tell.
//!
//! **Code over today's behaviour.** Today the unit prints every break, acknowledged ones
//! included, on every run whenever any break exists (`cutover.rs:1769-1777`, with stdout appended
//! to `event-continuity.log`). The watchd `log` row (`spira/watchers:142`) therefore re-alarms
//! every 15 min. The carried set fixes that. This is a deliberate parity break and is listed
//! under T5.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `reconcile.continuity.observe` (Probe) → store `event-continuity` | store | 5 s (`spira-lc/src/client.rs:18`) | none (read) | `cannot tell` when `SPIRA_RUN` does not resolve or the ledger has a malformed row (`cutover.rs:1724,1732`) |
//! | `inbox.append` (shared runner) | host | 10 s | `continuity/<machine>/<key>/<seq>` | a line with no `[watch:reconcile.continuity]` tag |
//!
//! `--acknowledge` stays an operator verb on the store and is not a task effect
//! (`cutover.rs:1748-1767`).
//!
//! ## Silence
//!
//! A clean store, or one whose breaks are all already announced or acknowledged, yields nothing:
//! no Emit and no effect. Only the in-memory probe and its outcome happen.
//!
//! ## Replaces
//!
//! - The `spira-event-continuity-prod.{service,timer}` units.
//! - `StandardOutput=append:$SPIRA_RUN/event-continuity.log`.
//! - The watchd `event-continuity|log` row (`spira/watchers:140-142`). It is retired once the line
//!   goes through `inbox.append`.
//! - `spira/test-event-continuity-units.sh`, which asserts the unit files and is replaced by T1.
//! - Children: one `spira-lc` exec every 15 min.
//! - Config keys: `SPIRA_RUN` (read server-side, unchanged) and
//!   `tasks.reconcile_continuity.interval_ms = 900000`.
//!
//! ## Reverse
//!
//! Release rollback restores the unit and the watchd row, or the task is set to `Observe`, which
//! shadows the `inbox.append`. The store's tracker is unaffected either way.
//!

use std::collections::BTreeMap;

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use super::kit::{Cadence, Cfg, Decision, Input, Spec};
pub use crate::watchers::INBOX_APPEND;

pub const NAME: &str = "reconcile.continuity";
pub const STREAM: &str = "task.reconcile.continuity";
pub const TICK: EventKindTag = "reconcile.continuity.tick";
pub const STATE: EventKindTag = "reconcile.continuity.state";

pub const OBSERVE: &str = "reconcile.continuity.observe";
/// The tag every line this task appends carries.
pub const TAG: &str = "[watch:reconcile.continuity]";
pub const EFFECTS: &[&str] = &[OBSERVE, INBOX_APPEND];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_RECONCILE_CONTINUITY_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 5_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Break {
    pub machine: String,
    pub key: String,
    pub seq: u64,
    pub event: String,
    pub from_state: String,
    pub previous_to_state: String,
    pub acknowledged: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub breaks: Vec<Break>,
}

/// Per break (T3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Unseen,
    Announced,
    Acknowledged,
    Gone,
}

/// `state × event → state` per break. Announcing an announced break is refused, naming
/// `announced`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[(Phase::Unseen, "break", Phase::Announced), (Phase::Unseen, "acknowledged", Phase::Acknowledged), (Phase::Announced, "acknowledged", Phase::Acknowledged), (Phase::Announced, "vanished", Phase::Gone)];

/// `announced`, keyed `machine/key/seq`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub breaks: BTreeMap<String, Phase>,
}

pub struct ReconcileContinuity {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// One line per new unacknowledged break, once.
pub fn decide(_cfg: &Cfg, _carry: &Carry, _input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    unimplemented!("reconcile.continuity: not ported yet")
}

super::hosted!(ReconcileContinuity);
