//! For one timeline of review state, `cockpit-remote watch` and the view handler flip the cockpit
//! to the same windows in the same order and leave the same dismissals. The script is the oracle
//! and runs unmodified against a stubbed `hunk` and `tmux`; the handler asks an HTTP stub of the
//! hunk daemon that verifies every signature it is shown, and launches a stubbed `tmux`.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use runtime::ports::VirtualClock;
use runtime::{Engine, Executor, MemJournal};
use spira_runtime::watchers::view::View;

const FORGET: u64 = 99;
const SECOND: u64 = 1000;
fn engine(journal: Arc<MemJournal>, clock: &VirtualClock, enabled: bool) -> Engine {
    let reg = View { interval_ms: 3 * SECOND, forget_secs: FORGET, enabled }.registration();
    Engine::open(journal, Arc::new(clock.clone()), vec![reg], 0).unwrap()
}

#[test]
fn without_a_view_program_the_handler_schedules_nothing() {
    let clock = VirtualClock::new(1_800_000_000 * SECOND);
    let mut e = engine(MemJournal::new(), &clock, false);
    let (_, stream, kind) = View::boot();
    e.start(stream, kind);
    e.run_until_idle().unwrap();
    assert!(e.pending_effects().is_empty() && e.next_timer_at().is_none());
}

#[test]
fn the_watchers_manifest_no_longer_carries_the_row() {
    let manifest = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/watchers")).unwrap();
    assert!(manifest.lines().any(|l| l.starts_with("perf-watch|")), "the scan must be able to see a row");
    assert!(!manifest.lines().any(|l| l.trim_start_matches('?').starts_with("view|")), "spira-runtime hosts the view follower");
}
