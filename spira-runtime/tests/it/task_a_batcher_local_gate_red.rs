//! A provisional member whose own gate goes red never stops, restarts or preempts a running
//! pass: the red is recorded on the round, and read where a pass ends or before the next one
//! starts. A green pass lands the round with the member in it; a red pass, and a round between
//! passes, eject the recorded member first, with the gate's lines, before any attribution.

use spira_runtime::tasks::a::batcher_local::*;
use spira_runtime::tasks::a::kit::Act;

const NOW: u64 = 1_794_294_300_000;
const LEG: &str = "spira.r9.certify1";

fn carried(state: State) -> Carry {
    let rc = RepoCarry {
        state,
        round_key: Some("k".into()),
        batch: Some("r9".into()),
        members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())],
        wt: "w".into(),
        base: "b1".into(),
        leg: Some(LEG.into()),
        leg_at_ms: NOW - 180_000,
        run: 1,
        head: "H".into(),
        leg_head: "H".into(),
        ..RepoCarry::default()
    };
    Carry { repos: [("spira".to_string(), rc)].into() }
}

fn snapshot_marked(gate_red: Vec<(&str, &str)>, mark: &str) -> Snapshot {
    let gate_red = gate_red.into_iter().map(|(id, tip)| (id.to_string(), tip.to_string(), mark.to_string())).collect();
    let round = Round { id: "r9".into(), members: vec!["sp-a".into(), "sp-b".into()], head: "H".into(), worktree: "w".into(), driven: true, phase: "certifying".into(), gate_red, ..Round::default() };
    let repo = Repo { open_round: Some(round), base_sha: "b1".into(), live_legs: Some(vec![LEG.into()]), ..Repo::default() };
    Snapshot { repos: [("spira".to_string(), repo)].into(), now_ms: NOW }
}

const RED: &str = "suites-failed; failing step: test-a.sh; failing line: error: callers.rs:726 retires x_secs";

/// The carry after one tick that saw `mark` against sp-a at `tip`.
fn recorded(state: State, tip: &str, mark: &str) -> Carry {
    decide(&carried(state), &snapshot_marked(vec![("sp-a", tip)], mark)).0
}

fn ended(carry: &Carry, code: i32, out: &str) -> (Carry, Vec<Act>) {
    let leg = carry.repos["spira"].leg.clone().expect("a leg");
    let ran = runtime::units::UnitRan { handle: runtime::units::UnitHandle::of(NAME, &leg).unwrap(), exit: runtime::units::Exit::Code(code), duration_ms: 1 };
    let out = runtime::units::RanOutput::captured(out.as_bytes(), b"");
    on_ran_with(carry, &ran, &out, NOW).expect("the carried leg ended")
}

fn names(acts: &[Act]) -> Vec<&str> {
    acts.iter().map(|a| a.effect.as_str()).collect()
}

fn legs_of(acts: &[Act], effect: &str) -> Vec<Leg> {
    acts.iter().filter(|a| a.effect == effect).map(|a| serde_json::from_str(&a.input).expect("a leg")).collect()
}

#[test]
fn a_gate_red_recorded_while_a_pass_runs_stops_nothing_and_leaves_the_pass_alone() {
    for state in [State::Corpus, State::Attributing, State::Certifying] {
        let (next, acts) = decide(&carried(state), &snapshot_marked(vec![("sp-a", "t1")], RED));
        assert!(acts.is_empty(), "{state:?}: the running pass is not stopped, restarted or rebuilt: {acts:?}");
        let rc = &next.repos["spira"];
        assert_eq!((rc.state, rc.leg.as_deref()), (state, Some(LEG)), "the pass goes on");
        assert_eq!(rc.gate_red, [("sp-a".to_string(), "t1".to_string(), RED.to_string())], "the red is evidence on the round");
    }
}

#[test]
fn a_pass_that_ends_green_lands_the_round_with_the_red_provisional_member_in_it() {
    let (next, acts) = ended(&recorded(State::Certifying, "t1", RED), 0, "verdict=green\n");
    assert!(!names(&acts).contains(&EJECT_GATE_RED), "{acts:?}");
    let land = legs_of(&acts, LAND).remove(0);
    assert_eq!(land.members.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["sp-a", "sp-b"], "{acts:?}");
    assert_eq!(next.repos["spira"].state, State::Landing);
}

