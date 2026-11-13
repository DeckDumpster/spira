//! A member whose own gate went red at the tip a round holds leaves it at once, through one
//! preempt that carries the gate's mark; a mark at another tip, or one already being acted on,
//! does nothing.

use spira_runtime::tasks::a::batcher_local::*;
use spira_runtime::tasks::a::kit::Act;

const NOW: u64 = 1_794_294_300_000;

fn carried(state: State) -> Carry {
    let rc = RepoCarry {
        state,
        round_key: Some("k".into()),
        batch: Some("r9".into()),
        members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())],
        wt: "/w".into(),
        base: "b1".into(),
        leg: Some("spira.r9.corpus1".into()),
        leg_at_ms: NOW - 180_000,
        run: 1,
        head: "H".into(),
        leg_head: "H".into(),
        ..RepoCarry::default()
    };
    Carry { repos: [("spira".to_string(), rc)].into() }
}

fn snapshot(gate_red: Vec<(&str, &str)>, in_flight: Option<Vec<String>>) -> Snapshot {
    let gate_red = gate_red.into_iter().map(|(id, tip)| (id.to_string(), tip.to_string(), "suites-failed".to_string())).collect();
    let round = Round { id: "r9".into(), members: vec!["sp-a".into(), "sp-b".into()], head: "H".into(), worktree: "/w".into(), driven: true, phase: "certifying".into(), gate_red, ..Round::default() };
    let repo = Repo { open_round: Some(round), base_sha: "b1".into(), live_legs: Some(vec!["spira.r9.corpus1".into()]), in_flight, ..Repo::default() };
    Snapshot { repos: [("spira".to_string(), repo)].into(), now_ms: NOW }
}

fn preempts(acts: &[Act]) -> Vec<Leg> {
    acts.iter().filter(|a| a.effect == PREEMPT).map(|a| serde_json::from_str(&a.input).expect("a leg")).collect()
}

#[test]
fn a_gate_red_at_the_tip_the_round_holds_preempts_the_pass_naming_the_member_and_the_mark() {
    for state in [State::Corpus, State::Attributing, State::Certifying] {
        let (next, acts) = decide(&carried(state), &snapshot(vec![("sp-a", "t1")], None));
        let legs = preempts(&acts);
        assert_eq!(legs.len(), 1, "{state:?}: {acts:?}");
        assert_eq!((legs[0].bead.as_str(), legs[0].batch.as_str()), ("sp-a", "r9"));
        assert!(legs[0].why.contains("sp-a") && legs[0].why.contains("suites-failed"), "{}", legs[0].why);
        assert_eq!(next.repos["spira"].state, state, "the round goes on while the preempt runs");
    }
}

#[test]
fn a_gate_red_at_another_tip_than_the_one_the_round_holds_preempts_nothing() {
    let (_, acts) = decide(&carried(State::Corpus), &snapshot(vec![("sp-a", "older")], None));
    assert!(preempts(&acts).is_empty(), "{acts:?}");
}

#[test]
fn a_preempt_still_in_flight_is_not_asked_again() {
    let (_, acts) = decide(&carried(State::Corpus), &snapshot(vec![("sp-a", "t1")], Some(vec![PREEMPT.to_string()])));
    assert!(preempts(&acts).is_empty(), "{acts:?}");
}

#[test]
fn an_ejected_member_leaves_the_carried_round_when_its_preempt_is_answered() {
    let (_, acts) = decide(&carried(State::Corpus), &snapshot(vec![("sp-a", "t1")], None));
    let leg = preempts(&acts).remove(0);
    let (next, _) = on_outcome(&carried(State::Corpus), PREEMPT, true, "{}", &leg, NOW);
    assert_eq!(next.repos["spira"].members, [("sp-b".to_string(), "t2".to_string())]);
    let (kept, _) = on_outcome(&carried(State::Corpus), PREEMPT, false, "refused", &leg, NOW);
    assert_eq!(kept.repos["spira"].members.len(), 2, "a refused preempt ejected nobody");
}

#[test]
fn the_preempt_the_batcher_asks_is_one_the_queue_cli_parses_with_its_evidence_on_stdin() {
    let ids = vec!["sp-a".to_string(), "sp-b".to_string()];
    let argv: Vec<String> = std::iter::once("round".to_string()).chain(batcher_cut::io::preempt_args("r9", "spira", &ids)).collect();
    match queue::cli::parse(&argv) {
        Ok(queue::cli::Cmd::Round(queue::cli::Round::Preempt { batch, repo, eject, reason, suites })) => {
            assert_eq!((batch.as_str(), repo.as_deref(), eject.as_str(), suites.as_str()), ("r9", Some("spira"), "sp-a,sp-b", batcher_cut::io::GATE_STEP));
            assert!(matches!(reason, queue::cli::Text::Stdin), "{reason:?}");
        }
        other => panic!("not a preempt: {other:?}"),
    }
}
