//! T1–T6 for `roundvm.drain`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/roundvm-drain.md`. Red until the task is ported.

use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use spira_runtime::tasks::c::roundvm_drain::{self as task, Carry, Phase, Ready, RoundvmDrain as H, Snapshot};

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "300"), ("SPIRA_ROUND_VM_WARM_IDLE_SECS", "1800"), ("SPIRA_ROUND_VM_SPILL", "pve")];
const TICK: u64 = 300_000;
const NOW: u64 = 1_793_664_000_000;

fn pool(ready_age_min: Option<u64>, leased: &[&str]) -> Snapshot {
    Snapshot { ready: ready_age_min.map(|m| Ready { handle: "vm-105".into(), ready_since: NOW - m * 60_000 }), leased: leased.iter().map(|s| s.to_string()).collect(), spill_mode: "pve".into(), idle_bound_secs: 1_800, ..Snapshot::default() }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn t1_registered_boots_and_arms_a_5min_tick() {
    t::t1_registered_and_boots::<H>(SET, TICK);
}

#[test]
fn t2_no_ready_vm_and_a_young_one_are_the_probe_alone() {
    t::t2_silent::<H>(SET, &json(&pool(None, &[])));
    t::t2_silent::<H>(SET, &json(&pool(Some(10), &[])));
}

#[test]
fn t3_an_aged_vm_is_released_and_a_release_of_a_leased_vm_is_refused_naming_the_lease() {
    let h = t::handler::<H>(SET);
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(pool(Some(40), &[])), NOW).unwrap();
    assert_eq!((c.phase, names(&acts)), (Phase::Releasing, vec![task::RELEASE]));
    let (c, _) = task::decide(&h.cfg, &c, &Input::Outcome { effect: task::RELEASE.into(), ok: true, body: "Released".into() }, NOW).unwrap();
    assert_eq!(c.phase, Phase::Idle);
    let refused = task::decide(&h.cfg, &Carry { phase: Phase::Releasing }, &Input::Outcome { effect: task::RELEASE.into(), ok: false, body: "leased by round r-9".into() }, NOW).unwrap_err();
    assert!(refused.contains("lease"), "{refused}");
}

#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_nothing() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../src/tasks/c/roundvm_drain.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_draining_never_touches_a_vm_a_live_round_holds_and_ec2_always_releases_at_once() {
    let h = t::handler::<H>(SET);
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(pool(Some(40), &["vm-105"])), NOW).unwrap();
    assert!(acts.is_empty(), "a leased VM is never drained: {acts:?}");
    let ec2 = Snapshot { spill_mode: "ec2-always".into(), ..pool(Some(1), &[]) };
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(ec2), NOW).unwrap();
    assert_eq!(names(&acts), [task::RELEASE]);
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { world_halted: true, ..pool(Some(40), &[]) }), NOW).unwrap();
    assert!(acts.is_empty(), "nothing while halted: {acts:?}");
}

#[test]
fn t6_two_virtual_days_are_deterministic_and_journal_nothing_without_a_release() {
    t::t6_deterministic::<H>(|seed| t::rig::<H>(seed, SET, &json(&pool(Some(10), &[]))), 2, 1);
}
