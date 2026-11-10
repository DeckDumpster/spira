//! T1–T6 for `reconcile.heimdall`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/reconcile-heimdall.md`, scoped to the in-scheduler checks
//! (the outside watch is the watchdog, S15). Red until the task is ported.

use std::collections::BTreeMap;

use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use spira_runtime::tasks::b::reconcile_heimdall::{self as task, Carry, KeyState, Phase, ReconcileHeimdall as H, Snapshot, TaskStatus};

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "120"), ("SPIRA_HEIMDALL_CONFIRM", "2"), ("SPIRA_HEIMDALL_CLEAR", "2"), ("SPIRA_HEIMDALL_REFRESH", "3600"), ("SPIRA_HEIMDALL_PASS_STALE", "900")];
const TICK: u64 = 120_000;
const KEY: &str = "sentinel-stale";

/// Nothing firing, a reachable store, the sentinel armed and fired a minute ago.
fn quiet(now: u64) -> Snapshot {
    Snapshot { sentinel: TaskStatus { armed: true, last_fired_ms: Some(now.saturating_sub(60_000)), effect_failures: 0 }, db_reachable: true, ..Snapshot::default() }
}

/// The sentinel has not fired for an hour.
fn stalled(now: u64) -> Snapshot {
    Snapshot { sentinel: TaskStatus { armed: true, last_fired_ms: Some(now.saturating_sub(3_600_000)), effect_failures: 0 }, ..quiet(now) }
}

fn keyed(phase: Phase, seen: u32) -> Carry {
    Carry { keys: BTreeMap::from([(KEY.to_string(), KeyState { phase, seen, ..KeyState::default() })]), ..Carry::default() }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn t1_registered_boots_and_arms_a_2min_tick() {
    t::t1_registered_and_boots::<H>(SET, TICK);
}

#[test]
fn t2_nothing_firing_with_a_reachable_store_writes_nothing() {
    t::t2_silent::<H>(SET, &json(&quiet(0)));
}

#[test]
fn t3_a_key_confirms_fires_clears_and_a_raise_while_unreachable_is_refused() {
    let h = t::handler::<H>(SET);
    let now = 7_200_000;
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(stalled(now)), now).unwrap();
    assert_eq!((c.keys[KEY].phase, names(&acts)), (Phase::Seen, vec![]), "one sighting is not yet firing");
    let (c, acts) = task::decide(&h.cfg, &c, &Input::Observed(stalled(now + TICK)), now + TICK).unwrap();
    assert_eq!((c.keys[KEY].phase, names(&acts)), (Phase::Firing, vec![task::RAISE]));
    let (c, _) = task::decide(&h.cfg, &c, &Input::Observed(quiet(now + 2 * TICK)), now + 2 * TICK).unwrap();
    assert_eq!(c.keys[KEY].phase, Phase::Unseen);
    let (c, acts) = task::decide(&h.cfg, &c, &Input::Observed(quiet(now + 3 * TICK)), now + 3 * TICK).unwrap();
    assert_eq!((c.keys[KEY].phase, names(&acts)), (Phase::Cleared, vec![task::CLEAR]));
    let refused = task::decide(&h.cfg, &keyed(Phase::Seen, 1), &Input::Observed(Snapshot { db_reachable: false, ..stalled(now) }), now).unwrap_err();
    assert!(refused.contains("unreachable"), "{refused}");
}

#[test]
fn t4_its_effects_are_runnable_and_the_module_forks_neither_date_nor_systemctl() {
    let sim = sim::doubles::service().names();
    let src = include_str!("../../src/tasks/b/reconcile_heimdall.rs");
    t::t4_runnable::<H>(src, &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_a_confirmed_stall_raises_once_and_a_store_outage_writes_nothing() {
    let h = t::handler::<H>(SET);
    let now = 7_200_000;
    let (_, acts) = task::decide(&h.cfg, &keyed(Phase::Seen, 1), &Input::Observed(stalled(now)), now).unwrap();
    assert_eq!(names(&acts), [task::RAISE]);
    let outage = task::decide(&h.cfg, &keyed(Phase::Seen, 1), &Input::Observed(Snapshot { db_reachable: false, ..stalled(now) }), now);
    assert!(matches!(&outage, Err(e) if e.contains("unreachable")), "no store write while unreachable: {outage:?}");
}

const MIRROR_SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "120"), ("SPIRA_HEIMDALL_CONFIRM", "2"), ("SPIRA_HEIMDALL_CLEAR", "2"), ("SPIRA_HEIMDALL_REFRESH", "3600"), ("SPIRA_HEIMDALL_PASS_STALE", "900"), ("SPIRA_HEIMDALL_MIRROR_STALE", "90000")];

fn mirror_stale() -> String {
    json(&Snapshot { db_reachable: true, mirror_age_secs: Some(200_000), ..Snapshot::default() })
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rig = t::rig::<H>(1, MIRROR_SET, &mirror_stale());
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&rig, task::NAME);
    assert!(rows.iter().any(|k| k == "keys/mirror-stale"), "{rows:?}");
}

#[test]
fn a_boot_neither_forgets_a_sighting_nor_raises_without_its_second() {
    t::assert_boot_matches_an_unstopped_run::<H>(MIRROR_SET, vec![mirror_stale()], |_| {});
}
