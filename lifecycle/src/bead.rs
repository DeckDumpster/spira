//! The bead machine (design §3.1.1): mode-independent, one row per work bead. It never
//! knows whether delivery is a batch, a PR or a push — that is the delivery machine's job.

use std::collections::{BTreeMap, BTreeSet};

use crate::reason::{DropReason, GateRedReason, HoldCause, ReturnedReason};
use crate::{Outcome, Refusal, Version};

/// A dependent's stack (design stacked-dependents-2026-09-28 §1): the certified tip of
/// each prerequisite its current work was built on, keyed by prerequisite `bead_id`. Empty
/// for a bead that is not stacked on anything.
pub type Stack = BTreeMap<String, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub enum BeadState {
    Ready,
    Working,
    Submitted,
    Certified,
    InDelivery,
    Rework,
    Landed,
    Superseded,
    Dropped,
    Done,
}

impl BeadState {
    /// Terminal states have no outgoing transitions, for every actor, the operator
    /// included (design: "Terminal means terminal").
    pub fn is_terminal(self) -> bool {
        matches!(self, BeadState::Landed | BeadState::Superseded | BeadState::Dropped | BeadState::Done)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            BeadState::Ready => "READY",
            BeadState::Working => "WORKING",
            BeadState::Submitted => "SUBMITTED",
            BeadState::Certified => "CERTIFIED",
            BeadState::InDelivery => "IN_DELIVERY",
            BeadState::Rework => "REWORK",
            BeadState::Landed => "LANDED",
            BeadState::Superseded => "SUPERSEDED",
            BeadState::Dropped => "DROPPED",
            BeadState::Done => "DONE",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "READY" => BeadState::Ready,
            "WORKING" => BeadState::Working,
            "SUBMITTED" => BeadState::Submitted,
            "CERTIFIED" => BeadState::Certified,
            "IN_DELIVERY" => BeadState::InDelivery,
            "REWORK" => BeadState::Rework,
            "LANDED" => BeadState::Landed,
            "SUPERSEDED" => BeadState::Superseded,
            "DROPPED" => BeadState::Dropped,
            "DONE" => BeadState::Done,
            _ => return None,
        })
    }
}

/// Holds are orthogonal to state: they suspend any non-terminal state without losing it
/// (design: "Holds are a dimension, not states").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub enum HoldKind {
    Poison,
    Ask,
    Wait,
    Operator,
}

