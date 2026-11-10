//! T1–T6 for `incident.alarms`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/incident-alarms.md`. Contract amended 2026-11-06: the
//! fixtures carry the store's real `section snapshot` (parsed sections, the full goodput pass
//! record) and the goodput task's `scheduler status`, which is what the crate's alarms pass reads.

use std::collections::BTreeMap;

use incident::alarms_pass::{self as ap, AttemptReading, DeadMan};
use spira_runtime::tasks::b::incident_alarms::{self as task, Carry, Deadman, Floor, GoodputStatus, IncidentAlarms as H, Snapshot};
use spira_runtime::tasks::b::kit::{names, testing as t, Input};

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "60"), (task::DEADMAN_KEY, "180"), (task::HEARTBEAT_URL_KEY, "")];
const MIN: u64 = 60_000;
const NOW: u64 = 10 * MIN;

fn rfc(ms: u64) -> String {
    reconciler_engine::clock::iso_of(ms / 1000)
}

/// A goodput pass record at `at_ms`, with `no_lever` sections and a binding.
fn pass(at_ms: u64, no_lever: &[&str], binding: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "n": 9, "list_rev": 3, "at": rfc(at_ms), "holder": "reconcile.goodput/1", "mode": "acting",
        "binding": binding,
        "no_lever": no_lever.iter().map(|s| serde_json::json!({"section": s, "conditions": ["BelowFloor"], "since": 0})).collect::<Vec<_>>(),
    })
}

/// The Alarms section as the pass at `now_ms` would publish it, fresh, so a pass that reads it
/// back finds its status unchanged.
fn alarms_section(now_ms: u64) -> serde_json::Value {
    let dm = DeadMan::Fresh { age_secs: 30 };
    let (status, _) = ap::status_of(None, None, &dm, &AttemptReading::default(), None, now_ms / 1000);
    let mut body = ap::section_body(&status, now_ms / 1000);
    body["metadata"]["resourceVersion"] = serde_json::json!("7");
    serde_json::json!({ "section": body, "freshness": { "state": "Fresh" } })
}

fn lifecycle_below_floor(now_ms: u64) -> serde_json::Value {
    serde_json::json!({ "section": {
        "apiVersion": "spira/v1", "kind": "Lifecycle", "metadata": { "name": "all", "owner": "spira-lc", "observedAt": rfc(now_ms) },
        "status": { "observedGeneration": 0, "conditions": [{ "type": "BelowFloor", "status": "True", "reason": "rate below floor", "lastTransitionTime": rfc(now_ms) }] },
    }, "freshness": { "state": "Fresh" } })
}

/// A fresh goodput task and pass record, nothing to page, no intents, the Alarms section
/// published as this pass would publish it.
fn idle(now: u64) -> Snapshot {
    Snapshot {
        section_snapshot: serde_json::json!({ "sections": [alarms_section(now)], "intents": [], "pass": pass(now.saturating_sub(30_000), &[], None) }),
        goodput: GoodputStatus { armed: true, last_fired_ms: Some(now.saturating_sub(30_000)) },
        ..Snapshot::default()
    }
}

fn idle_carry() -> Carry {
    Carry { deadman: Deadman::Fresh, knob_inited: true, ..Carry::default() }
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
    let (next, acts) = task::decide(&h.cfg, &idle_carry(), &Input::Observed(idle(NOW)), NOW).unwrap();
    assert!(acts.is_empty() && next == idle_carry(), "idle: {acts:?} {next:?}");
    t::t2_silent::<H>(SET, &json(&idle(0)));
}

