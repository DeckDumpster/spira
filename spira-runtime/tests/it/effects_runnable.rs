//! T4: every effect a task declares has a real runner in the service's router and a double in
//! the simulator's quiet world, so a task never asks the sim for something it cannot answer.
//! `effects_runnable` refuses a planted unknown name first (its positive control).

use runtime::taskrig::{effects_runnable, PLANTED_UNKNOWN};
use spira_runtime::tasks;

#[test_id(t2z7g08)]
#[test]
fn every_declared_effect_has_a_real_runner_and_a_sim_double() {
    let declared: Vec<&str> = tasks::all()
        .flat_map(|t| t.effects.iter().copied())
        .collect();
    assert!(
        declared.len() >= 20,
        "the scan must see the task table: {declared:?}"
    );
    let real: Vec<&str> = tasks::routes().keys().copied().collect();
    let doubles = sim::doubles::service().names();
    let sim: Vec<&str> = doubles.iter().map(String::as_str).collect();
    assert!(
        !sim.contains(&PLANTED_UNKNOWN),
        "the planted name must stay unknown to the sim"
    );
    assert_eq!(effects_runnable(&declared, &real, &sim), Ok(()));
}

#[test_id(tcfolgp)]
#[test]
fn no_task_named_sweep_repo_contract_is_registered() {
    assert!(!tasks::all().any(|t| t.name == "sweep.repo_contract"));
}

#[test_id(tft1a70)]
#[test]
fn no_task_named_watch_warm_full_is_registered() {
    assert!(!tasks::all()
        .chain(tasks::pending())
        .any(|t| t.name == "watch.warm_full"));
}

#[test_id(tji9hsn)]
#[test]
fn no_task_named_sweep_nix_gc_is_registered() {
    assert!(!tasks::all()
        .chain(tasks::pending())
        .any(|t| t.name == "sweep.nix_gc"));
}

#[test_id(tji0dwq)]
#[test]
fn the_unit_install_starts_to_collect_nix_garbage_belongs_to_no_task() {
    let ran = runtime::units::UnitRan {
        handle: runtime::units::UnitHandle::of(install::NIX_GC_UNIT_TASK, "rel1").unwrap(),
        exit: runtime::units::Exit::Code(0),
        duration_ms: 1,
    };
    assert!(
        ran.key_for(install::NIX_GC_UNIT_TASK).is_some(),
        "control: the key reads back for the task name the unit carries"
    );
    assert!(
        tasks::all()
            .chain(tasks::pending())
            .all(|t| ran.key_for(t.name).is_none()),
        "a task would take the collection's end as its own"
    );
}
