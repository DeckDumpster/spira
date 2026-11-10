//! A task's carry is kept as keyed rows of irreducible facts, and a boot that finds only those
//! rows repeats no side effect and misses nothing. Each family below restarts a task on the
//! journal it left and asks what the first ticks then do.

use std::sync::{Arc, Mutex};

use runtime::journal::{StateRow, Turn};
use runtime::taskrig::TaskRig;

fn rows_of(r: &TaskRig, task: &str) -> Vec<String> {
    r.journal.state().into_keys().filter(|(t, _)| t == task).map(|(_, k)| k).collect()
}

fn asked(r: &TaskRig, effect: &str) -> usize {
    r.requested().iter().filter(|q| q.name == effect).count()
}

/// The journal as a store of the previous release left it: a task's whole carry under `carry`.
fn legacy(r: &TaskRig, task: &str, stream: &str, tag: &str, body: &str, at: u64) {
    let ev = runtime::event::Event {
        id: runtime::event::EventId("legacy".into()),
        stream: runtime::event::StreamKey(stream.into()),
        kind: runtime::event::EventKind::new(Box::leak(tag.to_string().into_boxed_str()), body),
        at,
        cause: None,
        correlation: runtime::event::CorrelationId("legacy".into()),
    };
    let value = serde_json::to_string(&vec![ev]).unwrap();
    use runtime::journal::Journal;
    r.journal.commit(&Turn { state: vec![StateRow { task: task.into(), skey: "carry".into(), value, at }], ..Turn::default() }).unwrap();
}

mod repo_events {
    use super::*;
    use spira_runtime::watchers::repo_events::*;
    use spira_runtime::watchers::INBOX_APPEND;

    fn run(id: u64) -> Run {
        Run { id, workflow: "Deploy".into(), conclusion: "failure".into(), branch: "main".into(), event: "push".into(), title: format!("t{id}") }
    }

    fn registration() -> runtime::Registration {
        RepoEvents { interval_ms: 45_000 }.registration()
    }

    fn rig(forge: Arc<Mutex<Vec<Run>>>) -> TaskRig {
        TaskRig::new(1, registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&Snapshot { seed: vec![], repos: vec![("o/r".into(), Ok(forge.lock().unwrap().clone()))] }).unwrap())).double(INBOX_APPEND, |_| Ok(String::new()))
    }

    fn start(r: &mut TaskRig) {
        let (_, stream, kind) = RepoEvents::boot();
        r.tick(stream, kind).unwrap();
        r.advance(45_000).unwrap();
    }

    fn restart(r: &mut TaskRig) {
        r.reopen(registration()).unwrap();
        start(r);
    }

    #[test]
    fn the_carry_is_a_cursor_per_repository_and_not_one_whole_row() {
        let forge = Arc::new(Mutex::new(vec![run(7), run(5)]));
        let mut r = rig(forge);
        start(&mut r);
        assert_eq!(rows_of(&r, NAME), ["cursors/o/r", "seeded"]);
    }

    #[test]
    fn a_boot_rebuild_announces_nothing_seen_and_everything_new_since_the_cursor() {
        let forge = Arc::new(Mutex::new(vec![run(1)]));
        let mut r = rig(forge.clone());
        start(&mut r);
        *forge.lock().unwrap() = vec![run(3), run(2), run(1)];
        r.advance(45_000).unwrap();
        assert_eq!(asked(&r, INBOX_APPEND), 2);
        *forge.lock().unwrap() = vec![run(5), run(4), run(3), run(2), run(1)];
        restart(&mut r);
        r.advance(45_000 * 2).unwrap();
        assert_eq!(asked(&r, INBOX_APPEND), 4, "after the restart, runs 4 and 5 are said once each and 1 to 3 not again");
    }

    #[test]
    fn a_run_whose_conclusion_changes_below_the_cursor_is_not_announced_again() {
        let forge = Arc::new(Mutex::new(vec![run(3)]));
        let mut r = rig(forge.clone());
        start(&mut r);
        let mut rerun = run(3);
        rerun.conclusion = "success".into();
        *forge.lock().unwrap() = vec![rerun];
        r.advance(45_000).unwrap();
        assert_eq!(asked(&r, INBOX_APPEND), 0);
    }

    #[test]
    fn a_failure_already_said_is_not_said_again_after_a_boot() {
        let down = Arc::new(Mutex::new(true));
        let d = down.clone();
        let mut r = TaskRig::new(1, registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&Snapshot { seed: vec![], repos: vec![("o/r".into(), if *d.lock().unwrap() { Err("503".into()) } else { Ok(vec![]) })] }).unwrap())).double(INBOX_APPEND, |_| Ok(String::new()));
        start(&mut r);
        assert_eq!(asked(&r, INBOX_APPEND), 1);
        restart(&mut r);
        assert_eq!(asked(&r, INBOX_APPEND), 1, "the standing failure was said again");
    }

    #[test]
    fn a_store_holding_the_whole_carry_is_read_once_and_then_kept_as_rows() {
        let forge = Arc::new(Mutex::new(vec![run(3), run(2), run(1)]));
        let mut r = rig(forge);
        legacy(&r, NAME, STREAM, STATE, r#"{"seeded":true,"runs":["run|o/r|1|failure","run|o/r|2|failure","run|o/r|3|failure"],"adopted":["o/r"],"failed":[]}"#, 0);
        restart(&mut r);
        assert_eq!(asked(&r, INBOX_APPEND), 0, "the old carry's runs were announced again");
        r.advance(45_000).unwrap();
        assert_eq!(rows_of(&r, NAME), ["carry", "cursors/o/r", "seeded"]);
        assert_eq!(r.journal.state()[&(NAME.to_string(), "carry".to_string())], "[]");
    }
}

