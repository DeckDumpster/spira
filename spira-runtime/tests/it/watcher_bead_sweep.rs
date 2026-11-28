use std::sync::{Arc, Mutex};

use runtime::taskrig::TaskRig;
use spira_runtime::watch_topics::Change;
use spira_runtime::watchers::bead_sweep::*;
use spira_runtime::watchers::{INBOX_APPEND, WATCH_PUBLISH};

fn sweep() -> BeadSweep {
    BeadSweep { interval_ms: 60_000, ask_label: "ask".into(), home_repo: "spira".into(), hold_min: 30, rework_min: 5, cert_min: 5, wait_min: 45, batch_min: 60, stop_review_secs: 3600 }
}

fn published(r: &TaskRig) -> Vec<Change> {
    r.requested().iter().filter(|q| q.name == WATCH_PUBLISH).map(|q| serde_json::from_str(&q.input).unwrap()).collect()
}

fn rig(world: Arc<Mutex<Snapshot>>) -> TaskRig {
    let mut r = TaskRig::new(1, sweep().registration())
        .unwrap()
        .double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap()))
        .double(INBOX_APPEND, |l| Ok(l.into()))
        .double(WATCH_PUBLISH, |l| Ok(l.into()));
    let (_, stream, kind) = BeadSweep::boot();
    r.tick(stream, kind).unwrap();
    r
}

/// A hold with no review date, held long enough to be stale: its line's age moves every pass.
fn stale_hold(id: &str) -> Row {
    Row { id: id.into(), state: "READY".into(), holds: vec!["manual".into()], reason: "why".into(), held_at: Some(-7200.0), ..Row::default() }
}

#[test_id(t7aeoxx)]
#[test]
fn a_suspect_held_across_three_passes_is_set_once_and_cleared_once_when_it_leaves() {
    let world = Arc::new(Mutex::new(Snapshot { rows: vec![stale_hold("sp-a")], ..Snapshot::default() }));
    let mut r = rig(world.clone());
    r.advance(60_000).unwrap();
    r.advance(60_000).unwrap();
    let held = published(&r);
    let [set] = held.as_slice() else { panic!("one set: {held:?}") };
    assert_eq!((set.row.as_str(), set.set.keys().map(String::as_str).collect::<Vec<_>>(), set.clear.len()), (NAME, vec!["sp-a"], 0));
    assert!(set.set["sp-a"].text.starts_with("SUSPECT sp-a READY STALE HOLD manual, held 2h00m"), "{set:?}");

    world.lock().unwrap().rows.clear();
    r.advance(60_000).unwrap();
    r.advance(60_000).unwrap();
    let all = published(&r);
    assert_eq!(all.len(), 2, "{all:?}");
    assert_eq!((all[1].set.len(), all[1].clear.clone()), (0, vec!["sp-a".to_string()]));
}

/// A line naming no item is held as `all`; a reservation's line names its item; one item's two
/// findings are one value.
#[test_id(t2lqvdj)]
#[test]
fn each_finding_is_held_under_the_item_it_names_and_all_when_it_names_none() {
    let stop = Stop { repo: "r".into(), lanes: vec!["claims".into()], owner: "o".into(), until: String::new(), since: -7200.0 };
    let reservation = Reservation { bead: "sp-kept".into(), owner: "o".into(), reason: String::new(), until: String::new(), since: -7200.0 };
    let both = Row { id: "sp-b".into(), state: "READY".into(), claim: Some("cannot-tell".into()), holds: vec!["poison".into()], ..Row::default() };
    let r = rig(Arc::new(Mutex::new(Snapshot { rows: vec![both], stops: vec![stop], reserved: vec![reservation], ..Snapshot::default() })));
    let held = published(&r);
    let [set] = held.as_slice() else { panic!("one change: {held:?}") };
    assert_eq!(set.set.keys().collect::<Vec<_>>(), ["all", "sp-b", "sp-kept"]);
    assert!(set.set["sp-b"].text.contains("CLAIM UNJUDGED") && set.set["sp-b"].text.contains("POISONED"), "{set:?}");
}

/// The repair's report is an event, not a finding the next pass still sees.
#[test_id(tpra1n1)]
#[test]
fn a_repaired_line_is_said_to_the_inbox_and_held_as_no_condition() {
    let bead = Bead { id: "sp-n".into(), status: "open".into(), ..Bead::default() };
    let held = Row { id: "sp-p".into(), state: "READY".into(), holds: vec!["poison".into()], ..Row::default() };
    let world = Arc::new(Mutex::new(Snapshot { rows: vec![held], content: vec![bead], ..Snapshot::default() }));
    let mut r = TaskRig::new(1, sweep().registration())
        .unwrap()
        .double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap()))
        .double(REPAIR, |_| Ok(serde_json::to_string(&Repaired { ids: vec!["sp-n".into()], ..Repaired::default() }).unwrap()))
        .double(INBOX_APPEND, |l| Ok(l.into()))
        .double(WATCH_PUBLISH, |l| Ok(l.into()));
    let (_, stream, kind) = BeadSweep::boot();
    r.tick(stream, kind).unwrap();
    r.run_until_idle().unwrap();
    let said: Vec<_> = r.requested().iter().filter(|q| q.name == INBOX_APPEND).map(|q| q.input.clone()).collect();
    assert!(said.iter().any(|l| l.contains("REPAIRED sp-n")), "{said:?}");
    let subjects: Vec<String> = published(&r).iter().flat_map(|c| c.set.keys().cloned()).collect();
    assert_eq!(subjects, ["sp-p"]);
}
