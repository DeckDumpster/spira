//! T1–T6 for `reconcile.section`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/reconcile-section.md`. Red until the task is ported.

use spira_runtime::tasks::b::kit::{testing as t, Input};
use spira_runtime::tasks::b::reconcile_section::{self as task, Carry, Phase, ReconcileSection as H, Snapshot};

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "60")];
const MIN: u64 = 60_000;

fn snap(published: bool) -> Snapshot {
    Snapshot { published, reason: if published { "published".into() } else { "unchanged within 5m".into() } }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

fn sim() -> Vec<String> {
    sim::doubles::service().names()
}

#[test]
fn t1_registered_boots_and_arms_a_60s_tick() {
    t::t1_registered_and_boots::<H>(SET, MIN);
}

#[test]
fn t2_a_published_false_answer_journals_nothing() {
    t::t2_silent::<H>(SET, &json(&snap(false)));
}

#[test]
fn t3_idle_to_published_is_recorded_once_and_reads_back() {
    let h = t::handler::<H>(SET);
    let (next, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(snap(true)), MIN).unwrap();
    assert_eq!(next.phase, Phase::Published);
    assert!(acts.is_empty(), "a publish answer asks for nothing more: {acts:?}");
    let refused = task::decide(&h.cfg, &Carry::default(), &Input::Outcome { effect: "reconcile.section.bogus".into(), ok: true, body: String::new() }, MIN).unwrap_err();
    assert!(refused.contains("idle"), "an outcome the task never asked for is refused naming the state: {refused}");
    let rig = t::rig::<H>(1, SET, &json(&snap(true)));
    t::assert_ported(&rig, task::STREAM);
    assert_eq!(rig.journaled().iter().filter(|e| e.kind.tag == task::STATE).count(), 1);
    assert_eq!(t::recorded::<H>(&rig), Some(next));
}

#[test]
fn t4_its_one_effect_is_runnable_and_the_module_spawns_nothing() {
    let sim = sim();
    t::t4_runnable::<H>(include_str!("../../src/tasks/b/reconcile_section.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_one_publish_per_tick_as_the_old_unit_made() {
    let h = t::handler::<H>(SET);
    for s in [snap(false), snap(true)] {
        let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(s.clone()), MIN).unwrap();
        assert!(acts.is_empty(), "{s:?}: the publish is the tick's probe, the decision asks for nothing: {acts:?}");
    }
    let mut rig = t::rig::<H>(1, SET, &json(&snap(false)));
    rig.advance(10 * MIN).unwrap();
    t::assert_ported(&rig, task::STREAM);
    assert_eq!(t::fired_at::<H>(&rig), (0..=10).map(|i| i * MIN).collect::<Vec<_>>());
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rig = t::rig::<H>(1, SET, &json(&snap(true)));
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&rig, task::NAME);
    assert!(rows.iter().any(|k| k == "phase"), "{rows:?}");
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    t::assert_boot_matches_an_unstopped_run::<H>(SET, vec![json(&snap(true))], |_| {});
}

#[test]
fn a_boot_does_not_record_the_publish_a_second_time() {
    let mut rig = t::rig::<H>(1, SET, &json(&snap(true)));
    let recorded = |rig: &runtime::taskrig::TaskRig| rig.journaled().iter().filter(|e| e.kind.tag == task::STATE).count();
    assert_eq!(recorded(&rig), 1);
    rig.reopen(t::handler::<H>(SET).registration()).unwrap();
    rig.tick(task::STREAM, runtime::event::EventKind::new(task::TICK, "")).unwrap();
    assert_eq!(recorded(&rig), 1, "the publish was recorded again after the restart");
}
