//! T1–T6 for `cert.sample` (contract `wiki/projects/spira/designs/tasks/cert-sample.md`), a P4
//! tick driving a P2 machine. The behaviour tests are red until the task is ported: the stub's
//! `decide` panics, the rig quarantines the stream, and `assert_healthy` turns that into the
//! failure.

use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_deterministic, assert_journal_budget, assert_silent_tick, effects_runnable, TaskRig};
use runtime::units::{Exit, UnitHandle, UnitRan, UnitSpec, UNIT_START};
use runtime::EventKind;
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::d::cert_full::HistoryRow;
use spira_runtime::tasks::d::cert_sample::*;
use spira_runtime::tasks::d::pattern::transition;

/// 2026-11-03 00:00 UTC.
const DAY0: u64 = 1_793_664_000_000;
const MIN: u64 = 60_000;
const C1: &str = "1111111111111111111111111111111111111111";

fn config() -> Config {
    Config { run: "/r".into() }
}

fn suites(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("test-{i:02}.sh")).collect()
}

fn tip(n: usize, results: Option<Vec<HistoryRow>>) -> Snapshot {
    Snapshot { tip: C1.into(), round: "r1".into(), suites: suites(n), history: vec![], results }
}

fn green(suite: &str) -> HistoryRow {
    HistoryRow { round: "r1".into(), commit: C1.into(), tree: "t1".into(), suite: suite.into(), verdict: "pass".into() }
}

fn rig(seed: u64, world: Arc<Mutex<Snapshot>>) -> TaskRig {
    let (_, stream, kind) = CertSample::boot();
    let mut r = TaskRig::new(seed, CertSample::new(config()).registration())
        .unwrap()
        .double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap()))
        .double(BRANCH, |_| Ok("{}".into()))
        .double(UNIT_START, |i| {
            let spec: UnitSpec = serde_json::from_str(i).map_err(|e| e.to_string())?;
            Ok(serde_json::to_string(&UnitHandle::of(&spec.task, &spec.key).map_err(|e| e.to_string())?).unwrap())
        })
        .double(FILE_BEAD, |_| Ok("sp-red1".into()))
        .double(RECORD, |l| Ok(l.into()));
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

fn ran(r: &TaskRig) -> EventKind {
    let spec: UnitSpec = serde_json::from_str(asked(r, UNIT_START).last().expect("a unit.start")).unwrap();
    UnitRan { handle: UnitHandle::of(NAME, &spec.key).unwrap(), exit: Exit::Code(0), duration_ms: 10 * MIN }.event_kind()
}

#[test]
fn t1_the_row_is_registered_and_the_calendar_fires_next_at_thirty_seven_past() {
    let t = acc::row(NAME);
    assert_eq!((t.stream(), t.effects, t.keys.concat()), (STREAM, EFFECTS, KEYS.to_vec()));
    assert_eq!(next_tick(DAY0 + 10 * 60 * MIN + 10 * MIN), DAY0 + 10 * 60 * MIN + 37 * MIN, "the next fire after 10:10 is 10:37");
    let s = acc::pinned_settings(&[]).expect("settings from the complete fixture");
    assert_eq!(Config::pinned().map(|_| ()), Ok(()), "every key resolves through the config door");
    let mut r = TaskRig::new(1, (t.build)(&s)).unwrap();
    let (_, stream, kind) = (t.boot)();
    r.tick(stream, kind).unwrap();
    assert_eq!(r.engine().next_timer_at(), Some(next_tick(0)));
    assert_eq!((t.cadence_ms)(&s), next_tick(0));
    assert_eq!(r.requested().iter().map(|q| q.name.as_str()).collect::<Vec<_>>(), [OBSERVE]);
}

#[test]
fn t2_a_tip_with_no_suites_journals_nothing() {
    let mut r = rig(1, Arc::new(Mutex::new(tip(0, None))));
    assert_silent_tick(&mut r, STREAM);
    acc::assert_healthy(&r, STREAM);
}

#[test]
fn t3_a_tick_starts_one_leg_its_end_records_and_a_run_in_idle_is_refused() {
    let mut r = rig(1, Arc::new(Mutex::new(tip(8, None))));
    assert_eq!(carry(&r).phase, Phase::Running, "Idle → Running on a tick with suites");
    assert_eq!((asked(&r, BRANCH).len(), asked(&r, UNIT_START).len()), (1, 1));
    assert_eq!(carry(&r).picks.len(), 8 / SUBSET_DIV, "a quarter of the suites");
    r.tick(STREAM, ran(&r)).unwrap();
    acc::assert_healthy(&r, STREAM);
    assert_eq!(carry(&r).phase, Phase::Idle, "Running → Idle on a leg with no results");
    assert_eq!(asked(&r, RECORD).len(), 1, "a SweepFault is recorded");
    r.tick(STREAM, ran(&r)).unwrap();
    let refused: Vec<String> = r.journaled().iter().filter(|e| e.kind.tag == REFUSED).map(|e| e.kind.body.clone()).collect();
    assert!(refused.iter().any(|w| w.contains("Idle")), "unit.ran in Idle is refused naming the state: {refused:?}");
    assert_eq!(transition(TRANSITIONS, NAME, Phase::Idle, "unit.ran results").map_err(|e| e.contains("Idle")), Err(true));
}

#[test]
fn t4_every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("d/cert_sample.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(!real.contains(&"cert.sample.bogus") && !sim.contains(&"cert.sample.bogus"));
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

/// The summary (`cert-sweep/src/lib.rs:196-198`) and the same pick for the same seed.
#[test]
fn t5_a_green_sample_records_the_summary_cert_sweep_wrote() {
    let picked = |seed| {
        let r = rig(seed, Arc::new(Mutex::new(tip(8, None))));
        carry(&r).picks
    };
    assert_eq!(picked(3), picked(3), "the same seed draws the same subset");
    let all = suites(8);
    let mut r = rig(3, Arc::new(Mutex::new(tip(8, Some(all.iter().map(|s| green(s)).collect())))));
    r.tick(STREAM, ran(&r)).unwrap();
    acc::assert_healthy(&r, STREAM);
    let rec = asked(&r, RECORD);
    assert!(rec.iter().any(|l| l.contains("SAMPLE round=r1 ran=2 red=0 fault=0 wall=")), "{rec:?}");
}

#[test]
fn t6_two_days_of_ticks_replay_identically_inside_the_budget() {
    let r = assert_deterministic(|seed| rig(seed, Arc::new(Mutex::new(tip(0, None)))), 7, 2);
    acc::assert_healthy(&r, STREAM);
    assert_journal_budget(&r, 6);
}
