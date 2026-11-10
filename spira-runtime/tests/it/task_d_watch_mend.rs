//! T1–T6 for `watch.mend` (contract `wiki/projects/spira/designs/tasks/watch-mend.md`). The
//! behaviour tests are red until the task is ported: the stub's `decide` panics, the rig
//! quarantines the stream, and `assert_healthy` turns that into the failure.

use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_silent_tick, effects_runnable, TaskRig};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::d::watch_mend::*;

fn config() -> Config {
    Config { run: "/r".into() }
}

fn quiet() -> Snapshot {
    Snapshot { now: 1000, overdue: vec![], mends: vec![("sp-a".into(), 2)], stops: vec![] }
}

fn overdue() -> Snapshot {
    Snapshot { overdue: vec![Overdue { batch: "b1".into(), pass: 2, suite: "test-x.sh".into(), deadline: 400 }], ..quiet() }
}

fn stop(reached: Result<bool, String>) -> Snapshot {
    Snapshot { stops: vec![Stop { repo: "spira".into(), bead: "sp-x".into(), state: "LANDED".into(), since: 500, reached }], ..quiet() }
}

type World = Arc<Mutex<Snapshot>>;

fn rig(seed: u64, world: World) -> TaskRig {
    let (_, stream, kind) = WatchMend::boot();
    let mut r = TaskRig::new(seed, WatchMend::new(config()).registration())
        .unwrap()
        .double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap()))
        .double(LOG_APPEND, |l| Ok(l.into()))
        .double(LIFT_STOP, |_| Ok("{}".into()));
    r.tick(stream, kind).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn asked(r: &TaskRig, name: &str) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == name).map(|q| q.input.clone()).collect()
}

fn carry(r: &TaskRig) -> Carry {
    r.journaled().iter().rev().find(|e| e.kind.tag == STATE).map(|e| serde_json::from_str(&e.kind.body).unwrap()).unwrap_or_default()
}

#[test]
fn t1_the_row_is_registered_and_its_boot_arms_the_two_minute_tick() {
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
fn t2_no_breach_and_no_stop_journals_nothing() {
    let mut r = rig(1, Arc::new(Mutex::new(quiet())));
    assert_silent_tick(&mut r, STREAM);
    acc::assert_healthy(&r, STREAM);
}

#[test]
fn t3_seen_is_the_current_breach_set_and_a_stop_lifts_only_when_reached() {
    let w = Arc::new(Mutex::new(overdue()));
    let mut r = rig(1, w.clone());
    assert!(carry(&r).seeded && carry(&r).seen.len() == 1, "unseeded → seeded(seen=S)");
    assert_eq!(asked(&r, LOG_APPEND).len(), 1);
    r.advance(CADENCE_MS).unwrap();
    assert_eq!(asked(&r, LOG_APPEND).len(), 1, "a breach already seen is not repeated");
    *w.lock().unwrap() = quiet();
    r.advance(CADENCE_MS).unwrap();
    assert!(carry(&r).seen.is_empty(), "seen=S → seen=S' when the breach clears");
    *w.lock().unwrap() = overdue();
    r.advance(CADENCE_MS).unwrap();
    assert_eq!(asked(&r, LOG_APPEND).len(), 2, "a breach that returns is announced again");
    *w.lock().unwrap() = stop(Err("history unreadable".into()));
    r.advance(CADENCE_MS).unwrap();
    assert!(asked(&r, LIFT_STOP).is_empty(), "an Err keeps the stop held");
    *w.lock().unwrap() = stop(Ok(true));
    r.advance(CADENCE_MS).unwrap();
    acc::assert_healthy(&r, STREAM);
    assert_eq!(asked(&r, LIFT_STOP).len(), 1, "held → lifted on reached=Ok(true)");
}

#[test]
fn t4_every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("d/watch_mend.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(!real.contains(&"watch.mend.bogus") && !sim.contains(&"watch.mend.bogus"));
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

/// `mend_watch.rs:22,29` and `pause_sweep.rs:23-24`, exactly, with no timestamp prefix.
#[test]
fn t5_the_log_lines_are_mend_watchs_and_pause_sweeps() {
    let mut triple = overdue();
    triple.mends = vec![("sp-m".into(), 3)];
    let r = rig(1, Arc::new(Mutex::new(triple)));
    assert_eq!(asked(&r, LOG_APPEND), ["MEND-WATCH OVERDUE b1#2#test-x.sh is MENDING 600s past its deadline; the sweep did not return it", "MEND-WATCH MENDS sp-m was mended 3 times in 24h (limit 2)"]);
    let r = rig(1, Arc::new(Mutex::new(stop(Ok(true)))));
    assert_eq!(asked(&r, LOG_APPEND), ["pause-sweep: lifted the stop on spira: its bead condition was met"]);
    let r = rig(1, Arc::new(Mutex::new(stop(Err("history unreadable".into())))));
    assert_eq!(asked(&r, LOG_APPEND), ["pause-sweep: kept spira: history unreadable"]);
}

#[test]
fn t7_acts_carry_the_act_tables_keys_not_a_digest_of_their_input() {
    let snap = Snapshot { stops: vec![Stop { repo: "spira".into(), bead: "sp-x".into(), state: "LANDED".into(), since: 500, reached: Ok(true) }], ..overdue() };
    let (_, first) = decide(&Carry::default(), &snap, 10_000, &config());
    let (_, later) = decide(&Carry::default(), &snap, 20_000, &config());
    let key = |acts: &[runtime::event::EffectSpec], name: &str| acts.iter().find(|a| a.name == name).map(|a| a.key.clone());
    assert_eq!(key(&first, LIFT_STOP).as_deref(), Some("watch.mend:lift:spira:500"), "a lift is keyed by its repo and the stop's since");
    assert_eq!(key(&first, LIFT_STOP), key(&later, LIFT_STOP), "the same stop is the same lift on every tick");
    assert!(key(&first, LOG_APPEND).is_some_and(|k| k.starts_with("watch.mend:") && k.ends_with(":10000")), "a log line is keyed by its breach and the tick");
    assert_ne!(key(&first, LOG_APPEND), key(&later, LOG_APPEND), "the same line on a later tick is a new request, not a refused repeat");
}

#[test]
fn t8_the_pattern_passes_each_act_key_through_to_its_effect_command() {
    use runtime::event::EffectSpec;
    use runtime::event::Command;
    let acts = vec![EffectSpec::keyed(LOG_APPEND, "watch.mend:k1", "same input", 5), EffectSpec::keyed(LOG_APPEND, "watch.mend:k2", "same input", 5)];
    let cmds = spira_runtime::tasks::d::pattern::commands("watch.mend.state", &0u8, &0u8, acts);
    let keys: Vec<String> = cmds.into_iter().filter_map(|c| if let Command::Effect(e) = c { Some(e.key) } else { None }).collect();
    assert_eq!(keys, ["watch.mend:k1", "watch.mend:k2"], "two effects with identical input keep their own keys");
}

fn standing_breach() -> TaskRig {
    rig(1, Arc::new(Mutex::new(overdue())))
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&standing_breach(), NAME);
    assert!(rows.iter().any(|k| k == "seen"), "{rows:?}");
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(standing_breach, || WatchMend::new(config()).registration(), STREAM, WatchMend::boot().2);
}
