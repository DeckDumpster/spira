use std::sync::{Arc, Mutex};

use runtime::event::EventKind;
use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::watchers::inbox_keeper::*;
use spira_runtime::watchers::CONCIERGE_WAKE;

const START_MS: u64 = 10_000_000;

fn registration() -> runtime::Registration {
    InboxKeeper { interval_ms: 60_000, stall_secs: 600, backoff_secs: 3600 }.registration()
}

fn waiting() -> Snapshot {
    Snapshot { waiter_alive: false, unread: 2, cursor_mtime: 0 }
}

fn rig_alive_at_start() -> (TaskRig, Arc<Mutex<Snapshot>>) {
    let world = Arc::new(Mutex::new(Snapshot { waiter_alive: true, unread: 0, cursor_mtime: 0 }));
    let seen = world.clone();
    let mut r = TaskRig::new(1, registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*seen.lock().unwrap()).unwrap())).double(CONCIERGE_WAKE, |l| Ok(l.into()));
    r.advance(START_MS).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    (r, world)
}

fn monitor_gone_and_the_stall_window_just_passed() -> TaskRig {
    let (mut r, world) = rig_alive_at_start();
    *world.lock().unwrap() = waiting();
    r.advance(540_000).unwrap();
    r.clock.advance(70_000);
    r
}

fn monitor_gone_and_a_wake_just_sent() -> TaskRig {
    let (mut r, world) = rig_alive_at_start();
    *world.lock().unwrap() = waiting();
    r.advance(600_000).unwrap();
    assert!(r.requested().iter().any(|q| q.name == CONCIERGE_WAKE), "the scenario must have woken the pane");
    r
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&monitor_gone_and_a_wake_just_sent(), NAME);
    for k in ["last_seen", "last_wake", "persisted_seen", "wake_failed"] {
        assert!(rows.iter().any(|r| r == k), "{k} missing from {rows:?}");
    }
}

#[test]
fn a_boot_wakes_the_pane_when_the_stall_began_before_it() {
    kit::assert_boot_matches_an_unstopped_run(monitor_gone_and_the_stall_window_just_passed, registration, STREAM, EventKind::new(TICK, ""));
}

#[test]
fn a_boot_does_not_wake_the_pane_again_inside_the_backoff() {
    kit::assert_boot_matches_an_unstopped_run(monitor_gone_and_a_wake_just_sent, registration, STREAM, EventKind::new(TICK, ""));
}

mod real_world {
    use super::*;
    use runtime::engine::LastOutcome;
    use spira_runtime::world::World;
    use std::fs::{self, File};
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    const RETRY_SECS: u64 = 600;

    fn registration() -> runtime::Registration {
        InboxKeeper { interval_ms: 60_000, stall_secs: 600, backoff_secs: RETRY_SECS }.registration()
    }

    fn lock_path(run: &Path) -> std::path::PathBuf {
        run.join("watchd/concierge-inbox.tail.lock")
    }

    fn fixture(lines: usize, cursor: Option<u64>) -> (testkit::TempDir, World) {
        let dir = testkit::TempDir::new("keeper-world");
        let wd = dir.join("watchd");
        fs::create_dir_all(&wd).unwrap();
        let inbox = wd.join("concierge-inbox.log");
        fs::write(&inbox, "line\n".repeat(lines)).unwrap();
        if let Some(c) = cursor {
            let f = wd.join("concierge-inbox.cursor");
            fs::write(&f, format!("{c}\n")).unwrap();
            File::options().write(true).open(&f).unwrap().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1)).unwrap();
        }
        let world = World { run_dir: dir.path().to_path_buf(), inbox, ..World::default() };
        (dir, world)
    }

    fn hold(run: &Path) -> File {
        watchd::lock::try_lock(&lock_path(run)).unwrap().expect("the lock is free")
    }

    fn rig(world: World, wake: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static) -> TaskRig {
        let mut r = TaskRig::new(1, registration()).unwrap().double(OBSERVE, move |i| world.keeper_snapshot(i)).double(CONCIERGE_WAKE, wake);
        r.advance(START_MS).unwrap();
        r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
        r
    }

    fn outcome(r: &TaskRig) -> Option<LastOutcome> {
        r.engine().status().tasks.into_iter().find(|t| t.name == NAME).and_then(|t| t.last_outcome)
    }

    fn wakes(r: &TaskRig) -> Vec<String> {
        r.requested().iter().filter(|q| q.name == CONCIERGE_WAKE).map(|q| q.input.clone()).collect()
    }

    #[test]
    fn a_killed_waiter_with_lines_unread_is_woken_with_the_real_rearm_command_within_a_stall_and_a_tick() {
        let (dir, world) = fixture(2, Some(0));
        let held = hold(dir.path());
        let mut r = rig(world, |l| Ok(l.into()));
        r.advance(120_000).unwrap();
        assert!(wakes(&r).is_empty(), "a held lock is a live waiter");
        drop(held);
        r.advance((600 + 60) * 1000).unwrap();
        let sent = wakes(&r);
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(sent[0].contains("watchd next concierge-inbox"), "{}", sent[0]);
        assert!(!sent[0].contains("inbox-triage"));
    }

    #[test]
    fn a_held_lock_with_lines_unread_never_wakes() {
        let (dir, world) = fixture(2, Some(0));
        let _held = hold(dir.path());
        let mut r = rig(world, |l| Ok(l.into()));
        r.advance(3_600_000).unwrap();
        assert!(wakes(&r).is_empty());
        assert_eq!(outcome(&r), Some(LastOutcome::Succeeded));
    }

    #[test]
    fn a_free_lock_with_the_cursor_at_the_line_count_never_wakes() {
        let (_dir, world) = fixture(2, Some(2));
        let mut r = rig(world, |l| Ok(l.into()));
        r.advance(3_600_000).unwrap();
        assert!(wakes(&r).is_empty());
    }

    #[test]
    fn a_missing_watchd_dir_fails_the_observe_and_the_task_reads_failed() {
        let (dir, world) = fixture(2, Some(0));
        fs::remove_dir_all(dir.join("watchd")).unwrap();
        let r = rig(world, |l| Ok(l.into()));
        assert_eq!(outcome(&r), Some(LastOutcome::Failed));
    }

    #[test]
    fn an_unreadable_inbox_fails_the_observe_and_the_task_reads_failed() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, world) = fixture(2, Some(0));
        fs::set_permissions(&world.inbox, fs::Permissions::from_mode(0o0)).unwrap();
        if fs::read(&world.inbox).is_ok() {
            return;
        }
        let r = rig(world, |l| Ok(l.into()));
        assert_eq!(outcome(&r), Some(LastOutcome::Failed));
    }

    #[test]
    fn a_wake_that_failed_keeps_the_task_failed_until_a_waiter_is_seen_again() {
        let (dir, world) = fixture(2, Some(0));
        let held = hold(dir.path());
        let mut r = rig(world, |_| Err("pane busy".into()));
        drop(held);
        r.advance((600 + 60) * 1000).unwrap();
        assert_eq!(wakes(&r).len(), 1);
        for _ in 0..3 {
            r.advance(60_000).unwrap();
            assert_eq!(outcome(&r), Some(LastOutcome::Failed));
        }
        let _again = hold(dir.path());
        r.advance(60_000).unwrap();
        assert_eq!(outcome(&r), Some(LastOutcome::Succeeded));
    }
}
