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

#[test_id(t5xx9ks)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&a_failed_pass_already_said(), NAME);
    assert!(rows.iter().any(|k| k == "said"), "{rows:?}");
}

#[test_id(ts76zkb)]
#[test]
fn a_boot_does_not_say_the_same_failed_pass_again() {
    kit::assert_boot_matches_an_unstopped_run(a_failed_pass_already_said, registration, STREAM, EventKind::new(TICK, ""));
}

#[test_id(tcvw2oh)]
#[test]
fn an_apply_lost_at_a_restart_says_nothing_to_the_inbox() {
    let mut rig = TaskRig::new(1, registration()).unwrap().hanging(APPLY).double(INBOX_APPEND, |l| Ok(l.into()));
    rig.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    assert!(rig.requested().iter().any(|q| q.name == APPLY));
    rig.reopen(registration()).unwrap();
    rig.run_until_idle().unwrap();
    let said: Vec<&str> = rig.requested().iter().filter(|q| q.name == INBOX_APPEND).map(|q| q.input.as_str()).collect();
    assert!(said.is_empty(), "{said:?}");
    assert!(!rig.wide_set("overrides.failed"));
}
