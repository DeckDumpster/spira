use std::sync::{Arc, Mutex};

use runtime::event::EventKind;
use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::watchers::supersede_duty::*;
use spira_runtime::watch_topics::Change;
use spira_runtime::watchers::{INBOX_APPEND, WATCH_PUBLISH};

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

#[test_id(tiii9ba)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let read = kit::assert_kept_as_rows(&a_hold_read_as_something_else(), NAME);
    assert!(read.iter().any(|k| k == "not_a_request/sp-old"), "{read:?}");
    let failed = kit::assert_kept_as_rows(&a_request_whose_write_failed_and_was_said(), NAME);
    assert!(failed.iter().any(|k| k == "failed"), "{failed:?}");
}

#[test_id(tj7pmhu)]
#[test]
fn a_boot_asks_the_world_only_about_the_holds_it_had_not_read() {
    kit::assert_boot_matches_an_unstopped_run(a_hold_read_as_something_else, registration, STREAM, EventKind::new(TICK, ""));
}

#[test_id(to9ygod)]
#[test]
fn a_boot_does_not_say_a_failed_write_or_its_successor_line_again() {
    kit::assert_boot_matches_an_unstopped_run(a_request_whose_write_failed_and_was_said, registration, STREAM, EventKind::new(TICK, ""));
}

#[test_id(t88dpc1)]
#[test]
fn two_writes_failing_in_one_batch_are_both_remembered_as_failed() {
    let req = |id: &str| Held { id: id.into(), version: "3".into(), reading: Reading::Request { succ: "sp-new".into(), own: "HELD".into(), succ_state: "LANDED".into() } };
    let world = serde_json::to_string(&Snapshot { held: vec![req("sp-a"), req("sp-b")] }).unwrap();
    let mut rig = rig_reading(world, Err("lifecycle refused"));
    rig.run_until_idle().unwrap();
    let failed: Vec<String> = rig.journaled().iter().filter(|e| e.kind.tag == FAILED).map(|e| e.kind.body.clone()).collect();
    assert_eq!(failed.len(), 2, "{failed:?}");
}

fn published(r: &TaskRig) -> Vec<Change> {
    r.requested().iter().filter(|q| q.name == WATCH_PUBLISH).map(|q| serde_json::from_str(&q.input).unwrap()).collect()
}

fn watched(world: Arc<Mutex<String>>, act: Result<&'static str, &'static str>) -> TaskRig {
    let mut r = TaskRig::new(1, registration())
        .unwrap()
        .double(OBSERVE, move |_| Ok(world.lock().unwrap().clone()))
        .double(ACT, move |_| act.map(String::from).map_err(String::from))
        .double(INBOX_APPEND, |l| Ok(l.into()))
        .double(WATCH_PUBLISH, |l| Ok(l.into()));
    let (_, stream, boot) = SupersedeDuty::boot();
    r.tick(stream, boot).unwrap();
    r
}

fn request() -> String {
    held(Reading::Request { succ: "sp-new".into(), own: "HELD".into(), succ_state: "LANDED".into() })
}

#[test_id(tx6z6ke)]
#[test]
fn a_request_held_across_three_passes_is_set_once_and_cleared_once_when_it_leaves() {
    let world = Arc::new(Mutex::new(request()));
    let mut r = watched(world.clone(), Ok(""));
    r.advance(60_000).unwrap();
    r.advance(60_000).unwrap();
    let held = published(&r);
    let [set] = held.as_slice() else { panic!("one set: {held:?}") };
    assert_eq!((set.row.as_str(), set.set.keys().map(String::as_str).collect::<Vec<_>>(), set.clear.len()), (NAME, vec!["sp-old"], 0));
    assert_eq!(set.set["sp-old"].text, "SUPERSEDE REQUEST sp-old: successor sp-new");

    *world.lock().unwrap() = serde_json::to_string(&Snapshot::default()).unwrap();
    r.advance(60_000).unwrap();
    r.advance(60_000).unwrap();
    let all = published(&r);
    assert_eq!(all.len(), 2, "{all:?}");
    assert_eq!((all[1].set.len(), all[1].clear.clone()), (0, vec!["sp-old".to_string()]));
}

#[test_id(t2lwnpv)]
#[test]
fn a_failing_write_changes_the_value_once_and_an_unreadable_pass_leaves_it_held() {
    let world = Arc::new(Mutex::new(request()));
    let mut r = watched(world.clone(), Err("lifecycle refused"));
    for _ in 0..3 {
        r.advance(60_000).unwrap();
    }
    *world.lock().unwrap() = held(Reading::Unreadable);
    r.advance(60_000).unwrap();
    let texts: Vec<String> = published(&r).iter().map(|c| {
        assert!(c.clear.is_empty(), "{c:?}");
        c.set["sp-old"].text.clone()
    }).collect();
    assert_eq!(texts, ["SUPERSEDE REQUEST sp-old: successor sp-new", "SUPERSEDE REQUEST sp-old: supersede failed — will retry"]);
}
