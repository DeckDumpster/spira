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

#[test_id(thks9oc)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&passes_then(&[1000, 2900], 4700), NAME);
    for k in ["state/flow:backlog", "alerted/flow:backlog"] {
        assert!(rows.iter().any(|r| r == k), "{k} missing from {rows:?}");
    }
}

#[test_id(tlk9lul)]
#[test]
fn a_boot_alerts_a_gap_whose_grace_began_before_it() {
    kit::assert_boot_matches_an_unstopped_run(gap_inside_its_grace, registration, STREAM, EventKind::new(TICK, ""));
}

#[test_id(tvaadph)]
#[test]
fn a_boot_does_not_alert_a_gap_that_was_already_alerted() {
    kit::assert_boot_matches_an_unstopped_run(gap_already_alerted, registration, STREAM, EventKind::new(TICK, ""));
}

#[test_id(tep8rdf)]
#[test]
fn a_full_pool_under_a_roomy_ceiling_is_satisfied_through_handle() {
    use spira_runtime::tasks::a::sentinel_call;
    let slots = Slots { live: 6, ceiling: 40, ready: 2, capacity_paused: false };
    let world = FlowSnapshot { slots: Ok((slots, slots)), ..backlog_grown_at(1000) };
    let mut r = TaskRig::new(1, registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&world).unwrap())).double(MAIL, |m| Ok(m.into())).double(STATUS, |s| Ok(s.into()));
    let called = sentinel_call::Carry { task_limit: Some(sentinel::plan::TaskLimit { name: "pool".into(), limit: 6, held: 6, holders: vec![] }), ..Default::default() };
    r.tick(sentinel_call::STREAM, EventKind::new(sentinel_call::STATE, &serde_json::to_string(&called).unwrap())).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    let carry: Carry = serde_json::from_str(&r.engine().carry_body(NAME, STATE).expect("the watcher carried its verdicts")).unwrap();
    assert_eq!(carry.words["flow:idle-capacity"], "satisfied");
}

#[test_id(tt5ii53)]
#[test]
fn room_under_the_pool_mails_through_handle_and_no_carry_is_unobservable() {
    use spira_runtime::tasks::a::sentinel_call;
    let slots = Slots { live: 4, ceiling: 40, ready: 2, capacity_paused: false };
    let world = FlowSnapshot { slots: Ok((slots, slots)), ..backlog_grown_at(1000) };
    let rig = |limit: Option<i64>| {
        let world = world.clone();
        let mut r = TaskRig::new(1, ReconcilerFlow { grace_secs: 0, ..ReconcilerFlow { interval_ms: 1_800_000, window_hours: 0.5, baseline_hours: 24.0, grace_secs: 0, unobservable_grace_secs: 3600, rework_window_hours: 6.0, sentinel_timer_secs: 900, shadow_kinds: vec![] } }.registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&world).unwrap())).double(MAIL, |m| Ok(m.into())).double(STATUS, |s| Ok(s.into()));
        if let Some(held) = limit {
            let called = sentinel_call::Carry { task_limit: Some(sentinel::plan::TaskLimit { name: "pool".into(), limit: 6, held, holders: vec![] }), ..Default::default() };
            r.tick(sentinel_call::STREAM, EventKind::new(sentinel_call::STATE, &serde_json::to_string(&called).unwrap())).unwrap();
        }
        r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
        r
    };
    let word = |r: &TaskRig| serde_json::from_str::<Carry>(&r.engine().carry_body(NAME, STATE).unwrap()).unwrap().words["flow:idle-capacity"].clone();
    let with_room = rig(Some(4));
    assert_eq!(word(&with_room), "gap");
    assert!(with_room.requested().iter().any(|q| q.input.contains("pool 6") && q.input.contains("live 4")), "{:?}", with_room.requested());
    assert_eq!(word(&rig(None)), "unobservable");
}
