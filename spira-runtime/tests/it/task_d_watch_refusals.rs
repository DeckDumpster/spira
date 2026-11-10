//! T1–T6 for `watch.refusals` (contract `wiki/projects/spira/designs/tasks/watch-refusals.md`).
//! The behaviour tests are red until the task is ported: the stub's `decide` panics, the rig
//! quarantines the stream, and `assert_healthy` turns that into the failure.

use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_deterministic, assert_journal_budget, assert_silent_tick, effects_runnable, TaskRig, DAY_MS};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::d::watch_refusals::*;

const HOUR: u64 = 3_600_000;

fn config() -> Config {
    Config { window_secs: WINDOW_SECS, baseline: BASELINE, repeat_secs: REPEAT_SECS, max_files: MAX_FILES }
}

fn class(event: &str, n: u64) -> Class {
    Class { actor: "sentinel".into(), event: event.into(), from_state: "SUBMITTED".into(), refusal: "not-ready".into(), n, first_at: 0, last_at: 0, examples: vec![("sp-a".into(), 0)] }
}

fn world(classes: Vec<Class>) -> Arc<Mutex<Snapshot>> {
    Arc::new(Mutex::new(Snapshot { now: 0, classes }))
}

fn rig(seed: u64, world: Arc<Mutex<Snapshot>>, filer: fn(&str) -> Result<String, String>) -> TaskRig {
    let (_, stream, kind) = WatchRefusals::boot();
    let mut r = TaskRig::new(seed, WatchRefusals::new(config()).registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap())).double(FILE, filer);
    r.tick(stream, kind).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn filed_ok(_: &str) -> Result<String, String> {
    Ok("sp-inc1".into())
}

fn asked(r: &TaskRig) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == FILE).map(|q| q.input.clone()).collect()
}

fn carry(r: &TaskRig) -> Carry {
    r.journaled().iter().rev().find(|e| e.kind.tag == STATE).map(|e| serde_json::from_str(&e.kind.body).unwrap()).unwrap_or_default()
}

#[test]
fn t1_the_row_is_registered_and_its_boot_arms_the_hourly_tick() {
    let row = acc::row(NAME);
    assert_eq!((row.stream(), row.effects, row.keys.concat()), (STREAM, EFFECTS, KEYS.to_vec()));
    let s = acc::pinned_settings(&[]).expect("settings from the complete fixture");
    assert_eq!((row.cadence_ms)(&s), CADENCE_MS);
    let mut r = TaskRig::new(1, (row.build)(&s)).unwrap();
    let (_, stream, kind) = (row.boot)();
    r.tick(stream, kind).unwrap();
    assert_eq!(r.engine().next_timer_at(), Some(CADENCE_MS));
    assert_eq!(r.requested().iter().map(|q| q.name.as_str()).collect::<Vec<_>>(), [OBSERVE]);
}

#[test]
fn t2_classes_at_or_under_baseline_journal_nothing() {
    let mut r = rig(1, world(vec![class("claim", BASELINE), class("submit", 1)]), filed_ok);
    assert_silent_tick(&mut r, STREAM);
    acc::assert_healthy(&r, STREAM);
}

#[test]
fn t3_a_class_is_filed_on_success_due_again_after_the_window_and_pruned_after_seven() {
    let w = world(vec![class("claim", 9)]);
    let mut r = rig(1, w.clone(), filed_ok);
    assert_eq!(asked(&r).len(), 1, "unfiled → filed once its filing succeeds");
    assert_eq!(carry(&r).filed.len(), 1);
    r.advance(12 * HOUR).unwrap();
    assert_eq!(asked(&r).len(), 1, "inside repeat_secs it is not filed again");
    r.advance(13 * HOUR).unwrap();
    assert_eq!(asked(&r).len(), 2, "filed → due again after repeat_secs");
    *w.lock().unwrap() = Snapshot::default();
    r.advance(8 * DAY_MS).unwrap();
    acc::assert_healthy(&r, STREAM);
    assert!(carry(&r).filed.is_empty(), "filed → pruned after 7 × repeat");
    let mut failing = rig(1, world(vec![class("claim", 9)]), |_| Err("store down".into()));
    failing.advance(HOUR).unwrap();
    assert!(carry(&failing).filed.is_empty(), "a failed filing leaves the class unfiled");
    assert_eq!(asked(&failing).len(), 2, "and it is retried next pass");
}

#[test]
fn t4_every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("d/watch_refusals.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(!real.contains(&"watch.refusals.bogus") && !sim.contains(&"watch.refusals.bogus"));
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

/// `title` and `body` (`refusal-watch/src/lib.rs:158-163,270-280`) and the worst-first bound.
#[test]
fn t5_the_filing_is_the_one_refusal_watch_made() {
    let r = rig(1, world(vec![class("claim", 9)]), filed_ok);
    let f = asked(&r);
    assert!(f.len() == 1 && f[0].contains("refused claim from SUBMITTED (not-ready) by sentinel"), "{f:?}");
    assert!(f[0].contains("shows the whole timeline."), "the body ends with the ops-bead pointer: {f:?}");
    let many: Vec<Class> = (0..12).map(|i| class(&format!("e{i:02}"), 6 + i)).collect();
    let r = rig(1, world(many), filed_ok);
    let f = asked(&r);
    assert_eq!(f.len(), MAX_FILES, "at most max_files per pass");
    assert!(f[0].contains("refused e11 "), "worst first: {:?}", f[0]);
}

#[test]
fn t6_seven_days_of_a_repeating_class_replay_identically_inside_the_budget() {
    let r = assert_deterministic(|seed| rig(seed, world(vec![class("claim", 9)]), filed_ok), 7, 7);
    acc::assert_healthy(&r, STREAM);
    // The rig fires the tick at exactly 7 days too, where `due`'s `now - filed >= repeat` files
    // the eighth; one per day is counted over the seven days before it.
    assert_eq!(r.requested().iter().filter(|q| q.name == FILE && q.at < 7 * DAY_MS).count(), 7, "one filing per day");
    assert_journal_budget(&r, 2);
}

fn filed_class() -> TaskRig {
    rig(1, world(vec![class("claim", 9)]), filed_ok)
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&filed_class(), NAME);
    assert!(rows.iter().any(|k| k.starts_with("filed/")), "{rows:?}");
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(filed_class, || WatchRefusals::new(config()).registration(), STREAM, WatchRefusals::boot().2);
}
