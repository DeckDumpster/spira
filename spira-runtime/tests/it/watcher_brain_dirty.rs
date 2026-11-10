use std::sync::{Arc, Mutex};

use runtime::event::EventKind;
use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::watchers::brain_dirty::*;
use spira_runtime::watchers::INBOX_APPEND;

fn registration() -> runtime::Registration {
    BrainDirty { interval_ms: 60_000, age_secs: 1800 }.registration()
}

fn dirty(paths: &[&str]) -> Snapshot {
    Snapshot { wiki: "/w".into(), not_repo: false, dirty: paths.iter().map(|p| (p.to_string(), 0)).collect() }
}

fn one_path_said_and_another_dirtied() -> TaskRig {
    let world = Arc::new(Mutex::new(dirty(&["old.md"])));
    let seen = world.clone();
    let mut r = TaskRig::new(1, registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*seen.lock().unwrap()).unwrap())).double(INBOX_APPEND, |l| Ok(l.into()));
    r.advance(2_000_000).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    *world.lock().unwrap() = dirty(&["old.md", "new.md"]);
    r.clock.advance(1);
    r
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&one_path_said_and_another_dirtied(), NAME);
    assert!(rows.iter().any(|k| k == "said"), "{rows:?}");
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(one_path_said_and_another_dirtied, registration, STREAM, EventKind::new(TICK, ""));
}
