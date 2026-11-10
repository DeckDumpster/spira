//! T1–T6 for `watch.sop_lint` (contract `wiki/projects/spira/designs/tasks/watch-sop-lint.md`).
//! The behaviour tests are red until the task is ported: the stub's `decide` panics, the rig
//! quarantines the stream, and `assert_healthy` turns that into the failure.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_deterministic, assert_journal_budget, assert_silent_tick, effects_runnable, TaskRig, DAY_MS};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::d::watch_sop_lint::*;

/// `sop/src/logic/tests.rs:84`.
const GOOD: &str = "SYMPTOM: disk is full\nCHECK: df -h\nFIX: clear /tmp\n";

fn config() -> Config {
    Config { db: "/db".into() }
}

fn shelf(extra: &[(&str, &str)]) -> Snapshot {
    let mut s: BTreeMap<String, String> = ["sop-a", "sop-b", "sop-c"].iter().map(|k| (k.to_string(), GOOD.to_string())).collect();
    s.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    Snapshot { halted: false, shelf: Some(s), word_cap: 250 }
}

fn blind() -> Snapshot {
    Snapshot { halted: false, shelf: None, word_cap: 250 }
}

fn rig(seed: u64, world: Arc<Mutex<Snapshot>>) -> TaskRig {
    let (_, stream, kind) = WatchSopLint::boot();
    let mut r = TaskRig::new(seed, WatchSopLint::new(config()).registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap())).double(FILE_INCIDENT, |_| Ok("{}".into()));
    r.tick(stream, kind).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn filed(r: &TaskRig) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == FILE_INCIDENT).map(|q| q.input.clone()).collect()
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
fn t2_a_valid_shelf_journals_nothing() {
    let mut r = rig(1, Arc::new(Mutex::new(shelf(&[]))));
    assert_silent_tick(&mut r, STREAM);
    acc::assert_healthy(&r, STREAM);
}

/// Stateless: the outcomes are none, file(blind) and file(finding); halted files nothing.
#[test]
fn t3_each_outcome_is_one_filing_or_none_and_a_halted_world_files_nothing() {
    assert!(filed(&rig(1, Arc::new(Mutex::new(shelf(&[]))))).is_empty());
    assert_eq!(filed(&rig(1, Arc::new(Mutex::new(blind())))).len(), 1);
    assert_eq!(filed(&rig(1, Arc::new(Mutex::new(shelf(&[("sop-bad", "SYMPTOM: only this\n")]))))).len(), 1);
    let halted = Snapshot { halted: true, ..shelf(&[("sop-bad", "SYMPTOM: only this\n")]) };
    let mut r = rig(1, Arc::new(Mutex::new(halted)));
    r.advance(3 * CADENCE_MS).unwrap();
    acc::assert_healthy(&r, STREAM);
    assert!(filed(&r).is_empty(), "a filing while halted is refused");
}

#[test]
fn t4_every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("d/watch_sop_lint.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(!real.contains(&"watch.sop_lint.bogus") && !sim.contains(&"watch.sop_lint.bogus"));
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

/// The selftest's find and blind cases (`spira/sop-lint-watch.sh:10-33`): the titles it filed.
#[test]
fn t5_the_titles_are_the_ones_sop_lint_watch_filed() {
    let f = filed(&rig(1, Arc::new(Mutex::new(shelf(&[("sop-bad", "SYMPTOM: only this\n")])))));
    assert!(f.len() == 1 && f[0].contains(FINDING_TITLE) && f[0].contains("FAIL  sop-bad"), "{f:?}");
    let f = filed(&rig(1, Arc::new(Mutex::new(blind()))));
    assert!(f.len() == 1 && f[0].contains("sop lint could not read the shelf (control cannot check)"), "{f:?}");
}

#[test]
fn t6_a_finding_standing_two_days_replays_identically_at_one_filing_an_hour() {
    let r = assert_deterministic(|seed| rig(seed, Arc::new(Mutex::new(shelf(&[("sop-bad", "SYMPTOM: only this\n")])))), 7, 2);
    acc::assert_healthy(&r, STREAM);
    assert_eq!(filed(&r).len() as u64, 2 * DAY_MS / CADENCE_MS + 1, "a standing finding re-files every hour, as today");
    // 49 filings (the boot tick and 48 hourly ones) journal a request and an outcome each: 98
    // events in 48 h, over a budget of 2/h by the fence post, so the ceiling is 3/h.
    assert_journal_budget(&r, 3);
}

fn standing_finding() -> TaskRig {
    rig(1, Arc::new(Mutex::new(shelf(&[("sop-bad", "SYMPTOM: only this\n")]))))
}

#[test]
fn a_stateless_task_journals_no_carry_row() {
    let r = standing_finding();
    assert_eq!(filed(&r).len(), 1);
    assert!(r.journal.state().into_iter().all(|((t, _), v)| t != NAME || v.is_empty()), "{NAME} kept a row");
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(standing_finding, || WatchSopLint::new(config()).registration(), STREAM, WatchSopLint::boot().2);
}
