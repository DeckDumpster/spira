//! T1–T6 for `cert.full` (contract `wiki/projects/spira/designs/tasks/cert-full.md`), a P4 tick
//! driving a P2 machine. The behaviour tests are red until the task is ported: the stub's
//! `decide` panics, the rig quarantines the stream, and `assert_healthy` turns that into the
//! failure.

use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_deterministic, assert_journal_budget, assert_silent_tick, effects_runnable, TaskRig};
use runtime::units::{Exit, UnitHandle, UnitRan, UnitSpec, UNIT_START, UNIT_STOP};
use runtime::EventKind;
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::d::cert_full::*;
use spira_runtime::tasks::d::pattern::transition;

/// 2026-11-03 00:00 UTC.
const DAY0: u64 = 1_793_664_000_000;
const MIN: u64 = 60_000;
const C1: &str = "1111111111111111111111111111111111111111";
const C2: &str = "2222222222222222222222222222222222222222";

fn config() -> Config {
    Config { run: "/r".into(), metrics_endpoint: "127.0.0.1:9".into() }
}

fn row(round: &str, commit: &str, tree: &str, verdict: &str) -> HistoryRow {
    HistoryRow { round: round.into(), commit: commit.into(), tree: tree.into(), suite: "test-a.sh".into(), verdict: verdict.into() }
}

/// The tip's tree already has an all-green history.
fn certified() -> Snapshot {
    Snapshot { tip: C1.into(), tree: "t1".into(), round: "r1".into(), suites: vec!["test-a.sh".into()], history: vec![row("r1", C1, "t1", "pass")], same_tree: [(C1.to_string(), true)].into(), leases: Some(vec![]), results: None }
}

/// A new tip, green at r1 before it; `results` is what its leg reports.
fn new_tip(results: Option<Vec<HistoryRow>>) -> Snapshot {
    Snapshot { tip: C2.into(), tree: "t2".into(), round: "r2".into(), same_tree: [(C1.to_string(), false)].into(), results, ..certified() }
}

fn rig(seed: u64, world: Arc<Mutex<Snapshot>>) -> TaskRig {
    let (_, stream, kind) = CertFull::boot();
    let mut r = TaskRig::new(seed, CertFull::new(config()).registration())
        .unwrap()
        .double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap()))
        .double(CHECKOUT, |_| Ok("{}".into()))
        .double(UNIT_START, |i| {
            let spec: UnitSpec = serde_json::from_str(i).map_err(|e| e.to_string())?;
            Ok(serde_json::to_string(&UnitHandle::of(&spec.task, &spec.key).map_err(|e| e.to_string())?).unwrap())
        })
        .double(UNIT_STOP, |_| Ok("{}".into()))
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
    UnitRan { handle: UnitHandle::of(NAME, &spec.key).unwrap(), exit: Exit::Code(0), duration_ms: 20 * MIN }.event_kind()
}

fn refused(r: &TaskRig) -> Vec<String> {
    r.journaled().iter().filter(|e| e.kind.tag == REFUSED).map(|e| e.kind.body.clone()).collect()
}

#[test]
fn t1_the_row_is_registered_and_the_calendar_fires_next_at_seven_past() {
    let t = acc::row(NAME);
    assert_eq!((t.stream(), t.effects, t.keys.concat()), (STREAM, EFFECTS, KEYS.to_vec()));
    assert_eq!(next_tick(DAY0 + 10 * 60 * MIN + 20 * MIN), DAY0 + 11 * 60 * MIN + 7 * MIN, "the next fire after 10:20 is 11:07");
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
fn t2_an_already_certified_tree_journals_nothing() {
    let mut r = rig(1, Arc::new(Mutex::new(certified())));
    assert_silent_tick(&mut r, STREAM);
    acc::assert_healthy(&r, STREAM);
}

#[test]
fn t3_the_machine_records_each_row_of_its_table_and_refuses_a_run_in_idle() {
    let busy = Snapshot { leases: Some(vec!["round-9".into()]), ..new_tip(None) };
    let w = Arc::new(Mutex::new(busy));
    let mut r = rig(1, w.clone());
    assert_eq!(carry(&r).phase, Phase::WaitRound, "Idle → WaitRound when the tree is not certified");
    assert!(asked(&r, UNIT_START).is_empty(), "no leg while a round-VM lease is held");
    *w.lock().unwrap() = new_tip(Some(vec![row("r2", C2, "t2", "pass")]));
    r.advance(MIN).unwrap();
    assert_eq!(carry(&r).phase, Phase::Running, "WaitRound → Running when the leases are empty");
    assert_eq!((asked(&r, CHECKOUT).len(), asked(&r, UNIT_START).len()), (1, 1));
    r.tick(STREAM, ran(&r)).unwrap();
    acc::assert_healthy(&r, STREAM);
    assert_ne!(carry(&r).phase, Phase::Running, "Running → Collect on unit.ran");
    let mut idle = rig(1, Arc::new(Mutex::new(certified())));
    let stray = UnitRan { handle: UnitHandle::of(NAME, "cert.full:x:1").unwrap(), exit: Exit::Code(0), duration_ms: 1 }.event_kind();
    idle.tick(STREAM, stray).unwrap();
    assert!(refused(&idle).iter().any(|w| w.contains("Idle")), "unit.ran in Idle is refused naming the state: {:?}", refused(&idle));
    assert_eq!(transition(TRANSITIONS, NAME, Phase::Idle, "unit.ran").map_err(|e| e.contains("Idle")), Err(true));
}

#[test]
fn t4_every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("d/cert_full.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(!real.contains(&"cert.full.bogus") && !sim.contains(&"cert.full.bogus"));
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

/// `Event::render` (`cert-sweep/src/lib.rs:180-201`): a new red against the last green round, and
/// a leg that left no results.
#[test]
fn t5_the_event_lines_are_the_ones_cert_sweep_recorded() {
    let mut r = rig(1, Arc::new(Mutex::new(new_tip(Some(vec![row("r2", C2, "t2", "fail")])))));
    // Every leg the red adds (three reruns, then the probe at the last green round) ends with
    // its own unit.ran, as the D-Bus source reports it; the rig's unit.start double ends none.
    let mut legs = 0;
    while asked(&r, UNIT_START).len() > legs && legs < 10 {
        legs = asked(&r, UNIT_START).len();
        r.tick(STREAM, ran(&r)).unwrap();
        r.advance(MIN).unwrap();
    }
    acc::assert_healthy(&r, STREAM);
    assert_eq!(legs, 5, "the pass, three reruns, and the probe at the last green round");
    let all: Vec<String> = r.requested().iter().map(|q| q.input.clone()).collect();
    assert!(all.iter().any(|i| i.contains("NEW RED test-a.sh: last green round r1 (111111111), first red round r2 (222222222)")), "{all:?}");
    let mut r = rig(1, Arc::new(Mutex::new(new_tip(Some(vec![])))));
    r.tick(STREAM, ran(&r)).unwrap();
    acc::assert_healthy(&r, STREAM);
    assert!(asked(&r, RECORD).iter().any(|i| i.contains("SWEEP FAULT mode=full round=r2:")), "{:?}", asked(&r, RECORD));
}

#[test]
fn t6_two_days_replay_identically_inside_the_budget() {
    let r = assert_deterministic(|seed| rig(seed, Arc::new(Mutex::new(certified()))), 7, 2);
    acc::assert_healthy(&r, STREAM);
    assert_journal_budget(&r, 1);
}
