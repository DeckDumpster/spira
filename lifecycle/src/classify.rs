//! The one-time migration classifier (design §4): assigns every existing work bead a state
//! by reading the six legacy records in a fixed precedence, rather than a machine transition
//! — there is no prior row to compare-and-swap against, only history to make sense of once.
//!
//! Pure, like the rest of this crate: every fact this module reads (bd status, labels,
//! batch membership, a content-on-base proof) is gathered by
//! the caller and handed in on [`BeadFacts`]. This module never shells out, reads a file or
//! asks git anything — see `spira-lc`'s own I/O layer for that.

use std::collections::BTreeSet;

use crate::bead::{BeadState, HoldKind};
use crate::delivery::DeliveryState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BdStatus {
    Open,
    InProgress,
    Closed,
}

/// Every fact the classifier's precedence table (design §4) reads, already gathered by the
/// caller. Nothing here is I/O: `content_on_base` is the result
/// of a git ancestry/merge-tree check the producer already ran (never a commit-subject
/// match — a subject-matching implementation is exactly the defect this classifier exists
/// to not repeat, per the migration fixture requirement recorded on this bead).
#[derive(Debug, Clone)]
pub struct BeadFacts {
    pub bd_status: BdStatus,
    /// Meaningful only when `bd_status` is `InProgress`. The cutover deploy classifies a
    /// quiesced, drained store (design §5.3), so a real caller passes `false` here; the
    /// field exists so a live-holder scenario is still an explicit, testable input rather
    /// than an assumption baked into the function.
    pub holder_alive: bool,
    pub labels: BTreeSet<String>,
    /// Whether `labels` carries the configured ask-hold name (`schema.sh name ask`, an
    /// operator-settable value never hardcoded here — law-schema-over-code). Computed by
    /// the caller, which can read the accessor; this module never can, since it does no I/O.
    pub has_ask_hold: bool,
    /// `Some(id)` when a `supersedes` dependency names `id` as this bead's replacement.
    pub supersedes: Option<String>,
    /// A merge-tree or ancestry proof that this bead's work is
    /// already on base (rule 1's "content on base"). Never derived from a commit subject.
    pub content_on_base: bool,
    /// This bead is a member of a batch that is still OPEN (rule 2). A member of a batch
    /// that has since closed/settled is not — the batch machine's own exit event handles
    /// that member instead, and this classifier falls through to bd status for it.
    pub batch_open_member: bool,
    /// A branch for this bead exists and carries commits the base does not, i.e. work is
    /// still outstanding on it.
    pub branch_ahead: bool,
    /// A commit on base whose message carries a `spira: land <id>` line naming this bead —
    /// a batch landing, whose branch is gone. Found by the caller from base's own log.
    pub landing_commit: Option<String>,
    /// bd's `issue_type` is `epic`: a container, never claimed.
    pub is_epic: bool,
}

#[derive(Debug, Clone)]
pub struct Classification {
    pub state: BeadState,
    pub delivery: Option<DeliveryState>,
    pub pr: Option<String>,
    /// The deciding rule's name — written as the event's evidence (design §4: "each decision
    /// is written as an event, with the classifier as the actor and the deciding rule as
    /// evidence").
    pub rule: &'static str,
    pub holds: BTreeSet<HoldKind>,
    /// Named disagreements between the deciding rule and some other, lower-precedence piece
    /// of evidence. Empty means every oracle this classifier consulted agrees; a non-empty
    /// report is exactly the input to design §4's contradiction report, reviewed after the
    /// fact rather than gating the deploy.
    pub contradictions: Vec<String>,
}

/// Classifies one bead's facts into a state, per design §4's fixed precedence:
/// 1. terminal evidence (supersede, drop, landed-ancestry, content-on-base), in that order;
/// 2. batch membership;
/// 3. bd status, with holder liveness folded into the in-progress arm.
pub fn classify(f: &BeadFacts) -> Classification {
    let mut holds = BTreeSet::new();
    if f.labels.contains("spira-poison") {
        holds.insert(HoldKind::Poison);
    }
    if f.has_ask_hold {
        holds.insert(HoldKind::Ask);
    }

    if f.is_epic {
        return match f.bd_status {
            BdStatus::Closed => finish(f, BeadState::Done, None, None, "epic-closed", holds),
            _ => finish(f, BeadState::Open, None, None, "epic-container", holds),
        };
    }

    if f.supersedes.is_some() {
        return finish(f, BeadState::Superseded, None, None, "terminal-supersede", holds);
    }
    if f.labels.contains("spira-dropped") {
        return finish(f, BeadState::Dropped, None, None, "terminal-dropped", holds);
    }
    if f.content_on_base || f.labels.contains("content-landed") {
        return finish(f, BeadState::Landed, None, None, "terminal-content-on-base", holds);
    }

    if f.landing_commit.is_some() {
        return finish(f, BeadState::Landed, None, None, "terminal-landing-line", holds);
    }

    if f.batch_open_member {
        return finish(f, BeadState::InDelivery, Some(DeliveryState::Batched), None, "batch-membership", holds);
    }

    match f.bd_status {
        BdStatus::Closed => finish(f, BeadState::Dropped, None, None, "residue-closed-no-landing-evidence", holds),
        BdStatus::InProgress if f.holder_alive => finish(f, BeadState::Working, None, None, "holder-alive", holds),
        BdStatus::InProgress => finish(f, BeadState::Ready, None, None, "holder-dead-ghost-reclaim", holds),
        BdStatus::Open => finish(f, BeadState::Ready, None, None, "open-status", holds),
    }
}

