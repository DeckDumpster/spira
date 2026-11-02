use super::round_controller::{status, LANE_BLOCKED_PASSES, SLOT_WAIT_SECS};
use super::*;

fn m(bead: &str, tip: &str) -> Member {
    Member { bead: bead.into(), tip: tip.into() }
}

fn round(batch: &str, from: BatchState, to: BatchState, members: &[Member], parent: Option<&str>, cause: &str) -> Record {
    Record::Round(Round {
        at: String::new(),
        repo: "repo-b".into(),
        batch: batch.into(),
        state_from: from,
        state_to: to,
        members: members.to_vec(),
        parent: parent.map(String::from),
        cause: cause.into(),
        verdict: None,
    })
}

fn bead(id: &str, to: &str, event: &str) -> Record {
    Record::Transition(Transition::Bead {
        at: String::new(),
        id: id.into(),
        from: "IN_DELIVERY".into(),
        to: to.into(),
        event: event.into(),
        tip: None,
        code: None,
    })
}

use spira_desired_state::section::ConditionStatus;
use BatchState::{Abandoned, Attributing, CiRunning, Green, Landed, Open, Rebuilding};

/// The state a round is cut from.
const CUT: BatchState = BatchState::None;

/// A world where `parent` sits ATTRIBUTING and a child stacked on it went green at t=100.
fn stacked() -> ReplayWorld {
    let mut w = ReplayWorld::default();
    let a = m("b-1", "aaa");
    w.apply(0, &round("P", CUT, Open, &[m("b-0", "000")], None, "MemberAdded"));
    w.apply(10, &round("P", Open, CiRunning, &[m("b-0", "000")], None, "PassStarted"));
    w.apply(20, &round("P", CiRunning, Attributing, &[m("b-0", "000")], None, "infra"));
    w.apply(30, &round("C", CUT, Open, std::slice::from_ref(&a), Some("P"), "MemberAdded"));
    w.apply(40, &round("C", Open, CiRunning, std::slice::from_ref(&a), Some("P"), "PassStarted"));
    w.apply(100, &round("C", CiRunning, Green, std::slice::from_ref(&a), Some("P"), "green"));
    w.apply(110, &round("C", Green, Abandoned, &[a], Some("P"), "Abandon"));
    w
}

#[test]
fn a_parent_with_a_pass_running_is_driven_and_a_terminal_one_blocks_nothing() {
    let mut w = stacked();
    w.apply(200, &round("P", Attributing, Rebuilding, &[m("b-0", "000")], None, "PassRebuilt"));
    assert!(w.rounds["P"].leased());
    assert_eq!(status(&w, "repo-b", 300).conditions[0].status, ConditionStatus::False);
    w.apply(300, &round("P", Rebuilding, Abandoned, &[m("b-0", "000")], None, "Abandon"));
    let st = status(&w, "repo-b", 400);
    assert_eq!(st.conditions[0].status, ConditionStatus::False);
    assert_eq!(st.green_unlanded_minutes, 5, "the member still waits to land");
}

#[test]
fn a_red_tip_cut_again_is_a_repeat_until_it_goes_green_lands_or_leaves() {
    let a = m("b-7", "ccc");
    let red = |w: &mut ReplayWorld, batch: &str, at: u64, cause: &str| {
        w.apply(at, &round(batch, CUT, Open, std::slice::from_ref(&a), None, "MemberAdded"));
        w.apply(at + 1, &round(batch, Open, CiRunning, std::slice::from_ref(&a), None, "PassStarted"));
        w.apply(at + 2, &round(batch, CiRunning, Attributing, std::slice::from_ref(&a), None, cause));
    };
    let mut w = ReplayWorld::default();
    red(&mut w, "R1", 0, "PassRed");
    assert!(w.repeats.is_empty(), "the first cut is not a repeat");
    red(&mut w, "R2", 10, "red-attributed");
    red(&mut w, "R3", 20, "red-attributed");
    assert_eq!(w.repeats[&a].cuts, 2);
    let st = status(&w, "repo-b", 30);
    assert_eq!(st.repeat_identity_open, 1);
    assert_eq!(st.conditions[1].reason, "b-7@ccc re-cut 2 time(s) after red");

    let mut green = w.clone();
    green.apply(30, &round("R4", CUT, Open, std::slice::from_ref(&a), None, "MemberAdded"));
    green.apply(31, &round("R4", Open, CiRunning, std::slice::from_ref(&a), None, "PassStarted"));
    green.apply(32, &round("R4", CiRunning, Green, std::slice::from_ref(&a), None, "green"));
    assert!(green.repeats.is_empty());
    green.apply(33, &round("R4", Green, Landed, std::slice::from_ref(&a), None, "FastForward"));
    assert!(green.green_waiting.is_empty(), "a landed round's members no longer wait");
    assert_eq!(green.lanes["repo-b"].last_landed, Some(33));

    for (to, event) in [("REWORK", "Returned"), ("CERTIFIED", "Hold"), ("LANDED", "Delivered"), ("DROPPED", "Drop"), ("SUPERSEDED", "Supersede")] {
        let mut left = w.clone();
        left.apply(40, &bead("b-7", to, event));
        assert!(left.repeats.is_empty(), "{to} {event}");
        assert_eq!(left.beads["b-7"], to);
    }
    let mut requeued = w.clone();
    requeued.apply(40, &bead("b-7", "CERTIFIED", "Requeued"));
    assert_eq!(requeued.repeats.len(), 1, "a requeue is the re-cut itself");
}

