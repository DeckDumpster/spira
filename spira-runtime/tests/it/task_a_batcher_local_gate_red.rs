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
    Snapshot { repos: [("spira".to_string(), repo)].into(), now_ms: NOW, ..Default::default() }
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

#[test_id(tl8ucsd)]
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

#[test_id(tz8vcin)]
#[test]
fn a_pass_that_ends_green_lands_the_round_with_the_red_provisional_member_in_it() {
    let (next, acts) = ended(&recorded(State::Certifying, "t1", RED), 0, "verdict=green\n");
    assert!(!names(&acts).contains(&EJECT_GATE_RED), "{acts:?}");
    let land = legs_of(&acts, LAND).remove(0);
    assert_eq!(land.members.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["sp-a", "sp-b"], "{acts:?}");
    assert_eq!(next.repos["spira"].state, State::Landing);
}

#[test_id(tnra7gv)]
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

#[test_id(tyzu9ui)]
#[test]
fn a_round_between_passes_ejects_the_recorded_member_before_the_next_pass_starts() {
    let mut carry = recorded(State::Corpus, "t1", RED);
    carry.repos.get_mut("spira").unwrap().leg = Some("spira.r9.corpus1".into());
    let (_, acts) = ended(&carry, 0, "");
    assert_eq!(names(&acts), [EJECT_GATE_RED], "the certify pass is not started over a member already red: {acts:?}");
}

#[test_id(tfheh84)]
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

#[test_id(tme7wii)]
#[test]
fn a_leg_that_ended_without_a_verdict_does_not_eject_from_the_leg_end_but_from_the_certify_that_reruns_the_pass() {
    let carry = recorded(State::Certifying, "t1", RED);
    let leg = carry.repos["spira"].leg.clone().expect("a leg");
    let ran = runtime::units::UnitRan { handle: runtime::units::UnitHandle::of(NAME, &leg).unwrap(), exit: runtime::units::Exit::Unknown, duration_ms: 1 };
    let out = runtime::units::RanOutput::captured(b"", b"");
    let (_, acts) = on_ran_with(&carry, &ran, &out, NOW).expect("the carried leg ended");
    assert!(!acts.is_empty(), "the leg end still drives the round: {acts:?}");
    assert!(!names(&acts).contains(&EJECT_GATE_RED), "{acts:?}");
}

#[test_id(tul43dm)]
#[test]
fn a_gate_red_that_says_nothing_about_the_tree_or_sits_at_another_tip_ejects_nothing() {
    for (tip, mark) in [("t1", "timeout"), ("t1", "no-rebase; failing step: x"), ("older", RED)] {
        let carry = recorded(State::Certifying, tip, mark);
        let (_, acts) = ended(&carry, 1, "verdict=red\n");
        assert!(!names(&acts).contains(&EJECT_GATE_RED), "{tip} {mark}: {acts:?}");
    }
}

#[test_id(tf7gznm)]
#[test]
fn an_idle_repo_whose_pool_holds_only_submitted_beads_cuts_a_round_naming_them() {
    let member = |id: &str, tip: &str| Member { id: id.into(), state: "SUBMITTED".into(), version: 5, tip: tip.into(), sifted_tip: Some(tip.into()), title: id.into(), priority: 2, ..Member::default() };
    let pool = vec![member("sp-s1", "t1"), member("sp-s2", "t2")];
    let branch_tips = pool.iter().map(|m| (m.id.clone(), m.tip.clone())).collect();
    let snap = Snapshot { repos: [("spira".to_string(), Repo { pool, branch_tips, base_sha: "b1".into(), ..Repo::default() })].into(), now_ms: NOW, ..Default::default() };
    let (carry, acts) = decide(&Carry::default(), &snap);
    assert_eq!(carry.repos["spira"].state, State::Assembling, "{acts:?}");
    let cut: Leg = serde_json::from_str(&acts.iter().find(|a| a.effect == ASSEMBLE).expect("a cut").input).unwrap();
    let assembled = Assembled { merged: cut.members.clone(), wt: "w/r1".into(), base: "b1".into(), suites: true, ..Assembled::default() };
    let (_, acts) = on_outcome(&carry, ASSEMBLE, true, &serde_json::to_string(&assembled).unwrap(), &cut, NOW);
    let open: Leg = serde_json::from_str(&acts.iter().find(|a| a.effect == ROUND_OPEN).unwrap_or_else(|| panic!("{acts:?}")).input).unwrap();
    assert_eq!(open.members.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["sp-s1", "sp-s2"]);
}

fn held_body(id: &str) -> String {
    let held = batcher_cut::io::HeldEject { id: id.into(), reason: format!("gate red: {RED}"), csv: "test-a.sh".into(), said: "queue round eject exited 79: a pass of round r9 is running".into() };
    format!("{}{}", batcher_cut::io::EJECT_HELD, serde_json::to_string(&held).unwrap())
}

