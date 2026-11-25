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

#[test_id(txtktl5)]
#[test]
fn t1_registered_boots_and_arms_a_2min_tick() {
    t::t1_registered_and_boots::<H>(SET, TICK);
}

#[test_id(t62cg2h)]
#[test]
fn t2_nothing_firing_with_a_reachable_store_writes_nothing() {
    t::t2_silent::<H>(SET, &json(&quiet(0)));
}

#[test_id(t12339t)]
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

#[test_id(tvghezk)]
#[test]
fn t4_its_effects_are_runnable_and_the_module_forks_neither_date_nor_systemctl() {
    let sim = sim::doubles::service().names();
    let src = include_str!("../../src/tasks/b/reconcile_heimdall.rs");
    t::t4_runnable::<H>(src, &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test_id(tt85ocp)]
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

#[test_id(tlgtxe5)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rig = t::rig::<H>(1, MIRROR_SET, &mirror_stale());
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&rig, task::NAME);
    assert!(rows.iter().any(|k| k == "keys/mirror-stale"), "{rows:?}");
}

#[test_id(t7gqe82)]
#[test]
fn a_boot_neither_forgets_a_sighting_nor_raises_without_its_second() {
    t::assert_boot_matches_an_unstopped_run::<H>(MIRROR_SET, vec![mirror_stale()], |_| {});
}

#[test_id(tq6rbfp)]
#[test]
fn the_sentinel_is_found_by_its_real_task_name_in_the_scheduler_status() {
    let doc = serde_json::json!({"tasks": [{"name": "sentinel-pass", "next_due_at": null, "last_fired_at": 1_000_000u64, "last_outcome": "succeeded"}]});
    assert_eq!(task::sentinel_status(Some(&doc)), TaskStatus { armed: false, last_fired_ms: Some(1_000_000), effect_failures: 0 });
    assert_eq!(task::sentinel_status(None), TaskStatus { armed: true, last_fired_ms: None, effect_failures: 0 });
}

struct Conditions;

impl spira_store::lc_call::Transport for Conditions {
    fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
        let out = if argv.first().map(String::as_str) == Some("condition") && argv.get(1).map(String::as_str) == Some("list") { "{\"conditions\":[]}" } else { "" };
        Ok(spira_store::lc_call::Reply { code: 0, out: out.into(), err: String::new(), read_ended: false })
    }
}

const STILL_SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "120"), ("SPIRA_HEIMDALL_CONFIRM", "2"), ("SPIRA_HEIMDALL_CLEAR", "1"), ("SPIRA_HEIMDALL_REFRESH", "3600"), ("SPIRA_HEIMDALL_PASS_STALE", "900"), ("SPIRA_HEIMDALL_STILL_SECS", "180")];
const NOW_MS: u64 = 10_000_000_000;

/// A builder `sp-a` whose lease runs `lease_ahead` seconds past now, with a still file whose
/// transcript was last written `quiet` seconds ago.
fn still_builder(run: &std::path::Path, lease_ahead: i64, quiet: i64) {
    let now = (NOW_MS / 1000) as i64;
    std::fs::create_dir_all(run.join("dwarf")).unwrap();
    let pidfile = run.join("dwarf-builder-sp-a.pid");
    std::fs::write(&pidfile, "1\n").unwrap();
    std::fs::write(hel::reap::lease_file(&pidfile), (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 + lease_ahead).to_string()).unwrap();
    std::fs::write(run.join("dwarf/sp-a.still"), serde_json::json!({ "since": now - quiet, "transcript": now - quiet, "last": "cargo test" }).to_string()).unwrap();
}

fn observed(run: &std::path::Path) -> Snapshot {
    let lc = spira_store::lc_call::LcClient::over(std::sync::Arc::new(Conditions), true);
    let sock = run.join("no-scheduler.sock");
    serde_json::from_str(&task::observe(&lc, run, sock.to_str().unwrap(), NOW_MS).unwrap()).unwrap()
}

/// The raises of the second pass that sees `snap`, which `SPIRA_HEIMDALL_CONFIRM` makes the first to confirm.
fn raises(snap: Snapshot) -> Vec<String> {
    let h = t::handler::<H>(STILL_SET);
    let (carry, _) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(snap.clone()), NOW_MS).unwrap();
    let (_, acts) = task::decide(&h.cfg, &carry, &Input::Observed(snap), NOW_MS + TICK).unwrap();
    acts.iter().filter(|a| a.name == task::RAISE).map(|a| a.key.clone()).collect()
}

#[test_id(tm4kq7z)]
#[test]
fn a_still_file_older_than_the_threshold_with_a_live_lease_raises_builder_still() {
    let run = testkit::TempDir::new("heimdall-still-old");
    still_builder(run.path(), 600, 200);
    let keys = raises(observed(run.path()));
    assert_eq!(keys, ["heimdall/builder-still:sp-a/raise/1"]);
}

#[test_id(tr9c2vj)]
#[test]
fn a_fresh_still_file_or_an_expired_lease_does_not_raise() {
    let run = testkit::TempDir::new("heimdall-still-fresh");
    still_builder(run.path(), 600, 100);
    assert!(raises(observed(run.path())).is_empty(), "under the threshold");
    still_builder(run.path(), -5, 200);
    assert!(observed(run.path()).stills.is_empty(), "an expired lease's file is not read");
    assert!(raises(observed(run.path())).is_empty());
}

#[test_id(tw1p6bg)]
#[test]
fn removing_the_still_file_clears_the_alert() {
    let run = testkit::TempDir::new("heimdall-still-clear");
    still_builder(run.path(), 600, 200);
    let h = t::handler::<H>(STILL_SET);
    let still = observed(run.path());
    let (carry, _) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(still.clone()), NOW_MS).unwrap();
    let (carry, _) = task::decide(&h.cfg, &carry, &Input::Observed(still), NOW_MS + TICK).unwrap();
    assert_eq!(carry.keys["builder-still:sp-a"].phase, Phase::Firing);
    std::fs::remove_file(run.path().join("dwarf/sp-a.still")).unwrap();
    let (carry, acts) = task::decide(&h.cfg, &carry, &Input::Observed(observed(run.path())), NOW_MS + 2 * TICK).unwrap();
    assert_eq!(names(&acts), [task::CLEAR]);
    assert_eq!(carry.keys["builder-still:sp-a"].phase, Phase::Cleared);
}