#[test]
fn a_pass_that_ends_red_ejects_the_recorded_member_first_with_the_gates_lines_before_any_attribution() {
    for (state, leg) in [(State::Certifying, LEG), (State::Corpus, "spira.r9.corpus1")] {
        let mut carry = recorded(state, "t1", RED);
        carry.repos.get_mut("spira").unwrap().leg = Some(leg.into());
        let (_, acts) = ended(&carry, 1, "verdict=red\n");
        assert_eq!(names(&acts), [EJECT_GATE_RED], "{state:?}: ejected first, nothing attributed or bisected: {acts:?}");
        let leg = legs_of(&acts, EJECT_GATE_RED).remove(0);
        assert_eq!((leg.bead.as_str(), leg.batch.as_str()), ("sp-a", "r9"));
        assert!(leg.reds[0].contains("failing step: test-a.sh") && leg.reds[0].contains("callers.rs:726"), "{:?}", leg.reds);
    }
}

#[test]
fn a_round_between_passes_ejects_the_recorded_member_before_the_next_pass_starts() {
    let mut carry = recorded(State::Corpus, "t1", RED);
    carry.repos.get_mut("spira").unwrap().leg = Some("spira.r9.corpus1".into());
    let (_, acts) = ended(&carry, 0, "");
    assert_eq!(names(&acts), [EJECT_GATE_RED], "the certify pass is not started over a member already red: {acts:?}");
}

#[test]
fn a_member_ejected_for_its_gate_red_leaves_the_carried_round_and_the_survivors_are_certified_on_the_new_head() {
    let mut carry = recorded(State::Certifying, "t1", RED);
    let (_, acts) = ended(&carry, 1, "verdict=red\n");
    let leg = legs_of(&acts, EJECT_GATE_RED).remove(0);
    carry.repos.get_mut("spira").unwrap().leg = None;
    let (next, acts) = on_outcome(&carry, EJECT_GATE_RED, true, "{}", &leg, NOW);
    assert_eq!(names(&acts), [CERTIFY], "{acts:?}");
    let survivors: Vec<String> = legs_of(&acts, CERTIFY)[0].members.iter().map(|(id, _)| id.clone()).collect();
    assert_eq!(survivors, ["sp-b"]);
    assert!(next.repos["spira"].members.iter().all(|(id, _)| id != "sp-a"));
}

#[test]
fn a_leg_that_ended_without_a_verdict_does_not_eject_from_the_leg_end_but_from_the_certify_that_reruns_the_pass() {
    let carry = recorded(State::Certifying, "t1", RED);
    let leg = carry.repos["spira"].leg.clone().expect("a leg");
    let ran = runtime::units::UnitRan { handle: runtime::units::UnitHandle::of(NAME, &leg).unwrap(), exit: runtime::units::Exit::Unknown, duration_ms: 1 };
    let out = runtime::units::RanOutput::captured(b"", b"");
    let (_, acts) = on_ran_with(&carry, &ran, &out, NOW).expect("the carried leg ended");
    assert!(!names(&acts).contains(&EJECT_GATE_RED), "{acts:?}");
}

#[test]
fn a_gate_red_that_says_nothing_about_the_tree_or_sits_at_another_tip_ejects_nothing() {
    for (tip, mark) in [("t1", "timeout"), ("t1", "no-rebase; failing step: x"), ("older", RED)] {
        let carry = recorded(State::Certifying, tip, mark);
        let (_, acts) = ended(&carry, 1, "verdict=red\n");
        assert!(!names(&acts).contains(&EJECT_GATE_RED), "{tip} {mark}: {acts:?}");
    }
}

#[test]
fn an_idle_repo_whose_pool_holds_only_submitted_beads_cuts_a_round_naming_them() {
    let member = |id: &str, tip: &str| Member { id: id.into(), state: "SUBMITTED".into(), version: 5, tip: tip.into(), sifted_tip: Some(tip.into()), title: id.into(), priority: 2, ..Member::default() };
    let pool = vec![member("sp-s1", "t1"), member("sp-s2", "t2")];
    let branch_tips = pool.iter().map(|m| (m.id.clone(), m.tip.clone())).collect();
    let snap = Snapshot { repos: [("spira".to_string(), Repo { pool, branch_tips, base_sha: "b1".into(), ..Repo::default() })].into(), now_ms: NOW };
    let (carry, acts) = decide(&Carry::default(), &snap);
    assert_eq!(carry.repos["spira"].state, State::Assembling, "{acts:?}");
    let cut: Leg = serde_json::from_str(&acts.iter().find(|a| a.effect == ASSEMBLE).expect("a cut").input).unwrap();
    let assembled = Assembled { merged: cut.members.clone(), wt: "w/r1".into(), base: "b1".into(), suites: true, ..Assembled::default() };
    let (_, acts) = on_outcome(&carry, ASSEMBLE, true, &serde_json::to_string(&assembled).unwrap(), &cut, NOW);
    let open: Leg = serde_json::from_str(&acts.iter().find(|a| a.effect == ROUND_OPEN).unwrap_or_else(|| panic!("{acts:?}")).input).unwrap();
    assert_eq!(open.members.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["sp-s1", "sp-s2"]);
}