#[test_id(tlzq080)]
#[test]
fn a_gate_red_eject_held_for_a_running_pass_is_sent_again_when_the_pass_ends() {
    let (carry, acts) = ended(&recorded(State::Certifying, "t1", RED), 1, "verdict=red\n");
    let leg = legs_of(&acts, EJECT_GATE_RED).remove(0);
    let (held, acts) = on_outcome(&carry, EJECT_GATE_RED, false, &held_body("sp-a"), &leg, NOW);
    assert!(!names(&acts).contains(&ABANDON) && !names(&acts).contains(&PAUSE), "{acts:?}");
    assert_eq!(held.repos["spira"].eject_held.as_ref().map(|h| h.eject.id.as_str()), Some("sp-a"));

    let mut snap = snapshot_marked(vec![], RED);
    snap.repos.get_mut("spira").unwrap().scoped_batches = Some(vec!["r9".into()]);
    let (still, acts) = decide(&held, &snap);
    assert!(!names(&acts).contains(&EJECT), "a live pass scope holds the eject: {acts:?}");
    snap.repos.get_mut("spira").unwrap().scoped_batches = Some(vec![]);
    let (issued, acts) = decide(&still, &snap);
    let eject = legs_of(&acts, EJECT);
    assert_eq!(eject.len(), 1, "{acts:?}");
    assert!(eject[0].held.as_ref().is_some_and(|h| h.reason.contains("callers.rs:726")), "{:?}", eject[0].held);

    let (done, _) = on_outcome(&issued, EJECT, true, "{}", &eject[0], NOW);
    let rc = &done.repos["spira"];
    assert_eq!(rc.members, vec![("sp-b".to_string(), "t2".to_string())]);
    assert!(rc.eject_held.is_none() && rc.certify_held, "{rc:?}");
}

#[test_id(tyho24q)]
#[test]
fn a_gate_red_eject_held_after_an_earlier_member_left_drops_only_that_member() {
    let (carry, acts) = ended(&recorded(State::Certifying, "t1", RED), 1, "verdict=red\n");
    let leg = Leg { bead: "sp-a,sp-b".into(), ..legs_of(&acts, EJECT_GATE_RED).remove(0) };
    let (held, acts) = on_outcome(&carry, EJECT_GATE_RED, false, &held_body("sp-b"), &leg, NOW);
    assert!(!names(&acts).contains(&ABANDON), "{acts:?}");
    let rc = &held.repos["spira"];
    assert!(rc.members.iter().all(|(id, _)| id != "sp-a"), "{rc:?}");
    assert_eq!(rc.eject_held.as_ref().map(|h| h.eject.id.as_str()), Some("sp-b"));
}

/// The round as the queue shows it a minute after a red pass: red, driven by nobody, the eject not yet answered.
fn red_round_snapshot(in_flight: Vec<String>) -> Snapshot {
    let mut snap = snapshot_marked(vec![], RED);
    let repo = snap.repos.get_mut("spira").unwrap();
    let round = repo.open_round.as_mut().unwrap();
    round.red = true;
    round.driven = false;
    round.phase = "red".into();
    repo.live_legs = Some(vec![]);
    repo.in_flight = Some(in_flight);
    snap.now_ms = NOW + 60_000;
    snap
}

fn eject_in_flight() -> (Carry, Leg) {
    let (carry, acts) = ended(&recorded(State::Certifying, "t1", RED), 1, "verdict=red\n");
    (carry, legs_of(&acts, EJECT_GATE_RED).remove(0))
}

#[test_id(t1w6ljq)]
#[test]
fn an_eject_in_flight_is_not_overtaken_by_an_attribution_or_an_abandon_of_the_red_round() {
    let (carry, _) = eject_in_flight();
    let (next, acts) = decide(&carry, &red_round_snapshot(vec![EJECT_GATE_RED.into()]));
    assert!(acts.is_empty(), "{acts:?}");
    assert!(next.repos["spira"].eject_gate_red.is_some());
}

#[test_id(t2k9xfa)]
#[test]
fn an_eject_that_succeeds_after_a_long_wait_certifies_the_survivors() {
    let (carry, leg) = eject_in_flight();
    let (waited, _) = decide(&carry, &red_round_snapshot(vec![EJECT_GATE_RED.into()]));
    let (next, acts) = on_outcome(&waited, EJECT_GATE_RED, true, "{}", &leg, NOW + 90_000);
    assert_eq!(names(&acts), [CERTIFY], "{acts:?}");
    assert!(next.repos["spira"].eject_gate_red.is_none());
}

#[test_id(t3m4rzc)]
#[test]
fn an_eject_missing_from_in_flight_after_a_restart_is_sent_again_once() {
    let (carry, leg) = eject_in_flight();
    let (_, acts) = decide(&carry, &red_round_snapshot(vec![]));
    assert_eq!(names(&acts), [EJECT_GATE_RED], "{acts:?}");
    assert_eq!(legs_of(&acts, EJECT_GATE_RED)[0], leg);
}

#[test_id(t4q8pne)]
#[test]
fn a_refused_gate_red_eject_abandons_and_clears_the_request() {
    let (carry, leg) = eject_in_flight();
    let (next, acts) = on_outcome(&carry, EJECT_GATE_RED, false, "refused", &leg, NOW + 90_000);
    assert!(names(&acts).contains(&ABANDON), "{acts:?}");
    assert!(next.repos.get("spira").is_none_or(|rc| rc.eject_gate_red.is_none()));
}
