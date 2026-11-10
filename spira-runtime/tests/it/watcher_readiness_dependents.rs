use runtime::event::EventKind;
use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::watchers::readiness_dependents::*;

fn registration() -> runtime::Registration {
    ReadinessDependents { interval_ms: 60_000 }.registration()
}

fn through_revision_41() -> TaskRig {
    let mut r = TaskRig::new(1, registration()).unwrap().double(RUN, |_| Ok(r#"{"through":41,"changed":3}"#.into()));
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    r.clock.advance(1);
    r
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&through_revision_41(), NAME);
    assert!(rows.iter().any(|k| k == "through"), "{rows:?}");
}

#[test]
fn a_boot_asks_for_the_changes_after_the_revision_it_had_reached() {
    kit::assert_boot_matches_an_unstopped_run(through_revision_41, registration, STREAM, EventKind::new(TICK, ""));
}
