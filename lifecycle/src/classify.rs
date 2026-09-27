//! The one-time migration classifier (design §4): assigns every existing work bead a state
//! by reading the six legacy records in a fixed precedence, rather than a machine transition
//! — there is no prior row to compare-and-swap against, only history to make sense of once.
//!
//! Pure, like the rest of this crate: every fact this module reads (bd status, labels, the
//! landstate ledger, batch membership, git ancestry, a content-on-base proof) is gathered by
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

/// The `landstate/<id>` ledger's state token. Absence (no file, or a file this parser does
/// not recognize) is `None` on [`BeadFacts::landstate`], not a variant here — the design's
/// own table writes "landstate none" as a distinct case from any named state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LandState {
    Withdrawn,
    Red,
    Ejected,
    Gating,
    Gated,
    Certified,
    Batched,
    /// Written by the Sending when a merge-tree proof shows the tip's content already on
    /// base — the same non-subject evidence `content_on_base` on [`BeadFacts`] carries, kept
    /// as its own variant because the ledger may record it before this run's own git check.
    Content,
    Landed,
    /// pr-mode delivery: the reason column carries `pr-open:<name>`.
    RebasedPrOpen(String),
    /// push-mode delivery swept by a base move: the tip moved, so certification is void
    /// regardless of what it had been (design §4's own push-mode rule).
    RebasedSwept,
    RebasedRecutSwept,
}

/// Parses one `landstate/<id>` file's contents: `"<STATE> <tip> <at> [reason...]"`, exactly
/// `land_mark`'s own write format. Returns `None` for a state word or a `REBASED` reason
/// this classifier does not recognize, so the caller falls through to the next precedence
/// tier rather than misreading garbage as a specific state.
pub fn parse_landstate(raw: &str) -> Option<(LandState, Option<String>)> {
    let mut parts = raw.split_whitespace();
    let state = parts.next()?;
    let tip = parts.next().filter(|t| *t != "none").map(str::to_string);
    let _at = parts.next();
    let reason: String = parts.collect::<Vec<_>>().join(" ");
    let ls = match state {
        "WITHDRAWN" => LandState::Withdrawn,
        "RED" => LandState::Red,
        "EJECTED" => LandState::Ejected,
        "GATING" => LandState::Gating,
        "GATED" => LandState::Gated,
        "CERTIFIED" => LandState::Certified,
        "BATCHED" => LandState::Batched,
        "CONTENT" => LandState::Content,
        "LANDED" => LandState::Landed,
        "REBASED" => {
            if let Some(name) = reason.strip_prefix("pr-open:") {
                LandState::RebasedPrOpen(name.to_string())
            } else if reason == "swept" {
                LandState::RebasedSwept
            } else if reason == "recut-swept" {
                LandState::RebasedRecutSwept
            } else {
                return None;
            }
        }
        _ => return None,
    };
    Some((ls, tip))
}

