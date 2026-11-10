use std::sync::{Arc, Mutex};

use runtime::event::EventKind;
use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::watchers::inbox_keeper::*;
use spira_runtime::watchers::CONCIERGE_WAKE;

const START_MS: u64 = 10_000_000;

fn registration() -> runtime::Registration {
    InboxKeeper { interval_ms: 60_000, stall_secs: 600, backoff_secs: 3600 }.registration()
}

fn waiting() -> Snapshot {
    Snapshot { triage_alive: false, inbox_mtime: u64::MAX / 4 }
}

fn rig_alive_at_start() -> (TaskRig, Arc<Mutex<Snapshot>>) {
    let world = Arc::new(Mutex::new(Snapshot { triage_alive: true, inbox_mtime: 0 }));
    let seen = world.clone();
    let mut r = TaskRig::new(1, registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*seen.lock().unwrap()).unwrap())).double(CONCIERGE_WAKE, |l| Ok(l.into()));
    r.advance(START_MS).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    (r, world)
}

fn monitor_gone_and_the_stall_window_just_passed() -> TaskRig {
    let (mut r, world) = rig_alive_at_start();
    *world.lock().unwrap() = waiting();
    r.advance(540_000).unwrap();
    r.clock.advance(70_000);
    r
}

fn monitor_gone_and_a_wake_just_sent() -> TaskRig {
    let (mut r, world) = rig_alive_at_start();
    *world.lock().unwrap() = waiting();
    r.advance(600_000).unwrap();
    assert!(r.requested().iter().any(|q| q.name == CONCIERGE_WAKE), "the scenario must have woken the pane");
    r
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&monitor_gone_and_a_wake_just_sent(), NAME);
    for k in ["last_seen", "last_wake", "persisted_seen"] {
        assert!(rows.iter().any(|r| r == k), "{k} missing from {rows:?}");
    }
}

#[test]
fn a_boot_wakes_the_pane_when_the_stall_began_before_it() {
    kit::assert_boot_matches_an_unstopped_run(monitor_gone_and_the_stall_window_just_passed, registration, STREAM, EventKind::new(TICK, ""));
}

#[test]
fn a_boot_does_not_wake_the_pane_again_inside_the_backoff() {
    kit::assert_boot_matches_an_unstopped_run(monitor_gone_and_a_wake_just_sent, registration, STREAM, EventKind::new(TICK, ""));
}
