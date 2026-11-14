use std::collections::{BTreeMap, BTreeSet};

use lifecycle::bead::{BeadState, HoldKind, Stack};

use super::*;
use crate::control::records::ClaimInputs;

fn rec(id: &str, state: BeadState) -> LifecycleRecord {
    let mut r = LifecycleRecord::born(id, Some("spira"), None, 100);
    r.row.state = state;
    r.row.tip = Some(format!("tip-{id}"));
    r.claim = Some(ClaimInputs::default());
    r
}

fn blocked_on(id: &str, state: BeadState, blocker: &str) -> LifecycleRecord {
    let mut r = rec(id, state);
    r.claim.as_mut().unwrap().blocks.push(blocker.into());
    r
}

fn rows(recs: &[&LifecycleRecord]) -> BTreeMap<String, LifecycleRecord> {
    recs.iter().map(|r| (r.row.bead_id.clone(), (*r).clone())).collect()
}

fn ctx() -> Ctx {
    Ctx { now: 1_000, stack_max: 4, incident_label: "incident".into(), ..Ctx::default() }
}

fn verdict(cand: &LifecycleRecord, others: &[&LifecycleRecord], ctx: &Ctx) -> Judgement {
    let mut all = others.to_vec();
    all.push(cand);
    judge(cand, &rows(&all), &BTreeMap::new(), ctx)
}

fn blocked(of: &str) -> (Verdict, Option<String>) {
    (Verdict::Blocked, Some(of.into()))
}

fn seen(j: &Judgement) -> (Verdict, Option<String>) {
    (j.verdict, j.blocker.clone())
}

#[test]
fn a_member_of_an_open_integration_is_carried_and_the_integration_is_not() {
    let int = rec("I", BeadState::Ready);
    let mut member = rec("A", BeadState::Ready);
    member.carried_by = Some("I".into());
    assert_eq!(seen(&verdict(&member, &[&int], &ctx())), (Verdict::Carried, Some("I".into())));
    assert_eq!(verdict(&int, &[], &ctx()).verdict, Verdict::Claimable);
}

#[test]
fn a_member_carried_by_a_landed_integration_is_claimable_and_one_carried_by_no_record_is_carried() {
    let landed = rec("I", BeadState::Landed);
    let mut member = rec("A", BeadState::Ready);
    member.carried_by = Some("I".into());
    assert_eq!(verdict(&member, &[&landed], &ctx()).verdict, Verdict::Claimable);
    assert_eq!(verdict(&member, &[], &ctx()).verdict, Verdict::Carried);
}

#[test]
fn an_incident_never_stacks_on_a_certified_fix_but_claims_once_it_lands() {
    let certified = rec("A", BeadState::Certified);
    let mut inc = blocked_on("B", BeadState::Ready, "A");
    inc.claim.as_mut().unwrap().labels.push("incident".into());
    assert_eq!(seen(&verdict(&inc, &[&certified], &ctx())), blocked("A"));
    let plain = blocked_on("B", BeadState::Ready, "A");
    let j = verdict(&plain, &[&certified], &ctx());
    assert_eq!((j.verdict, j.depth), (Verdict::Claimable, 1), "a non-incident still stacks");
    let landed = rec("A", BeadState::Landed);
    let j = verdict(&inc, &[&landed], &ctx());
    assert_eq!((j.verdict, j.depth), (Verdict::Claimable, 0));
}

#[test]
fn stacked_blocker_certified_is_claimable() {
    let j = verdict(&blocked_on("B", BeadState::Ready, "A"), &[&rec("A", BeadState::Certified)], &ctx());
    assert_eq!((j.verdict, j.depth), (Verdict::Claimable, 1));
}

#[test]
fn a_rework_on_the_stack_the_gate_went_red_on_is_not_claimable_until_the_tip_moves() {
    let mut a = rec("A", BeadState::Certified);
    let mut b = blocked_on("B", BeadState::Rework, "A");
    b.row.stack = Stack::from([("A".to_string(), "tip-A".to_string())]);
    b.row.reason = Some("suites-failed".into());
    assert_eq!(verdict(&b, &[&a], &ctx()).verdict, Verdict::StackUnchanged);
    a.row.tip = Some("moved".into());
    assert_eq!(verdict(&b, &[&a], &ctx()).verdict, Verdict::Claimable);
    a.row.tip = Some("tip-A".into());
    b.row.reason = Some("base_withdrawn: A x".into());
    assert_eq!(verdict(&b, &[&a], &ctx()).verdict, Verdict::Claimable, "only a gate-red reason repeats");
}

