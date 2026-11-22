use std::sync::{Arc, Mutex};

use runtime::event::EventKind;
use runtime::ports::{Clock, VirtualClock};
use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::watchers::publish_backlog::*;
use spira_runtime::watchers::INBOX_APPEND;

fn registration() -> runtime::Registration {
    PublishBacklog { interval_ms: 60_000, recheck_ms: 7_000, max_count: 50, max_age_secs: 10_800 }.registration()
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
    r.clock.advance(1_900_000);
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    *world.lock().unwrap() = read(Read::Measured { count: 1, arrival: Some(0), forge_tip_age_secs: None });
    r.clock.advance(1);
    r
}

#[test_id(tazbns7)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&blind_then_measurable(), NAME);
    for k in ["standing/r", "blind_since/r"] {
        assert!(rows.iter().any(|r| r == k), "{k} missing from {rows:?}");
    }
}

#[test_id(tqt2et5)]
#[test]
fn a_boot_says_sighted_after_the_blind_spell_it_began_before() {
    kit::assert_boot_matches_an_unstopped_run(blind_then_measurable, registration, STREAM, EventKind::new(TICK, ""));
}

#[test_id(ts0bkun)]
#[test]
fn a_boot_says_clear_for_a_backlog_that_was_over_before_it() {
    kit::assert_boot_matches_an_unstopped_run(over_then_under, registration, STREAM, EventKind::new(TICK, ""));
}

type Shared<T> = Arc<Mutex<Vec<T>>>;

fn lines_and_calls(answers: impl Fn(usize) -> Read + Send + Sync + 'static) -> (TaskRig, Shared<String>, Shared<u64>) {
    let lines = Arc::new(Mutex::new(vec![]));
    let calls = Arc::new(Mutex::new(vec![]));
    let (l, c) = (lines.clone(), calls.clone());
    let clock: Arc<Mutex<Option<VirtualClock>>> = Arc::new(Mutex::new(None));
    let seen = clock.clone();
    let rig = TaskRig::new(1, registration())
        .unwrap()
        .double(OBSERVE, move |_| {
            let mut c = c.lock().unwrap();
            c.push(seen.lock().unwrap().as_ref().unwrap().now_ms());
            Ok(serde_json::to_string(&read(answers(c.len() - 1))).unwrap())
        })
        .double(INBOX_APPEND, move |line| {
            l.lock().unwrap().push(line.to_string());
            Ok(line.into())
        });
    *clock.lock().unwrap() = Some(rig.clock.clone());
    (rig, lines, calls)
}

#[test]
fn a_read_that_fails_once_is_remeasured_and_pages_nothing() {
    let (mut r, lines, calls) = lines_and_calls(|n| if n == 0 { Read::Blind("deadline".into()) } else { Read::Measured { count: 0, arrival: None, forge_tip_age_secs: None } });
    r.clock.advance(100_000);
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    r.advance(7_000).unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2, "the failed read is measured again");
    assert!(calls[1] - calls[0] <= 7_000, "{calls:?}");
    let lines = lines.lock().unwrap();
    assert!(!lines.iter().any(|l| l.contains("BLIND") || l.contains("SIGHTED")), "{lines:?}");
}

#[test]
fn a_read_that_fails_twice_pages_blind_at_the_recheck_and_only_once() {
    let (mut r, lines, _) = lines_and_calls(|_| Read::Blind("deadline".into()));
    r.clock.advance(100_000);
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    assert!(lines.lock().unwrap().is_empty(), "the first failure pages nothing");
    r.advance(7_000).unwrap();
    assert_eq!(lines.lock().unwrap().iter().filter(|l| l.contains("BLIND")).count(), 1);
    r.advance(60_000).unwrap();
    r.advance(60_000).unwrap();
    assert_eq!(lines.lock().unwrap().iter().filter(|l| l.contains("BLIND")).count(), 1);
}

fn doubted() -> TaskRig {
    let world = Arc::new(Mutex::new(read(Read::Blind("no reflog".into()))));
    let mut r = rig_reading(world.clone());
    r.clock.advance(100_000);
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    *world.lock().unwrap() = read(Read::Measured { count: 0, arrival: None, forge_tip_age_secs: None });
    r.clock.advance(1);
    r
}

#[test]
fn a_boot_between_the_doubt_and_its_recheck_matches_an_unstopped_run() {
    kit::assert_boot_matches_an_unstopped_run(doubted, registration, STREAM, EventKind::new(TICK, ""));
}

#[test]
fn the_doubt_is_kept_as_its_own_rows() {
    let rows = kit::assert_kept_as_rows(&doubted(), NAME);
    assert!(rows.iter().any(|r| r == "doubt/r"), "{rows:?}");
}

#[test]
fn an_over_repo_that_fails_once_is_rechecked_and_keeps_its_standing_silently() {
    let over = || Read::Measured { count: 60, arrival: Some(0), forge_tip_age_secs: None };
    let (mut r, lines, calls) = lines_and_calls(move |n| if n == 1 { Read::Blind("deadline".into()) } else { over() });
    r.clock.advance(1_900_000);
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    r.advance(60_000).unwrap();
    r.advance(7_000).unwrap();
    assert!(calls.lock().unwrap().len() >= 3, "the failure after the OVER is rechecked");
    let lines = lines.lock().unwrap();
    assert_eq!(lines.iter().filter(|l| l.contains("OVER")).count(), 1, "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("BLIND") || l.contains("SIGHTED") || l.contains("CLEAR")), "{lines:?}");
}
