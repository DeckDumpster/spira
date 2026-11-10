//! T1–T6 for `watch.testenv_cap` (contract `wiki/projects/spira/designs/tasks/watch-testenv-cap.md`),
//! the 15 s backstop that kills over-cap `testenv` runs. The behaviour tests are red until the
//! task is ported: the stub's `decide` panics, the rig quarantines the stream, and
//! `assert_healthy` turns that into the failure.

use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_silent_tick, effects_runnable, TaskRig};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::e::watch_testenv_cap::*;
use spira_runtime::watchers::INBOX_APPEND;

fn config() -> Config {
    Config { suite_cap_s: 600, gate_cap_s: 300, load_wait_s: 60, slot_wait_s: 30, inbox: "/q/inbox".into() }
}

fn run(pid: u32, age: u64, argv: &[&str]) -> Proc {
    Proc { pid, start_ticks: 1000 + u64::from(pid), argv: argv.iter().map(|s| s.to_string()).collect(), age_secs: age }
}

const SUITE: &[&str] = &["/r/bin/testenv", "--suites", "test-a.sh", "spira"];
const GATE: &[&str] = &["/r/bin/testenv", "--deadline", "300", "spira"];

fn world(procs: Vec<Proc>) -> Arc<Mutex<Snapshot>> {
    Arc::new(Mutex::new(Snapshot { procs }))
}

fn rig(seed: u64, world: Arc<Mutex<Snapshot>>, kill: fn(&str) -> Result<String, String>) -> TaskRig {
    let (_, stream, kind) = WatchTestenvCap::boot();
    let mut r = TaskRig::new(seed, WatchTestenvCap::new(config()).registration())
        .unwrap()
        .double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap()))
        .double(KILL, kill)
        .double(INBOX_APPEND, |l| Ok(l.into()))
        .double(METRIC, |_| Ok("{}".into()));
    r.tick(stream, kind).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn killed(_: &str) -> Result<String, String> {
    Ok("killed".into())
}

fn asked(r: &TaskRig, name: &str) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == name).map(|q| q.input.clone()).collect()
}

#[test]
fn t1_the_row_is_registered_and_its_boot_arms_the_fifteen_second_tick() {
    let t = acc::row(NAME);
    assert_eq!((t.stream(), t.effects, t.keys.concat()), (STREAM, EFFECTS, KEYS.to_vec()));
    let s = acc::pinned_settings(&[]).expect("settings from the complete fixture");
    assert_eq!((t.cadence_ms)(&s), CADENCE_MS);
    assert_eq!(Config::pinned().map(|_| ()), Ok(()), "every key resolves through the config door");
    let mut r = TaskRig::new(1, (t.build)(&s)).unwrap();
    let (_, stream, kind) = (t.boot)();
    r.tick(stream, kind).unwrap();
    assert_eq!(r.engine().next_timer_at(), Some(CADENCE_MS));
    assert_eq!(r.requested().iter().map(|q| q.name.as_str()).collect::<Vec<_>>(), [OBSERVE]);
}

#[test]
fn t2_no_run_and_runs_under_cap_journal_nothing() {
    let mut r = rig(1, world(vec![]), killed);
    assert_silent_tick(&mut r, STREAM);
    acc::assert_healthy(&r, STREAM);
    let mut r = rig(1, world(vec![run(10, 100, SUITE), run(11, 100, GATE), run(12, 9999, &["/r/bin/testenv", "suites", "status"])]), killed);
    assert_silent_tick(&mut r, STREAM);
    acc::assert_healthy(&r, STREAM);
}

/// Per breach: judged → killed → said, or judged → refused with nothing said.
#[test]
fn t3_a_breach_is_said_only_after_its_kill_and_a_refused_kill_says_nothing() {
    let r = rig(1, world(vec![run(4242, 800, SUITE)]), killed);
    assert_eq!(asked(&r, KILL).len(), 1, "judged → kill");
    let order: Vec<&str> = r.requested().iter().map(|q| q.name.as_str()).filter(|n| *n != OBSERVE).collect();
    assert_eq!(order, [KILL, INBOX_APPEND, METRIC], "killed → said, the line on the kill's outcome");
    let r = rig(1, world(vec![run(4242, 800, SUITE)]), |_| Ok("refused: identity changed".into()));
    acc::assert_healthy(&r, STREAM);
    assert!(asked(&r, INBOX_APPEND).is_empty() && asked(&r, METRIC).is_empty(), "said without killed is refused");
}

#[test]
fn t4_every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("e/watch_testenv_cap.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(!real.contains(&"watch.testenv_cap.bogus") && !sim.contains(&"watch.testenv_cap.bogus"));
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

/// `cap::breaches` on the `cap.rs` cases and `report`'s line (`cap_main.rs:23-37`), no tag. A gate run
/// carries its cap plus `SETTLE` (`cap.rs:16`): 300 + 30 + slot wait 30 + grace 60 = 420.
#[test]
fn t5_the_breaches_and_the_line_are_the_ones_testenv_cap_made() {
    let procs = vec![run(1, 751, SUITE), run(2, 750, SUITE), run(3, 421, GATE), run(4, 420, GATE)];
    let r = rig(1, world(procs), killed);
    acc::assert_healthy(&r, STREAM);
    let kills = asked(&r, KILL);
    assert_eq!(kills.len(), 2, "only past cap + waits + grace: {kills:?}");
    let r = rig(1, world(vec![run(4242, 800, SUITE)]), killed);
    assert_eq!(asked(&r, INBOX_APPEND), ["1970-01-01T00:00:00Z testenv over-cap killed: pid 4242 tier=suite wall=800s cap=600s cmd=/r/bin/testenv --suites test-a.sh spira"]);
}

fn registration() -> runtime::event::Registration {
    WatchTestenvCap::new(config()).registration()
}

fn over_cap() -> TaskRig {
    rig(1, world(vec![run(7, 900, SUITE)]), killed)
}

#[test]
fn a_cap_that_carries_nothing_keeps_no_row_after_killing() {
    let mut r = over_cap();
    r.advance(CADENCE_MS).unwrap();
    assert!(!asked(&r, KILL).is_empty());
    let kept: Vec<_> = r.journal.state().into_iter().filter(|((t, _), v)| t == NAME && !v.is_empty()).collect();
    assert!(kept.is_empty(), "{kept:?}");
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(over_cap, registration, STREAM, runtime::event::EventKind::new(TICK, ""));
}
