//! `World::flow_snapshot` brings the exporter up to the lifecycle log by asking spira-lc only for
//! the log's head revision, never for a walk of every event.

use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};

use runtime::RuntimeError;
use spira_runtime::world::World;

#[test]
fn the_flow_export_asks_for_the_event_head_and_not_the_whole_log() {
    let dir = testkit::TempDir::new("world-flow-export");
    let sock = dir.path().join("lc.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let asked: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let seen = asked.clone();
    std::thread::spawn(move || {
        runtime::socket::answer(&listener, &|argv| {
            seen.lock().unwrap().push(argv.to_vec());
            match argv.iter().map(String::as_str).collect::<Vec<_>>()[..] {
                ["event-seq", "--head"] => Ok(r#"{"seq":7}"#.into()),
                _ => Err(RuntimeError::new("the double has no such verb")),
            }
        })
    });
    let run = dir.path().join("run");
    std::fs::create_dir_all(run.join("tsd")).unwrap();
    std::fs::write(run.join("tsd/.bead-stage-checkpoint-lifecycle.json"), r#"{"last_seq":7}"#).unwrap();
    let w = World { lc_socket: sock, run_dir: run, ..World::default() };
    let snap: serde_json::Value = serde_json::from_str(&w.flow_snapshot(r#"{"now":1700000000,"window_hours":24.0,"baseline_hours":168.0,"rework_window_hours":24.0,"sentinel_timer_secs":300}"#).unwrap_or_else(|e| panic!("{e}"))).unwrap();
    assert_eq!(snap["export"], serde_json::json!({"Ok": null}), "{snap}");
    let asked = asked.lock().unwrap();
    assert!(asked.iter().any(|a| a == &["event-seq", "--head"]), "{asked:?}");
    assert!(asked.iter().all(|a| a[0] != "event-seq" || a.contains(&"--head".to_string())), "{asked:?}");
}

const PARAMS: &str = r#"{"now":1700000000,"window_hours":24.0,"baseline_hours":168.0,"rework_window_hours":24.0,"sentinel_timer_secs":300}"#;

/// A pass over a log whose head is 7 and whose checkpoint is `exported`, with an exporter that logs
/// each mode it is run in and fails `check`; the snapshot's `export` and the modes run.
fn pass_with_failing_check(exported: u64) -> (serde_json::Value, String) {
    let dir = testkit::TempDir::new("world-flow-check");
    let sock = dir.path().join("lc.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    std::thread::spawn(move || {
        runtime::socket::answer(&listener, &|argv| match argv.iter().map(String::as_str).collect::<Vec<_>>()[..] {
            ["event-seq", "--head"] => Ok(r#"{"seq":7}"#.into()),
            _ => Err(RuntimeError::new("the double has no such verb")),
        })
    });
    let run = dir.path().join("run");
    std::fs::create_dir_all(run.join("tsd")).unwrap();
    std::fs::write(run.join("tsd/.bead-stage-checkpoint-lifecycle.json"), format!(r#"{{"last_seq":{exported}}}"#)).unwrap();
    let ran = dir.path().join("ran");
    let bin = dir.path().join("fake-export");
    testkit::write_exe(&bin, &format!("#!/bin/sh\necho \"$1\" >> {}\n[ \"$1\" = check ] && {{ echo 'trails the log' >&2; exit 1; }}\nexit 0\n", ran.display()));
    let w = World { lc_socket: sock, run_dir: run, export_bin: bin.display().to_string(), ..World::default() };
    let snap: serde_json::Value = serde_json::from_str(&w.flow_snapshot(PARAMS).unwrap_or_else(|e| panic!("{e}"))).unwrap();
    (snap, std::fs::read_to_string(ran).unwrap_or_default())
}

#[test]
fn a_current_checkpoint_still_runs_the_check_and_a_failed_check_blinds_the_pass() {
    let (snap, ran) = pass_with_failing_check(7);
    assert_eq!(ran, "check\n", "the exporter is not launched when the log is not ahead of it");
    assert!(snap["export"]["Err"].as_str().is_some_and(|e| e.contains("check: exit 1") && e.contains("trails the log")), "{snap}");
}

#[test]
fn a_run_that_exported_the_log_is_still_refused_when_the_check_fails() {
    let (snap, ran) = pass_with_failing_check(3);
    assert_eq!(ran, "lifecycle\ncheck\n");
    assert!(snap["export"]["Err"].is_string(), "{snap}");
}
