use std::sync::{Arc, Mutex};

use runtime::event::EventKind;
use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::watchers::publish_backlog::*;
use spira_runtime::watchers::INBOX_APPEND;

fn registration() -> runtime::Registration {
    PublishBacklog { interval_ms: 60_000, max_count: 50, max_age_secs: 10_800 }.registration()
}

fn read(r: Read) -> Snapshot {
    Snapshot { repos: vec![("r".into(), r)] }
}

fn rig_reading(world: Arc<Mutex<Snapshot>>) -> TaskRig {
    TaskRig::new(1, registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap())).double(INBOX_APPEND, |l| Ok(l.into()))
}

fn blind_then_measurable() -> TaskRig {
    let world = Arc::new(Mutex::new(read(Read::Blind("no reflog".into()))));
    let mut r = rig_reading(world.clone());
    r.clock.advance(100_000);
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    r.advance(125_000).unwrap();
    *world.lock().unwrap() = read(Read::Measured { count: 0, arrival: None, forge_tip_age_secs: None });
    r
}

fn over_then_under() -> TaskRig {
    let world = Arc::new(Mutex::new(read(Read::Measured { count: 60, arrival: Some(0), forge_tip_age_secs: None })));
    let mut r = rig_reading(world.clone());
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    *world.lock().unwrap() = read(Read::Measured { count: 1, arrival: Some(0), forge_tip_age_secs: None });
    r.clock.advance(1);
    r
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&blind_then_measurable(), NAME);
    for k in ["standing/r", "blind_since/r"] {
        assert!(rows.iter().any(|r| r == k), "{k} missing from {rows:?}");
    }
}

#[test]
fn a_boot_says_sighted_after_the_blind_spell_it_began_before() {
    kit::assert_boot_matches_an_unstopped_run(blind_then_measurable, registration, STREAM, EventKind::new(TICK, ""));
}

#[test]
fn a_boot_says_clear_for_a_backlog_that_was_over_before_it() {
    kit::assert_boot_matches_an_unstopped_run(over_then_under, registration, STREAM, EventKind::new(TICK, ""));
}