fn finish(
    f: &BeadFacts,
    state: BeadState,
    delivery: Option<DeliveryState>,
    pr: Option<String>,
    rule: &'static str,
    holds: BTreeSet<HoldKind>,
) -> Classification {
    let mut contradictions = Vec::new();

    // Every independent tier 1-2 oracle, in isolation, names the state it alone would
    // imply. A different state named by anything but the winning rule is a genuine
    // disagreement between two of the legacy records (design §2.3).
    let mut signals: Vec<(&'static str, BeadState)> = Vec::new();
    if f.supersedes.is_some() {
        signals.push(("supersedes-dependency", BeadState::Superseded));
    }
    if f.labels.contains("spira-dropped") {
        signals.push(("spira-dropped-label", BeadState::Dropped));
    }
    if f.content_on_base {
        signals.push(("content-on-base-proof", BeadState::Landed));
    }
    if f.labels.contains("content-landed") {
        signals.push(("content-landed-label", BeadState::Landed));
    }
    if f.landing_commit.is_some() {
        signals.push(("landing-line-on-base", BeadState::Landed));
    }
    if f.batch_open_member {
        signals.push(("open-batch-membership", BeadState::InDelivery));
    }
    for (name, s) in &signals {
        if *s != state {
            contradictions.push(format!(
                "{name} points to {} but rule {rule} chose {}",
                s.as_str(),
                state.as_str()
            ));
        }
    }

    // A bead a holder claims (bd in_progress) whose authoritative state is not WORKING: an
    // active worker is operating against a row that no longer says it is theirs to hold —
    // exactly the "no precondition on the writer" hazard class (design §2.2).
    if matches!(f.bd_status, BdStatus::InProgress) && state != BeadState::Working {
        contradictions.push(format!("bd status is in_progress but rule {rule} chose {}, not WORKING", state.as_str()));
    }

    // A closed bead landing on a non-terminal state, other than through live batch
    // membership, means bd's own status disagrees with every other record consulted.
    if matches!(f.bd_status, BdStatus::Closed) && !state.is_terminal() && rule != "batch-membership" {
        contradictions.push(format!("bd status is closed but rule {rule} chose the non-terminal state {}", state.as_str()));
    }

    // The submitted label surviving on a bead already at a terminal state is exactly the
    // stale-label residue `bead_close_on_land` never cleaned up (design §2.3).
    if f.labels.contains("spira-submitted") && state.is_terminal() {
        contradictions.push(format!("spira-submitted label is still set on a bead rule {rule} classified terminal ({})", state.as_str()));
    }

    // The closed/no-evidence residue rule never observed a genuine terminal fact — it is a
    // default, not a conclusion, and design §4 says exactly this gets corrected by an
    // explicit operator event after review.
    if rule == crate::bead::RESIDUE_RULE {
        contradictions.push(
            "closed with no supersede, drop, landed-ancestry or content-on-base evidence at all — \
             DROPPED is a default for operator review, not a conclusion"
                .to_string(),
        );
    }

    contradictions.sort();
    contradictions.dedup();
    Classification { state, delivery, pr, rule, holds, contradictions }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead::HoldKind;

    fn labels(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn base_facts() -> BeadFacts {
        BeadFacts {
            bd_status: BdStatus::Open,
            holder_alive: false,
            labels: BTreeSet::new(),
            has_ask_hold: false,
            supersedes: None,
            content_on_base: false,
            batch_open_member: false,
            branch_ahead: false,
            landing_commit: None,
            is_epic: false,
        }
    }

    #[test]
    fn an_epic_is_open_never_ready_and_done_once_closed() {
        let mut f = base_facts();
        f.is_epic = true;
        let c = classify(&f);
        assert_eq!((c.state, c.rule), (BeadState::Open, "epic-container"));
        f.bd_status = BdStatus::InProgress;
        assert_eq!(classify(&f).state, BeadState::Open);
        f.bd_status = BdStatus::Closed;
        assert_eq!(classify(&f).state, BeadState::Done);
        f.is_epic = false;
        f.bd_status = BdStatus::Open;
        assert_eq!(classify(&f).state, BeadState::Ready, "the same facts on a task are READY");
    }

    #[test]
    fn ready_open_no_branch() {
        let c = classify(&base_facts());
        assert_eq!(c.state, BeadState::Ready);
        assert_eq!(c.rule, "open-status");
        assert!(c.contradictions.is_empty(), "{:?}", c.contradictions);
    }

    #[test]
    fn ready_open_with_prior_branch_is_still_legal() {
        let mut f = base_facts();
        f.branch_ahead = true;
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Ready);
        assert!(c.contradictions.is_empty());
    }

    #[test]
    fn held_is_a_hold_on_top_of_ready_not_a_state_of_its_own() {
        let mut f = base_facts();
        f.has_ask_hold = true;
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Ready);
        assert!(c.holds.contains(&HoldKind::Ask));
        assert!(c.contradictions.is_empty());
    }

    #[test]
    fn stale_submitted_label_on_a_landed_closed_bead() {
        let mut f = base_facts();
        f.bd_status = BdStatus::Closed;
        f.content_on_base = true;
        f.labels = labels(&["spira-submitted"]);
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Landed);
        assert_eq!(c.contradictions.len(), 1);
        assert!(c.contradictions[0].contains("spira-submitted"));
    }

    #[test]
    fn closed_never_landed_branch_alive_is_a_dropped_residue_guess() {
        let mut f = base_facts();
        f.bd_status = BdStatus::Closed;
        f.branch_ahead = true;
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Dropped);
        assert_eq!(c.rule, "residue-closed-no-landing-evidence");
        assert_eq!(c.contradictions.len(), 1);
    }

    #[test]
    fn a_landing_line_on_base_lands_a_closed_bead_whose_branch_is_gone() {
        let mut f = base_facts();
        f.bd_status = BdStatus::Closed;
        f.landing_commit = Some("444bd3f3c".into());
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Landed);
        assert_eq!(c.rule, "terminal-landing-line");
        assert!(c.contradictions.is_empty(), "{:?}", c.contradictions);
    }

    #[test]
    fn a_landing_line_outranks_an_open_batch() {
        let mut f = base_facts();
        f.batch_open_member = true;
        f.landing_commit = Some("abc".into());
        assert_eq!(classify(&f).rule, "terminal-landing-line");
    }

    #[test]
    fn being_worked_with_a_live_holder_is_working() {
        let mut f = base_facts();
        f.bd_status = BdStatus::InProgress;
        f.holder_alive = true;
        assert_eq!(classify(&f).state, BeadState::Working);
    }

    #[test]
    fn landed_open_and_poisoned() {
        let mut f = base_facts();
        f.content_on_base = true;
        f.labels = labels(&["spira-poison"]);
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Landed);
        assert!(c.holds.contains(&HoldKind::Poison));
    }

    // Content on base is never commit-subject matching: the only evidence is the independent
    // ancestry/merge-tree proof the producer computed.
    #[test]
    fn content_on_base_lands_regardless_of_labels() {
        let mut f = base_facts();
        f.bd_status = BdStatus::Closed;
        f.content_on_base = true;
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Landed);
        assert_eq!(c.rule, "terminal-content-on-base");
    }

    #[test]
    fn supersede_outranks_every_other_terminal_signal() {
        let mut f = base_facts();
        f.supersedes = Some("sp-successor".to_string());
        f.labels = labels(&["spira-dropped"]);
        f.content_on_base = true;
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Superseded);
        assert_eq!(c.rule, "terminal-supersede");
    }

    #[test]
    fn dropped_label_outranks_content_on_base() {
        let mut f = base_facts();
        f.labels = labels(&["spira-dropped"]);
        f.content_on_base = true;
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Dropped);
        assert_eq!(c.rule, "terminal-dropped");
        assert!(c.contradictions.iter().any(|s| s.contains("content-on-base-proof")));
    }

    #[test]
    fn batch_membership_outranks_bd_status() {
        let mut f = base_facts();
        f.batch_open_member = true;
        let c = classify(&f);
        assert_eq!(c.state, BeadState::InDelivery);
        assert_eq!(c.delivery, Some(DeliveryState::Batched));
        assert_eq!(c.rule, "batch-membership");
    }

    #[test]
    fn classifying_the_same_facts_twice_agrees() {
        let f = base_facts();
        let a = classify(&f);
        let b = classify(&f);
        assert_eq!(a.state, b.state);
        assert_eq!(a.rule, b.rule);
        assert_eq!(a.contradictions, b.contradictions);
    }
}
