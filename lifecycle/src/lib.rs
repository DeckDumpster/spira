//! lifecycle — the three pure transition machines (bead, delivery, batch) and the event
//! log fold that replays them. Every function here is pure: no I/O, no clock, no git.
//! A caller that needs the current time or a git fact computes it and passes it in as
//! evidence on the event; nothing in this crate ever reaches out for one.

pub mod batch;
pub mod bead;
pub mod classify;
pub mod delivery;
pub mod replay;

/// The row-version a compare-and-swap checks. Every applied transition increments it by
/// exactly one; a refused one leaves it untouched.
pub type Version = u64;

/// Why a machine refused an event. A refusal is not an error: the producer acted on a
/// stale or mistaken view, and the row is returned unchanged (`law-a-refusal-names-its-exit`
/// applies to callers of this crate, not to the crate itself, but the same discipline holds:
/// every refusal names the check that failed).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Refusal {
    /// The event's `expect` did not match the row's current state.
    ExpectMismatch { expected: String, actual: String },
    /// The event's `version` did not match the row's current version.
    StaleVersion { given: Version, current: Version },
    /// No transition exists for this (state, event) pair.
    IllegalTransition { state: String, event: String },
    /// The event's evidence names a tip other than the row's tip.
    TipMismatch { event_tip: String, row_tip: String },
    /// The row is in a terminal state; terminal states have no outgoing transitions.
    Terminal { state: String },
}

/// The result of applying one event to one row. `row` is the new row when `applied` is
/// true, and the untouched input row when it is false — a refusal never mutates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome<Row> {
    pub applied: bool,
    pub row: Row,
    pub refusal: Option<Refusal>,
}

impl<Row> Outcome<Row> {
    pub fn applied(row: Row) -> Self {
        Outcome { applied: true, row, refusal: None }
    }

    pub fn refuse(row: Row, refusal: Refusal) -> Self {
        Outcome { applied: false, row, refusal: Some(refusal) }
    }
}