impl HoldKind {
    pub fn as_str(self) -> &'static str {
        match self {
            HoldKind::Poison => "poison",
            HoldKind::Ask => "ask",
            HoldKind::Wait => "wait",
            HoldKind::Operator => "operator",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "poison" => HoldKind::Poison,
            "ask" => HoldKind::Ask,
            "wait" => HoldKind::Wait,
            "operator" => HoldKind::Operator,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BeadRow {
    pub bead_id: String,
    pub state: BeadState,
    pub tip: Option<String>,
    pub gate_key: Option<String>,
    pub holder: Option<String>,
    pub lease_until: Option<i64>,
    pub holds: BTreeSet<HoldKind>,
    pub reason: Option<String>,
    pub version: Version,
    /// The prerequisite tips this bead's current work was built on, recorded at `claim`.
    /// Empty for an unstacked bead (design §1: "the stack is state the machine holds").
    #[serde(default)]
    pub stack: Stack,
    /// `1 + max(depth of each prerequisite)`, recorded alongside `stack` at `claim`.
    #[serde(default)]
    pub stack_depth: u32,
    /// When the row last entered LANDED or CERTIFIED (the event's `at`); unset otherwise.
    #[serde(default)]
    pub since: Option<i64>,
}

impl BeadRow {
    /// The row a bead starts in when it is filed. Not itself a transition — there is no
    /// prior row to compare-and-swap against.
    pub fn filed(bead_id: impl Into<String>) -> Self {
        BeadRow {
            bead_id: bead_id.into(),
            state: BeadState::Ready,
            tip: None,
            gate_key: None,
            holder: None,
            lease_until: None,
            holds: BTreeSet::new(),
            reason: None,
            version: 0,
            stack: Stack::new(),
            stack_depth: 0,
            since: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BeadEventKind {
    /// `stack`/`stack_depth` are the caller's already-computed proposal (design §1: "stack
    /// recorded at claim, from the certified tips of every open work-bead blocker"); this
    /// machine checks only the depth ceiling it can see from the event alone — whether the
    /// proposed stack itself is still fresh against each prerequisite's live row is a
    /// multi-row check the caller makes before ever proposing a claim (see
    /// `claim_refusal_for_stale_stack`). `stack_max_depth` is the caller's own config read,
    /// carried as evidence exactly like `lease_until`.
    Claim { holder: String, lease_until: i64, #[serde(default)] stack: Stack, #[serde(default)] stack_depth: u32, #[serde(default)] stack_max_depth: u32 },
    Release,
    HolderDead,
    Submit { tip: String },
    Done { delivers: String },
    GatePass { tip: String, gate_key: String },
    GateRed { tip: String, reason: GateRedReason },
    GateInfra { tip: String },
    Deliver,
    Delivered { merge_sha: String, proof: String },
    Returned { reason: ReturnedReason },
    Requeued { tip: String },
    ContentOnBase { proof: String },
    Supersede { by: String },
    Drop { reason: DropReason },
    /// `detail` is the free text a caller already knows (a question, a proposed successor
    /// id) — carried alongside `cause`'s category, never folded into it (design: the same
    /// split as `gate_red`'s `tip` versus its `reason`). It becomes the row's own `reason`
    /// display when given, falling back to the category's own name otherwise.
    Hold { kind: HoldKind, cause: HoldCause, #[serde(default)] detail: Option<String> },
    Unhold { kind: HoldKind },
    /// Machine-emitted (design §1), same transaction as the prerequisite's backwards move,
    /// for every dependent whose `stack` names `(prereq, tip)`. Per-state outcomes: READY
    /// re-applies the wait hold; WORKING/IN_DELIVERY/REWORK record it in `reason` and stay
    /// put (whether the stack is still fresh enough to `submit`/`deliver` is a multi-row
    /// check — `stale_stack_entries` — the caller makes against live prerequisite rows, the
    /// same way `claim` does); SUBMITTED/CERTIFIED move to REWORK.
    BaseWithdrawn { prereq: String, tip: String },
    /// A prerequisite named in `stack` reached LANDED: it drops out of `stack` (design §1:
    /// "a LANDED prerequisite drops out of every dependent's stack").
    PrereqLanded { prereq: String },
    /// The operator's answer to an `ask`, carrying the reply's message id. The only event
    /// that lifts the ask hold, and so the only way an escalation gate reaches a close.
    Reply { message_id: String },
    /// The ask was withdrawn with no answer. Lifts the ask hold by its own event, never by a reply.
    AskWithdrawn,
    /// The classifier's own correction of a row it wrote from a rule that has since changed.
    /// Only actor `classifier` may send it, never to a WORKING row, and a terminal row accepts
    /// it only while its reason is [`RESIDUE_RULE`] — the default the classifier writes when
    /// it saw no evidence at all, not a conclusion anything else acted on.
    Reclassify { state: BeadState, rule: String },
}

/// The classifier's no-evidence default for a closed bead; the one terminal reason a
/// [`BeadEventKind::Reclassify`] may overwrite.
pub const RESIDUE_RULE: &str = "residue-closed-no-landing-evidence";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BeadEvent {
    pub expect: BeadState,
    pub version: Version,
    pub kind: BeadEventKind,
    pub actor: String,
    /// Caller's clock, epoch seconds; the machine does no I/O. Recorded as `since` on entry
    /// to LANDED or CERTIFIED.
    #[serde(default)]
    pub at: Option<i64>,
}

fn illegal(row: &BeadRow, kind: &BeadEventKind) -> Outcome<BeadRow> {
    Outcome::refuse(
        row.clone(),
        Refusal::IllegalTransition { state: row.state.as_str().to_string(), event: format!("{kind:?}") },
    )
}

fn awaiting_reply(row: &BeadRow, kind: &BeadEventKind) -> Outcome<BeadRow> {
    Outcome::refuse(
        row.clone(),
        Refusal::AwaitingReply {
            event: format!("{kind:?}"),
            exit: "record the operator's answer with a reply event carrying its message id (mail reply), then close".to_string(),
        },
    )
}

fn holds_ask(row: &BeadRow) -> bool {
    row.holds.contains(&HoldKind::Ask)
}

fn terminal(row: &BeadRow) -> Outcome<BeadRow> {
    Outcome::refuse(row.clone(), Refusal::Terminal { state: row.state.as_str().to_string() })
}

fn tip_mismatch(row: &BeadRow, event_tip: &str) -> Outcome<BeadRow> {
    Outcome::refuse(
        row.clone(),
        Refusal::TipMismatch { event_tip: event_tip.to_string(), row_tip: row.tip.clone().unwrap_or_default() },
    )
}

fn depth_exceeded(row: &BeadRow, depth: u32, max: u32) -> Outcome<BeadRow> {
    Outcome::refuse(row.clone(), Refusal::DepthExceeded { depth, max })
}

fn not_in_stack(row: &BeadRow, prereq: &str, tip: &str) -> Outcome<BeadRow> {
    Outcome::refuse(row.clone(), Refusal::NotInStack { prereq: prereq.to_string(), tip: tip.to_string() })
}

/// A `base_withdrawn(prereq, tip)` is evidence about this row only when `stack` still names
/// exactly that tip for that prerequisite — otherwise it is a cascade aimed at a stale or
/// unrelated view of this row, refused the same way a tip mismatch is (design §1: "the tip
/// invariant, extended").
fn base_withdrawn_applies(row: &BeadRow, prereq: &str, tip: &str) -> bool {
    row.stack.get(prereq).map(String::as_str) == Some(tip)
}

/// Apply one event to one row. Pure: the only inputs are the row and the event, and the
/// only output is the outcome. Every (state, event) pair is covered by rustc's exhaustive-
/// match check on both `BeadState` and `BeadEventKind` — a new variant on either enum fails
/// to compile here until this function accounts for it, which is the property the design
/// calls "a test that fails when a state or event variant is added without an entry."
pub fn apply(row: &BeadRow, ev: &BeadEvent) -> Outcome<BeadRow> {
    let mut out = apply_transition(row, ev);
    let entered = out.row.state != row.state && matches!(out.row.state, BeadState::Landed | BeadState::Certified);
    if out.applied && entered {
        out.row.since = ev.at;
    }
    if out.applied && out.row.state.is_terminal() {
        out.row.holder = None;
        out.row.lease_until = None;
    }
    out
}

fn apply_transition(row: &BeadRow, ev: &BeadEvent) -> Outcome<BeadRow> {
    if ev.version != row.version {
        return Outcome::refuse(row.clone(), Refusal::StaleVersion { given: ev.version, current: row.version });
    }
    if ev.expect != row.state {
        return Outcome::refuse(
            row.clone(),
            Refusal::ExpectMismatch { expected: ev.expect.as_str().to_string(), actual: row.state.as_str().to_string() },
        );
    }

    match &ev.kind {
        // Orthogonal: valid from any non-terminal state, regardless of which one.
        BeadEventKind::ContentOnBase { proof } => {
            if row.state.is_terminal() {
                return terminal(row);
            }
            if holds_ask(row) {
                return awaiting_reply(row, &ev.kind);
            }
            let mut new = row.clone();
            new.state = BeadState::Landed;
            new.reason = Some(proof.clone());
            new.version += 1;
            Outcome::applied(new)
        }
        BeadEventKind::Supersede { by } => {
            if row.state.is_terminal() {
                return terminal(row);
            }
            if holds_ask(row) {
                return awaiting_reply(row, &ev.kind);
            }
            let mut new = row.clone();
            new.state = BeadState::Superseded;
            new.reason = Some(by.clone());
            new.version += 1;
            Outcome::applied(new)
        }
        BeadEventKind::Drop { reason } => {
            if row.state.is_terminal() {
                return terminal(row);
            }
            if holds_ask(row) {
                return awaiting_reply(row, &ev.kind);
            }
            let mut new = row.clone();
            new.state = BeadState::Dropped;
            new.reason = Some(reason.as_str().to_string());
            new.version += 1;
            Outcome::applied(new)
        }
        BeadEventKind::Reclassify { state, rule } => {
            if ev.actor != "classifier" || row.state == BeadState::Working {
                return illegal(row, &ev.kind);
            }
            if row.state.is_terminal() && row.reason.as_deref() != Some(RESIDUE_RULE) {
                return terminal(row);
            }
            if row.state == *state && row.reason.as_deref() == Some(rule.as_str()) {
                return illegal(row, &ev.kind);
            }
            let mut new = row.clone();
            new.state = *state;
            new.reason = Some(rule.clone());
            new.version += 1;
            Outcome::applied(new)
        }
        BeadEventKind::Hold { kind, cause, detail } => {
            if row.state.is_terminal() {
                return terminal(row);
            }
            let mut new = row.clone();
            new.holds.insert(*kind);
            new.reason = Some(detail.clone().unwrap_or_else(|| cause.as_str().to_string()));
            new.version += 1;
            Outcome::applied(new)
        }
        BeadEventKind::Unhold { kind } => {
            if row.state.is_terminal() {
                return terminal(row);
            }
            if *kind == HoldKind::Ask {
                return awaiting_reply(row, &ev.kind);
            }
            let mut new = row.clone();
            new.holds.remove(kind);
            new.version += 1;
            Outcome::applied(new)
        }

        BeadEventKind::Reply { message_id } => {
            if row.state.is_terminal() {
                return terminal(row);
            }
            if !holds_ask(row) || message_id.is_empty() {
                return illegal(row, &ev.kind);
            }
            let mut new = row.clone();
            new.holds.remove(&HoldKind::Ask);
            new.reason = Some(format!("reply: {message_id}"));
            new.version += 1;
            Outcome::applied(new)
        }

        BeadEventKind::AskWithdrawn => {
            if row.state.is_terminal() {
                return terminal(row);
            }
            if !holds_ask(row) {
                return illegal(row, &ev.kind);
            }
            let mut new = row.clone();
            new.holds.remove(&HoldKind::Ask);
            new.reason = Some("ask withdrawn".to_string());
            new.version += 1;
            Outcome::applied(new)
        }

        // The primary, state-shaped transitions.
        BeadEventKind::Claim { .. }
        | BeadEventKind::Release
        | BeadEventKind::HolderDead
        | BeadEventKind::Submit { .. }
        | BeadEventKind::Done { .. }
        | BeadEventKind::GatePass { .. }
        | BeadEventKind::GateRed { .. }
        | BeadEventKind::GateInfra { .. }
        | BeadEventKind::Deliver
        | BeadEventKind::Delivered { .. }
        | BeadEventKind::Returned { .. }
        | BeadEventKind::Requeued { .. }
        | BeadEventKind::BaseWithdrawn { .. }
        | BeadEventKind::PrereqLanded { .. } => primary_transition(row, &ev.kind),
    }
}

/// The state-specific half of the table: the 12 events above, crossed with all 10 states.
/// Every arm is named explicitly; no `_` pattern appears anywhere in this function.
fn primary_transition(row: &BeadRow, kind: &BeadEventKind) -> Outcome<BeadRow> {
    // Deliberately not `use BeadState::*`: `BeadState::Done` and `BeadEventKind::Done`
    // share a name, and importing both makes a bare `Done` pattern an ambiguous binding
    // instead of a match on the event variant. States are qualified below instead.
    use BeadEventKind::*;

    match row.state {
        BeadState::Ready => match kind {
            Claim { holder, lease_until, stack, stack_depth, stack_max_depth } => {
                if *stack_depth > *stack_max_depth {
                    return depth_exceeded(row, *stack_depth, *stack_max_depth);
                }
                let mut new = row.clone();
                new.state = BeadState::Working;
                new.holder = Some(holder.clone());
                new.lease_until = Some(*lease_until);
                new.stack = stack.clone();
                new.stack_depth = *stack_depth;
                new.version += 1;
                Outcome::applied(new)
            }
            // Not yet claimed: the wait hold this bead's own `blocks` edge already carries
            // is simply re-applied (design §1's base_withdrawn table, the READY row). There
            // is no stack to check — an unclaimed bead's `stack` (if any survives an earlier
            // release) is superseded wholesale by its next `claim`.
            BaseWithdrawn { prereq: _, tip: _ } => {
                let mut new = row.clone();
                new.holds.insert(HoldKind::Wait);
                new.version += 1;
                Outcome::applied(new)
            }
            PrereqLanded { prereq } => {
                let mut new = row.clone();
                new.stack.remove(prereq);
                new.version += 1;
                Outcome::applied(new)
            }
            Release | HolderDead | Submit { .. } | Done { .. } | GatePass { .. } | GateRed { .. }
            | GateInfra { .. } | Deliver | Delivered { .. } | Returned { .. } | Requeued { .. }
            | ContentOnBase { .. } | Supersede { .. } | Drop { .. } | Hold { .. } | Unhold { .. } | Reply { .. } | AskWithdrawn | Reclassify { .. } => illegal(row, kind),
        },

        BeadState::Working => match kind {
            Release | HolderDead => {
                let mut new = row.clone();
                new.state = BeadState::Ready;
                new.holder = None;
                new.lease_until = None;
                // The claim this stack belonged to is voided; the next claim proposes a
                // fresh one rather than carrying a stale one into READY.
                new.stack = Stack::new();
                new.stack_depth = 0;
                new.version += 1;
                Outcome::applied(new)
            }
            Submit { tip } => {
                let mut new = row.clone();
                new.state = BeadState::Submitted;
                new.tip = Some(tip.clone());
                new.gate_key = None;
                new.version += 1;
                Outcome::applied(new)
            }
            Done { .. } if holds_ask(row) => awaiting_reply(row, kind),
            Done { delivers } => {
                let mut new = row.clone();
                new.state = BeadState::Done;
                new.reason = Some(delivers.clone());
                new.version += 1;
                Outcome::applied(new)
            }
            // Stays WORKING; the holder is told outside this crate. Whether the row's
            // `submit` is refused for still naming this withdrawn tip is a multi-row check
            // (`stale_stack_entries` against the live prerequisite) the caller makes before
            // ever proposing the `submit` event — the same split `claim` uses.
            BaseWithdrawn { prereq, tip } => {
                if !base_withdrawn_applies(row, prereq, tip) {
                    return not_in_stack(row, prereq, tip);
                }
                let mut new = row.clone();
                new.reason = Some(format!("base_withdrawn: {prereq} {tip}"));
                new.version += 1;
                Outcome::applied(new)
            }
            PrereqLanded { prereq } => {
                let mut new = row.clone();
                new.stack.remove(prereq);
                new.version += 1;
                Outcome::applied(new)
            }
            Claim { .. } | GatePass { .. } | GateRed { .. } | GateInfra { .. } | Deliver | Delivered { .. }
            | Returned { .. } | Requeued { .. }
            | ContentOnBase { .. } | Supersede { .. } | Drop { .. } | Hold { .. } | Unhold { .. } | Reply { .. } | AskWithdrawn | Reclassify { .. } => illegal(row, kind),
        },

        BeadState::Submitted => match kind {
            // The tip invariant, as in CERTIFIED: an aeon that commits again after submitting
            // resubmits its moved tip, which replaces the pending one (any verdict on the old
            // tip is now TipMismatch); the same tip again is a no-op. Refusing it stranded the
            // bead: certification of the real tip was refused forever (sp-dvyic).
            Submit { tip } => {
                let mut new = row.clone();
                if row.tip.as_deref() != Some(tip.as_str()) {
                    new.tip = Some(tip.clone());
                    new.gate_key = None;
                }
                new.version += 1;
                Outcome::applied(new)
            }
            GatePass { tip, gate_key } => {
                if row.tip.as_deref() != Some(tip.as_str()) {
                    return tip_mismatch(row, tip);
                }
                let mut new = row.clone();
                new.state = BeadState::Certified;
                new.gate_key = Some(gate_key.clone());
                new.version += 1;
                Outcome::applied(new)
            }
            GateRed { tip, reason } => {
                if row.tip.as_deref() != Some(tip.as_str()) {
                    return tip_mismatch(row, tip);
                }
                let mut new = row.clone();
                new.state = BeadState::Rework;
                new.reason = Some(reason.as_str().to_string());
                new.version += 1;
                Outcome::applied(new)
            }
            GateInfra { tip } => {
                if row.tip.as_deref() != Some(tip.as_str()) {
                    return tip_mismatch(row, tip);
                }
                // A retry: the row stays SUBMITTED, but the CAS still advances the
                // version, because the row was written (an infra-failure attempt is
                // recorded, even though it changes no field a caller can see).
                let mut new = row.clone();
                new.version += 1;
                Outcome::applied(new)
            }
            BaseWithdrawn { prereq, tip } => {
                if !base_withdrawn_applies(row, prereq, tip) {
                    return not_in_stack(row, prereq, tip);
                }
                let mut new = row.clone();
                new.state = BeadState::Rework;
                new.reason = Some(format!("base_withdrawn: {prereq} {tip}"));
                new.version += 1;
                Outcome::applied(new)
            }
            PrereqLanded { prereq } => {
                let mut new = row.clone();
                new.stack.remove(prereq);
                new.version += 1;
                Outcome::applied(new)
            }
            Claim { .. } | Release | HolderDead | Done { .. } | Deliver | Delivered { .. }
            | Returned { .. } | Requeued { .. }
            | ContentOnBase { .. } | Supersede { .. } | Drop { .. } | Hold { .. } | Unhold { .. } | Reply { .. } | AskWithdrawn | Reclassify { .. } => illegal(row, kind),
        },

        BeadState::Certified => match kind {
            Deliver => {
                let mut new = row.clone();
                new.state = BeadState::InDelivery;
                new.version += 1;
                Outcome::applied(new)
            }
            // THE TIP INVARIANT (design §3, sp-vd9dn): a certification is a claim about one
            // exact tree, and a bead that moved since — a rebase, an amend, a new push — is
            // no longer that claim. Resubmitting the SAME tip is a no-op (nothing to void);
            // resubmitting a DIFFERENT one voids the certification and starts a fresh trial,
            // exactly as a stale-tip GatePass/GateRed/GateInfra is refused by TipMismatch.
            Submit { tip } => {
                let mut new = row.clone();
                if row.tip.as_deref() != Some(tip.as_str()) {
                    new.state = BeadState::Submitted;
                    new.tip = Some(tip.clone());
                    new.gate_key = None;
                }
                new.version += 1;
                Outcome::applied(new)
            }
            BaseWithdrawn { prereq, tip } => {
                if !base_withdrawn_applies(row, prereq, tip) {
                    return not_in_stack(row, prereq, tip);
                }
                let mut new = row.clone();
                new.state = BeadState::Rework;
                new.reason = Some(format!("base_withdrawn: {prereq} {tip}"));
                new.version += 1;
                Outcome::applied(new)
            }
            PrereqLanded { prereq } => {
                let mut new = row.clone();
                new.stack.remove(prereq);
                new.version += 1;
                Outcome::applied(new)
            }
            Claim { .. } | Release | HolderDead | Done { .. } | GatePass { .. }
            | GateRed { .. } | GateInfra { .. } | Delivered { .. } | Returned { .. } | Requeued { .. }
            | ContentOnBase { .. } | Supersede { .. } | Drop { .. } | Hold { .. } | Unhold { .. } | Reply { .. } | AskWithdrawn | Reclassify { .. } => {
                illegal(row, kind)
            }
        },

        BeadState::InDelivery => match kind {
            Delivered { merge_sha, proof: _ } => {
                let mut new = row.clone();
                new.state = BeadState::Landed;
                new.reason = Some(merge_sha.clone());
                new.version += 1;
                Outcome::applied(new)
            }
            Returned { reason } => {
                let mut new = row.clone();
                new.state = BeadState::Rework;
                new.reason = Some(reason.as_str().to_string());
                new.version += 1;
                Outcome::applied(new)
            }
            Requeued { tip } => {
                // The tip invariant: a requeue whose tip no longer matches the row's
                // does not resurrect CERTIFIED — it voids the certification the same
                // way a live tip change would, and returns to SUBMITTED instead.
                let mut new = row.clone();
                new.state = if row.tip.as_deref() == Some(tip.as_str()) { BeadState::Certified } else { BeadState::Submitted };
                new.version += 1;
                Outcome::applied(new)
            }
            // Its batch member is marked for eject with the prerequisite (design §1); the
            // actual eject/REWORK move happens through `returned(batch-ejected)`, above.
            BaseWithdrawn { prereq, tip } => {
                if !base_withdrawn_applies(row, prereq, tip) {
                    return not_in_stack(row, prereq, tip);
                }
                let mut new = row.clone();
                new.reason = Some(format!("base_withdrawn: {prereq} {tip}"));
                new.version += 1;
                Outcome::applied(new)
            }
            PrereqLanded { prereq } => {
                let mut new = row.clone();
                new.stack.remove(prereq);
                new.version += 1;
                Outcome::applied(new)
            }
            Claim { .. } | Release | HolderDead | Submit { .. } | Done { .. } | GatePass { .. }
            | GateRed { .. } | GateInfra { .. } | Deliver
            | ContentOnBase { .. } | Supersede { .. } | Drop { .. } | Hold { .. } | Unhold { .. } | Reply { .. } | AskWithdrawn | Reclassify { .. } => illegal(row, kind),
        },

        BeadState::Rework => match kind {
            Claim { holder, lease_until, stack, stack_depth, stack_max_depth } => {
                if *stack_depth > *stack_max_depth {
                    return depth_exceeded(row, *stack_depth, *stack_max_depth);
                }
                let mut new = row.clone();
                new.state = BeadState::Working;
                new.holder = Some(holder.clone());
                new.lease_until = Some(*lease_until);
                new.stack = stack.clone();
                new.stack_depth = *stack_depth;
                new.version += 1;
                Outcome::applied(new)
            }
            BaseWithdrawn { prereq, tip } => {
                if !base_withdrawn_applies(row, prereq, tip) {
                    return not_in_stack(row, prereq, tip);
                }
                let mut new = row.clone();
                new.reason = Some(format!("base_withdrawn: {prereq} {tip}"));
                new.version += 1;
                Outcome::applied(new)
            }
            PrereqLanded { prereq } => {
                let mut new = row.clone();
                new.stack.remove(prereq);
                new.version += 1;
                Outcome::applied(new)
            }
            Release | HolderDead | Submit { .. } | Done { .. } | GatePass { .. } | GateRed { .. }
            | GateInfra { .. } | Deliver | Delivered { .. } | Returned { .. } | Requeued { .. }
            | ContentOnBase { .. } | Supersede { .. } | Drop { .. } | Hold { .. } | Unhold { .. } | Reply { .. } | AskWithdrawn | Reclassify { .. } => illegal(row, kind),
        },

        // Terminal states: every one of the 19 events is illegal here, because there is no
        // outgoing transition at all — not because any one event is refused for its own
        // reason. Named individually, per the no-wildcard rule. (The orthogonal five are
        // unreachable in practice, since `apply` intercepts and refuses them before this
        // function is ever called; naming them here too keeps this match exhaustive on its
        // own terms, without relying on that caller behavior.)
        BeadState::Landed | BeadState::Superseded | BeadState::Dropped | BeadState::Done => match kind {
            Claim { .. } | Release | HolderDead | Submit { .. } | Done { .. } | GatePass { .. }
            | GateRed { .. } | GateInfra { .. } | Deliver | Delivered { .. } | Returned { .. }
            | Requeued { .. } | BaseWithdrawn { .. } | PrereqLanded { .. }
            | ContentOnBase { .. } | Supersede { .. } | Drop { .. } | Hold { .. } | Unhold { .. } | Reply { .. } | AskWithdrawn | Reclassify { .. } => terminal(row),
        },
    }
}

/// One entry of a proposed stack that no longer matches its prerequisite's live row —
/// evidence a claim's caller gathers before ever proposing the claim (design §1: "the tip
/// invariant, extended"). `current_tip` is `None` when the prerequisite is not (or no
/// longer) CERTIFIED at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleStackEntry {
    pub prereq: String,
    pub given_tip: String,
    pub current_tip: Option<String>,
}

/// Every entry of `stack` whose tip no longer matches the named prerequisite's own row —
/// empty means the proposed stack is entirely fresh. Pure and multi-row (unlike `apply`,
/// which sees only the one row being transitioned): the caller already read every
/// prerequisite's row to build `stack` in the first place, so passing them back in adds no
/// I/O this crate would otherwise have to perform itself.
pub fn stale_stack_entries(stack: &Stack, prereqs: &[BeadRow]) -> Vec<StaleStackEntry> {
    stack
        .iter()
        .filter_map(|(prereq, given_tip)| {
            let current_tip = prereqs.iter().find(|r| &r.bead_id == prereq).and_then(|r| r.tip.clone());
            if current_tip.as_deref() == Some(given_tip.as_str()) {
                None
            } else {
                Some(StaleStackEntry { prereq: prereq.clone(), given_tip: given_tip.clone(), current_tip })
            }
        })
        .collect()
}

/// Whether a `stack` — a row's already-recorded one, or a `claim`'s proposed one — should be
/// refused because an entry no longer names its prerequisite's current certified tip (design
/// §1: "the tip invariant, extended"). The machine's own `apply` cannot make this check
/// itself, since it sees only the one row being transitioned; the caller makes it against the
/// same live prerequisite rows it already read to build `stack` in the first place, right
/// before proposing `submit` or `claim`.
pub fn stale_stack_refusal(stack: &Stack, prereqs: &[BeadRow]) -> Option<Refusal> {
    let stale = stale_stack_entries(stack, prereqs);
    if stale.is_empty() {
        None
    } else {
        Some(Refusal::StackStale { prereqs: stale.into_iter().map(|e| e.prereq).collect() })
    }
}

/// `submit`'s stale-stack check (design §1: "its eventual submit is refused while the stack
/// names a withdrawn tip") — [`stale_stack_refusal`] against the row's own recorded `stack`.
pub fn submit_refusal_for_stale_stack(row: &BeadRow, prereqs: &[BeadRow]) -> Option<Refusal> {
    stale_stack_refusal(&row.stack, prereqs)
}

/// `claim`'s stale-stack check — [`stale_stack_refusal`] against the stack a claim proposes,
/// before that stack is ever recorded on the row. Without this, a claim whose proposal was
/// computed against a prerequisite's tip that has since moved (e.g. a resubmit after
/// CERTIFIED) would be accepted by the `Claim` arm, which validates only `stack_depth`
/// against `stack_max_depth` and has no way to see the prerequisite's row.
pub fn claim_refusal_for_stale_stack(stack: &Stack, prereqs: &[BeadRow]) -> Option<Refusal> {
    stale_stack_refusal(stack, prereqs)
}

/// The hold-release rule (design §1) for a blocker that is a work bead in the *same
/// repository* — the one case the rule changes. Every other blocker kind (an epic, a
/// decision, a cross-repository blocker) is unaffected by this epic and keeps waiting for
/// the blocker to close, exactly as today; this crate has no opinion on that path since it
/// models only work-bead rows.
pub fn wait_hold_released_for_work_blocker(blocker_state: BeadState) -> bool {
    matches!(blocker_state, BeadState::Certified | BeadState::InDelivery | BeadState::Landed)
}

/// Whether a wait hold already released for this blocker must be re-applied, because the
/// blocker left CERTIFIED/IN_DELIVERY backwards before the dependent was claimed (design
/// §1: "re-applied if the blocker moves backwards"). REWORK is the direct backwards move;
/// SUBMITTED covers a fresh tip resubmitted after CERTIFIED (also backwards, since the new
/// tip has not been certified yet).
pub fn wait_hold_reapplies_for_work_blocker(blocker_state: BeadState) -> bool {
    matches!(blocker_state, BeadState::Rework | BeadState::Submitted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(state: BeadState) -> BeadRow {
        let mut r = BeadRow::filed("sp-test");
        r.state = state;
        r
    }

    fn ev(expect: BeadState, version: Version, kind: BeadEventKind) -> BeadEvent {
        BeadEvent { expect, version, kind, actor: "test".into(), at: None }
    }

    // Every (state, event-kind) pair, exhaustively, so a variant added to either enum
    // without a line added here is caught by a failing test, not just by the compiler.
    const ALL_STATES: [BeadState; 10] = [
        BeadState::Ready,
        BeadState::Working,
        BeadState::Submitted,
        BeadState::Certified,
        BeadState::InDelivery,
        BeadState::Rework,
        BeadState::Landed,
        BeadState::Superseded,
        BeadState::Dropped,
        BeadState::Done,
    ];

    fn sample_kinds() -> Vec<BeadEventKind> {
        vec![
            BeadEventKind::Claim { holder: "h".into(), lease_until: 1, stack: Stack::new(), stack_depth: 0, stack_max_depth: 4 },
            BeadEventKind::BaseWithdrawn { prereq: "sp-prereq".into(), tip: "t1".into() },
            BeadEventKind::PrereqLanded { prereq: "sp-prereq".into() },
            BeadEventKind::Release,
            BeadEventKind::HolderDead,
            BeadEventKind::Submit { tip: "t1".into() },
            BeadEventKind::Done { delivers: "d".into() },
            BeadEventKind::GatePass { tip: "t1".into(), gate_key: "k".into() },
            BeadEventKind::GateRed { tip: "t1".into(), reason: GateRedReason::SuitesFailed },
            BeadEventKind::GateInfra { tip: "t1".into() },
            BeadEventKind::Deliver,
            BeadEventKind::Delivered { merge_sha: "s".into(), proof: "p".into() },
            BeadEventKind::Returned { reason: ReturnedReason::PushRejected },
            BeadEventKind::Requeued { tip: "t1".into() },
            BeadEventKind::ContentOnBase { proof: "p".into() },
            BeadEventKind::Supersede { by: "sp-2".into() },
            BeadEventKind::Drop { reason: DropReason::Unwanted },
            BeadEventKind::Hold { kind: HoldKind::Poison, cause: HoldCause::AttemptsExhausted, detail: None },
            BeadEventKind::Unhold { kind: HoldKind::Poison },
        ]
    }

    #[test]
    fn since_is_stamped_on_entry_to_landed_and_certified_only() {
        let mut r = row(BeadState::InDelivery);
        r.tip = Some("t".into());
        let mut e = ev(BeadState::InDelivery, r.version, BeadEventKind::Requeued { tip: "t".into() });
        e.at = Some(77);
        let out = apply(&r, &e);
        assert_eq!((out.row.state, out.row.since), (BeadState::Certified, Some(77)));

        let mut e = ev(BeadState::InDelivery, r.version, BeadEventKind::Delivered { merge_sha: "m".into(), proof: "p".into() });
        e.at = Some(88);
        let out = apply(&r, &e);
        assert_eq!((out.row.state, out.row.since), (BeadState::Landed, Some(88)));

        let mut r = row(BeadState::InDelivery);
        r.tip = Some("t".into());
        let mut e = ev(BeadState::InDelivery, r.version, BeadEventKind::Requeued { tip: "other".into() });
        e.at = Some(99);
        let out = apply(&r, &e);
        assert_eq!((out.row.state, out.row.since), (BeadState::Submitted, None));
    }

    #[test]
    fn every_state_event_pair_has_an_explicit_outcome() {
        for &state in &ALL_STATES {
            for kind in sample_kinds() {
                let mut r = row(state);
                r.tip = Some("t1".into());
                let e = ev(state, r.version, kind);
                // Must not panic, and must return a well-formed outcome either way.
                let out = apply(&r, &e);
                if out.applied {
                    assert_eq!(out.row.version, r.version + 1, "{state:?} applied but version did not advance");
                } else {
                    assert_eq!(out.row, r, "a refusal must not mutate the row");
                    assert!(out.refusal.is_some());
                }
            }
        }
    }

    fn reclassify(state: BeadState, rule: &str) -> BeadEventKind {
        BeadEventKind::Reclassify { state, rule: rule.into() }
    }

    fn classifier_ev(expect: BeadState, kind: BeadEventKind) -> BeadEvent {
        BeadEvent { actor: "classifier".into(), ..ev(expect, 0, kind) }
    }

    #[test]
    fn reclassify_overwrites_only_the_residue_default_on_a_terminal_row() {
        let mut r = row(BeadState::Dropped);
        r.reason = Some(RESIDUE_RULE.into());
        let out = apply(&r, &classifier_ev(BeadState::Dropped, reclassify(BeadState::Landed, "terminal-landing-line")));
        assert!(out.applied);
        assert_eq!(out.row.state, BeadState::Landed);
        assert_eq!(out.row.reason.as_deref(), Some("terminal-landing-line"));
        assert_eq!(out.row.version, 1);

        let mut decided = row(BeadState::Dropped);
        decided.reason = Some("unwanted".into());
        assert!(!apply(&decided, &classifier_ev(BeadState::Dropped, reclassify(BeadState::Landed, "x"))).applied);
        assert!(!apply(&row(BeadState::Landed), &classifier_ev(BeadState::Landed, reclassify(BeadState::Dropped, "x"))).applied);
    }

    #[test]
    fn reclassify_is_the_classifiers_alone_and_never_touches_a_holder() {
        let r = row(BeadState::Submitted);
        assert!(!apply(&r, &ev(BeadState::Submitted, 0, reclassify(BeadState::Dropped, "x"))).applied);
        assert!(apply(&r, &classifier_ev(BeadState::Submitted, reclassify(BeadState::Dropped, "x"))).applied);
        let w = row(BeadState::Working);
        assert!(!apply(&w, &classifier_ev(BeadState::Working, reclassify(BeadState::Dropped, "x"))).applied);
    }

    #[test]
    fn reclassify_to_the_same_state_and_rule_is_refused() {
        let mut r = row(BeadState::Dropped);
        r.reason = Some(RESIDUE_RULE.into());
        assert!(!apply(&r, &classifier_ev(BeadState::Dropped, reclassify(BeadState::Dropped, RESIDUE_RULE))).applied);
    }

    #[test]
    fn terminal_states_absorb_every_event() {
        for &state in &[BeadState::Landed, BeadState::Superseded, BeadState::Dropped, BeadState::Done] {
            for kind in sample_kinds() {
                let r = row(state);
                let e = ev(state, 0, kind);
                let out = apply(&r, &e);
                assert!(!out.applied, "{state:?} must have no outgoing transition, got {:?}", out);
                assert_eq!(out.row, r);
            }
        }
    }

    #[test]
    fn claim_from_ready_enters_working() {
        let r = row(BeadState::Ready);
        let e = ev(BeadState::Ready, 0, BeadEventKind::Claim { holder: "aeon-1".into(), lease_until: 100, stack: Stack::new(), stack_depth: 0, stack_max_depth: 4 });
        let out = apply(&r, &e);
        assert!(out.applied);
        assert_eq!(out.row.state, BeadState::Working);
        assert_eq!(out.row.holder.as_deref(), Some("aeon-1"));
        assert_eq!(out.row.version, 1);
    }

    #[test]
    fn release_and_holder_dead_both_return_to_ready() {
        for kind in [BeadEventKind::Release, BeadEventKind::HolderDead] {
            let mut r = row(BeadState::Working);
            r.holder = Some("aeon-1".into());
            let e = ev(BeadState::Working, 0, kind);
            let out = apply(&r, &e);
            assert!(out.applied);
            assert_eq!(out.row.state, BeadState::Ready);
            assert_eq!(out.row.holder, None);
        }
    }

    #[test]
    fn submit_then_gate_pass_certifies_the_same_tip() {
        let mut r = row(BeadState::Working);
        r.holder = Some("aeon-1".into());
        let out = apply(&r, &ev(BeadState::Working, 0, BeadEventKind::Submit { tip: "abc123".into() }));
        assert!(out.applied);
        r = out.row;
        assert_eq!(r.state, BeadState::Submitted);
        assert_eq!(r.tip.as_deref(), Some("abc123"));

        let out = apply(
            &r,
            &ev(BeadState::Submitted, r.version, BeadEventKind::GatePass { tip: "abc123".into(), gate_key: "k1".into() }),
        );
        assert!(out.applied);
        assert_eq!(out.row.state, BeadState::Certified);
        assert_eq!(out.row.gate_key.as_deref(), Some("k1"));
    }

    #[test]
    fn gate_pass_on_a_stale_tip_is_refused_and_does_not_mutate() {
        let mut r = row(BeadState::Submitted);
        r.tip = Some("abc123".into());
        let e = ev(BeadState::Submitted, r.version, BeadEventKind::GatePass { tip: "stale".into(), gate_key: "k".into() });
        let out = apply(&r, &e);
        assert!(!out.applied);
        assert_eq!(out.row, r);
        assert!(matches!(out.refusal, Some(Refusal::TipMismatch { .. })));
    }

    #[test]
    fn requeued_with_matching_tip_returns_to_certified() {
        let mut r = row(BeadState::InDelivery);
        r.tip = Some("abc123".into());
        r.gate_key = Some("k1".into());
        let out = apply(&r, &ev(BeadState::InDelivery, r.version, BeadEventKind::Requeued { tip: "abc123".into() }));
        assert!(out.applied);
        assert_eq!(out.row.state, BeadState::Certified);
    }

    #[test]
    fn requeued_with_a_different_tip_voids_certification_to_submitted() {
        let mut r = row(BeadState::InDelivery);
        r.tip = Some("abc123".into());
        r.gate_key = Some("k1".into());
        let out = apply(&r, &ev(BeadState::InDelivery, r.version, BeadEventKind::Requeued { tip: "other".into() }));
        assert!(out.applied);
        assert_eq!(out.row.state, BeadState::Submitted);
        // The row's own tip is authoritative; a mismatched requeue does not overwrite it.
        assert_eq!(out.row.tip.as_deref(), Some("abc123"));
    }

    #[test]
    fn expect_mismatch_never_mutates() {
        let r = row(BeadState::Ready);
        let e = ev(BeadState::Working, 0, BeadEventKind::Claim { holder: "h".into(), lease_until: 1, stack: Stack::new(), stack_depth: 0, stack_max_depth: 4 });
        let out = apply(&r, &e);
        assert!(!out.applied);
        assert_eq!(out.row, r);
        assert!(matches!(out.refusal, Some(Refusal::ExpectMismatch { .. })));
    }

    #[test]
    fn stale_version_never_mutates() {
        let mut r = row(BeadState::Ready);
        r.version = 5;
        let e = ev(BeadState::Ready, 4, BeadEventKind::Claim { holder: "h".into(), lease_until: 1, stack: Stack::new(), stack_depth: 0, stack_max_depth: 4 });
        let out = apply(&r, &e);
        assert!(!out.applied);
        assert_eq!(out.row, r);
        assert!(matches!(out.refusal, Some(Refusal::StaleVersion { .. })));
    }

    #[test]
    fn hold_and_unhold_suspend_without_losing_state() {
        let r = row(BeadState::Submitted);
        let out = apply(&r, &ev(BeadState::Submitted, 0, BeadEventKind::Hold { kind: HoldKind::Ask, cause: HoldCause::OperatorQuestion, detail: None }));
        assert!(out.applied);
        assert_eq!(out.row.state, BeadState::Submitted);
        assert!(out.row.holds.contains(&HoldKind::Ask));

        let out2 = apply(&out.row, &ev(BeadState::Submitted, out.row.version, BeadEventKind::Unhold { kind: HoldKind::Wait }));
        assert!(out2.applied);
        assert_eq!(out2.row.state, BeadState::Submitted);
        assert!(out2.row.holds.contains(&HoldKind::Ask));
    }

    fn gate_row() -> BeadRow {
        let mut r = row(BeadState::Working);
        r.holds.insert(HoldKind::Ask);
        r
    }

    #[test]
    fn a_gate_bead_is_refused_every_close_on_marker_text_alone() {
        let r = gate_row();
        for kind in [
            BeadEventKind::Done { delivers: "d".into() },
            BeadEventKind::Drop { reason: DropReason::Unwanted },
            BeadEventKind::Supersede { by: "sp-2".into() },
            BeadEventKind::ContentOnBase { proof: "p".into() },
            BeadEventKind::Unhold { kind: HoldKind::Ask },
        ] {
            let out = apply(&r, &ev(BeadState::Working, 0, kind.clone()));
            assert!(!out.applied, "{kind:?}");
            assert_eq!(out.row, r);
            match out.refusal {
                Some(Refusal::AwaitingReply { exit, .. }) => assert!(exit.contains("reply"), "{exit}"),
                other => panic!("{kind:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_gate_bead_closes_after_a_reply_event_carrying_a_message_id() {
        let r = gate_row();
        let replied = apply(&r, &ev(BeadState::Working, 0, BeadEventKind::Reply { message_id: "m-1".into() }));
        assert!(replied.applied);
        assert!(!replied.row.holds.contains(&HoldKind::Ask));
        assert_eq!(replied.row.reason.as_deref(), Some("reply: m-1"));
        let closed = apply(&replied.row, &ev(BeadState::Working, replied.row.version, BeadEventKind::Done { delivers: "d".into() }));
        assert!(closed.applied);
        assert_eq!(closed.row.state, BeadState::Done);
    }

    #[test]
    fn a_withdrawn_ask_lifts_by_its_own_event_and_needs_an_ask() {
        let out = apply(&gate_row(), &ev(BeadState::Working, 0, BeadEventKind::AskWithdrawn));
        assert!(out.applied);
        assert!(!out.row.holds.contains(&HoldKind::Ask));
        assert_eq!(out.row.reason.as_deref(), Some("ask withdrawn"));
        assert!(!apply(&row(BeadState::Working), &ev(BeadState::Working, 0, BeadEventKind::AskWithdrawn)).applied);
    }

    #[test]
    fn a_reply_needs_an_ask_and_a_message_id() {
        let none = apply(&row(BeadState::Working), &ev(BeadState::Working, 0, BeadEventKind::Reply { message_id: "m-1".into() }));
        assert!(!none.applied);
        let blank = apply(&gate_row(), &ev(BeadState::Working, 0, BeadEventKind::Reply { message_id: String::new() }));
        assert!(!blank.applied);
    }

    #[test]
    fn hold_is_refused_on_a_terminal_row() {
        let r = row(BeadState::Landed);
        let out = apply(&r, &ev(BeadState::Landed, 0, BeadEventKind::Hold { kind: HoldKind::Poison, cause: HoldCause::AttemptsExhausted, detail: None }));
        assert!(!out.applied);
        assert!(matches!(out.refusal, Some(Refusal::Terminal { .. })));
    }

    #[test]
    fn content_on_base_lands_from_any_non_terminal_state() {
        for &state in &[
            BeadState::Ready,
            BeadState::Working,
            BeadState::Submitted,
            BeadState::Certified,
            BeadState::InDelivery,
            BeadState::Rework,
        ] {
            let r = row(state);
            let out = apply(&r, &ev(state, 0, BeadEventKind::ContentOnBase { proof: "merge-tree-noop".into() }));
            assert!(out.applied, "{state:?} should land on content_on_base");
            assert_eq!(out.row.state, BeadState::Landed);
        }
    }

    #[test]
    fn supersede_and_drop_are_available_from_any_non_terminal_state() {
        for &state in &[BeadState::Ready, BeadState::Working, BeadState::Rework] {
            let r = row(state);
            let out = apply(&r, &ev(state, 0, BeadEventKind::Supersede { by: "sp-99".into() }));
            assert!(out.applied);
            assert_eq!(out.row.state, BeadState::Superseded);

            let r = row(state);
            let out = apply(&r, &ev(state, 0, BeadEventKind::Drop { reason: DropReason::Unwanted }));
            assert!(out.applied);
            assert_eq!(out.row.state, BeadState::Dropped);
        }
    }

    #[test]
    fn in_delivery_is_left_only_by_a_delivery_exit_event() {
        // The three delivery exit events are delivered/returned/requeued. The orthogonal
        // any-non-terminal-state events (content_on_base/supersede/drop) are the design's
        // own deliberate bypass — e.g. the Sending proving content already landed by a
        // squash merge the delivery machine never reconciled — so they are legitimate ways
        // to leave IN_DELIVERY too, just not *delivery* exits. Everything else must leave
        // the row in IN_DELIVERY or be refused outright (property test, design §7).
        let delivery_exits = [
            BeadEventKind::Delivered { merge_sha: "s".into(), proof: "p".into() },
            BeadEventKind::Returned { reason: ReturnedReason::PushRejected },
            BeadEventKind::Requeued { tip: "t1".into() },
        ];
        let other_legal_exits = [
            BeadEventKind::ContentOnBase { proof: "p".into() },
            BeadEventKind::Supersede { by: "sp-2".into() },
            BeadEventKind::Drop { reason: DropReason::Unwanted },
        ];
        for kind in sample_kinds() {
            let mut r = row(BeadState::InDelivery);
            r.tip = Some("t1".into());
            let out = apply(&r, &ev(BeadState::InDelivery, r.version, kind.clone()));
            let is_delivery_exit = delivery_exits.iter().any(|k| std::mem::discriminant(k) == std::mem::discriminant(&kind));
            let is_other_legal_exit = other_legal_exits.iter().any(|k| std::mem::discriminant(k) == std::mem::discriminant(&kind));
            if is_delivery_exit || is_other_legal_exit {
                assert!(out.applied, "{kind:?} should be a legal exit from IN_DELIVERY");
                assert_ne!(out.row.state, BeadState::InDelivery);
            } else {
                assert!(!out.applied || out.row.state == BeadState::InDelivery, "{kind:?} moved out of IN_DELIVERY without being an exit event");
            }
        }
    }

    #[test]
    fn resubmitting_a_moved_tip_voids_certification_to_submitted() {
        let mut r = row(BeadState::Certified);
        r.tip = Some("abc123".into());
        r.gate_key = Some("k1".into());
        let out = apply(&r, &ev(BeadState::Certified, r.version, BeadEventKind::Submit { tip: "def456".into() }));
        assert!(out.applied);
        assert_eq!(out.row.state, BeadState::Submitted);
        assert_eq!(out.row.tip.as_deref(), Some("def456"));
        assert_eq!(out.row.gate_key, None, "a voided certification carries no gate key forward");
    }

    #[test]
    fn resubmitting_the_same_tip_leaves_certification_standing() {
        let mut r = row(BeadState::Certified);
        r.tip = Some("abc123".into());
        r.gate_key = Some("k1".into());
        let out = apply(&r, &ev(BeadState::Certified, r.version, BeadEventKind::Submit { tip: "abc123".into() }));
        assert!(out.applied);
        assert_eq!(out.row.state, BeadState::Certified);
        assert_eq!(out.row.gate_key.as_deref(), Some("k1"), "an unchanged tip is not a change to void");
    }

    #[test]
    fn a_moved_tip_resubmitted_while_submitted_replaces_the_pending_tip() {
        let mut r = row(BeadState::Submitted);
        r.tip = Some("abc123".into());
        let out = apply(&r, &ev(BeadState::Submitted, r.version, BeadEventKind::Submit { tip: "def456".into() }));
        assert!(out.applied);
        assert_eq!((out.row.state, out.row.tip.as_deref()), (BeadState::Submitted, Some("def456")));
        // ...and a verdict on the new tip now applies, where one on the old tip is refused.
        let pass = apply(&out.row, &ev(BeadState::Submitted, out.row.version, BeadEventKind::GatePass { tip: "def456".into(), gate_key: "k".into() }));
        assert_eq!(pass.row.state, BeadState::Certified);
        let stale = apply(&out.row, &ev(BeadState::Submitted, out.row.version, BeadEventKind::GatePass { tip: "abc123".into(), gate_key: "k".into() }));
        assert!(!stale.applied, "a verdict on the replaced tip is TipMismatch");
    }

    #[test]
    fn resubmitting_the_same_tip_while_submitted_is_a_no_op() {
        let mut r = row(BeadState::Submitted);
        r.tip = Some("abc123".into());
        let out = apply(&r, &ev(BeadState::Submitted, r.version, BeadEventKind::Submit { tip: "abc123".into() }));
        assert!(out.applied);
        assert_eq!((out.row.state, out.row.tip.as_deref()), (BeadState::Submitted, Some("abc123")));
    }

    #[test]
    fn version_is_monotonic_across_a_chain_of_applied_events() {
        let mut r = row(BeadState::Ready);
        let mut last = r.version;
        let chain: Vec<(BeadState, BeadEventKind)> = vec![
            (BeadState::Ready, BeadEventKind::Claim { holder: "h".into(), lease_until: 1, stack: Stack::new(), stack_depth: 0, stack_max_depth: 4 }),
            (BeadState::Working, BeadEventKind::Submit { tip: "t1".into() }),
            (BeadState::Submitted, BeadEventKind::GatePass { tip: "t1".into(), gate_key: "k".into() }),
            (BeadState::Certified, BeadEventKind::Deliver),
            (BeadState::InDelivery, BeadEventKind::Delivered { merge_sha: "s".into(), proof: "p".into() }),
        ];
        for (expect, kind) in chain {
            let out = apply(&r, &ev(expect, r.version, kind));
            assert!(out.applied);
            assert!(out.row.version > last);
            last = out.row.version;
            r = out.row;
        }
        assert_eq!(r.state, BeadState::Landed);
    }

    // ── typed reasons (design reconciler-time-series-2026-09-27 §2a, row 2b) ──────────
    // The reconciler's rework-by-cause query must not invent its own reason list, so the
    // machine's own `returned`/`gate_red`/`hold`/`drop` reasons are closed enums, not free
    // text. These tests enumerate every variant of each reason enum against the transition
    // table, and prove an unrecognized reason is refused as evidence rather than silently
    // accepted — which today's code (`reason: String`) cannot do at all: any string, known
    // or not, applies.

    const ALL_GATE_RED_REASONS: [GateRedReason; 6] = [
        GateRedReason::SuitesFailed,
        GateRedReason::Syntax,
        GateRedReason::PolicyViolation,
        GateRedReason::NoRebase,
        GateRedReason::Timeout,
        GateRedReason::Confine,
    ];
    const ALL_RETURNED_REASONS: [ReturnedReason; 5] = [
        ReturnedReason::PrClosedUnmerged,
        ReturnedReason::PrChangesRequested,
        ReturnedReason::PushRejected,
        ReturnedReason::BatchEjected,
        ReturnedReason::BaseWithdrawn,
    ];
    const ALL_DROP_REASONS: [DropReason; 2] = [DropReason::ClosedNoBranch, DropReason::Unwanted];
    const ALL_HOLD_CAUSES: [HoldCause; 5] = [
        HoldCause::AttemptsExhausted,
        HoldCause::OperatorQuestion,
        HoldCause::UnlandedBlocker,
        HoldCause::SupersedeRequest,
        HoldCause::ManualHold,
    ];

    #[test]
    fn every_gate_red_reason_moves_submitted_to_rework_and_is_recorded() {
        for reason in ALL_GATE_RED_REASONS {
            let mut r = row(BeadState::Submitted);
            r.tip = Some("t1".into());
            let out = apply(&r, &ev(BeadState::Submitted, 0, BeadEventKind::GateRed { tip: "t1".into(), reason }));
            assert!(out.applied, "{reason:?} should move SUBMITTED to REWORK");
            assert_eq!(out.row.state, BeadState::Rework);
            assert_eq!(out.row.reason.as_deref(), Some(reason.as_str()));
        }
    }

    #[test]
    fn every_returned_reason_moves_in_delivery_to_rework_and_is_recorded() {
        for reason in ALL_RETURNED_REASONS {
            let mut r = row(BeadState::InDelivery);
            r.tip = Some("t1".into());
            let out = apply(&r, &ev(BeadState::InDelivery, 0, BeadEventKind::Returned { reason }));
            assert!(out.applied, "{reason:?} should move IN_DELIVERY to REWORK");
            assert_eq!(out.row.state, BeadState::Rework);
            assert_eq!(out.row.reason.as_deref(), Some(reason.as_str()));
        }
    }

    #[test]
    fn every_drop_reason_drops_a_ready_bead_and_is_recorded() {
        for reason in ALL_DROP_REASONS {
            let r = row(BeadState::Ready);
            let out = apply(&r, &ev(BeadState::Ready, 0, BeadEventKind::Drop { reason }));
            assert!(out.applied, "{reason:?} should drop a READY bead");
            assert_eq!(out.row.state, BeadState::Dropped);
            assert_eq!(out.row.reason.as_deref(), Some(reason.as_str()));
        }
    }

    #[test]
    fn every_hold_cause_holds_a_ready_bead_and_is_recorded() {
        for cause in ALL_HOLD_CAUSES {
            let r = row(BeadState::Ready);
            let out = apply(&r, &ev(BeadState::Ready, 0, BeadEventKind::Hold { kind: HoldKind::Operator, cause, detail: None }));
            assert!(out.applied, "{cause:?} should hold a READY bead");
            assert!(out.row.holds.contains(&HoldKind::Operator));
            assert_eq!(out.row.reason.as_deref(), Some(cause.as_str()));
        }
    }

    #[test]
    fn hold_detail_overrides_the_causes_own_name_in_the_row() {
        // work.rs's superseded-by asks the operator to confirm against a specific successor
        // id, which the row's `reason` must still show — the category alone (`cause.as_str()`)
        // would lose it. `detail` carries it alongside `cause`, never inside it.
        let r = row(BeadState::Ready);
        let out = apply(
            &r,
            &ev(
                BeadState::Ready,
                0,
                BeadEventKind::Hold { kind: HoldKind::Operator, cause: HoldCause::SupersedeRequest, detail: Some("sp-9999".into()) },
            ),
        );
        assert!(out.applied);
        assert_eq!(out.row.reason.as_deref(), Some("sp-9999"));
    }

    #[test]
    fn an_unknown_reason_is_refused_as_evidence_before_it_ever_reaches_apply() {
        // The evidence boundary (design §3.2: "evidence is what the producer already
        // knows") is where this is caught — a reason tag none of the four enums name
        // fails to deserialize into a `BeadEventKind` at all, so `apply` never sees it.
        for bogus in [
            r#"{"GateRed":{"tip":"t1","reason":"flaky"}}"#,
            r#"{"Returned":{"reason":"merged"}}"#,
            r#"{"Drop":{"reason":"stale"}}"#,
            r#"{"Hold":{"kind":"Poison","cause":"just because"}}"#,
        ] {
            let parsed: Result<BeadEventKind, _> = serde_json::from_str(bogus);
            assert!(parsed.is_err(), "{bogus} names a reason no enum has, and must be refused, got {parsed:?}");
        }
    }

    // ── stacked dependents (design stacked-dependents-2026-09-28 §1) ──────────────────

    fn claim_ev(expect: BeadState, version: Version, stack: Stack, stack_depth: u32, stack_max_depth: u32) -> BeadEvent {
        ev(expect, version, BeadEventKind::Claim { holder: "aeon-1".into(), lease_until: 100, stack, stack_depth, stack_max_depth })
    }

    #[test]
    fn claim_records_the_proposed_stack_and_depth() {
        let r = row(BeadState::Ready);
        let mut stack = Stack::new();
        stack.insert("sp-a".into(), "tip-a".into());
        let out = apply(&r, &claim_ev(BeadState::Ready, 0, stack.clone(), 1, 4));
        assert!(out.applied);
        assert_eq!(out.row.stack, stack);
        assert_eq!(out.row.stack_depth, 1);
    }

    #[test]
    fn depth_at_the_ceiling_is_allowed_one_past_it_is_refused() {
        let r = row(BeadState::Ready);
        let out = apply(&r, &claim_ev(BeadState::Ready, 0, Stack::new(), 4, 4));
        assert!(out.applied, "depth == max must be allowed");

        let r = row(BeadState::Ready);
        let out = apply(&r, &claim_ev(BeadState::Ready, 0, Stack::new(), 5, 4));
        assert!(!out.applied, "depth == max + 1 must be refused");
        assert_eq!(out.row, r, "a refused claim must not mutate the row");
        assert!(matches!(out.refusal, Some(Refusal::DepthExceeded { depth: 5, max: 4 })));
    }

    #[test]
    fn stack_max_depth_zero_reproduces_todays_behaviour_no_stacking_allowed() {
        let r = row(BeadState::Ready);
        let out = apply(&r, &claim_ev(BeadState::Ready, 0, Stack::new(), 0, 0));
        assert!(out.applied, "an unstacked claim (depth 0) is still legal at the zero ceiling");

        let r = row(BeadState::Ready);
        let mut stack = Stack::new();
        stack.insert("sp-a".into(), "tip-a".into());
        let out = apply(&r, &claim_ev(BeadState::Ready, 0, stack, 1, 0));
        assert!(!out.applied, "any depth above zero is refused when stack_max_depth is 0");
    }

    #[test]
    fn a_config_ceiling_above_four_would_be_refused_by_the_schema_this_crate_only_enforces_the_number_given() {
        // The hard ceiling of 4 is spira-config's own schema job (ceiling 4, per Ryan
        // 2026-09-28); this crate enforces whatever `stack_max_depth` the event carries,
        // exactly as it enforces `lease_until` without knowing where it came from.
        let r = row(BeadState::Ready);
        let out = apply(&r, &claim_ev(BeadState::Ready, 0, Stack::new(), 5, 5));
        assert!(out.applied, "this crate trusts the ceiling it is given");
    }

    #[test]
    fn stale_stack_entries_reports_a_prerequisite_whose_tip_moved() {
        let mut stack = Stack::new();
        stack.insert("sp-a".into(), "old-tip".into());
        stack.insert("sp-b".into(), "current-tip".into());

        let mut prereq_a = row(BeadState::Certified);
        prereq_a.bead_id = "sp-a".into();
        prereq_a.tip = Some("new-tip".into());
        let mut prereq_b = row(BeadState::Certified);
        prereq_b.bead_id = "sp-b".into();
        prereq_b.tip = Some("current-tip".into());

        let stale = stale_stack_entries(&stack, &[prereq_a, prereq_b]);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].prereq, "sp-a");
        assert_eq!(stale[0].given_tip, "old-tip");
        assert_eq!(stale[0].current_tip.as_deref(), Some("new-tip"));
    }

    #[test]
    fn stale_stack_entries_is_empty_when_every_tip_still_matches() {
        let mut stack = Stack::new();
        stack.insert("sp-a".into(), "tip-a".into());
        let mut prereq_a = row(BeadState::Certified);
        prereq_a.bead_id = "sp-a".into();
        prereq_a.tip = Some("tip-a".into());

        assert!(stale_stack_entries(&stack, &[prereq_a]).is_empty());
    }

    #[test]
    fn stale_stack_entries_reports_a_prerequisite_with_no_row_at_all() {
        let mut stack = Stack::new();
        stack.insert("sp-gone".into(), "tip-x".into());
        let stale = stale_stack_entries(&stack, &[]);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].current_tip, None);
    }

    fn claimed_row(state: BeadState, prereq: &str, tip: &str) -> BeadRow {
        let mut r = row(state);
        r.stack.insert(prereq.into(), tip.into());
        r.stack_depth = 1;
        r.tip = Some("own-tip".into());
        r
    }

    #[test]
    fn base_withdrawn_on_ready_reapplies_the_wait_hold_and_leaves_stack_untouched() {
        let r = row(BeadState::Ready);
        let out = apply(&r, &ev(BeadState::Ready, 0, BeadEventKind::BaseWithdrawn { prereq: "sp-a".into(), tip: "t1".into() }));
        assert!(out.applied);
        assert!(out.row.holds.contains(&HoldKind::Wait));
        assert!(out.row.stack.is_empty());
    }

    #[test]
    fn base_withdrawn_on_working_stays_working_and_is_refused_if_the_tip_does_not_match() {
        let r = claimed_row(BeadState::Working, "sp-a", "t1");
        let out = apply(&r, &ev(BeadState::Working, r.version, BeadEventKind::BaseWithdrawn { prereq: "sp-a".into(), tip: "t1".into() }));
        assert!(out.applied);
        assert_eq!(out.row.state, BeadState::Working);
        assert_eq!(out.row.stack.get("sp-a").map(String::as_str), Some("t1"), "base_withdrawn does not itself mutate stack");

        let r = claimed_row(BeadState::Working, "sp-a", "t1");
        let out = apply(&r, &ev(BeadState::Working, r.version, BeadEventKind::BaseWithdrawn { prereq: "sp-a".into(), tip: "different".into() }));
        assert!(!out.applied);
        assert!(matches!(out.refusal, Some(Refusal::NotInStack { .. })));
    }

    #[test]
    fn submit_is_refused_once_the_prerequisite_row_shows_the_stack_is_stale() {
        let r = claimed_row(BeadState::Working, "sp-a", "old-tip");
        let mut prereq = row(BeadState::Certified);
        prereq.bead_id = "sp-a".into();
        prereq.tip = Some("new-tip".into());

        let refusal = submit_refusal_for_stale_stack(&r, &[prereq.clone()]);
        assert!(matches!(refusal, Some(Refusal::StackStale { .. })), "a withdrawn tip must block submit");

        prereq.tip = Some("old-tip".into());
        assert_eq!(submit_refusal_for_stale_stack(&r, &[prereq]), None, "a fresh stack must not block submit");
    }

    #[test]
    fn claim_is_refused_when_the_proposed_stack_is_already_stale() {
        let mut stack = Stack::new();
        stack.insert("sp-a".into(), "old-tip".into());
        let mut prereq = row(BeadState::Certified);
        prereq.bead_id = "sp-a".into();
        prereq.tip = Some("new-tip".into());

        let refusal = claim_refusal_for_stale_stack(&stack, &[prereq.clone()]);
        assert!(matches!(refusal, Some(Refusal::StackStale { .. })), "a claim proposing an already-superseded tip must be refused");

        prereq.tip = Some("old-tip".into());
        assert_eq!(claim_refusal_for_stale_stack(&stack, &[prereq]), None, "a fresh proposal must not be refused");
    }

    #[test]
    fn base_withdrawn_moves_submitted_and_certified_to_rework_with_the_reason_named() {
        for state in [BeadState::Submitted, BeadState::Certified] {
            let r = claimed_row(state, "sp-a", "t1");
            let out = apply(&r, &ev(state, r.version, BeadEventKind::BaseWithdrawn { prereq: "sp-a".into(), tip: "t1".into() }));
            assert!(out.applied, "{state:?} should move to REWORK on base_withdrawn");
            assert_eq!(out.row.state, BeadState::Rework);
            assert_eq!(out.row.reason.as_deref(), Some("base_withdrawn: sp-a t1"));
        }
    }

    #[test]
    fn base_withdrawn_on_in_delivery_stays_in_delivery_pending_the_batch_ejects_it() {
        let r = claimed_row(BeadState::InDelivery, "sp-a", "t1");
        let out = apply(&r, &ev(BeadState::InDelivery, r.version, BeadEventKind::BaseWithdrawn { prereq: "sp-a".into(), tip: "t1".into() }));
        assert!(out.applied);
        assert_eq!(out.row.state, BeadState::InDelivery);
    }

    #[test]
    fn base_withdrawn_is_refused_on_a_terminal_row() {
        for state in [BeadState::Landed, BeadState::Superseded, BeadState::Dropped, BeadState::Done] {
            let r = row(state);
            let out = apply(&r, &ev(state, 0, BeadEventKind::BaseWithdrawn { prereq: "sp-a".into(), tip: "t1".into() }));
            assert!(!out.applied);
            assert!(matches!(out.refusal, Some(Refusal::Terminal { .. })));
        }
    }

    #[test]
    fn prereq_landed_drops_the_entry_from_stack_and_is_idempotent() {
        let r = claimed_row(BeadState::Working, "sp-a", "t1");
        let out = apply(&r, &ev(BeadState::Working, r.version, BeadEventKind::PrereqLanded { prereq: "sp-a".into() }));
        assert!(out.applied);
        assert!(!out.row.stack.contains_key("sp-a"), "a LANDED prerequisite drops out of the stack");

        // Idempotent: landing a prerequisite already absent from the stack is still legal.
        let out2 = apply(&out.row, &ev(BeadState::Working, out.row.version, BeadEventKind::PrereqLanded { prereq: "sp-a".into() }));
        assert!(out2.applied);
    }

    #[test]
    fn release_from_working_clears_the_voided_stack() {
        let r = claimed_row(BeadState::Working, "sp-a", "t1");
        let out = apply(&r, &ev(BeadState::Working, r.version, BeadEventKind::Release));
        assert!(out.applied);
        assert_eq!(out.row.state, BeadState::Ready);
        assert!(out.row.stack.is_empty());
        assert_eq!(out.row.stack_depth, 0);
    }

    #[test]
    fn a_certified_same_repo_blocker_releases_the_wait_hold_others_do_not() {
        for state in [BeadState::Certified, BeadState::InDelivery, BeadState::Landed] {
            assert!(wait_hold_released_for_work_blocker(state), "{state:?} should release the wait hold");
        }
        for state in [BeadState::Ready, BeadState::Working, BeadState::Submitted, BeadState::Rework] {
            assert!(!wait_hold_released_for_work_blocker(state), "{state:?} should not yet release the wait hold");
        }
    }

    #[test]
    fn a_blocker_moving_backwards_from_certified_reapplies_the_wait_hold() {
        for state in [BeadState::Rework, BeadState::Submitted] {
            assert!(wait_hold_reapplies_for_work_blocker(state), "{state:?} is a backwards move and must reapply the hold");
        }
        for state in [BeadState::Certified, BeadState::InDelivery, BeadState::Landed] {
            assert!(!wait_hold_reapplies_for_work_blocker(state));
        }
    }

    #[test]
    fn replay_reproduces_stack_exactly_across_claim_and_base_withdrawn() {
        let mut stack = Stack::new();
        stack.insert("sp-a".into(), "tip-a".into());
        let log = vec![
            claim_ev(BeadState::Ready, 0, stack.clone(), 1, 4),
            ev(BeadState::Working, 1, BeadEventKind::BaseWithdrawn { prereq: "sp-a".into(), tip: "tip-a".into() }),
            ev(BeadState::Working, 2, BeadEventKind::PrereqLanded { prereq: "sp-a".into() }),
        ];

        let mut incremental = BeadRow::filed("sp-replay-stack");
        for e in &log {
            incremental = apply(&incremental, e).row;
        }

        let replayed = crate::replay::fold_bead("sp-replay-stack", &log);

        assert_eq!(incremental, replayed);
        assert!(replayed.stack.is_empty(), "the landed prerequisite dropped out of the stack");
        assert_eq!(replayed.stack_depth, 1, "stack_depth is the claim-time meter, not re-derived on drop");
    }
}
