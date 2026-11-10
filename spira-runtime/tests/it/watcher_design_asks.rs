use std::sync::{Arc, Mutex};

use runtime::event::EventKind;
use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::watchers::design_asks::*;
use spira_runtime::watchers::INBOX_APPEND;

const REL: &str = "wiki/projects/zed/designs/alpha.md";

fn registration() -> runtime::Registration {
    DesignAsks { interval_ms: 600_000, repeat_secs: 7200 }.registration()
}

fn uncovered_draft() -> Snapshot {
    Snapshot { failure: None, asks: String::new(), pages: vec![Page { rel: REL.into(), status: "draft".into(), writer: "sp-w".into(), urls: vec![] }] }
}

fn failing() -> Snapshot {
    Snapshot { failure: Some("the brain is gone".into()), ..Snapshot::default() }
}

fn told_once(world: Snapshot) -> TaskRig {
    let world = Arc::new(Mutex::new(world));
    let mut r = TaskRig::new(1, registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap())).double(INBOX_APPEND, |l| Ok(l.into()));
    r.advance(10_000_000).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    r.clock.advance(1);
    r
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&told_once(uncovered_draft()), NAME);
    assert!(rows.iter().any(|k| k == &format!("since/{REL}")), "{rows:?}");
}

#[test]
fn a_boot_does_not_tell_a_page_again_inside_its_repeat_window() {
    kit::assert_boot_matches_an_unstopped_run(|| told_once(uncovered_draft()), registration, STREAM, EventKind::new(TICK, ""));
}

#[test]
fn a_boot_does_not_say_a_failure_again_that_was_already_said() {
    kit::assert_boot_matches_an_unstopped_run(|| told_once(failing()), registration, STREAM, EventKind::new(TICK, ""));
}
