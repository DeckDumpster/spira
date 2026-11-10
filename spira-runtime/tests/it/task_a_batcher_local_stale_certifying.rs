//! A repo state of Certifying whose batch the lifecycle already reports LANDED or ABANDONED, with
//! no unit running its leg, is reconciled to Idle and the next round is cut in the same tick.

use spira_runtime::tasks::a::batcher_local::*;

const NOW: u64 = 1_794_294_300_000;
const BATCH: &str = "spira-20261110T055337Z";

/// The row the live store held while the task was stuck, as `spira-lc journal state` printed it.
const STORED: &str = r#"{"state":"Certifying","batch":"spira-20261110T055337Z","leg":"spira.e14c63e58a6e.certify2","leg_at_ms":1794291717130,"run":2,"leg_restarts":0,"leg_missing":0,"paused_on":null,"skip_why":null,"waiting":null,"stacked":false,"attributed":false,"round_key":"sp-1o7bgj:0302664 sp-5qhmb0:ce834a5 sp-kn42ye:2166196 sp-two0b3.2:41be105 sp-vwhmus:c4b4121"}"#;

fn stuck() -> Carry {
    Carry { repos: [("spira".to_string(), serde_json::from_str::<RepoCarry>(STORED).unwrap())].into() }
}

fn waiting_pool(settled: Vec<String>) -> Snapshot {
    let member = Member { id: "sp-olw42f".into(), state: "CERTIFIED".into(), version: 5, tip: "t1".into(), sifted_tip: Some("t1".into()), title: "work".into(), priority: 2, ..Member::default() };
    let repo = Repo { branch_tips: [("sp-olw42f".to_string(), "t1".to_string())].into(), pool: vec![member], base_sha: "b2".into(), settled, live_legs: Some(vec![]), ..Repo::default() };
    Snapshot { repos: [("spira".to_string(), repo)].into(), now_ms: NOW }
}

fn names(acts: &[spira_runtime::tasks::a::kit::Act]) -> Vec<&str> {
    acts.iter().map(|a| a.effect.as_str()).collect()
}

#[test]
fn a_certifying_row_for_a_landed_batch_is_dropped_and_the_next_round_is_cut() {
    let (carry, acts) = decide(&stuck(), &waiting_pool(vec![BATCH.into()]));
    assert_eq!(names(&acts), [FETCH, ASSEMBLE], "the waiting member is cut at once: {acts:?}");
    let rc = &carry.repos["spira"];
    assert_eq!((rc.state, rc.batch.as_deref()), (State::Assembling, None));
    assert!(rc.reconciled.as_deref().is_some_and(|w| w.contains(BATCH)), "the carry says what was reconciled: {:?}", rc.reconciled);

    let assemble: Leg = serde_json::from_str(&acts[1].input).unwrap();
    let merged = Assembled { merged: vec![("sp-olw42f".into(), "t1".into())], wt: "/w".into(), base: "b2".into(), ..Assembled::default() };
    let (_, acts) = on_outcome(&carry, ASSEMBLE, true, &serde_json::to_string(&merged).unwrap(), &assemble, NOW);
    assert_eq!(names(&acts), [ROUND_OPEN], "{acts:?}");
}

#[test]
fn the_reconcile_is_told_in_the_pass_log() {
    let (next, _) = decide(&stuck(), &waiting_pool(vec![BATCH.into()]));
    let lines = notes(&stuck(), &next);
    assert!(lines.iter().any(|l| l.contains(BATCH) && l.contains("LANDED or ABANDONED")), "{lines:?}");
}

#[test]
fn a_certifying_row_whose_batch_the_lifecycle_does_not_call_settled_is_left_to_its_leg() {
    let (carry, acts) = decide(&stuck(), &waiting_pool(vec![]));
    assert!(!names(&acts).contains(&ASSEMBLE), "{acts:?}");
    assert!(carry.repos["spira"].reconciled.is_none());
}
