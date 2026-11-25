//! `World::flow_snapshot` brings the exporter up to the lifecycle log by asking spira-lc only for
//! the log's head revision, never for a walk of every event.

#[macro_use]
extern crate spira_test_id;
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};

use runtime::RuntimeError;
use spira_runtime::world::World;

#[test_id(t5qrh17)]
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

/// A one-shot-per-request metrics store answering every export with one `bead-stage` sample at `stage_ts` seconds.
fn metrics_with_newest_stage(stage_ts: i64) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut buf = [0u8; 4096];
            let _ = std::io::Read::read(&mut stream, &mut buf);
            let body = format!("{{\"metric\":{{\"__name__\":\"spira_tsd_value\"}},\"values\":[1],\"timestamps\":[{}]}}\n", stage_ts * 1000);
            let _ = std::io::Write::write_all(&mut stream, format!("HTTP/1.0 200 OK\r\n\r\n{body}").as_bytes());
        }
    });
    addr
}

/// A pass over a log whose head is 7 and whose checkpoint is `exported`, whose newest event is at 1700000000 and
/// whose newest `bead-stage` sample trails it by `trail_secs`, with an exporter that logs each mode it is run in;
/// the snapshot's `export` and the modes run.
fn pass_trailing_by(exported: u64, trail_secs: i64) -> (serde_json::Value, String) {
    let dir = testkit::TempDir::new("world-flow-check");
    let sock = dir.path().join("lc.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    std::thread::spawn(move || {
        runtime::socket::answer(&listener, &|argv| match argv.iter().map(String::as_str).collect::<Vec<_>>()[..] {
            ["event-seq", "--head"] => Ok(r#"{"seq":7}"#.into()),
            ["audits", "--newest"] => Ok(r#"{"newest_at":1700000000}"#.into()),
            _ => Err(RuntimeError::new("the double has no such verb")),
        })
    });
    let run = dir.path().join("run");
    std::fs::create_dir_all(run.join("tsd")).unwrap();
    std::fs::write(run.join("tsd/.bead-stage-checkpoint-lifecycle.json"), format!(r#"{{"last_seq":{exported}}}"#)).unwrap();
    let ran = dir.path().join("ran");
    let bin = dir.path().join("fake-export");
    testkit::write_exe(&bin, &format!("#!/bin/sh\necho \"$1\" >> {}\nexit 0\n", ran.display()));
    let w = World {
        lc_socket: sock,
        run_dir: run,
        export_bin: bin.display().to_string(),
        metrics_endpoint: Some(metrics_with_newest_stage(1_700_000_000 - trail_secs)),
        ..World::default()
    };
    let snap: serde_json::Value = serde_json::from_str(&w.flow_snapshot(PARAMS).unwrap_or_else(|e| panic!("{e}"))).unwrap();
    (snap, std::fs::read_to_string(ran).unwrap_or_default())
}

#[test_id(tudl1oh)]
#[test]
fn a_current_checkpoint_starts_no_exporter_and_a_trailing_family_blinds_the_pass() {
    let (snap, ran) = pass_trailing_by(7, 601);
    assert_eq!(ran, "", "no program is started when the log is not ahead of the checkpoint");
    assert!(snap["export"]["Err"].as_str().is_some_and(|e| e.contains("601s older")), "{snap}");
}

#[test_id(t06oxug)]
#[test]
fn a_current_checkpoint_and_a_family_within_the_limit_is_ok_with_no_program() {
    let (snap, ran) = pass_trailing_by(7, 60);
    assert_eq!(ran, "");
    assert_eq!(snap["export"], serde_json::json!({"Ok": null}), "{snap}");
}

#[test_id(t8cvgpz)]
#[test]
fn a_run_that_exported_the_log_is_still_refused_when_the_family_trails() {
    let (snap, ran) = pass_trailing_by(3, 900);
    assert_eq!(ran, "lifecycle\n");
    assert!(snap["export"]["Err"].is_string(), "{snap}");
}
