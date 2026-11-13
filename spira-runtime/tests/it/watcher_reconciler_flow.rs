use std::sync::{Arc, Mutex};

use runtime::event::EventKind;
use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::watchers::reconciler_flow::*;

fn registration() -> runtime::Registration {
    ReconcilerFlow { interval_ms: 1_800_000, window_hours: 0.5, baseline_hours: 24.0, grace_secs: 1800, unobservable_grace_secs: 3600, rework_window_hours: 6.0, sentinel_timer_secs: 900, shadow_kinds: vec![] }.registration()
}

fn backlog_grown_at(now: u64) -> FlowSnapshot {
    let slots = Slots { live: 3, ceiling: 3, ready: 0, capacity_paused: false };
    FlowSnapshot {
        now,
        halted: false,
        export: Ok(()),
        velocity_floor: None,
        dwell_limit: None,
        backlog: Ok(500),
        baseline: Ok(10.0),
        stage: Ok(Stage { waiting: 0, velocity: (1.0, 1.0), dwell: (10.0, 5, 10.0, 50), round_health: (0, 4, 0, 40), rework: (0, 5, 30.0), regression: vec![] }),
        slots: Ok((slots, slots)),
        sentinel: Ok(10),
    }
}

/// Passes at each of `passes`, then a world whose clock reads `next`.
fn passes_then(passes: &[u64], next: u64) -> TaskRig {
    let world = Arc::new(Mutex::new(backlog_grown_at(0)));
    let seen = world.clone();
    let mut r = TaskRig::new(1, registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*seen.lock().unwrap()).unwrap())).double(MAIL, |m| Ok(m.into())).double(STATUS, |s| Ok(s.into()));
    for now in passes {
        *world.lock().unwrap() = backlog_grown_at(*now);
        r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    }
    *world.lock().unwrap() = backlog_grown_at(next);
    r.clock.advance(1);
    r
}

fn gap_inside_its_grace() -> TaskRig {
    passes_then(&[1000], 2900)
}

fn gap_already_alerted() -> TaskRig {
    passes_then(&[1000, 2900], 4700)
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&passes_then(&[1000, 2900], 4700), NAME);
    for k in ["state/flow:backlog", "alerted/flow:backlog"] {
        assert!(rows.iter().any(|r| r == k), "{k} missing from {rows:?}");
    }
}

#[test]
fn a_boot_alerts_a_gap_whose_grace_began_before_it() {
    kit::assert_boot_matches_an_unstopped_run(gap_inside_its_grace, registration, STREAM, EventKind::new(TICK, ""));
}

#[test]
fn a_boot_does_not_alert_a_gap_that_was_already_alerted() {
    kit::assert_boot_matches_an_unstopped_run(gap_already_alerted, registration, STREAM, EventKind::new(TICK, ""));
}
