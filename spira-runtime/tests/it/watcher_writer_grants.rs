use std::sync::{Arc, Mutex};

use runtime::event::EventKind;
use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::watchers::writer_grants::*;
use spira_runtime::watchers::INBOX_APPEND;

type Answer = Arc<Mutex<Result<String, String>>>;
type Lines = Arc<Mutex<Vec<String>>>;

fn registration() -> runtime::Registration {
    WriterGrants { interval_ms: 60_000 }.registration()
}

fn answer(missing: &[&str], error: Option<&str>) -> Result<String, String> {
    Ok(serde_json::to_string(&Snapshot { checked_at: Some(1), writer: true, missing: missing.iter().map(|t| t.to_string()).collect(), error: error.map(str::to_string) }).unwrap())
}

fn rig(first: Result<String, String>) -> (TaskRig, Answer, Lines) {
    let world: Answer = Arc::new(Mutex::new(first));
    let lines: Lines = Arc::new(Mutex::new(vec![]));
    let (seen, kept) = (world.clone(), lines.clone());
    let mut r = TaskRig::new(1, registration()).unwrap().double(OBSERVE, move |_| seen.lock().unwrap().clone()).double(INBOX_APPEND, move |l| {
        kept.lock().unwrap().push(l.to_string());
        Ok(l.into())
    });
    r.advance(2_000_000).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    (r, world, lines)
}

fn next_tick(r: &mut TaskRig) {
    r.advance(60_000).unwrap();
}

#[test_id(tnc7e3q)]
#[test]
fn a_missing_table_is_paged_once_naming_it_and_the_provisioning_verb() {
    let (_, _, lines) = rig(answer(&["dependencies"], None));
    let said = lines.lock().unwrap().clone();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("dependencies") && said[0].contains("admin-provision-writer"), "{said:?}");
}

#[test_id(t2k9wzp)]
#[test]
fn the_same_answer_on_later_ticks_pages_nothing_more() {
    let (mut r, _, lines) = rig(answer(&["dependencies"], None));
    for _ in 0..3 {
        next_tick(&mut r);
    }
    assert_eq!(lines.lock().unwrap().len(), 1);
}

#[test_id(tq5hd0m)]
#[test]
fn a_grant_lost_again_after_a_clear_answer_pages_again() {
    let (mut r, world, lines) = rig(answer(&["dependencies"], None));
    *world.lock().unwrap() = answer(&[], None);
    next_tick(&mut r);
    *world.lock().unwrap() = answer(&["dependencies"], None);
    next_tick(&mut r);
    assert_eq!(lines.lock().unwrap().len(), 2);
}

#[test_id(txm3vb8)]
#[test]
fn a_check_that_cannot_run_is_paged_apart_from_a_missing_table() {
    let (mut r, world, lines) = rig(answer(&["dependencies"], None));
    *world.lock().unwrap() = answer(&["dependencies"], Some("access denied"));
    next_tick(&mut r);
    let said = lines.lock().unwrap().clone();
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(said[1].contains("cannot check") && said[1].contains("access denied"), "{said:?}");
}

#[test_id(tw8c1yr)]
#[test]
fn a_failed_observe_pages_nothing_and_forgets_nothing() {
    let (mut r, world, lines) = rig(Err("lc-serve is down".into()));
    assert!(lines.lock().unwrap().is_empty());
    *world.lock().unwrap() = answer(&["dependencies"], None);
    next_tick(&mut r);
    *world.lock().unwrap() = Err("lc-serve is down".into());
    next_tick(&mut r);
    *world.lock().unwrap() = answer(&["dependencies"], None);
    next_tick(&mut r);
    assert_eq!(lines.lock().unwrap().len(), 1, "an outage must not make the same failure page twice");
}

fn said_one_and_changed_the_answer() -> TaskRig {
    let (r, world, _) = rig(answer(&["dependencies"], None));
    *world.lock().unwrap() = answer(&["dependencies", "issues"], None);
    r.clock.advance(1);
    r
}

#[test_id(tr4gn6a)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&said_one_and_changed_the_answer(), NAME);
    assert!(rows.iter().any(|k| k == "said"), "{rows:?}");
}

#[test_id(tj1pu5e)]
#[test]
fn a_boot_repeats_no_page_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(said_one_and_changed_the_answer, registration, STREAM, EventKind::new(TICK, ""));
}


#[test_id(t2exbft)]
#[test]
fn an_answer_that_is_not_a_snapshot_pages_nothing() {
    let (mut r, world, lines) = rig(Ok("usage: spira-lc show <bead-id> | ...".into()));
    next_tick(&mut r);
    *world.lock().unwrap() = Ok(String::new());
    next_tick(&mut r);
    assert!(lines.lock().unwrap().is_empty(), "{:?}", lines.lock().unwrap());
}


#[test_id(tsa57nn)]
#[test]
fn a_timed_out_observe_pages_nothing() {
    let lines: Lines = Arc::new(Mutex::new(vec![]));
    let kept = lines.clone();
    let mut r = TaskRig::new(1, registration()).unwrap().timing_out().double(OBSERVE, |_| answer(&["dependencies"], None)).double(INBOX_APPEND, move |l| {
        kept.lock().unwrap().push(l.to_string());
        Ok(l.into())
    });
    r.advance(2_000_000).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    next_tick(&mut r);
    assert!(lines.lock().unwrap().is_empty());
}
