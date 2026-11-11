use runtime::event::EventKind;
use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::watchers::round_duty::*;
use spira_runtime::watchers::INBOX_APPEND;

fn registration() -> runtime::Registration {
    RoundDuty { interval_ms: 15_000, min: 1, max_open: 1, remind_secs: 600, idle_cut_secs: 900, lease_ttl_secs: 300, halt_page_secs: 3_600 }.registration()
}

fn certified_work_and_no_round() -> Snapshot {
    Snapshot { certified: 1, by_repo: "spira=1".into(), state: RoundState::None, lines: [("300".to_string(), 5)].into(), ..Snapshot::default() }
}

fn round_due_said() -> TaskRig {
    let world = serde_json::to_string(&certified_work_and_no_round()).unwrap();
    let mut r = TaskRig::new(1, registration()).unwrap().double(OBSERVE, move |_| Ok(world.clone())).double(INBOX_APPEND, |l| Ok(l.into()));
    r.advance(1_000_000).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    assert!(r.requested().iter().any(|q| q.name == INBOX_APPEND && q.input.contains("ROUND DUE")), "the scenario must have said ROUND DUE");
    r.clock.advance(1);
    r
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&round_due_said(), NAME);
    for k in ["last_due", "last_c", "last_change", "lines/300"] {
        assert!(rows.iter().any(|r| r == k), "{k} missing from {rows:?}");
    }
}

#[test]
fn a_boot_does_not_call_a_round_due_again_inside_the_reminder_window() {
    kit::assert_boot_matches_an_unstopped_run(round_due_said, registration, STREAM, EventKind::new(TICK, ""));
}
