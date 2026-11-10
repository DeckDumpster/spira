use std::sync::{Arc, Mutex};

use runtime::event::EventKind;
use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::watchers::asks::*;
use spira_runtime::watchers::INBOX_APPEND;

fn registration() -> runtime::Registration {
    Asks { interval_ms: 60_000 }.registration()
}

fn open(ids: &[(&str, &str)]) -> Snapshot {
    Snapshot { open: ids.iter().map(|(i, t)| (i.to_string(), t.to_string())).collect(), held: vec![] }
}

fn baseline_then_a_new_ask() -> TaskRig {
    let world = Arc::new(Mutex::new(open(&[("sp-old", "already open")])));
    let seen = world.clone();
    let mut r = TaskRig::new(1, registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*seen.lock().unwrap()).unwrap())).double(INBOX_APPEND, |l| Ok(l.into()));
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    *world.lock().unwrap() = open(&[("sp-old", "already open"), ("sp-new", "decide X")]);
    r
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&baseline_then_a_new_ask(), NAME);
    assert!(rows.iter().any(|k| k == "seen") && rows.iter().any(|k| k == "seeded"), "{rows:?}");
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(baseline_then_a_new_ask, registration, STREAM, EventKind::new(TICK, ""));
}