/// Every fact the classifier's precedence table (design §4) reads, already gathered by the
/// caller. Nothing here is I/O: `content_on_base` and `tip_ancestor_of_base` are the results
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
    pub landstate: Option<LandState>,
    /// Whether the ledger's own recorded tip (not necessarily a still-existing branch) is an
    /// ancestor of the repository's base — the discriminating fact for rule 1's "landstate
    /// LANDED whose tip is an ancestor of base".
    pub tip_ancestor_of_base: bool,
    /// A merge-tree or ancestry proof, independent of the ledger, that this bead's work is
    /// already on base (rule 1's "content on base"). Never derived from a commit subject.
    pub content_on_base: bool,
    /// This bead is a member of a batch that is still OPEN (rule 2). A member of a batch
    /// that has since closed/settled is not — the batch machine's own exit event handles
    /// that member instead, and this classifier falls through to the ledger for it.
    pub batch_open_member: bool,
    /// A branch for this bead exists and carries commits the base does not, i.e. work is
    /// still outstanding on it. Distinguishes a live REWORK from a RED/EJECTED ledger entry
    /// whose branch has since been reset or deleted (design §2.1: READY's own legal shapes
    /// include a stale RED/EJECTED ledger with nothing left to rework).
    pub branch_ahead: bool,
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
/// 3. the ledger (landstate);
/// 4. bd status;
/// 5. branch and holder (folded into the bd-status arms below, since branch-ahead is only
///    ever a distinguishing fact for the RED/EJECTED ledger arms in tier 3, and holder
///    liveness only for the in-progress bd-status arm in tier 4).
pub fn classify(f: &BeadFacts) -> Classification {
    let mut holds = BTreeSet::new();
    if f.labels.contains("spira-poison") {
        holds.insert(HoldKind::Poison);
    }
    if f.has_ask_hold {
        holds.insert(HoldKind::Ask);
    }

    if f.supersedes.is_some() {
        return finish(f, BeadState::Superseded, None, None, "terminal-supersede", holds);
    }
    if f.labels.contains("spira-dropped") {
        return finish(f, BeadState::Dropped, None, None, "terminal-dropped", holds);
    }
    if matches!(f.landstate, Some(LandState::Landed)) && f.tip_ancestor_of_base {
        return finish(f, BeadState::Landed, None, None, "terminal-landed-ancestry", holds);
    }
    if f.content_on_base || matches!(f.landstate, Some(LandState::Content)) || f.labels.contains("content-landed") {
        return finish(f, BeadState::Landed, None, None, "terminal-content-on-base", holds);
    }

    if f.batch_open_member {
        return finish(f, BeadState::InDelivery, Some(DeliveryState::Batched), None, "batch-membership", holds);
    }

    if let Some(ls) = &f.landstate {
        let (state, delivery, pr, rule): (BeadState, Option<DeliveryState>, Option<String>, &'static str) = match ls {
            LandState::Certified => (BeadState::Certified, None, None, "ledger-certified"),
            LandState::Gating => (BeadState::Submitted, None, None, "ledger-gating"),
            LandState::Gated => (BeadState::Submitted, None, None, "ledger-gated"),
            LandState::RebasedPrOpen(pr) => (BeadState::InDelivery, Some(DeliveryState::PrOpen), Some(pr.clone()), "ledger-pr-open"),
            LandState::RebasedSwept => (BeadState::Submitted, None, None, "ledger-rebased-swept-tip-voided"),
            LandState::RebasedRecutSwept => (BeadState::Submitted, None, None, "ledger-rebased-recut-swept-tip-voided"),
            LandState::Batched => (BeadState::InDelivery, Some(DeliveryState::Batched), None, "ledger-batched"),
            LandState::Withdrawn => (BeadState::Ready, None, None, "ledger-withdrawn"),
            LandState::Red | LandState::Ejected if f.branch_ahead => (BeadState::Rework, None, None, "ledger-red-or-ejected"),
            LandState::Red | LandState::Ejected => (BeadState::Ready, None, None, "ledger-red-or-ejected-stale-no-branch"),
            LandState::Landed | LandState::Content => unreachable!("handled by the terminal-evidence tier above"),
        };
        return finish(f, state, delivery, pr, rule, holds);
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

    // Every independent tier 1-3 oracle, in isolation, names the state it alone would
    // imply. A different state named by anything but the winning rule is a genuine
    // disagreement between two of the six legacy records (design §2.3).
    let mut signals: Vec<(&'static str, BeadState)> = Vec::new();
    if f.supersedes.is_some() {
        signals.push(("supersedes-dependency", BeadState::Superseded));
    }
    if f.labels.contains("spira-dropped") {
        signals.push(("spira-dropped-label", BeadState::Dropped));
    }
    if matches!(f.landstate, Some(LandState::Landed)) {
        signals.push(("landstate-landed", BeadState::Landed));
    }
    if f.content_on_base {
        signals.push(("content-on-base-proof", BeadState::Landed));
    }
    if f.labels.contains("content-landed") {
        signals.push(("content-landed-label", BeadState::Landed));
    }
    if f.batch_open_member {
        signals.push(("open-batch-membership", BeadState::InDelivery));
    }
    if let Some(ls) = &f.landstate {
        let mapped = match ls {
            LandState::Certified => Some(BeadState::Certified),
            LandState::Gating | LandState::Gated => Some(BeadState::Submitted),
            LandState::RebasedPrOpen(_) => Some(BeadState::InDelivery),
            LandState::RebasedSwept | LandState::RebasedRecutSwept => Some(BeadState::Submitted),
            LandState::Batched => Some(BeadState::InDelivery),
            LandState::Withdrawn => Some(BeadState::Ready),
            LandState::Red | LandState::Ejected if f.branch_ahead => Some(BeadState::Rework),
            LandState::Red | LandState::Ejected => Some(BeadState::Ready),
            LandState::Landed | LandState::Content => None,
        };
        if let Some(m) = mapped {
            signals.push(("ledger", m));
        }
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
    if rule == "residue-closed-no-landing-evidence" {
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
            landstate: None,
            tip_ancestor_of_base: false,
            content_on_base: false,
            batch_open_member: false,
            branch_ahead: false,
        }
    }

    // ── the landstate ledger's file format ────────────────────────────────────────────
    #[test]
    fn parse_landstate_reads_land_marks_own_write_format() {
        let (ls, tip) = parse_landstate("LANDED abc123 1700000000 ").unwrap();
        assert_eq!(ls, LandState::Landed);
        assert_eq!(tip.as_deref(), Some("abc123"));

        let (ls, tip) = parse_landstate("CERTIFIED none 1700000000").unwrap();
        assert_eq!(ls, LandState::Certified);
        assert_eq!(tip, None);

        let (ls, _) = parse_landstate("REBASED sometip 1700000000 pr-open:service").unwrap();
        assert_eq!(ls, LandState::RebasedPrOpen("service".to_string()));

        let (ls, _) = parse_landstate("REBASED sometip 1700000000 swept").unwrap();
        assert_eq!(ls, LandState::RebasedSwept);

        let (ls, _) = parse_landstate("REBASED sometip 1700000000 recut-swept").unwrap();
        assert_eq!(ls, LandState::RebasedRecutSwept);
    }

    #[test]
    fn parse_landstate_refuses_an_unrecognized_state_or_rebased_reason() {
        assert!(parse_landstate("BOGUS tip 1700000000").is_none());
        assert!(parse_landstate("REBASED tip 1700000000 something-else").is_none());
    }

    // ── design §2.3's documented, "(legal)" combinations classify cleanly ─────────────

    #[test]
    fn ready_open_no_landstate_no_branch() {
        let f = base_facts();
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Ready);
        assert_eq!(c.rule, "open-status");
        assert!(c.contradictions.is_empty(), "{:?}", c.contradictions);
    }

    #[test]
    fn ready_open_with_prior_branch_is_still_legal() {
        let mut f = base_facts();
        f.branch_ahead = true; // a branch exists, but landstate is still none
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Ready);
        assert!(c.contradictions.is_empty());
    }

    #[test]
    fn landed_closed_with_ancestry_proof() {
        let mut f = base_facts();
        f.bd_status = BdStatus::Closed;
        f.landstate = Some(LandState::Landed);
        f.tip_ancestor_of_base = true;
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Landed);
        assert_eq!(c.rule, "terminal-landed-ancestry");
        assert!(c.contradictions.is_empty(), "{:?}", c.contradictions);
    }

    #[test]
    fn certified_open_submitted_label_and_ledger_agree() {
        let mut f = base_facts();
        f.landstate = Some(LandState::Certified);
        f.branch_ahead = true;
        f.labels = labels(&["spira-submitted"]);
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Certified);
        assert_eq!(c.rule, "ledger-certified");
        assert!(c.contradictions.is_empty(), "{:?}", c.contradictions);
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

    // ── design §2.3's documented contradictions still classify, but are flagged ───────

    #[test]
    fn stale_submitted_label_on_a_landed_closed_bead() {
        let mut f = base_facts();
        f.bd_status = BdStatus::Closed;
        f.landstate = Some(LandState::Landed);
        f.tip_ancestor_of_base = true;
        f.labels = labels(&["spira-submitted"]);
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Landed);
        assert_eq!(c.contradictions.len(), 1);
        assert!(c.contradictions[0].contains("spira-submitted"));
    }

    #[test]
    fn submitted_yet_red_lands_on_rework_via_the_ledger() {
        let mut f = base_facts();
        f.landstate = Some(LandState::Red);
        f.branch_ahead = true;
        f.labels = labels(&["spira-submitted"]);
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Rework);
        assert_eq!(c.rule, "ledger-red-or-ejected");
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
    fn closed_and_ungated_branch_gone() {
        let mut f = base_facts();
        f.bd_status = BdStatus::Closed;
        f.landstate = Some(LandState::Gated);
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Submitted);
        assert_eq!(c.rule, "ledger-gated");
        assert!(c.contradictions.iter().any(|s| s.contains("closed")));
    }

    #[test]
    fn closed_but_still_in_the_batch_pool() {
        let mut f = base_facts();
        f.bd_status = BdStatus::Closed;
        f.landstate = Some(LandState::Certified);
        f.branch_ahead = true;
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Certified);
        assert!(c.contradictions.iter().any(|s| s.contains("closed")));
    }

    #[test]
    fn landed_and_dropped_at_once_drop_wins_by_precedence() {
        let mut f = base_facts();
        f.bd_status = BdStatus::Closed;
        f.landstate = Some(LandState::Landed);
        f.tip_ancestor_of_base = true;
        f.labels = labels(&["spira-dropped", "spira-submitted"]);
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Dropped);
        assert_eq!(c.rule, "terminal-dropped");
        assert!(c.contradictions.iter().any(|s| s.contains("landstate-landed")));
    }

    #[test]
    fn being_worked_while_certified() {
        let mut f = base_facts();
        f.bd_status = BdStatus::InProgress;
        f.holder_alive = true;
        f.landstate = Some(LandState::Certified);
        f.branch_ahead = true;
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Certified);
        assert!(c.contradictions.iter().any(|s| s.contains("in_progress")));
    }

    #[test]
    fn landed_open_and_poisoned() {
        let mut f = base_facts();
        f.landstate = Some(LandState::Landed);
        f.tip_ancestor_of_base = true;
        f.labels = labels(&["spira-poison"]);
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Landed);
        assert!(c.holds.contains(&HoldKind::Poison));
    }

    // ── design §4's explicit pr-mode / push-mode rules ────────────────────────────────

    #[test]
    fn rebased_pr_open_enters_in_delivery_pr_open() {
        let mut f = base_facts();
        f.landstate = Some(LandState::RebasedPrOpen("service".to_string()));
        f.branch_ahead = true;
        let c = classify(&f);
        assert_eq!(c.state, BeadState::InDelivery);
        assert_eq!(c.delivery, Some(DeliveryState::PrOpen));
        assert_eq!(c.pr.as_deref(), Some("service"));
        assert!(c.contradictions.is_empty());
    }

    #[test]
    fn rebased_swept_voids_certification_back_to_submitted() {
        for ls in [LandState::RebasedSwept, LandState::RebasedRecutSwept] {
            let mut f = base_facts();
            f.landstate = Some(ls);
            f.branch_ahead = true;
            let c = classify(&f);
            assert_eq!(c.state, BeadState::Submitted, "tip moved, certification must be void");
        }
    }

    // ── the migration fixture requirement: content on base is never subject matching ──
    // These fixtures generalize the three production shapes recorded on this bead (a
    // conventional-commits scope form, a trailing-id form, and a landed fix whose own
    // commit's hash was rewritten but whose content and a later commit's ancestry both
    // prove it reached base) without naming them: any implementation reached by a
    // commit-subject regex would answer "not landed" about all three, and this classifier
    // must not repeat that (law-absence-needs-a-positive-control: a subject-matching stub
    // is the offender this suite plants and checks refuses to pass).

    #[test]
    fn content_on_base_lands_regardless_of_ledger_or_labels() {
        // The bead's own tip never had a "spira: land <id>" or "<id>:" subject, and the
        // ledger was never updated either — the ONLY evidence is the independent
        // ancestry/merge-tree proof the producer computed.
        let mut f = base_facts();
        f.bd_status = BdStatus::Closed;
        f.content_on_base = true;
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Landed);
        assert_eq!(c.rule, "terminal-content-on-base");
    }

    #[test]
    fn a_rewritten_hash_still_lands_via_content_proof_not_tip_ancestry() {
        // The bead's recorded ledger tip is NOT an ancestor of base (its hash was
        // rewritten by a later rebase), but a merge-tree proof independently shows the
        // content is already there — exactly the shape a subject match cannot see either
        // way, and an ancestry-only check would also miss.
        let mut f = base_facts();
        f.bd_status = BdStatus::Closed;
        f.landstate = Some(LandState::Landed);
        f.tip_ancestor_of_base = false; // the exact recorded tip sha is gone
        f.content_on_base = true; // but the content proof still succeeds
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Landed);
        assert_eq!(c.rule, "terminal-content-on-base");
    }

    // ── the precedence order itself ───────────────────────────────────────────────────

    #[test]
    fn supersede_outranks_every_other_terminal_signal() {
        let mut f = base_facts();
        f.supersedes = Some("sp-successor".to_string());
        f.labels = labels(&["spira-dropped"]);
        f.landstate = Some(LandState::Landed);
        f.tip_ancestor_of_base = true;
        f.content_on_base = true;
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Superseded);
        assert_eq!(c.rule, "terminal-supersede");
    }

    #[test]
    fn dropped_label_outranks_landed_ancestry_and_content() {
        let mut f = base_facts();
        f.labels = labels(&["spira-dropped"]);
        f.landstate = Some(LandState::Landed);
        f.tip_ancestor_of_base = true;
        f.content_on_base = true;
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Dropped);
        assert_eq!(c.rule, "terminal-dropped");
    }

    #[test]
    fn batch_membership_outranks_the_ledger() {
        let mut f = base_facts();
        f.batch_open_member = true;
        f.landstate = Some(LandState::Certified);
        let c = classify(&f);
        assert_eq!(c.state, BeadState::InDelivery);
        assert_eq!(c.delivery, Some(DeliveryState::Batched));
        assert_eq!(c.rule, "batch-membership");
    }

    #[test]
    fn the_ledger_outranks_bare_bd_status() {
        let mut f = base_facts();
        f.bd_status = BdStatus::Closed;
        f.landstate = Some(LandState::Gating);
        let c = classify(&f);
        assert_eq!(c.state, BeadState::Submitted);
        assert_eq!(c.rule, "ledger-gating");
    }

    // ── idempotency of the pure function itself: same facts, same answer ─────────────

    #[test]
    fn classifying_the_same_facts_twice_agrees() {
        let mut f = base_facts();
        f.landstate = Some(LandState::Certified);
        let a = classify(&f);
        let b = classify(&f);
        assert_eq!(a.state, b.state);
        assert_eq!(a.rule, b.rule);
        assert_eq!(a.contradictions, b.contradictions);
    }
}
