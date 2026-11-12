use runtime::event::EventKind;
use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::watchers::readiness_dependents::*;

fn registration() -> runtime::Registration {
    ReadinessDependents { interval_ms: 60_000 }.registration()
}

fn through_revision_41() -> TaskRig {
    let mut r = TaskRig::new(1, registration())
        .unwrap()
        .double(LOOK, |_| Ok(r#"{"through":41,"keys":["sp-a"]}"#.into()))
        .double(RUN, |_| Ok(r#"{"through":41,"changed":1}"#.into()));
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

#[test]
fn an_idle_look_journals_nothing() {
    let mut r = TaskRig::new(1, registration()).unwrap().double(LOOK, |_| Ok(r#"{"through":41,"keys":[]}"#.into()));
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    runtime::taskrig::assert_silent_tick(&mut r, STREAM);
}

#[test]
fn changed_keys_ask_for_one_run_and_its_success_moves_the_revision() {
    let mut r = TaskRig::new(1, registration())
        .unwrap()
        .double(LOOK, |_| Ok(r#"{"through":42,"keys":["sp-a","sp-b"]}"#.into()))
        .double(RUN, |_| Ok(r#"{"through":42,"changed":2}"#.into()));
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    r.clock.advance(1);
    let runs: Vec<_> = r.requested().iter().filter(|q| q.name == RUN).collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert!(runs[0].journaled && runs[0].input.contains("sp-a") && runs[0].input.contains("sp-b"), "{:?}", runs[0]);
    let states: Vec<_> = r.journaled().into_iter().filter(|e| e.kind.tag == STATE).collect();
    assert_eq!(states.len(), 1, "{states:?}");
    assert_eq!(states[0].kind.body, r#"{"through":42}"#);
}
