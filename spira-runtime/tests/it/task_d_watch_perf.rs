//! T1–T6 for `watch.perf` (contract `wiki/projects/spira/designs/tasks/watch-perf.md`). The
//! behaviour tests are red until the task is ported: the stub's `decide` panics, the rig
//! quarantines the stream, and `assert_healthy` turns the quarantine into the failure.

use std::sync::{Arc, Mutex};

use runtime::taskrig::{effects_runnable, TaskRig};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::d::watch_perf::*;
use spira_runtime::watch_topics::Change;
use spira_runtime::watchers::{INBOX_APPEND, WATCH_PUBLISH};

const MIN: u64 = 60_000;

fn config() -> Config {
    Config { budget_ms: 500, interval_secs: 300, remind_secs: 3600, inbox: "q/inbox".into(), metrics_endpoint: "127.0.0.1:9".into(), run: "r".into() }
}

fn timing(name: &str, millis: u64) -> Timing {
    Timing { name: name.into(), micros: millis * 1000, millis, phases: None, error: None, timed_out_ms: None }
}

/// The six probes of `spira/perf-probes`, every one inside the 500 ms budget.
fn idle() -> Snapshot {
    let timings = ["lc-list", "lc-show", "lc-ops-view", "work-list", "claim-check", "lc-ready"].iter().map(|n| timing(n, 40)).collect();
    Snapshot { timings, callers: [("concierge".to_string(), 10), ("sentinel".to_string(), 5)].into() }
}

fn slow(name: &str, millis: u64) -> Snapshot {
    let mut s = idle();
    s.timings.iter_mut().filter(|t| t.name == name).for_each(|t| *t = timing(name, millis));
    s
}

type World = Arc<Mutex<Snapshot>>;

fn rig(seed: u64, world: World) -> TaskRig {
    let (_, stream, kind) = WatchPerf::boot();
    let mut r = TaskRig::new(seed, WatchPerf::new(config()).registration())
        .unwrap()
        .double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap()))
        .double(INBOX_APPEND, |l| Ok(l.into()))
        .double(LOG, |l| Ok(l.into()))
        .double(SLOW_SAMPLE, |l| Ok(l.into()))
        .double(METRICS, |_| Ok("{}".into()))
        .double(WATCH_PUBLISH, |l| Ok(l.into()));
    r.tick(stream, kind).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn said(r: &TaskRig) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == INBOX_APPEND).map(|q| q.input.clone()).collect()
}

fn carry(r: &TaskRig) -> Carry {
    r.journaled().iter().rev().find(|e| e.kind.tag == STATE).map(|e| serde_json::from_str(&e.kind.body).unwrap()).unwrap_or_default()
}

