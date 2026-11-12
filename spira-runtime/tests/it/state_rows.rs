//! A task's carry is kept as keyed rows of irreducible facts, and a boot that finds only those
//! rows repeats no side effect and misses nothing. Each family below restarts a task on the
//! journal it left and asks what the first ticks then do.

use std::sync::{Arc, Mutex};

use runtime::taskrig::TaskRig;

fn rows_of(r: &TaskRig, task: &str) -> Vec<String> {
    r.journal.state().into_keys().filter(|(t, _)| t == task).map(|(_, k)| k).collect()
}

fn asked(r: &TaskRig, effect: &str) -> usize {
    r.requested().iter().filter(|q| q.name == effect).count()
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
