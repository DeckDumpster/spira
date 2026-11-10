use runtime::event::EventKind;
use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::watchers::supersede_duty::*;
use spira_runtime::watchers::INBOX_APPEND;

fn registration() -> runtime::Registration {
    SupersedeDuty { interval_ms: 60_000 }.registration()
}

fn held(reading: Reading) -> String {
    serde_json::to_string(&Snapshot { held: vec![Held { id: "sp-old".into(), version: "3".into(), reading }] }).unwrap()
}

fn rig_reading(world: String, act: Result<&'static str, &'static str>) -> TaskRig {
    let mut r = TaskRig::new(1, registration()).unwrap().double(OBSERVE, move |_| Ok(world.clone())).double(ACT, move |_| act.map(String::from).map_err(String::from)).double(INBOX_APPEND, |l| Ok(l.into()));
    let (_, stream, boot) = SupersedeDuty::boot();
    r.tick(stream, boot).unwrap();
    r.clock.advance(1);
    r
}

fn a_hold_read_as_something_else() -> TaskRig {
    rig_reading(held(Reading::NotARequest), Ok(""))
}

fn a_request_whose_write_failed_and_was_said() -> TaskRig {
    rig_reading(held(Reading::Request { succ: "sp-new".into(), own: "HELD".into(), succ_state: "LANDED".into() }), Err("lifecycle refused"))
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let read = kit::assert_kept_as_rows(&a_hold_read_as_something_else(), NAME);
    assert!(read.iter().any(|k| k == "not_a_request/sp-old"), "{read:?}");
    let failed = kit::assert_kept_as_rows(&a_request_whose_write_failed_and_was_said(), NAME);
    assert!(failed.iter().any(|k| k == "failed"), "{failed:?}");
}

#[test]
fn a_boot_asks_the_world_only_about_the_holds_it_had_not_read() {
    kit::assert_boot_matches_an_unstopped_run(a_hold_read_as_something_else, registration, STREAM, EventKind::new(TICK, ""));
}

#[test]
fn a_boot_does_not_say_a_failed_write_or_its_successor_line_again() {
    kit::assert_boot_matches_an_unstopped_run(a_request_whose_write_failed_and_was_said, registration, STREAM, EventKind::new(TICK, ""));
}

#[test]
fn two_writes_failing_in_one_batch_are_both_remembered_as_failed() {
    let req = |id: &str| Held { id: id.into(), version: "3".into(), reading: Reading::Request { succ: "sp-new".into(), own: "HELD".into(), succ_state: "LANDED".into() } };
    let world = serde_json::to_string(&Snapshot { held: vec![req("sp-a"), req("sp-b")] }).unwrap();
    let mut rig = rig_reading(world, Err("lifecycle refused"));
    rig.run_until_idle().unwrap();
    let failed: Vec<String> = rig.journaled().iter().filter(|e| e.kind.tag == FAILED).map(|e| e.kind.body.clone()).collect();
    assert_eq!(failed.len(), 2, "{failed:?}");
}
