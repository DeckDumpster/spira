//! T1–T6 for `incident.settle` (a P2 machine per the corrections), from the Acceptance section
//! of `wiki/projects/spira/designs/tasks/incident-settle.md`. Red until the task is ported.

use std::collections::BTreeMap;

use runtime::units::{Exit, UnitHandle, UnitRan};
use spira_runtime::tasks::b::incident_settle::{self as task, Carry, Detector, Fix, IncidentSettle as H, Inc, Landing, Phase, Snapshot};
use spira_runtime::tasks::b::kit::{names, testing as t, Input};

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "300"), ("SPIRA_INCIDENT_LABEL", "incident"), ("SPIRA_HOME_REPO", "spira")];
const TICK: u64 = 300_000;

fn fix(id: &str, state: &str, landing: Landing) -> Fix {
    Fix { id: id.into(), state: state.into(), landing, by: None }
}

fn inc(id: &str, fixes: Vec<Fix>, detector: Detector) -> Inc {
    Inc { id: id.into(), external_ref: format!("incident:spira-{id}.service"), labels: vec!["incident".into()], fixes, detector }
}

/// One incident waiting on an unlanded fix, one with no fix; both already carried.
fn idle() -> (Snapshot, Carry) {
    let snap = Snapshot { incidents: vec![inc("sp-i1", vec![fix("sp-f1", "SUBMITTED", Landing::NotInRelease)], Detector::NoProbe), inc("sp-i2", vec![], Detector::NoProbe)] };
    let carry = Carry {
        incidents: BTreeMap::from([("sp-i1".into(), Phase::Waiting), ("sp-i2".into(), Phase::Skipped)]),
        waiting: BTreeMap::from([("sp-i1".into(), "sp-i1\tfix sp-f1 not landed".into()), ("sp-i2".into(), "sp-i2\tno fix dependency".into())]),
    };
    (snap, carry)
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test_id(tfi57k4)]
#[test]
fn t1_registered_boots_and_arms_a_5min_tick() {
    t::t1_registered_and_boots::<H>(SET, TICK);
}

#[test_id(t1tmpij)]
#[test]
fn t2_standing_waits_and_skips_are_the_probe_alone() {
    let h = t::handler::<H>(SET);
    let (snap, carry) = idle();
    let (next, acts) = task::decide(&h.cfg, &carry, &Input::Observed(snap.clone()), TICK).unwrap();
    assert!(acts.is_empty() && next == carry, "idle: {acts:?}");
    t::t2_silent::<H>(SET, &json(&snap));
}

#[test_id(t3ka68k)]
#[test]
fn t3_open_waits_persists_closes_and_a_second_persist_for_one_fix_set_is_refused() {
    let h = t::handler::<H>(SET);
    let fix_in_force = || fix("sp-f1", "LANDED", Landing::InForce("abc123".into()));
    let (c, _) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { incidents: vec![inc("sp-i1", vec![fix("sp-f1", "SUBMITTED", Landing::NotInRelease)], Detector::NoProbe)] }), TICK).unwrap();
    assert_eq!(c.incidents.get("sp-i1"), Some(&Phase::Waiting));
    let (c, acts) = task::decide(&h.cfg, &c, &Input::Observed(Snapshot { incidents: vec![inc("sp-i1", vec![fix_in_force()], Detector::Firing("failed".into()))] }), 2 * TICK).unwrap();
    assert_eq!((c.incidents.get("sp-i1").copied(), names(&acts)), (Some(Phase::Persists), vec![task::NOTE]));
    let fired = Inc { labels: vec!["incident".into(), format!("{}x", task::FIRED_PREFIX)], ..inc("sp-i1", vec![fix_in_force()], Detector::Firing("failed".into())) };
    let refused = task::decide(&h.cfg, &Carry { incidents: BTreeMap::from([("sp-i1".into(), Phase::Persists)]), ..Carry::default() }, &Input::Observed(Snapshot { incidents: vec![fired] }), 3 * TICK).unwrap_err();
    assert!(refused.contains(task::FIRED_PREFIX), "{refused}");
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { incidents: vec![inc("sp-i3", vec![fix_in_force()], Detector::Quiet)] }), TICK).unwrap();
    assert_eq!((c.incidents.get("sp-i3").copied(), names(&acts)), (Some(Phase::Closing), vec![task::CLOSE]));
    let ran = UnitRan { handle: UnitHandle::of(task::NAME, "sp-i3").unwrap(), exit: Exit::Code(0), duration_ms: 1_000 };
    assert_eq!(ran.key_for(task::NAME).as_deref(), Some("sp-i3"));
    let (c, _) = task::decide(&h.cfg, &c, &Input::Ran(ran), TICK + 1_000).unwrap();
    assert_eq!(c.incidents.get("sp-i3"), Some(&Phase::Closed));
    let (c, _) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { incidents: vec![inc("sp-i4", vec![], Detector::NoProbe)] }), TICK).unwrap();
    assert_eq!(c.incidents.get("sp-i4"), Some(&Phase::Skipped));
}