#[test]
fn t3_the_deadman_pages_and_floors_move_and_a_second_floor_ask_is_refused() {
    let h = t::handler::<H>(SET);
    let stale = Snapshot { goodput: GoodputStatus { armed: true, last_fired_ms: Some(NOW - 10 * MIN) }, ..idle(NOW) };
    let (c, acts) = task::decide(&h.cfg, &idle_carry(), &Input::Observed(stale), NOW).unwrap();
    assert_eq!(c.deadman, Deadman::Stale);
    assert!(names(&acts).contains(&task::PAGE), "a stale dead-man pages: {acts:?}");
    let (c, _) = task::decide(&h.cfg, &c, &Input::Observed(idle(NOW + MIN)), NOW + MIN).unwrap();
    assert_eq!(c.deadman, Deadman::Fresh);
    let (c, _) = task::decide(&h.cfg, &c, &Input::Observed(Snapshot { observability_halted: true, ..idle(NOW + 2 * MIN) }), NOW + 2 * MIN).unwrap();
    assert_eq!(c.deadman, Deadman::Waiting);
    let unarmed = Snapshot { goodput: GoodputStatus { armed: false, last_fired_ms: None }, ..idle(NOW) };
    assert_eq!(task::decide(&h.cfg, &idle_carry(), &Input::Observed(unarmed), NOW).unwrap().0.deadman, Deadman::Waiting, "an unarmed goodput task is the switch off");
    let asked = Carry { floors: BTreeMap::from([("Lifecycle/all".to_string(), Floor::Asked)]), ..idle_carry() };
    let below = Snapshot { section_snapshot: serde_json::json!({ "sections": [alarms_section(NOW), lifecycle_below_floor(NOW)], "intents": [], "pass": pass(NOW - 30_000, &[], Some("Lifecycle/all")) }), ..idle(NOW) };
    let refused = task::decide(&h.cfg, &asked, &Input::Outcome { effect: task::FLOOR_ASK.into(), ok: true, body: "Lifecycle/all\tsp-ask".into() }, NOW).unwrap_err();
    assert!(refused.contains("asked") && refused.contains("Lifecycle/all"), "a second floor ask for the binding is refused naming the state: {refused}");
    let (_, acts) = task::decide(&h.cfg, &asked, &Input::Observed(below.clone()), NOW).unwrap();
    assert!(!names(&acts).contains(&task::FLOOR_ASK), "a standing floor is asked once: {acts:?}");
    let (c, acts) = task::decide(&h.cfg, &idle_carry(), &Input::Observed(below), NOW).unwrap();
    assert_eq!((c.floors.get("Lifecycle/all"), names(&acts).contains(&task::FLOOR_ASK)), (Some(&Floor::Asking), true), "below the floor, the binding is asked");
    let (c, _) = task::decide(&h.cfg, &c, &Input::Outcome { effect: task::FLOOR_ASK.into(), ok: true, body: "Lifecycle/all\tsp-ask".into() }, NOW).unwrap();
    assert_eq!((c.floors.get("Lifecycle/all"), c.floor_asks.get("Lifecycle/all").map(String::as_str)), (Some(&Floor::Asked), Some("sp-ask")));
    let (c, _) = task::decide(&h.cfg, &c, &Input::Observed(idle(NOW + MIN)), NOW + MIN).unwrap();
    assert!(c.floors.is_empty(), "a cleared floor drops its row");
}

#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_nothing() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../../src/tasks/b/incident_alarms.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_a_no_lever_pass_pages_and_an_observability_halt_only_waits() {
    let h = t::handler::<H>(SET);
    let mut no_lever = idle(NOW);
    no_lever.section_snapshot["pass"] = pass(NOW - 30_000, &["Lifecycle/all"], None);
    let (c, acts) = task::decide(&h.cfg, &idle_carry(), &Input::Observed(no_lever), NOW).unwrap();
    assert_eq!(names(&acts), [task::PAGE]);
    assert!(c.pages.iter().any(|p| p.section.contains("Lifecycle")), "{:?}", c.pages);
    let mut halted = Snapshot { observability_halted: true, ..idle(NOW) };
    halted.section_snapshot["pass"] = serde_json::Value::Null;
    let (c, acts) = task::decide(&h.cfg, &idle_carry(), &Input::Observed(halted), NOW).unwrap();
    assert_eq!(c.deadman, Deadman::Waiting);
    assert!(!names(&acts).contains(&task::PAGE) && !names(&acts).contains(&task::HEARTBEAT), "{acts:?}");
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rig = t::rig::<H>(1, SET, &json(&idle(0)));
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&rig, task::NAME);
    assert!(rows.iter().any(|k| k == "knob_inited"), "{rows:?}");
}

#[test]
fn a_boot_repeats_no_knob_init_and_misses_none() {
    t::assert_boot_matches_an_unstopped_run::<H>(SET, vec![json(&idle(0))], |_| {});
}

#[test]
fn a_boot_does_not_page_again_inside_the_page_window() {
    let mut no_lever = idle(0);
    no_lever.section_snapshot["pass"] = pass(0, &["Lifecycle/all"], None);
    let paged = json(&no_lever);
    t::assert_boot_matches_an_unstopped_run::<H>(SET, vec![paged.clone(), paged], |_| {});
}
