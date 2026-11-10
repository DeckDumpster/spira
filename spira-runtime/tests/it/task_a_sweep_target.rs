//! Acceptance T1–T6 for `sweep.target`, instantiated from its contract
//! (`wiki/projects/spira/designs/tasks/sweep-target.md`, Acceptance) on `TaskRig`. Red until the
//! port lands: `decide` is `unimplemented!`, and the row waits in `tasks::a::PENDING`. T6 runs two
//! virtual days (the contract asks seven, with a seeded fill curve); its idle budget is the
//! contract's "no journaled events while above the floor".

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use runtime::event::EventKind;

spira_runtime::family_a_acceptance! {
    module: sweep_target,
    registration: SweepTarget::new(BTreeMap::new()).registration(),
    tick: EventKind::new(TICK, ""),
    first_timer_ms: CADENCE_MS,
}

const GIB: u64 = 1 << 30;

fn target(id: &str, idle_secs: u64, busy: bool, gib: u64) -> Candidate {
    Candidate { id: id.into(), target: format!("/run/worktree/{id}/target"), idle_secs, busy, bytes: gib * GIB }
}

/// The contract's idle fixture: free space at the floor and an empty foreign-cache.
fn idle() -> Snapshot {
    Snapshot { free_bytes: Some(50 * GIB), floor_bytes: 50 * GIB, idle_secs_min: 21_600, candidates: vec![target("old", 90_000, false, 8)], foreign_stale: vec![] }
}

/// 20 GiB short of the floor, with idle, young and busy targets.
fn busy() -> Snapshot {
    Snapshot {
        free_bytes: Some(30 * GIB),
        candidates: vec![target("young", 600, false, 30), target("older", 90_000, false, 12), target("busy", 99_000, true, 30), target("oldest", 95_000, false, 10), target("spare", 50_000, false, 10)],
        ..idle()
    }
}

fn removed(acts: &[Act]) -> Vec<&str> {
    acts.iter().filter(|a| a.effect == REMOVE_TARGET).map(|a| a.key.as_str()).collect()
}

/// A recorded removal is not replayed: once the run is gone from the world it is asked for once.
#[test]
fn t3_a_recorded_removal_is_not_replayed() {
    let stale = ForeignRun { path: "/run/foreign-cache/spira/runs/r1".into(), age_secs: 7_200, busy: false, bytes: GIB };
    let world = Arc::new(Mutex::new(Snapshot { foreign_stale: vec![stale], ..idle() }));
    let (seen, gone) = (world.clone(), world.clone());
    let mut r = TaskRig::new(1, registration())
        .unwrap()
        .double(OBSERVE, move |_| Ok(serde_json::to_string(&*seen.lock().unwrap()).unwrap()))
        .double(REMOVE_TARGET, |_| Ok("0".into()))
        .double(REMOVE_FOREIGN_RUN, move |_| {
            gone.lock().unwrap().foreign_stale.clear();
            Ok(GIB.to_string())
        });
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    r.advance(5 * CADENCE_MS).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    let asked: Vec<_> = r.requested().iter().filter(|q| q.name == REMOVE_FOREIGN_RUN).collect();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(asked[0].journaled, "the removal is recorded");
}

/// `reap.rs:165-188` (`reap_idle`): below the floor, idle non-busy targets go oldest first until
/// the deficit is covered; the young and the busy stay. One target of overshoot is allowed.
#[test]
fn t5_below_the_floor_idle_targets_go_oldest_first() {
    let (_, acts) = decide(&Carry::default(), &busy());
    assert_eq!(removed(&acts).len(), 2, "{acts:?}");
    assert!(acts[0].input.contains("oldest") && acts[1].input.contains("older"), "{acts:?}");
    assert!(acts.iter().all(|a| !a.input.contains("young") && !a.input.contains("/busy/")), "{acts:?}");
}

/// `target_reap_main.rs:87-88`: at or above the floor no target is touched.
#[test]
fn t5_above_the_floor_no_target_is_touched() {
    assert_eq!(removed(&decide(&Carry::default(), &idle()).1), Vec::<&str>::new());
}

/// `foreign.rs:275`: stale, unbusy foreign runs go every tick; a busy or young one stays.
#[test]
fn t5_only_stale_unbusy_foreign_runs_go() {
    let runs = vec![
        ForeignRun { path: "/f/stale".into(), age_secs: 3_600, busy: false, bytes: 1 },
        ForeignRun { path: "/f/busy".into(), age_secs: 9_000, busy: true, bytes: 1 },
        ForeignRun { path: "/f/young".into(), age_secs: 3_599, busy: false, bytes: 1 },
    ];
    let acts = decide(&Carry::default(), &Snapshot { foreign_stale: runs, ..idle() }).1;
    assert_eq!(acts.iter().filter(|a| a.effect == REMOVE_FOREIGN_RUN).map(|a| a.key.as_str()).collect::<Vec<_>>(), ["/f/stale"]);
}

/// `target_reap_main.rs:89-92`: unreadable free space reclaims nothing.
#[test]
fn t5_unreadable_free_space_reclaims_nothing() {
    assert_eq!(removed(&decide(&Carry::default(), &Snapshot { free_bytes: None, ..busy() }).1), Vec::<&str>::new());
}

fn at_work() -> TaskRig {
    let mut r = rig(1, busy());
    r.advance(CADENCE_MS).unwrap();
    r
}

#[test]
fn nothing_is_kept_because_every_tick_re_observes() {
    let r = at_work();
    assert!(!r.requested().is_empty());
    let kept: Vec<_> = r.journal.state().into_iter().filter(|((t, _), v)| t == NAME && !v.is_empty()).collect();
    assert!(kept.is_empty(), "{kept:?}");
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(at_work, registration, STREAM, EventKind::new(TICK, ""));
}