#[test]
fn stack_plan_names_the_stacked_prerequisites_tip() {
    let j = verdict(&blocked_on("B", BeadState::Ready, "A"), &[&rec("A", BeadState::Certified)], &ctx());
    assert_eq!(j.depth, 1);
    assert_eq!(j.stack.get("A").map(String::as_str), Some("tip-A"));
}

#[test]
fn stack_plan_omits_a_landed_blocker_it_does_not_stack_on() {
    let j = verdict(&blocked_on("B", BeadState::Ready, "A"), &[&rec("A", BeadState::Landed)], &ctx());
    assert_eq!(j.depth, 0);
    assert!(j.stack.is_empty(), "a landed prerequisite is satisfied, not stacked on");
}

#[test]
fn stack_plan_too_deep_still_reports_the_attempted_stack() {
    let mut a = rec("A", BeadState::InDelivery);
    a.row.stack_depth = 4;
    let j = verdict(&blocked_on("B", BeadState::Ready, "A"), &[&a], &ctx());
    assert_eq!((j.verdict, j.depth), (Verdict::TooDeep, 5));
    assert_eq!(j.stack.get("A").map(String::as_str), Some("tip-A"));
}

fn conflict(id: &str, tip: &str) -> Ctx {
    Ctx { conflicts: BTreeSet::from([(id.to_string(), tip.to_string())]), ..ctx() }
}

#[test]
fn a_dependent_of_a_conflicting_parent_is_blocked_until_the_parent_rebases() {
    let mut a = rec("A", BeadState::Certified);
    let b = blocked_on("B", BeadState::Ready, "A");
    assert_eq!(verdict(&b, &[&a], &ctx()).verdict, Verdict::Claimable, "no marker: stacked as before");
    assert_eq!(seen(&verdict(&b, &[&a], &conflict("A", "tip-A"))), blocked("A"));
    a.row.tip = Some("tip-A-rebased".into());
    assert_eq!(verdict(&b, &[&a], &conflict("A", "tip-A")).verdict, Verdict::Claimable, "the parent's tip moved");
}

#[test]
fn an_old_marker_on_an_in_delivery_parent_does_not_block_its_dependent() {
    let a = rec("A", BeadState::InDelivery);
    let b = blocked_on("B", BeadState::Rework, "A");
    assert_eq!(seen(&verdict(&b, &[&a], &conflict("A", "tip-A"))), blocked("A"), "a held marker blocks");
    assert_eq!(verdict(&b, &[&a], &ctx()).verdict, Verdict::Claimable, "a lapsed marker is not in the context");
}

#[test]
fn a_dependent_waiting_on_a_conflicting_blocker_is_released_when_the_blocker_lands() {
    let mut a = rec("A", BeadState::Certified);
    let b = blocked_on("B", BeadState::Rework, "A");
    assert_eq!(seen(&verdict(&b, &[&a], &conflict("A", "tip-A"))), blocked("A"));
    a.row.state = BeadState::Landed;
    let j = verdict(&b, &[&a], &conflict("A", "tip-A"));
    assert_eq!((j.verdict, j.depth), (Verdict::Claimable, 0), "landed: one rebase, no stack");
}

#[test]
fn stacked_blocker_only_submitted_waits() {
    let j = verdict(&blocked_on("B", BeadState::Ready, "A"), &[&rec("A", BeadState::Submitted)], &ctx());
    assert_eq!(seen(&j), blocked("A"));
}

#[test]
fn a_blocker_landing_releases_its_dependent_with_no_hand_step_but_a_manual_hold_never_lifts() {
    let dep = blocked_on("B", BeadState::Ready, "A");
    let no_stack = Ctx { stack_max: 0, ..ctx() };
    for blocker in [BeadState::Working, BeadState::Submitted] {
        assert_eq!(seen(&verdict(&dep, &[&rec("A", blocker)], &no_stack)), blocked("A"), "{blocker:?}");
    }
    let landed = rec("A", BeadState::Landed);
    assert_eq!(verdict(&dep, &[&landed], &no_stack).verdict, Verdict::Claimable);
    let mut parked = dep.clone();
    parked.row.holds.insert(HoldKind::Manual);
    assert_eq!(verdict(&parked, &[&landed], &no_stack).verdict, Verdict::Held);
}