#[test_id(t1gyf4y)]
#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_nothing() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../../src/tasks/b/incident_settle.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test_id(tl77d08)]
#[test]
fn t5_landed_and_quiet_closes_with_the_old_evidence_and_dropped_fixes_do_not_block() {
    let h = t::handler::<H>(SET);
    let snap = Snapshot { incidents: vec![inc("sp-i1", vec![fix("sp-f1", "LANDED", Landing::InForce("abc123".into())), fix("sp-f2", "DROPPED", Landing::NotInRelease)], Detector::Quiet)] };
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(snap), 1_793_664_000_000).unwrap();
    assert_eq!(names(&acts), [task::CLOSE]);
    assert!(acts[0].input.contains("sp-f1") && acts[0].input.contains("abc123") && acts[0].input.contains("detector quiet at"), "{}", acts[0].input);
    let unlanded = Snapshot { incidents: vec![inc("sp-i1", vec![fix("sp-f1", "SUBMITTED", Landing::NotInRelease)], Detector::Firing("never read".into()))] };
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(unlanded), TICK).unwrap();
    assert!(acts.iter().all(|a| a.name == task::LOG), "an unlanded fix waits: {acts:?}");
}

#[test_id(tuenq8z)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rig = t::rig::<H>(1, SET, &json(&idle().0));
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&rig, task::NAME);
    assert!(rows.iter().any(|k| k == "incidents/sp-i1") && rows.iter().any(|k| k == "waiting/sp-i1"), "{rows:?}");
}

#[test_id(tt4zcyo)]
#[test]
fn a_boot_repeats_no_waiting_line_and_misses_none() {
    let (snap, _) = idle();
    let changed = Snapshot { incidents: vec![inc("sp-i1", vec![fix("sp-f1", "SUBMITTED", Landing::NotInRelease)], Detector::NoProbe), inc("sp-i2", vec![fix("sp-f2", "SUBMITTED", Landing::NotInRelease)], Detector::NoProbe)] };
    t::assert_boot_matches_an_unstopped_run::<H>(SET, vec![json(&snap), json(&changed)], |_| {});
}

fn no_probe(fixes: Vec<Fix>) -> Inc {
    Inc { external_ref: "incident:base-red:spira".into(), ..inc("sp-i1", fixes, Detector::NoProbe) }
}

fn close_input(i: Inc) -> String {
    let h = t::handler::<H>(SET);
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { incidents: vec![i] }), TICK).unwrap();
    assert_eq!(names(&acts), [task::CLOSE], "{acts:?}");
    acts[0].input.clone()
}

#[test_id(t11o7lg)]
#[test]
fn an_incident_with_no_probe_closes_when_its_fix_lands() {
    let input = close_input(no_probe(vec![fix("sp-f1", "LANDED", Landing::InForce("abc123".into()))]));
    for want in ["sp-f1", "abc123", "no detector to re-run"] {
        assert!(input.contains(want), "{want} missing from {input}");
    }
}

#[test_id(tjln28b)]
#[test]
fn a_no_probe_incident_closes_on_a_landing_the_release_lacks() {
    let input = close_input(no_probe(vec![fix("sp-f1", "LANDED", Landing::OnBase("abc123".into()))]));
    assert!(input.contains("abc123"), "{input}");
}