mod bead_sweep {
    use super::*;
    use spira_runtime::watchers::bead_sweep::*;
    use spira_runtime::watchers::INBOX_APPEND;

    fn sweep() -> BeadSweep {
        BeadSweep { interval_ms: 1000, ask_label: "ask".into(), home_repo: "spira".into(), hold_min: 30, rework_min: 5, cert_min: 5, wait_min: 45, batch_min: 60, stop_review_secs: 3600 }
    }

    fn poisoned(id: &str) -> Row {
        Row { id: id.into(), state: "READY".into(), holds: vec!["poison".into()], ..Row::default() }
    }

    fn rig(world: Arc<Mutex<Vec<Row>>>) -> TaskRig {
        TaskRig::new(1, sweep().registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&Snapshot { rows: world.lock().unwrap().clone(), ..Snapshot::default() }).unwrap())).double(INBOX_APPEND, |_| Ok(String::new()))
    }

    fn start(r: &mut TaskRig) {
        let (_, stream, kind) = BeadSweep::boot();
        r.tick(stream, kind).unwrap();
    }

    fn restart(r: &mut TaskRig) {
        r.reopen(sweep().registration()).unwrap();
        start(r);
    }

    #[test]
    fn each_seen_mark_is_its_own_row() {
        let mut r = rig(Arc::new(Mutex::new(vec![poisoned("sp-a"), poisoned("sp-b")])));
        start(&mut r);
        assert_eq!(rows_of(&r, NAME), ["seen/SUSPECT sp-a READY POISONED ()", "seen/SUSPECT sp-b READY POISONED ()"]);
    }

    #[test]
    fn a_boot_rebuild_repeats_no_suspect_and_misses_a_new_one() {
        let world = Arc::new(Mutex::new(vec![poisoned("sp-a")]));
        let mut r = rig(world.clone());
        start(&mut r);
        assert_eq!(asked(&r, INBOX_APPEND), 1);
        *world.lock().unwrap() = vec![poisoned("sp-a"), poisoned("sp-b")];
        restart(&mut r);
        assert_eq!(asked(&r, INBOX_APPEND), 2, "sp-a said again, or sp-b missed");
    }

    #[test]
    fn a_mark_that_cleared_before_the_boot_announces_again_when_it_returns() {
        let world = Arc::new(Mutex::new(vec![poisoned("sp-a")]));
        let mut r = rig(world.clone());
        start(&mut r);
        *world.lock().unwrap() = vec![];
        r.advance(1000).unwrap();
        assert_eq!(rows_of(&r, NAME).len(), 1);
        assert_eq!(r.journal.state().values().filter(|v| v.contains("SUSPECT")).count(), 0, "the cleared mark is overwritten, not left");
        restart(&mut r);
        *world.lock().unwrap() = vec![poisoned("sp-a")];
        r.advance(1000).unwrap();
        assert_eq!(asked(&r, INBOX_APPEND), 2);
    }
}
