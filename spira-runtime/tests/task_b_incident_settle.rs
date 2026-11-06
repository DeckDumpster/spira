//! T1–T6 for `incident.settle` (a P2 machine per the corrections), from the Acceptance section
//! of `wiki/projects/spira/designs/tasks/incident-settle.md`. Red until the task is ported.

use std::collections::BTreeMap;

use runtime::units::{Exit, UnitHandle, UnitRan};
use spira_runtime::tasks::b::incident_settle::{self as task, Carry, Detector, Fix, IncidentSettle as H, Inc, Landing, Phase, Snapshot};
use spira_runtime::tasks::b::kit::{names, testing as t, Input};

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "300"), ("SPIRA_INCIDENT_LABEL", "incident"), ("SPIRA_HOME_REPO", "spira")];
const TICK: u64 = 300_000;

fn fix(id: &str, state: &str, landing: Landing) -> Fix {
    Fix { id: id.into(), state: state.into(), landing }
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

#[test]
fn t1_registered_boots_and_arms_a_5min_tick() {
    t::t1_registered_and_boots::<H>(SET, TICK);
}

#[test]
fn t2_standing_waits_and_skips_are_the_probe_alone() {
    let h = t::handler::<H>(SET);
    let (snap, carry) = idle();
    let (next, acts) = task::decide(&h.cfg, &carry, &Input::Observed(snap.clone()), TICK).unwrap();
    assert!(acts.is_empty() && next == carry, "idle: {acts:?}");
    t::t2_silent::<H>(SET, &json(&snap));
}

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

#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_nothing() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../src/tasks/b/incident_settle.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

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

#[test]
fn t6_two_virtual_days_are_deterministic_and_within_2_events_an_hour() {
    let (snap, _) = idle();
    t::t6_deterministic::<H>(|seed| t::rig::<H>(seed, SET, &json(&snap)), 2, 2);
}