#[test_id(t1bfmvr)]
#[test]
fn an_incident_on_a_superseded_fix_closes_when_the_successor_landed() {
    let f = Fix { by: Some("sp-l9i5io".into()), ..fix("sp-qj6zsu", "LANDED", Landing::InForce("abc123".into())) };
    let input = close_input(no_probe(vec![f]));
    assert!(input.contains("sp-qj6zsu") && input.contains("sp-l9i5io"), "{input}");
}

#[test_id(t81db03)]
#[test]
fn a_no_probe_incident_whose_fix_was_ejected_stays_open() {
    let h = t::handler::<H>(SET);
    let snap = Snapshot { incidents: vec![no_probe(vec![fix("sp-f1", "REWORK", Landing::NotInRelease)])] };
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(snap), TICK).unwrap();
    assert_eq!(c.incidents.get("sp-i1"), Some(&Phase::Waiting));
    assert!(acts.iter().all(|a| a.name == task::LOG), "{acts:?}");
}

#[test_id(trqabfg)]
#[test]
fn a_close_lost_at_a_restart_leaves_the_incident_open() {
    let landed = json(&Snapshot { incidents: vec![inc("sp-i1", vec![fix("sp-f1", "LANDED", Landing::InForce("abc123".into()))], Detector::Quiet)] });
    let mut rig = runtime::taskrig::TaskRig::new(1, t::handler::<H>(SET).registration()).unwrap().hanging(task::CLOSE).double(task::OBSERVE, move |_| Ok(landed.clone()));
    let (_, stream, kind) = H::boot();
    rig.tick(stream, kind).unwrap();
    assert_eq!(rig.requested().iter().filter(|q| q.name == task::CLOSE).count(), 1);
    rig.reopen(t::handler::<H>(SET).registration()).unwrap();
    rig.run_until_idle().unwrap();
    let carried = t::recorded::<H>(&rig).unwrap();
    assert_eq!(carried.incidents.get("sp-i1"), Some(&Phase::Open), "{carried:?}");
    assert!(!rig.wide_set("incident.settle.refused"));
}

fn lost(name: &str, input: &str) -> Input<Snapshot> {
    Input::Lost(spira_runtime::tasks::b::kit::Act { name: name.into(), key: format!("key/{name}"), input: input.into(), deadline_ms: 1_000, probe: false })
}

fn asked_again(name: &str, input: &str) -> Vec<spira_runtime::tasks::b::kit::Act> {
    match lost(name, input) {
        Input::Lost(a) => vec![a],
        _ => unreachable!(),
    }
}

fn failed(effect: &str, body: &str) -> Input<Snapshot> {
    Input::Outcome { effect: effect.into(), ok: false, body: body.into() }
}

#[test_id(t0rsipn)]
#[test]
fn decide_reopens_a_lost_close_or_note_asks_a_lost_label_again_and_reads_a_failure_as_before() {
    let h = t::handler::<H>(SET);
    let carry = |phase| Carry { incidents: BTreeMap::from([("sp-i1".to_string(), phase)]), ..Carry::default() };
    let job = serde_json::to_string(&task::Job { id: "sp-i1".into(), ..task::Job::default() }).unwrap();
    for (effect, phase) in [(task::CLOSE, Phase::Closing), (task::NOTE, Phase::Persists)] {
        assert_eq!(task::decide(&h.cfg, &carry(phase), &lost(effect, &job), TICK).unwrap(), (carry(Phase::Open), vec![]), "{effect}");
        assert_eq!(task::decide(&h.cfg, &carry(phase), &failed(effect, "sp-i1: why"), TICK).unwrap(), (carry(Phase::Open), vec![]), "{effect}");
    }
    assert_eq!(task::decide(&h.cfg, &carry(Phase::Persists), &lost(task::LABEL, &job), TICK).unwrap(), (carry(Phase::Persists), asked_again(task::LABEL, &job)));
    assert_eq!(task::decide(&h.cfg, &carry(Phase::Persists), &failed(task::LABEL, "sp-i1: why"), TICK).unwrap(), (carry(Phase::Open), vec![]));
    assert_eq!(task::decide(&h.cfg, &carry(Phase::Waiting), &lost(task::LOG, &job), TICK).unwrap(), (carry(Phase::Waiting), vec![]));
    assert_eq!(task::decide(&h.cfg, &carry(Phase::Waiting), &failed(task::LOG, "sp-i1: why"), TICK).unwrap(), (carry(Phase::Waiting), vec![]));
}
