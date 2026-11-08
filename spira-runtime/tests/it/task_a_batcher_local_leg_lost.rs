//! A round batcher.local opened is always driven to an outcome: a leg whose unit never starts,
//! or is gone with no `unit.ran`, is started once more, then the round is abandoned with the
//! reason — it is never left open with nothing driving it.

use spira_runtime::tasks::a::batcher_local::*;
use spira_runtime::tasks::a::kit::Act;

const NOW: u64 = 1_793_000_000_000;
const TICK: u64 = CADENCE_MS;

fn corpus(leg: &str, at: u64) -> Carry {
    let rc = RepoCarry { state: State::Corpus, round_key: Some("k".into()), batch: Some("r9".into()), members: vec![("sp-a".into(), "t1".into())], wt: "/w".into(), base: "b1".into(), leg: Some(leg.into()), leg_at_ms: at, run: 1, ..RepoCarry::default() };
    Carry { repos: [("spira".to_string(), rc)].into() }
}

fn snap(now_ms: u64, live: Option<Vec<String>>) -> Snapshot {
    let repo = Repo { open_round: Some(Round { id: "r9".into(), worktree: "/w".into(), ..Round::default() }), live_legs: live, ..Repo::default() };
    Snapshot { repos: [("spira".to_string(), repo)].into(), now_ms }
}

fn names(acts: &[Act]) -> Vec<&str> {
    acts.iter().map(|a| a.effect.as_str()).collect()
}

/// Observes at `NOW + after` ms with no live leg, `n` times a tick apart.
fn observe_gone(mut carry: Carry, from: u64, n: u64) -> (Carry, Vec<Act>) {
    let mut acts = vec![];
    for i in 1..=n {
        let (c, a) = decide(&carry, &snap(from + i * TICK, Some(vec![])));
        carry = c;
        acts.extend(a);
    }
    (carry, acts)
}

#[test]
fn a_corpus_leg_that_never_started_is_started_once_more_within_a_minute_of_the_open() {
    let first = "spira.r9.corpus.1";
    let (carry, acts) = observe_gone(corpus(first, NOW), NOW, 2);
    assert_eq!(names(&acts), [CORPUS], "the missing leg is started again: {acts:?}");
    let leg: Leg = serde_json::from_str(&acts[0].input).unwrap();
    assert_ne!(leg.unit, first, "a new run, so a new unit");
    assert_eq!(carry.repos["spira"].state, State::Corpus);
    assert_eq!(carry.repos["spira"].leg.as_deref(), Some(leg.unit.as_str()));
}

#[test]
fn a_leg_the_unit_manager_lists_is_left_alone() {
    let mut carry = corpus("spira.r9.corpus.1", NOW);
    for i in 1..=6 {
        let (c, acts) = decide(&carry, &snap(NOW + i * TICK, Some(vec!["spira.r9.corpus.1".into()])));
        assert!(acts.is_empty(), "{acts:?}");
        carry = c;
    }
}

#[test]
fn a_snapshot_that_could_not_list_units_judges_nothing() {
    
    let mut carry = corpus("spira.r9.corpus.1", NOW);
    for i in 1..=6 {
        let (c, acts) = decide(&carry, &snap(NOW + i * TICK, None));
        assert!(acts.is_empty(), "{acts:?}");
        carry = c;
    }
}

#[test]
fn a_corpus_leg_that_is_gone_twice_abandons_the_round_naming_why() {
    let (carry, _) = observe_gone(corpus("spira.r9.corpus.1", NOW), NOW, 2);
    let second = carry.repos["spira"].leg.clone().unwrap();
    let (carry, acts) = observe_gone(carry, NOW + 2 * TICK, 2);
    assert_eq!(names(&acts), [ABANDON], "{acts:?}");
    let leg: Leg = serde_json::from_str(&acts[0].input).unwrap();
    assert_eq!(leg.batch, "r9");
    assert!(leg.why.contains("corpus") && leg.why.contains(&second) && leg.why.contains("started again"), "the reason names the leg: {}", leg.why);
    let rc = &carry.repos["spira"];
    assert_eq!((rc.state, rc.batch.clone(), rc.pace.infra), (State::Idle, None, 1), "an infrastructure fault, round forgotten");
}

#[test]
fn a_corpus_leg_that_could_not_be_started_is_started_once_more_then_abandoned() {
    let first = corpus("spira.r9.corpus.1", NOW);
    let leg = Leg { unit: "spira.r9.corpus.1".into(), batch: "r9".into(), ..Leg::on("spira") };
    let (again, acts) = on_outcome(&first, CORPUS, false, "no unit manager configured", &leg, NOW + 1000);
    assert_eq!(names(&acts), [CORPUS], "{acts:?}");
    assert_eq!(again.repos["spira"].state, State::Corpus, "a failed start is not a red corpus");
    let second = again.repos["spira"].leg.clone().unwrap();
    let leg2 = Leg { unit: second, batch: "r9".into(), ..Leg::on("spira") };
    let (done, acts) = on_outcome(&again, CORPUS, false, "no unit manager configured", &leg2, NOW + 2000);
    assert_eq!(names(&acts), [ABANDON], "{acts:?}");
    let abandon: Leg = serde_json::from_str(&acts[0].input).unwrap();
    assert!(abandon.why.contains("no unit manager configured"), "{}", abandon.why);
    assert_eq!(done.repos["spira"].state, State::Idle);
}

#[test]
fn a_round_open_with_no_driver_after_its_one_redrive_is_abandoned_not_paged() {
    let mut carry = Carry::default();
    let wait = Policy::default().orphan_wait_s * 1000;
    let mut all: Vec<String> = vec![];
    let mut t = NOW;
    while t < NOW + 3 * wait {
        let (c, acts) = decide(&carry, &snap(t, None));
        carry = c;
        all.extend(names(&acts).iter().map(|s| s.to_string()));
        t += TICK;
    }
    assert_eq!(all.iter().filter(|a| *a == REDRIVE).count(), 1, "re-driven once: {all:?}");
    assert_eq!(all.iter().filter(|a| *a == ABANDON).count(), 1, "then abandoned: {all:?}");
    assert!(!all.contains(&INBOX_APPEND.to_string()) && !all.contains(&FILE.to_string()), "an abandoned round pages nobody as open: {all:?}");
}
