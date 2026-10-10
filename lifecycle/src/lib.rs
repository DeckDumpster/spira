//! lifecycle — the pure transition machines (bead, delivery, batch, ask) and the event
//! log fold that replays them. Every function here is pure: no I/O, no clock, no git.
//! A caller that needs the current time or a git fact computes it and passes it in as
//! evidence on the event; nothing in this crate ever reaches out for one.

pub mod ask;
pub mod batch;
pub mod bead;
pub mod classify;
pub mod delivery;
pub mod provenance;
pub mod reason;
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
    /// A `claim`'s proposed `stack_depth` exceeds `stack_max_depth` (design stacked-
    /// dependents-2026-09-28 §1: "the meter and the cap").
    DepthExceeded { depth: u32, max: u32 },
    /// A `base_withdrawn` (or `prereq_landed`) named a `(prereq, tip)` the row's own `stack`
    /// does not carry — a misrouted or stale cascade, not evidence about this row.
    NotInStack { prereq: String, tip: String },
    /// `submit` while the row's `stack` still names a tip `base_withdrawn` disowned, or a
    /// `claim` whose proposed `stack` names a prerequisite tip that is no longer current.
    StackStale { prereqs: Vec<String> },
    /// The row holds an `ask`: an escalation gate closes only through a `reply` carrying the
    /// reply's message id (`law-a-gate-closes-on-the-reply`), never on marker text alone.
    /// `exit` names the event that does lift it.
    AwaitingReply { event: String, exit: String },
    /// A holder-only event (`renew`) sent by an actor that does not hold the row: a reaped
    /// aeon's late renewal must never extend the lease its successor now holds.
    NotHolder { actor: String, holder: Option<String> },
    /// A `manual` hold with no reason, a bare bead id (that is a dependency edge), or a
    /// snooze (that is a `wait` hold). `exit` names the mechanism that does the job.
    ManualHoldReason { reason: String, exit: String },
    /// `submit` at the tip a round already ejected as red for this bead: the same tree is
    /// red again, so only a changed tip can be submitted.
    EjectedRedTip { tip: String },
    /// `claim` of a REWORK row after a red gate verdict, proposing the very stack the red
    /// work was built on: only a moved prerequisite tip can change the outcome.
    StackUnchanged { prereqs: Vec<String> },
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