#[test_id(tffzych)]
#[test]
fn t1_the_row_is_registered_and_its_boot_arms_the_five_minute_tick() {
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

/// The contract's silence: the probe, one metrics push and the state Emit, never an inbox line.
#[test_id(tlt5nuf)]
#[test]
fn t2_an_idle_pass_pushes_metrics_and_its_state_and_says_nothing() {
    let mut r = rig(1, Arc::new(Mutex::new(idle())));
    let (journal, asked) = (r.journaled().len(), r.requested().len());
    r.advance(CADENCE_MS).unwrap();
    acc::assert_healthy(&r, STREAM);
    let names: Vec<&str> = r.requested()[asked..].iter().map(|q| q.name.as_str()).collect();
    assert_eq!(names, [OBSERVE, METRICS]);
    let tags: Vec<String> = r.journaled()[journal..].iter().map(|e| e.kind.tag.clone()).filter(|t| t == STATE).collect();
    assert_eq!(tags, [STATE]);
    assert!(said(&r).is_empty());
}

#[test_id(tmoovo8)]
#[test]
fn t3_an_alarm_is_printed_reminded_after_the_window_and_forgotten_when_it_clears() {
    let world = Arc::new(Mutex::new(slow("lc-list", 900)));
    let mut r = rig(1, world.clone());
    assert_eq!(said(&r).len(), 1, "absent → printed(now)");
    assert_eq!(carry(&r).alarms.get("PERF SLOW lc-list"), Some(&0));
    r.advance(30 * MIN).unwrap();
    assert_eq!(said(&r).len(), 1, "a reminder inside the remind window is refused");
    r.advance(31 * MIN).unwrap();
    acc::assert_healthy(&r, STREAM);
    assert_eq!(said(&r).len(), 2, "printed → reminded once the window has passed");
    *world.lock().unwrap() = idle();
    r.advance(CADENCE_MS).unwrap();
    assert!(!carry(&r).alarms.contains_key("PERF SLOW lc-list"), "printed → forgotten when it clears");
}

#[test_id(tskhdj7)]
#[test]
fn t4_every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("d/watch_perf.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(!real.contains(&"watch.perf.bogus") && !sim.contains(&"watch.perf.bogus"), "a planted watch.perf.bogus is refused");
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

/// `perf-watch/src/lib.rs` `alarms` and `inbox_line` on the same timings.
#[test_id(t2e3szg)]
#[test]
fn t5_the_lines_are_the_ones_perf_watch_wrote() {
    let r = rig(1, Arc::new(Mutex::new(slow("lc-list", 900))));
    assert_eq!(said(&r), ["1970-01-01T00:00:00Z [watch:perf-watch] PERF SLOW lc-list 900ms (limit 500ms) phases unreported; top callers: unknown"]);
    let mut unrunnable = idle();
    unrunnable.timings[1].error = Some("exit status: 1".into());
    let r = rig(1, Arc::new(Mutex::new(unrunnable)));
    assert_eq!(said(&r), ["1970-01-01T00:00:00Z [watch:perf-watch] PERF UNRUNNABLE lc-show: exit status: 1 (after 40ms; phases unreported)"]);
    let logged: Vec<&str> = r.requested().iter().filter(|q| q.name == LOG).map(|q| q.input.as_str()).collect();
    assert_eq!(logged, ["PERF UNRUNNABLE lc-show: exit status: 1 (after 40ms; phases unreported)"], "the log carries the bare alarm");
}

fn published(r: &TaskRig) -> Vec<Change> {
    r.requested().iter().filter(|q| q.name == WATCH_PUBLISH).map(|q| serde_json::from_str(&q.input).unwrap()).collect()
}

#[test_id(td41rgu)]
#[test]
fn a_slow_probe_held_across_three_passes_is_set_once_and_cleared_once_and_probes_are_distinct_subjects() {
    let mut two = slow("lc-list", 900);
    two.timings.iter_mut().filter(|t| t.name == "lc-show").for_each(|t| *t = timing("lc-show", 700));
    let world = Arc::new(Mutex::new(two));
    let mut r = rig(1, world.clone());
    for millis in [950, 990] {
        world.lock().unwrap().timings[0] = timing("lc-list", millis);
        r.advance(CADENCE_MS).unwrap();
    }
    let held = published(&r);
    let [set] = held.as_slice() else { panic!("one set: {held:?}") };
    assert_eq!((set.row.as_str(), set.set.keys().map(String::as_str).collect::<Vec<_>>(), set.clear.len()), (INBOX_TAG, vec!["lc-list", "lc-show"], 0));
    assert_eq!(set.set["lc-list"].text, "PERF SLOW lc-list 900ms (limit 500ms) phases unreported; top callers: unknown");
    assert_eq!(set.set["lc-list"].level, "P2");

    *world.lock().unwrap() = slow("lc-show", 700);
    r.advance(CADENCE_MS).unwrap();
    r.advance(CADENCE_MS).unwrap();
    let all = published(&r);
    assert_eq!(all.len(), 2, "{all:?}");
    assert_eq!((all[1].set.len(), all[1].clear.clone()), (0, vec!["lc-list".to_string()]));
    acc::assert_healthy(&r, STREAM);
}

#[test_id(tzrq160)]
#[test]
fn a_reminder_says_the_value_again_and_keeps_when_the_alarm_began() {
    let world = Arc::new(Mutex::new(slow("lc-list", 900)));
    let mut r = rig(1, world.clone());
    *world.lock().unwrap() = slow("lc-list", 950);
    r.advance(61 * MIN).unwrap();
    let held = published(&r);
    assert_eq!(held.len(), 2, "{held:?}");
    let again = &held[1].set["lc-list"];
    assert!(again.text.contains("950ms"), "{again:?}");
    assert_eq!(again.since, held[0].set["lc-list"].since);
}

fn standing_alarm() -> TaskRig {
    let mut r = rig(1, Arc::new(Mutex::new(slow("lc-list", 900))));
    r.advance(CADENCE_MS).unwrap();
    r
}

#[test_id(t62vqx5)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&standing_alarm(), NAME);
    assert!(rows.iter().any(|k| k == "last_pass") && rows.iter().any(|k| k.starts_with("alarms/")), "{rows:?}");
    assert!(!rows.iter().any(|k| k == "record"), "{rows:?}");
}

#[test_id(tiu14cq)]
#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(standing_alarm, || WatchPerf::new(config()).registration(), STREAM, WatchPerf::boot().2);
}
