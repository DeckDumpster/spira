//! T1–T6 for `watch.tower` (contract `wiki/projects/spira/designs/tasks/watch-tower.md`). The
//! behaviour tests are red until the task is ported: the stub's `decide` panics, the rig
//! quarantines the stream, and `assert_healthy` turns that into the failure.

use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_silent_tick, effects_runnable, TaskRig};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::d::watch_tower::*;

fn config() -> Config {
    Config { start_timeout_s: 360, probe_timeout_s: 30, db: "db".into(), home_repo: "spira".into(), ask_label: "ask".into(), snap_stale_s: 60, noverdict_max: 3, queue_throttle_override: "off".into() }
}

/// `SweepData::fixture_nominal`'s shape: nothing lapsed, a fresh snapshot, no breach.
fn nominal() -> Snapshot {
    Snapshot { failed_units: Some(vec![]), disk_pct: Some(40), tmp_pct: Some(10), mem_avail_mb: Some(8000), snap_age_s: Some(12), ..Snapshot::default() }
}

fn failing() -> Snapshot {
    Snapshot { failed_units: Some(vec!["spira-gate-prod.service".into()]), ..nominal() }
}

type World = Arc<Mutex<Snapshot>>;

fn rig(seed: u64, world: World) -> TaskRig {
    let (_, stream, kind) = WatchTower::boot();
    let mut r = TaskRig::new(seed, WatchTower::new(config()).registration())
        .unwrap()
        .double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap()))
        .double(PROMPT, |_| Ok("{}".into()))
        .double(ALARM, |_| Ok("{}".into()))
        .double(DRAIN_CHECK, |_| Ok("{}".into()))
        .double(LOG, |l| Ok(l.into()));
    r.tick(stream, kind).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn asked(r: &TaskRig, name: &str) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == name).map(|q| q.input.clone()).collect()
}

#[test_id(tfaclcp)]
#[test]
fn t1_the_row_is_registered_and_its_boot_arms_the_half_hour_tick() {
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

#[test_id(t9c6gya)]
#[test]
fn t2_a_nominal_sweep_with_the_prompt_unchanged_journals_nothing() {
    let mut r = rig(1, Arc::new(Mutex::new(nominal())));
    assert_eq!(asked(&r, PROMPT).len(), 1, "the first nominal sweep writes its prompt");
    assert_silent_tick(&mut r, STREAM);
    acc::assert_healthy(&r, STREAM);
}

#[test_id(toi0lv7)]
#[test]
fn t3_halted_acts_on_nothing_nominal_prompts_and_a_failure_prompts_and_alarms() {
    let halted_failing = Snapshot { halted: true, ..failing() };
    let mut r = rig(1, Arc::new(Mutex::new(halted_failing)));
    r.advance(4 * CADENCE_MS).unwrap();
    acc::assert_healthy(&r, STREAM);
    assert!(asked(&r, ALARM).is_empty() && asked(&r, PROMPT).is_empty(), "an alarm or prompt while halted is refused");
    let w = Arc::new(Mutex::new(nominal()));
    let mut r = rig(1, w.clone());
    assert_eq!(asked(&r, PROMPT).len(), 1, "nominal → prompt");
    *w.lock().unwrap() = failing();
    r.advance(4 * CADENCE_MS).unwrap();
    assert!(asked(&r, PROMPT).len() >= 2, "not-nominal → prompt");
    assert_eq!(asked(&r, ALARM).len(), 1, "not-nominal → one alarm per escalation");
}

#[test_id(tgpwbs0)]
#[test]
fn t4_every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("d/watch_tower.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(!real.contains(&"watch.tower.bogus") && !sim.contains(&"watch.tower.bogus"));
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

/// The nominal prompt line (`sweep/mod.rs:51`) and the failed-unit alarm (`sweep/escalate.rs:76-86`).
#[test_id(t067e0h)]
#[test]
fn t5_the_prompt_and_the_alarm_are_the_ones_watchtower_wrote() {
    let r = rig(1, Arc::new(Mutex::new(nominal())));
    assert_eq!(asked(&r, PROMPT).iter().map(|p| p.contains("SWEEP:NOMINAL lapsed=0 snap_age=12s nv_worst=0")).collect::<Vec<_>>(), [true]);
    let mut r = rig(1, Arc::new(Mutex::new(failing())));
    r.advance(4 * CADENCE_MS).unwrap();
    acc::assert_healthy(&r, STREAM);
    let a = asked(&r, ALARM);
    assert!(a.len() == 1 && a[0].contains("FAILED UNIT: spira-gate-prod.service failing for") && a[0].contains("incident:failed-unit-spira-gate-prod.service"), "{a:?}");
}

fn failing_past_the_warn_window() -> TaskRig {
    let mut r = rig(1, Arc::new(Mutex::new(failing())));
    r.advance(4 * CADENCE_MS).unwrap();
    r
}

fn nominal_prompt_written() -> TaskRig {
    rig(1, Arc::new(Mutex::new(nominal())))
}

#[test_id(tfa6lmc)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&failing_past_the_warn_window(), NAME);
    assert!(rows.iter().any(|k| k.starts_with("failed_units/")) && rows.iter().any(|k| k.starts_with("alarmed/")) && rows.iter().any(|k| k == "escalated"), "{rows:?}");
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&nominal_prompt_written(), NAME);
    assert!(rows.iter().any(|k| k == "prompt"), "{rows:?}");
}

#[test_id(t49u61q)]
#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(failing_past_the_warn_window, || WatchTower::new(config()).registration(), STREAM, WatchTower::boot().2);
}

#[test_id(t94cf26)]
#[test]
fn a_boot_does_not_rewrite_the_nominal_prompt_it_already_wrote() {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(nominal_prompt_written, || WatchTower::new(config()).registration(), STREAM, WatchTower::boot().2);
}

#[test_id(tj6n7f8)]
#[test]
fn two_failures_of_different_invocations_half_an_hour_apart_raise_no_failed_unit_alarm() {
    let on = |inv: &str| Snapshot { failed_invocations: [("spira-gate-prod.service".to_string(), inv.to_string())].into(), ..failing() };
    let w = Arc::new(Mutex::new(on("i1")));
    let mut r = rig(1, w.clone());
    *w.lock().unwrap() = on("i2");
    r.advance(CADENCE_MS).unwrap();
    acc::assert_healthy(&r, STREAM);
    let a = asked(&r, ALARM);
    assert!(a.is_empty(), "{a:?}");
}

#[test_id(t6oj3o3)]
#[test]
fn a_carry_from_before_invocations_loads_and_restarts_the_unit_age() {
    let old: Carry = serde_json::from_str(r#"{"failed_units":{"spira-gate-prod.service":1000}}"#).unwrap();
    assert_eq!(old.failed_units["spira-gate-prod.service"], FailedRow { first_seen: 1000, invocation: String::new() });
    let snap = Snapshot { failed_invocations: [("spira-gate-prod.service".to_string(), "i1".to_string())].into(), ..failing() };
    let (next, acts) = decide(&old, &snap, 1000 * 1000 + 40 * 60_000, &config());
    assert!(acts.iter().all(|a| a.name != ALARM));
    assert_eq!(next.failed_units["spira-gate-prod.service"].first_seen, 1000 + 40 * 60);
}
