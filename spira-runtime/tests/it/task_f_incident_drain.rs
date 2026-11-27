//! `incident.drain` on `TaskRig`: an empty spool asks only for its probe; a spool holding
//! anything asks for one drain; an entry the drain leaves for two cadences is a standing
//! condition.

use std::collections::BTreeMap;
use std::path::PathBuf;

use runtime::taskrig::{assert_silent_tick, effects_runnable, TaskRig};
use spira_runtime::tasks::f::incident_drain::*;
use spira_runtime::tasks::{self, f};

const CADENCE_MS: u64 = 60_000;
const MODULE: &str = include_str!("../../src/tasks/f/incident_drain.rs");
const LAUNCHERS: &[&str] = &["Command::new", "std::process", "process::Command", "libc::fork", "libc::exec", "posix_spawn", "nix::unistd"];
/// 2026-01-01T00:00:00Z.
const SPOOLED: i64 = 1_767_225_600;

fn handler() -> IncidentDrain {
    IncidentDrain { interval_ms: CADENCE_MS, config: BTreeMap::new() }
}

fn rig(spool: Result<Pending, String>) -> TaskRig {
    let snap = Snapshot { spool };
    let mut r = TaskRig::new(1, handler().registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&snap).unwrap())).double(DRAIN, |_| Ok(String::new()));
    let (_, stream, kind) = IncidentDrain::boot();
    r.tick(stream, kind).unwrap();
    r
}

fn drains(r: &TaskRig) -> usize {
    r.requested().iter().filter(|q| q.name == DRAIN).count()
}

fn holding(count: u64) -> Result<Pending, String> {
    Ok(Pending { count, oldest_age_s: 5 })
}

#[test_id(txu6zr5)]
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

#[test_id(tsqtzl8)]
#[test]
fn a_tick_with_an_empty_spool_asks_only_for_its_probe() {
    let mut r = rig(holding(0));
    assert_silent_tick(&mut r, STREAM);
    assert_eq!(drains(&r), 0, "{:?}", r.requested());
}

#[test_id(t48lfcb)]
#[test]
fn a_tick_with_a_spooled_event_asks_for_one_drain() {
    assert_eq!(drains(&rig(holding(970))), 1);
}

#[test_id(txuxwig)]
#[test]
fn an_unreadable_spool_drains_nothing() {
    assert_eq!(decide(&Snapshot { spool: Err("denied".into()) }), Decision::Skip("unreadable".into()));
    assert_eq!(drains(&rig(Err("denied".into()))), 0);
}

#[test_id(tgynsxc)]
#[test]
fn the_probe_counts_the_spool_and_ages_its_oldest_entry() {
    let dir = testkit::TempDir::new("incident-drain-probe");
    assert_eq!(pending(&dir.join("never-made"), SPOOLED), Ok(Pending::default()), "no spool yet is an empty one");
    for name in ["20260101T000100Z-b-1", "20260101T000000Z-a-1", "20260101T000000Z-a-2.bad", ".20260101T000000Z-c-1"] {
        std::fs::write(dir.join(name), "REF: r\n--\n").unwrap();
    }
    assert_eq!(pending(&dir, SPOOLED + 90), Ok(Pending { count: 2, oldest_age_s: 90 }));
    assert!(pending(&dir.join("20260101T000100Z-b-1"), SPOOLED).is_err(), "a spool that is not a directory cannot be read");
}

fn notes(stuck: &[(PathBuf, String)], now: i64) -> Vec<String> {
    let mut noted = vec![];
    note_stuck(stuck, now, 60, |title| {
        noted.push(title.to_string());
        Ok(())
    })
    .unwrap();
    noted
}

#[test_id(tzgj8k6)]
#[test]
fn an_entry_left_for_more_than_two_cadences_is_noted_with_the_count_and_the_reason() {
    let stuck: Vec<(PathBuf, String)> = vec![("spool/20260101T000100Z-b-1".into(), "later reason".into()), ("spool/20260101T000000Z-a-1".into(), "dedup could not read".into())];
    assert_eq!(notes(&stuck, SPOOLED + 121), ["2 incident event(s) stuck in the spool, oldest 2m: dedup could not read"]);
    assert_eq!(note_stuck(&stuck, SPOOLED + 121, 60, |_| Err("no inbox".into())), Err("no inbox".into()));
}

#[test_id(t54efw7)]
#[test]
fn a_younger_entry_and_an_empty_stuck_list_note_nothing() {
    let stuck: Vec<(PathBuf, String)> = vec![("spool/20260101T000000Z-a-1".into(), "why".into())];
    assert!(notes(&stuck, SPOOLED + 120).is_empty());
    assert!(notes(&[], SPOOLED + 9_999).is_empty());
}

#[test_id(teqrf17)]
#[test]
fn every_effect_has_a_real_runner_and_a_sim_double() {
    let row = f::TASKS.iter().find(|t| t.name == NAME).expect("the row is ported");
    assert_eq!(row.effects, EFFECTS);
    let real: Vec<&str> = tasks::routes().keys().copied().collect();
    let doubles = sim::doubles::service().names();
    let sim: Vec<&str> = doubles.iter().map(String::as_str).collect();
    assert!(effects_runnable(&["incident.drain.bogus"], &real, &sim).is_err());
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

#[test_id(tfu2ifq)]
#[test]
fn the_module_launches_no_process() {
    let launches = |src: &str| LAUNCHERS.iter().any(|l| src.contains(l));
    assert!(launches("let c = Command::new(tool);"), "the matcher finds a planted launcher");
    assert!(!launches(MODULE));
}