#[test]
fn a_record_whose_claim_inputs_were_never_written_cannot_be_told_and_is_never_claimable() {
    let mut r = rec("A", BeadState::Ready);
    r.claim = None;
    let j = verdict(&r, &[], &ctx());
    assert_eq!((j.verdict, j.missing), (Verdict::CannotTell, vec!["claim"]));
    let mut r = rec("A", BeadState::Ready);
    r.repo = None;
    let j = verdict(&r, &[], &ctx());
    assert_eq!((j.verdict, j.missing), (Verdict::CannotTell, vec!["repo"]));
}

#[test]
fn a_held_record_with_no_claim_inputs_is_held_not_cannot_tell() {
    let mut r = rec("A", BeadState::Ready);
    r.claim = None;
    r.row.holds.insert(HoldKind::Manual);
    assert_eq!(verdict(&r, &[], &ctx()).verdict, Verdict::Held);
}

#[test]
fn a_wait_hold_with_a_snooze_in_the_future_is_snoozed_and_a_lapsed_one_is_not() {
    let mut r = rec("A", BeadState::Ready);
    r.row.holds.insert(HoldKind::Wait);
    r.row.reason = Some(spira_store::lc_state::snooze_reason(2_000));
    assert_eq!(verdict(&r, &[], &ctx()).verdict, Verdict::Snoozed);
    r.row.reason = Some(spira_store::lc_state::snooze_reason(500));
    assert_eq!(verdict(&r, &[], &ctx()).verdict, Verdict::Claimable);
}

#[test]
fn a_no_loop_labelled_record_is_out_of_scope_and_a_missing_scope_label_is_too() {
    let mut r = rec("A", BeadState::Ready);
    r.claim.as_mut().unwrap().labels.push("fixture-parked".into());
    let c = Ctx { no_loop_label: "fixture-parked".into(), ..ctx() };
    assert_eq!(verdict(&r, &[], &c).verdict, Verdict::OutOfScope);
    let plain = rec("A", BeadState::Ready);
    let c = Ctx { scope_label: "mine".into(), ..ctx() };
    assert_eq!(verdict(&plain, &[], &c).verdict, Verdict::OutOfScope);
}

#[test]
fn a_reserved_id_or_a_stopped_repo_is_stopped_but_a_blocked_record_there_stays_blocked() {
    let r = rec("A", BeadState::Ready);
    let reserved = Ctx { reserved: BTreeSet::from(["A".to_string()]), ..ctx() };
    assert_eq!(verdict(&r, &[], &reserved).verdict, Verdict::Stopped);
    let stopped = Ctx { stopped_repos: BTreeSet::from(["spira".to_string()]), ..ctx() };
    assert_eq!(verdict(&r, &[], &stopped).verdict, Verdict::Stopped);
    let b = blocked_on("B", BeadState::Ready, "A");
    assert_eq!(seen(&verdict(&b, &[&rec("A", BeadState::Working)], &stopped)), blocked("A"));
}

#[test]
fn a_rowless_gate_blocks_while_open_and_releases_when_closed() {
    let b = blocked_on("B", BeadState::Ready, "G");
    let judged = |status: &str| {
        let rowless = BTreeMap::from([("G".to_string(), ("gate".to_string(), status.to_string()))]);
        judge(&b, &rows(&[&b]), &rowless, &ctx())
    };
    let open = judged("open");
    assert_eq!(seen(&open), blocked("G"));
    assert_eq!(open.gates.get("G"), Some(&None));
    assert_eq!(judged("closed").verdict, Verdict::Claimable);
}

#[test]
fn gates_name_every_blocker_read_with_its_gate() {
    let b = blocked_on("B", BeadState::Ready, "A");
    let a = rec("A", BeadState::Working);
    let j = verdict(&b, &[&a], &ctx());
    assert_eq!(j.gates, BTreeMap::from([("A".to_string(), Some(a.gate()))]));
}