#[test]
fn passes_that_end_with_no_verdict_block_the_lane_until_a_verdict() {
    let a = m("b-9", "ddd");
    let mut w = ReplayWorld::default();
    for i in 0..LANE_BLOCKED_PASSES {
        let b = format!("I{i}");
        w.apply(u64::from(i) * 10, &round(&b, CUT, Open, std::slice::from_ref(&a), None, "MemberAdded"));
        w.apply(u64::from(i) * 10 + 1, &round(&b, Open, CiRunning, std::slice::from_ref(&a), None, "PassStarted"));
        assert_eq!(status(&w, "repo-b", 0).lanes_blocked, 0);
        w.apply(u64::from(i) * 10 + 2, &round(&b, CiRunning, Attributing, std::slice::from_ref(&a), None, "infra"));
    }
    let st = status(&w, "repo-b", 100);
    assert_eq!(st.lanes_blocked, 1);
    assert_eq!(st.conditions[3].reason, format!("{LANE_BLOCKED_PASSES} passes in a row ended with no verdict"));
    w.apply(100, &round("V", CUT, Open, std::slice::from_ref(&a), None, "MemberAdded"));
    w.apply(101, &round("V", Open, CiRunning, std::slice::from_ref(&a), None, "PassStarted"));
    w.apply(102, &round("V", CiRunning, Attributing, std::slice::from_ref(&a), None, "red-attributed"));
    assert_eq!(status(&w, "repo-b", 103).lanes_blocked, 0, "a red is a verdict");
    w.apply(110, &round("W", CUT, Open, std::slice::from_ref(&a), None, "MemberAdded"));
    assert_eq!(status(&w, "repo-b", 110).rounds_since_landing, LANE_BLOCKED_PASSES + 2);
}

#[test]
fn an_open_round_waiting_past_the_grace_has_no_slot() {
    let mut w = ReplayWorld::default();
    w.apply(0, &round("O", CUT, Open, &[m("b-4", "eee")], None, "MemberAdded"));
    w.apply(5, &round("O", Open, Open, &[m("b-4", "eee"), m("b-5", "fff")], None, "MemberAdded"));
    assert_eq!(w.rounds["O"].since, 0, "a member joining an open round does not restart its wait");
    assert_eq!(status(&w, "repo-b", SLOT_WAIT_SECS - 1).waiting_with_no_slot, 0);
    let st = status(&w, "repo-b", SLOT_WAIT_SECS);
    assert_eq!(st.waiting_with_no_slot, 1);
    assert_eq!(st.conditions[2].status, ConditionStatus::True);
    assert_eq!(status(&w, "repo-z", SLOT_WAIT_SECS).waiting_with_no_slot, 0, "another repo's round is not this one's");
}

#[test]
fn measured_records_are_tallied_and_change_no_round() {
    let mut w = ReplayWorld::default();
    w.apply(0, &Record::CertPass(CertPass { at: String::new(), repo: "repo-a".into(), batch: "x".into(), pass: Some(1), pass_key: None, cause: "over-cap".into(), place: "ec2".into(), wall_secs: 1 }));
    w.apply(0, &Record::AlarmAttempt(AlarmAttempt { at: String::new(), caller: "batcher".into(), kind: "land-unverified".into(), outcome: "failed".into(), reason_code: "incident-no-id".into() }));
    w.apply(0, &Record::Transition(Transition::Batch { at: String::new(), id: "x".into(), from: Open, to: Open, event: "MemberAdded".into(), member: Some("b-1".into()), tip: None, sha: None, n: None, suites_s: None, build_s: None, red_suites: None }));
    assert_eq!(w.passes[&("repo-a".to_string(), "over-cap".to_string())], 1);
    assert_eq!(w.alarms.len(), 1);
    assert!(w.rounds.is_empty());
    assert_eq!(w.applied, 3);
    let mut other = w.clone();
    assert_eq!(other.fingerprint(), w.fingerprint());
    other.halted = true;
    assert_ne!(other.fingerprint(), w.fingerprint());
}
