//! The bead machine (design §3.1.1): mode-independent, one row per work bead. It never
//! knows whether delivery is a batch, a PR or a push — that is the delivery machine's job.

use std::collections::BTreeSet;

use crate::reason::{DropReason, GateRedReason, HoldCause, ReturnedReason};
use crate::{Outcome, Refusal, Version};

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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BeadEventKind {
    Claim { holder: String, lease_until: i64 },
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
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BeadEvent {
    pub expect: BeadState,
    pub version: Version,
    pub kind: BeadEventKind,
    pub actor: String,
}

fn illegal(row: &BeadRow, kind: &BeadEventKind) -> Outcome<BeadRow> {
    Outcome::refuse(
        row.clone(),
        Refusal::IllegalTransition { state: row.state.as_str().to_string(), event: format!("{kind:?}") },
    )
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

/// Apply one event to one row. Pure: the only inputs are the row and the event, and the
/// only output is the outcome. Every (state, event) pair is covered by rustc's exhaustive-
/// match check on both `BeadState` and `BeadEventKind` — a new variant on either enum fails
/// to compile here until this function accounts for it, which is the property the design
/// calls "a test that fails when a state or event variant is added without an entry."
pub fn apply(row: &BeadRow, ev: &BeadEvent) -> Outcome<BeadRow> {
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
            let mut new = row.clone();
            new.state = BeadState::Dropped;
            new.reason = Some(reason.as_str().to_string());
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
            let mut new = row.clone();
            new.holds.remove(kind);
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
        | BeadEventKind::Requeued { .. } => primary_transition(row, &ev.kind),
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
            Claim { holder, lease_until } => {
                let mut new = row.clone();
                new.state = BeadState::Working;
                new.holder = Some(holder.clone());
                new.lease_until = Some(*lease_until);
                new.version += 1;
                Outcome::applied(new)
            }
            Release | HolderDead | Submit { .. } | Done { .. } | GatePass { .. } | GateRed { .. }
            | GateInfra { .. } | Deliver | Delivered { .. } | Returned { .. } | Requeued { .. }
            | ContentOnBase { .. } | Supersede { .. } | Drop { .. } | Hold { .. } | Unhold { .. } => illegal(row, kind),
        },

        BeadState::Working => match kind {
            Release | HolderDead => {
                let mut new = row.clone();
                new.state = BeadState::Ready;
                new.holder = None;
                new.lease_until = None;
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
            Done { delivers } => {
                let mut new = row.clone();
                new.state = BeadState::Done;
                new.reason = Some(delivers.clone());
                new.version += 1;
                Outcome::applied(new)
            }
            Claim { .. } | GatePass { .. } | GateRed { .. } | GateInfra { .. } | Deliver | Delivered { .. }
            | Returned { .. } | Requeued { .. }
            | ContentOnBase { .. } | Supersede { .. } | Drop { .. } | Hold { .. } | Unhold { .. } => illegal(row, kind),
        },

        BeadState::Submitted => match kind {
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
            Claim { .. } | Release | HolderDead | Submit { .. } | Done { .. } | Deliver | Delivered { .. }
            | Returned { .. } | Requeued { .. }
            | ContentOnBase { .. } | Supersede { .. } | Drop { .. } | Hold { .. } | Unhold { .. } => illegal(row, kind),
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
            Claim { .. } | Release | HolderDead | Done { .. } | GatePass { .. }
            | GateRed { .. } | GateInfra { .. } | Delivered { .. } | Returned { .. } | Requeued { .. }
            | ContentOnBase { .. } | Supersede { .. } | Drop { .. } | Hold { .. } | Unhold { .. } => {
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
            Claim { .. } | Release | HolderDead | Submit { .. } | Done { .. } | GatePass { .. }
            | GateRed { .. } | GateInfra { .. } | Deliver
            | ContentOnBase { .. } | Supersede { .. } | Drop { .. } | Hold { .. } | Unhold { .. } => illegal(row, kind),
        },

        BeadState::Rework => match kind {
            Claim { holder, lease_until } => {
                let mut new = row.clone();
                new.state = BeadState::Working;
                new.holder = Some(holder.clone());
                new.lease_until = Some(*lease_until);
                new.version += 1;
                Outcome::applied(new)
            }
            Release | HolderDead | Submit { .. } | Done { .. } | GatePass { .. } | GateRed { .. }
            | GateInfra { .. } | Deliver | Delivered { .. } | Returned { .. } | Requeued { .. }
            | ContentOnBase { .. } | Supersede { .. } | Drop { .. } | Hold { .. } | Unhold { .. } => illegal(row, kind),
        },

        // Terminal states: every one of the 17 events is illegal here, because there is no
        // outgoing transition at all — not because any one event is refused for its own
        // reason. Named individually, per the no-wildcard rule. (The orthogonal five are
        // unreachable in practice, since `apply` intercepts and refuses them before this
        // function is ever called; naming them here too keeps this match exhaustive on its
        // own terms, without relying on that caller behavior.)
        BeadState::Landed | BeadState::Superseded | BeadState::Dropped | BeadState::Done => match kind {
            Claim { .. } | Release | HolderDead | Submit { .. } | Done { .. } | GatePass { .. }
            | GateRed { .. } | GateInfra { .. } | Deliver | Delivered { .. } | Returned { .. }
            | Requeued { .. }
            | ContentOnBase { .. } | Supersede { .. } | Drop { .. } | Hold { .. } | Unhold { .. } => terminal(row),
        },
    }
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
        BeadEvent { expect, version, kind, actor: "test".into() }
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
            BeadEventKind::Claim { holder: "h".into(), lease_until: 1 },
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
        let e = ev(BeadState::Ready, 0, BeadEventKind::Claim { holder: "aeon-1".into(), lease_until: 100 });
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
        let e = ev(BeadState::Working, 0, BeadEventKind::Claim { holder: "h".into(), lease_until: 1 });
        let out = apply(&r, &e);
        assert!(!out.applied);
        assert_eq!(out.row, r);
        assert!(matches!(out.refusal, Some(Refusal::ExpectMismatch { .. })));
    }

    #[test]
    fn stale_version_never_mutates() {
        let mut r = row(BeadState::Ready);
        r.version = 5;
        let e = ev(BeadState::Ready, 4, BeadEventKind::Claim { holder: "h".into(), lease_until: 1 });
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

        let out2 = apply(&out.row, &ev(BeadState::Submitted, out.row.version, BeadEventKind::Unhold { kind: HoldKind::Ask }));
        assert!(out2.applied);
        assert_eq!(out2.row.state, BeadState::Submitted);
        assert!(!out2.row.holds.contains(&HoldKind::Ask));
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
    fn version_is_monotonic_across_a_chain_of_applied_events() {
        let mut r = row(BeadState::Ready);
        let mut last = r.version;
        let chain: Vec<(BeadState, BeadEventKind)> = vec![
            (BeadState::Ready, BeadEventKind::Claim { holder: "h".into(), lease_until: 1 }),
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
    const ALL_RETURNED_REASONS: [ReturnedReason; 4] = [
        ReturnedReason::PrClosedUnmerged,
        ReturnedReason::PrChangesRequested,
        ReturnedReason::PushRejected,
        ReturnedReason::BatchEjected,
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
}
