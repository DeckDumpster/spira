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
