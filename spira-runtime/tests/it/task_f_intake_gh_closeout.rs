//! `intake.gh_closeout` on `TaskRig`: a tick with nothing pending asks only for its probe; a
//! tick with a pending closeout asks for one drain.

use std::collections::BTreeMap;

use runtime::taskrig::{assert_silent_tick, effects_runnable, TaskRig};
use spira_runtime::tasks::f::intake_gh_closeout::*;
use spira_runtime::tasks::{self, f};

const CADENCE_MS: u64 = 120_000;
const MODULE: &str = include_str!("../../src/tasks/f/intake_gh_closeout.rs");
const LAUNCHERS: &[&str] = &["Command::new", "std::process", "process::Command", "libc::fork", "libc::exec", "posix_spawn", "nix::unistd"];

fn handler() -> IntakeGhCloseout {
    IntakeGhCloseout { interval_ms: CADENCE_MS, config: BTreeMap::new() }
}

fn rig(pending: Result<u64, String>) -> TaskRig {
    let snap = Snapshot { halted: false, pending };
    let mut r = TaskRig::new(1, handler().registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&snap).unwrap())).double(DRAIN, |_| Ok(String::new()));
    let (_, stream, kind) = IntakeGhCloseout::boot();
    r.tick(stream, kind).unwrap();
    r
}

#[test]
fn the_task_is_registered_and_arms_its_cadence() {
    let row = tasks::all().find(|t| t.name == NAME).expect("the row is in tasks::all()");
    assert_eq!(row.stream(), STREAM);
    assert!(row.keys.iter().flat_map(|k| k.iter()).any(|k| *k == CADENCE_KEY), "the cadence is pinned config");
    let mut r = TaskRig::new(1, handler().registration()).unwrap();
    let (_, stream, kind) = (row.boot)();
    r.tick(stream, kind).unwrap();
    assert_eq!(r.engine().next_timer_at(), Some(CADENCE_MS));
}

#[test]
fn a_tick_with_nothing_pending_asks_only_for_its_probe() {
    let mut r = rig(Ok(0));
    assert_silent_tick(&mut r, STREAM);
    assert!(r.requested().iter().all(|q| q.name != DRAIN), "{:?}", r.requested());
}

#[test]
fn a_tick_with_a_pending_closeout_asks_for_one_drain() {
    let r = rig(Ok(2));
    assert_eq!(r.requested().iter().filter(|q| q.name == DRAIN).count(), 1);
}

#[test]
fn an_unreadable_or_halted_world_drains_nothing() {
    assert_eq!(decide(&Snapshot { halted: false, pending: Err("down".into()) }), Decision::Skip("unreadable".into()));
    assert_eq!(decide(&Snapshot { halted: true, pending: Ok(3) }), Decision::Skip("halted".into()));
    assert!(rig(Err("down".into())).requested().iter().all(|q| q.name != DRAIN));
}

#[test]
fn every_effect_has_a_real_runner_and_a_sim_double() {
    let row = f::TASKS.iter().find(|t| t.name == NAME).expect("the row is ported");
    assert_eq!(row.effects, EFFECTS);
    let real: Vec<&str> = tasks::routes().keys().copied().collect();
    let doubles = sim::doubles::service().names();
    let sim: Vec<&str> = doubles.iter().map(String::as_str).collect();
    assert!(effects_runnable(&["intake.gh_closeout.bogus"], &real, &sim).is_err());
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

#[test]
fn the_module_launches_no_process() {
    assert!(LAUNCHERS.iter().all(|l| !MODULE.contains(l)));
}
