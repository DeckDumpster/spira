//! T1–T6 for `watch.perf_happy` (contract `wiki/projects/spira/designs/tasks/watch-perf-happy.md`),
//! a P2 machine. The behaviour tests are red until the task is ported: the stub's `decide`
//! panics, the rig quarantines the stream, and `assert_healthy` turns that into the failure.

use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_silent_tick, effects_runnable, TaskRig};
use runtime::units::{Exit, UnitHandle, UnitRan, UnitSpec, UNIT_START};
use runtime::EventKind;
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::d::pattern::transition;
use spira_runtime::tasks::d::watch_perf_happy::*;

fn config() -> Config {
    Config { run: "/r".into() }
}

fn world(round: Round) -> Arc<Mutex<Snapshot>> {
    Arc::new(Mutex::new(Snapshot { round }))
}

fn rig(seed: u64, world: Arc<Mutex<Snapshot>>) -> TaskRig {
    let (_, stream, kind) = WatchPerfHappy::boot();
    let mut r = TaskRig::new(seed, WatchPerfHappy::new(config()).registration())
        .unwrap()
        .double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap()))
        .double(CHECKOUT, |_| Ok("{}".into()))
        .double(UNIT_START, |i| {
            let spec: UnitSpec = serde_json::from_str(i).map_err(|e| e.to_string())?;
            Ok(serde_json::to_string(&UnitHandle::of(&spec.task, &spec.key).map_err(|e| e.to_string())?).unwrap())
        })
        .double(TEARDOWN, |_| Ok("{}".into()))
        .double(LOG, |l| Ok(l.into()))
        .double(HISTORY, |l| Ok(l.into()));
    r.tick(stream, kind).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn carry(r: &TaskRig) -> Carry {
    r.journaled().iter().rev().find(|e| e.kind.tag == STATE).map(|e| serde_json::from_str(&e.kind.body).unwrap()).unwrap_or_default()
}

fn asked(r: &TaskRig, name: &str) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == name).map(|q| q.input.clone()).collect()
}

/// The `unit.ran` for the last run the task started, ended with `exit` after `ms`.
fn ran(r: &TaskRig, exit: Exit, ms: u64) -> EventKind {
    let spec: UnitSpec = serde_json::from_str(asked(r, UNIT_START).last().expect("a unit.start")).unwrap();
    UnitRan { handle: UnitHandle::of(NAME, &spec.key).unwrap(), exit, duration_ms: ms }.event_kind()
}

#[test]
fn t1_the_row_is_registered_and_its_boot_arms_the_hourly_tick() {
    let row = acc::row(NAME);
    assert_eq!((row.stream(), row.effects, row.keys.concat()), (STREAM, EFFECTS, KEYS.to_vec()));
    let s = acc::pinned_settings(&[]).expect("settings from the complete fixture");
    assert_eq!((row.cadence_ms)(&s), CADENCE_MS);
    assert_eq!(Config::pinned().map(|_| ()), Ok(()), "every key resolves through the config door");
    let mut r = TaskRig::new(1, (row.build)(&s)).unwrap();
    let (_, stream, kind) = (row.boot)();
    r.tick(stream, kind).unwrap();
    assert_eq!(r.engine().next_timer_at(), Some(CADENCE_MS));
    assert_eq!(r.requested().iter().map(|q| q.name.as_str()).collect::<Vec<_>>(), [OBSERVE]);
}

#[test]
fn t2_an_open_round_withholds_the_run_and_journals_nothing() {
    let mut r = rig(1, world(Round::Open));
    assert_silent_tick(&mut r, STREAM);
    acc::assert_healthy(&r, STREAM);
}

#[test]
fn t3_the_machine_moves_only_along_its_table_and_refuses_a_run_it_did_not_start() {
    let mut r = rig(1, world(Round::None));
    assert_eq!(carry(&r).phase, Phase::Running, "idle → running on round=none");
    assert_eq!(asked(&r, UNIT_START).len(), 1);
    let done = ran(&r, Exit::Code(0), 30_000);
    r.tick(STREAM, done.clone()).unwrap();
    acc::assert_healthy(&r, STREAM);
    assert_eq!(carry(&r).phase, Phase::Idle, "running → idle once measured");
    assert_eq!(carry(&r).history.len(), 1);
    r.tick(STREAM, done).unwrap();
    let refused: Vec<String> = r.journaled().iter().filter(|e| e.kind.tag == REFUSED).map(|e| e.kind.body.clone()).collect();
    assert!(refused.len() == 1 && refused[0].contains("Idle"), "unit.ran while idle is refused naming the state: {refused:?}");
    assert_eq!(transition(TRANSITIONS, NAME, Phase::Idle, "unit.ran measured").map_err(|e| e.contains("Idle")), Err(true));
    assert_eq!(transition(TRANSITIONS, NAME, Phase::Running, "unit.ran fixture-retry"), Ok(Phase::Running));
}

#[test]
fn t4_every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("d/watch_perf_happy.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(!real.contains(&"watch.perf_happy.bogus") && !sim.contains(&"watch.perf_happy.bogus"));
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

/// `happy::alarm` on the same outcomes, the line `perf-watch.log` received.
#[test]
fn t5_the_lines_are_the_ones_happy_path_logged() {
    let r = rig(1, world(Round::Unknown("lc down".into())));
    assert_eq!(asked(&r, LOG), ["PERF UNRUNNABLE happy-path: round state unknown, run withheld: lc down"]);
    let mut r = rig(1, world(Round::None));
    let slow = ran(&r, Exit::Code(0), 75_000);
    r.tick(STREAM, slow).unwrap();
    acc::assert_healthy(&r, STREAM);
    assert_eq!(asked(&r, LOG), ["PERF SLOW happy-path 75000ms (goal 60000ms) slowest phase: unknown; recent ms: 75000"]);
    assert_eq!(asked(&r, HISTORY).len(), 1, "the measured run is appended to the history");
}
