//! T4: every effect a task declares has a real runner in the service's router and a double in
//! the simulator's quiet world, so a task never asks the sim for something it cannot answer.
//! `effects_runnable` refuses a planted unknown name first (its positive control).

use runtime::taskrig::{effects_runnable, PLANTED_UNKNOWN};
use spira_runtime::tasks;

#[test]
fn every_declared_effect_has_a_real_runner_and_a_sim_double() {
    let declared: Vec<&str> = tasks::all().flat_map(|t| t.effects.iter().copied()).collect();
    assert!(declared.len() >= 20, "the scan must see the task table: {declared:?}");
    let real: Vec<&str> = tasks::routes().keys().copied().collect();
    let doubles = sim::doubles::service().names();
    let sim: Vec<&str> = doubles.iter().map(String::as_str).collect();
    assert!(!sim.contains(&PLANTED_UNKNOWN), "the planted name must stay unknown to the sim");
    assert_eq!(effects_runnable(&declared, &real, &sim), Ok(()));
}
