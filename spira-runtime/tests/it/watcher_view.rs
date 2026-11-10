use std::sync::{Arc, Mutex};

use runtime::event::EventKind;
use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::watchers::view::*;

fn registration() -> runtime::Registration {
    View { interval_ms: 3000, forget_secs: 99, enabled: true }.registration()
}

fn dismissed_a() -> Dismissed {
    Dismissed { ids: vec!["a".into()], absent: Default::default() }
}

fn a_review_seeded_then_ended() -> TaskRig {
    let live = Arc::new(Mutex::new(vec!["s".to_string()]));
    let seen = live.clone();
    let mut r = TaskRig::new(1, registration()).unwrap().double(OBSERVE, move |input| {
        let snap = Snapshot { seed: input == SEED, live: Some(seen.lock().unwrap().clone()), error: None, dismissed: dismissed_a() };
        Ok(serde_json::to_string(&snap).unwrap())
    }).double(FLIP, |to| Ok(to.into())).double(FORGET, |f| Ok(f.into()));
    r.advance(1_000_000).unwrap();
    let (_, stream, boot) = View::boot();
    r.tick(stream, boot).unwrap();
    r.advance(50_000).unwrap();
    live.lock().unwrap().clear();
    r
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&a_review_seeded_then_ended(), NAME);
    for k in ["want", "absent/a"] {
        assert!(rows.iter().any(|r| r == k), "{k} missing from {rows:?}");
    }
}

#[test]
fn a_boot_flips_the_cockpit_when_the_review_it_was_showing_ends_and_keeps_the_absence_clock() {
    kit::assert_boot_matches_an_unstopped_run(a_review_seeded_then_ended, registration, STREAM, EventKind::new(TICK, ""));
}
