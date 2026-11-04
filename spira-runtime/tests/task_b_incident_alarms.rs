//! T1–T6 for `incident.alarms`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/incident-alarms.md`. Red until the task is ported.

use std::collections::BTreeMap;

use spira_runtime::tasks::b::incident_alarms::{self as task, Carry, Deadman, Floor, IncidentAlarms as H, PassRecord, Snapshot};
use spira_runtime::tasks::b::kit::{names, testing as t, Input};

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "60"), (task::DEADMAN_KEY, "180"), (task::HEARTBEAT_URL_KEY, "")];
const MIN: u64 = 60_000;
const NOW: u64 = 10 * MIN;

/// A fresh pass record, nothing to page, no intents, the section published a minute ago.
fn idle() -> Snapshot {
    Snapshot {
        section_snapshot: serde_json::json!({"sections": [{"name": "alarms", "status": "ok", "published_ms": NOW - MIN}], "intents": []}),
        pass: Some(PassRecord { n: 9, at_ms: NOW - 30_000, mode: "act".into(), ..PassRecord::default() }),
        goodput_armed: true,
        ..Snapshot::default()
    }
}

fn idle_carry() -> Carry {
    Carry { deadman: Deadman::Fresh, published_at: Some(NOW - MIN), status: "ok".into(), ..Carry::default() }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn t1_registered_boots_and_arms_a_60s_tick() {
    t::t1_registered_and_boots::<H>(SET, MIN);
}

#[test]
fn t2_a_fresh_quiet_world_is_the_probe_alone() {
    let h = t::handler::<H>(SET);
    let (next, acts) = task::decide(&h.cfg, &idle_carry(), &Input::Observed(idle()), NOW).unwrap();
    assert!(acts.is_empty() && next == idle_carry(), "idle: {acts:?}");
    t::t2_silent::<H>(SET, &json(&idle()));
}

#[test]
fn t3_the_deadman_pages_and_floors_move_and_a_second_floor_ask_is_refused() {
    let h = t::handler::<H>(SET);
    let stale = Snapshot { pass: Some(PassRecord { at_ms: NOW - 10 * MIN, ..idle().pass.unwrap() }), ..idle() };
    let (c, acts) = task::decide(&h.cfg, &idle_carry(), &Input::Observed(stale), NOW).unwrap();
    assert_eq!(c.deadman, Deadman::Stale);
    assert!(names(&acts).contains(&task::PAGE), "a stale dead-man pages: {acts:?}");
    let (c, _) = task::decide(&h.cfg, &c, &Input::Observed(idle()), NOW + MIN).unwrap();
    assert_eq!(c.deadman, Deadman::Fresh);
    let (c, _) = task::decide(&h.cfg, &c, &Input::Observed(Snapshot { observability_halted: true, ..idle() }), NOW + 2 * MIN).unwrap();
    assert_eq!(c.deadman, Deadman::Waiting);
    let asked = Carry { floors: BTreeMap::from([("lifecycle".to_string(), Floor::Asked)]), ..idle_carry() };
    let below = Snapshot { section_snapshot: serde_json::json!({"sections": [{"name": "lifecycle", "conditions": [{"type": "BelowFloor", "binding": "lifecycle"}]}]}), ..idle() };
    let refused = task::decide(&h.cfg, &asked, &Input::Outcome { effect: task::FLOOR_ASK.into(), ok: true, body: "lifecycle".into() }, NOW).unwrap_err();
    assert!(refused.contains("asked") && refused.contains("lifecycle"), "a second floor ask for the binding is refused naming the state: {refused}");
    let (_, acts) = task::decide(&h.cfg, &asked, &Input::Observed(below), NOW).unwrap();
    assert!(!names(&acts).contains(&task::FLOOR_ASK), "a standing floor is asked once: {acts:?}");
}

#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_nothing() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../src/tasks/b/incident_alarms.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_a_no_lever_pass_pages_and_an_observability_halt_only_waits() {
    let h = t::handler::<H>(SET);
    let no_lever = Snapshot { pass: Some(PassRecord { no_lever: vec!["lifecycle/BelowFloor".into()], ..idle().pass.unwrap() }), ..idle() };
    let (c, acts) = task::decide(&h.cfg, &idle_carry(), &Input::Observed(no_lever), NOW).unwrap();
    assert_eq!(names(&acts), [task::PAGE]);
    assert!(c.pages.values().any(|p| p.identity.contains("lifecycle")));
    let (c, acts) = task::decide(&h.cfg, &idle_carry(), &Input::Observed(Snapshot { observability_halted: true, pass: None, ..idle() }), NOW).unwrap();
    assert_eq!(c.deadman, Deadman::Waiting);
    assert!(!names(&acts).contains(&task::PAGE) && !names(&acts).contains(&task::HEARTBEAT), "{acts:?}");
}

#[test]
fn t6_two_virtual_days_are_deterministic_and_within_13_events_an_hour() {
    t::t6_deterministic::<H>(|seed| t::rig::<H>(seed, SET, &json(&idle())), 2, 13);
}
