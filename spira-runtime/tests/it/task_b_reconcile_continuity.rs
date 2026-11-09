//! T1–T6 for `reconcile.continuity`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/reconcile-continuity.md`. Red until the task is ported.

use std::collections::BTreeMap;

use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use spira_runtime::tasks::b::reconcile_continuity::{self as task, Break, Carry, Phase, ReconcileContinuity as H, Snapshot};

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "900")];
const TICK: u64 = 900_000;
const KEY: &str = "bead/sp-a/12";

fn brk(acknowledged: Option<&str>) -> Break {
    Break { machine: "bead".into(), key: "sp-a".into(), seq: 12, event: "land".into(), from_state: "CERTIFIED".into(), previous_to_state: "SUBMITTED".into(), acknowledged: acknowledged.map(str::to_string) }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn t1_registered_boots_and_arms_a_15min_tick() {
    t::t1_registered_and_boots::<H>(SET, TICK);
}

#[test]
fn t2_a_clean_store_journals_nothing() {
    t::t2_silent::<H>(SET, &json(&Snapshot::default()));
}

#[test]
fn t3_a_break_is_announced_once_then_acknowledged_or_gone() {
    let h = t::handler::<H>(SET);
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { breaks: vec![brk(None)] }), TICK).unwrap();
    assert_eq!((c.breaks.get(KEY).copied(), names(&acts)), (Some(Phase::Announced), vec![task::INBOX_APPEND]));
    assert!(acts[0].input.contains(task::TAG), "the line carries the task's tag: {}", acts[0].input);
    let (c2, _) = task::decide(&h.cfg, &c, &Input::Observed(Snapshot { breaks: vec![brk(Some("ryan 2026-11-04"))] }), 2 * TICK).unwrap();
    assert_eq!(c2.breaks.get(KEY), Some(&Phase::Acknowledged));
    let (c3, _) = task::decide(&h.cfg, &c, &Input::Observed(Snapshot::default()), 2 * TICK).unwrap();
    assert!(c3.breaks.get(KEY).is_none_or(|p| *p == Phase::Gone), "a vanished break leaves the carried set");
    let refused = task::decide(&h.cfg, &c, &Input::Outcome { effect: task::INBOX_APPEND.into(), ok: true, body: KEY.into() }, 2 * TICK).unwrap_err();
    assert!(refused.contains("announced"), "{refused}");
    let rig = t::rig::<H>(1, SET, &json(&Snapshot { breaks: vec![brk(None)] }));
    t::assert_ported(&rig, task::STREAM);
    assert_eq!(t::recorded::<H>(&rig), Some(Carry { breaks: BTreeMap::from([(KEY.to_string(), Phase::Announced)]) }));
}

#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_nothing() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../../src/tasks/b/reconcile_continuity.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_the_first_announcement_matches_the_old_log_and_the_repeat_is_dropped() {
    let h = t::handler::<H>(SET);
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { breaks: vec![brk(None), brk(Some("acked"))] }), TICK).unwrap();
    assert_eq!(names(&acts), [task::INBOX_APPEND], "an acknowledged break is never announced");
    let (_, acts) = task::decide(&h.cfg, &c, &Input::Observed(Snapshot { breaks: vec![brk(None)] }), 2 * TICK).unwrap();
    assert!(acts.is_empty(), "the old unit re-printed every 15 min; the task does not: {acts:?}");
}
