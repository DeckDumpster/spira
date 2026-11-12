//! A step started for an earlier head of a round can never change a round whose head moved on:
//! its result is discarded, and a green pass on the current head lands the round.

use batcher_cut::attribute::Attribution;
use runtime::units::{Exit, RanOutput, UnitHandle, UnitRan};
use spira_runtime::tasks::a::batcher_local::*;

const NOW: u64 = 1_794_294_300_000;
const STEP: &str = "spira.r9.attribute2";

fn carried(head: &str, leg_head: &str) -> Carry {
    let rc = RepoCarry {
        state: State::Attributing,
        round_key: Some("k".into()),
        batch: Some("r9".into()),
        members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())],
        wt: "/w".into(),
        base: "b1".into(),
        leg: Some(STEP.into()),
        leg_at_ms: NOW - 60_000,
        run: 2,
        head: head.into(),
        leg_head: leg_head.into(),
        ..RepoCarry::default()
    };
    Carry { repos: [("spira".to_string(), rc)].into() }
}

fn snapshot(head: &str, phase: &str) -> Snapshot {
    let round = Round { id: "r9".into(), members: vec!["sp-a".into()], head: head.into(), worktree: "/w".into(), driven: true, phase: phase.into(), ..Round::default() };
    let repo = Repo { open_round: Some(round), base_sha: "b1".into(), live_legs: Some(vec![]), ..Repo::default() };
    Snapshot { repos: [("spira".to_string(), repo)].into(), now_ms: NOW }
}

fn ran() -> UnitRan {
    UnitRan { handle: UnitHandle::of(NAME, STEP).unwrap(), exit: Exit::Code(1), duration_ms: 1 }
}

fn fault() -> RanOutput {
    RanOutput::captured(Attribution::Fault("harness fault — install failed".into()).line().as_bytes(), b"")
}

fn effects(acts: &[spira_runtime::tasks::a::kit::Act]) -> Vec<&str> {
    acts.iter().map(|a| a.effect.as_str()).collect()
}

#[test]
fn a_step_result_for_another_head_than_the_rounds_current_one_changes_no_state() {
    assert!(on_ran_with(&carried("B", "A"), &ran(), &fault(), NOW).is_none());
}

#[test]
fn a_step_result_for_the_rounds_current_head_is_applied() {
    let (next, acts) = on_ran_with(&carried("A", "A"), &ran(), &fault(), NOW).expect("its own step");
    assert_ne!(next, carried("A", "A"));
    assert!(effects(&acts).contains(&ABANDON), "{acts:?}");
}

#[test]
fn the_first_head_seen_is_the_one_the_running_step_was_started_for() {
    let (next, _) = decide(&carried("", ""), &snapshot("A", "certifying"));
    let rc = &next.repos["spira"];
    assert_eq!((rc.head.as_str(), rc.leg_head.as_str()), ("A", "A"));
}

#[test]
fn a_head_that_moved_while_its_pass_runs_orphans_the_step_without_ending_the_round() {
    let (next, acts) = decide(&carried("A", "A"), &snapshot("B", "certifying"));
    assert!(acts.is_empty(), "{acts:?}");
    let rc = &next.repos["spira"];
    assert_eq!((rc.state, rc.head.as_str(), rc.leg.clone()), (State::Attributing, "B", None));
    assert!(on_ran_with(&next, &ran(), &fault(), NOW).is_none(), "the old step's end finds no leg of the round");
}

#[test]
fn a_green_pass_on_the_current_head_lands_the_round_whatever_the_older_step_then_returns() {
    let (landing, acts) = decide(&carried("A", "A"), &snapshot("B", "green"));
    assert_eq!(effects(&acts), [LC_CERTIFY, LAND], "{acts:?}");
    assert_eq!(landing.repos["spira"].state, State::Landing);
    assert!(on_ran_with(&landing, &ran(), &fault(), NOW).is_none());
}

#[test]
fn a_green_round_whose_head_never_moved_is_left_to_its_own_step() {
    let (next, acts) = decide(&carried("A", "A"), &snapshot("A", "green"));
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(next.repos["spira"].state, State::Attributing);
}
