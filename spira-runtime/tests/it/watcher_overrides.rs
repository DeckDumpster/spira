use runtime::event::EventKind;
use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::watchers::overrides::*;
use spira_runtime::watchers::INBOX_APPEND;

fn registration() -> runtime::Registration {
    Overrides { interval_ms: 60_000 }.registration()
}

fn a_failed_pass_already_said() -> TaskRig {
    let mut r = TaskRig::new(1, registration()).unwrap().double(APPLY, |_| Err("overrides: ghost: bead sp-x is not in the store".into())).double(INBOX_APPEND, |l| Ok(l.into()));
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    assert!(r.requested().iter().any(|q| q.name == INBOX_APPEND), "the scenario must have said the failure");
    r.clock.advance(1);
    r
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&a_failed_pass_already_said(), NAME);
    assert!(rows.iter().any(|k| k == "said"), "{rows:?}");
}

#[test]
fn a_boot_does_not_say_the_same_failed_pass_again() {
    kit::assert_boot_matches_an_unstopped_run(a_failed_pass_already_said, registration, STREAM, EventKind::new(TICK, ""));
}
