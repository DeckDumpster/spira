//! sp-nasmjd: a runtime watcher's inbox line that matches `SPIRA_CONCIERGE_PUSH_PATTERN` is
//! pushed — spooled beside the inbox and handed to the flusher — exactly as an
//! `inbox-triage append` line is, and a line that does not match is only recorded.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use spira_runtime::tasks::e::watch_throughput::{decide, Carry, Config, Snapshot};
use spira_runtime::world::World;

/// A flusher double: records each argv it is started with, then exits.
fn flusher(dir: &Path) -> (PathBuf, PathBuf) {
    let calls = dir.join("flusher.calls");
    let exe = dir.join("inbox-triage");
    testkit::write_exe(&exe, &format!("#!/bin/sh\necho \"$@\" >> '{}'\n", calls.display()));
    (exe, calls)
}

fn world(dir: &Path, exe: PathBuf) -> World {
    World { inbox: dir.join("inbox.log"), push_wake: "concierge.sh wake".into(), push_pattern: r"\bP0\b|RED on".into(), inbox_flusher: exe, ..World::default() }
}

/// The throughput watcher's BLIND page, as the watcher itself builds the effect's input.
fn blind_line() -> String {
    let cfg = Config { floor: 1, abandon_count: 3, abandon_window_s: 3600, restop_after_s: 0 };
    let v = decide(&Carry::default(), &Snapshot::default(), 1_762_500_000_000, &cfg);
    v.acts.into_iter().next().expect("a blind page").input
}

/// The flusher runs detached, so its record is waited for: the marker file is the signal.
fn wait_for(path: &Path) -> String {
    let end = Instant::now() + Duration::from_secs(5); // clock-safe: a cap on a wait for the flusher's marker file
    while Instant::now() < end { // clock-safe: a cap on a wait for the flusher's marker file
        if let Ok(t) = std::fs::read_to_string(path) {
            if !t.is_empty() {
                return t;
            }
        }
        std::thread::sleep(Duration::from_millis(10)); // clock-safe: polling for the flusher's marker file
    }
    String::new()
}

#[test]
fn a_watchers_p0_line_is_spooled_and_handed_to_the_flusher() {
    let d = testkit::TempDir::new("world-inbox-push");
    let (exe, calls) = flusher(d.path());
    let w = world(d.path(), exe);
    let line = blind_line();
    assert!(line.contains("P0 BLIND"), "the fixture is the watcher's P0 page: {line}");
    assert_eq!(w.inbox_append(&line).unwrap(), "appended, pushed");
    assert_eq!(std::fs::read_to_string(d.join("inbox.log")).unwrap(), format!("{line}\n"), "the line is recorded");
    assert_eq!(std::fs::read_to_string(d.join("inbox.log.push")).unwrap_or_default(), format!("{line}\n"), "the line is spooled for the push");
    assert_eq!(wait_for(&calls).trim(), "flush", "the flusher is started once, with its flush verb");
}

#[test]
fn a_line_that_does_not_match_is_recorded_and_not_pushed() {
    let d = testkit::TempDir::new("world-inbox-nopush");
    let (exe, calls) = flusher(d.path());
    let w = world(d.path(), exe);
    assert_eq!(w.inbox_append("2026-11-07T00:00:00Z [watch:asks] NEW ASK sp-abc12").unwrap(), "appended", "recorded, not pushed");
    assert!(d.join("inbox.log").exists());
    assert!(!d.join("inbox.log.push").exists(), "no spool for a plain line");
    assert!(!calls.exists(), "no flusher for a plain line");
}

#[test]
fn with_no_wake_configured_nothing_is_pushed() {
    let d = testkit::TempDir::new("world-inbox-nowake");
    let (exe, _) = flusher(d.path());
    let w = World { push_wake: String::new(), ..world(d.path(), exe) };
    assert_eq!(w.inbox_append(&blind_line()).unwrap(), "appended");
    assert!(!d.join("inbox.log.push").exists());
}
