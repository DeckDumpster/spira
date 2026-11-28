//! `incident.file`'s two runners against a store socket, under a pinned config: what the look
//! says of a row, and what a pass does to it. A process of its own, for the pinned config.

#[macro_use]
extern crate spira_test_id;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};

use runtime::event::EffectSpec;
use serde_json::{json, Value};
use spira_runtime::tasks::f::incident_file::*;
use spira_runtime::Effects;
use spira_store::incident::{raise, Payload, Row, State};

/// A store that serves one refused row to `incidents` and cannot tell anything else.
fn store(sock: &std::path::Path, row: Row) -> Arc<Mutex<Vec<Vec<String>>>> {
    let listener = std::os::unix::net::UnixListener::bind(sock).unwrap();
    let seen: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            let v: Value = serde_json::from_str(line.trim()).unwrap();
            let argv: Vec<String> = v.get("argv").unwrap_or(&v).as_array().unwrap().iter().map(|a| a.as_str().unwrap().to_string()).collect();
            let mut keyed = serde_json::to_value(&row).unwrap();
            keyed["key"] = json!("incident:a");
            let (code, out) = match (argv[0].as_str(), argv.get(1).map(String::as_str)) {
                ("incidents", Some("list")) => (0, json!([keyed]).to_string()),
                ("incidents", Some("claim")) => {
                    keyed["state"] = json!("filing");
                    (0, keyed.to_string())
                }
                ("incidents", Some("retry")) => (0, json!({ "retried": 1 }).to_string()),
                ("incidents", Some("record")) => (0, "{}".into()),
                _ => (2, "cannot tell: this store keeps only incidents".into()),
            };
            log.lock().unwrap().push(argv);
            let mut w = &stream;
            let _ = write!(w, "{{\"exit_code\":{code},\"len\":{}}}\n{out}", out.len());
        }
    });
    seen
}

#[test_id(tssueg4)]
#[test]
fn the_look_judges_each_row_under_the_pinned_map_and_a_pass_records_how_its_filing_ended() {
    let dir = testkit::TempDir::new("incident-file-runners");
    let map = dir.join("repos.psv");
    std::fs::write(&map, "spira|/src|queue.local|main|||plan,incident\n").unwrap();
    let run = dir.join("run");
    std::fs::create_dir_all(&run).unwrap();
    let paths = [map.to_str().unwrap().to_string(), run.to_str().unwrap().to_string()];
    let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_REPO_MAP", &paths[0]), ("SPIRA_RUN", &paths[1]), ("SPIRA_HOME_REPO", "spira"), ("SPIRA_DB", "db")]);
    let _env = testkit::env(&[("SPIRA_TOML", Some(toml.to_str().unwrap()))]);

    let payload = Payload { reference: "incident:a".into(), title: "t".into(), body: "SECRET PAYLOAD".into(), repo: Some("spira".into()), labels: "spira,incident".into(), ..Payload::default() };
    let row = Row { state: State::Refused, reason: Some("walled".into()), lanes: Some("plan".into()), ..raise(None, payload, 10) };
    let sock = dir.join("lc.sock");
    let seen = store(&sock, row);
    let world = spira_runtime::world::World { lc_socket: sock, run_dir: run.clone(), ..Default::default() };
    let effects = Effects { world, ..Default::default() };
    let config: BTreeMap<String, String> = [(MAP_KEY.to_string(), paths[0].clone()), (incident::intake::BATCH_KEY.to_string(), "5".to_string())].into();
    let spec = |name: &str, config: &BTreeMap<String, String>| EffectSpec::probe(name, serde_json::to_string(config).unwrap(), 1_000);

    let looked = run_observe(&effects, "k", &spec(OBSERVE, &config)).unwrap();
    assert!(!looked.contains("SECRET PAYLOAD"), "the look carries no payload: {looked}");
    let snap: Snapshot = serde_json::from_str(&looked).unwrap();
    assert_eq!((snap.rows.len(), snap.rows[0].lanes_now.as_deref()), (1, Some("incident,plan")));
    assert_eq!(decide(&snap, 0), Decision::File);

    let passed: Passed = serde_json::from_str(&run_file(&effects, "k", &spec(RUN, &config)).unwrap()).unwrap();
    assert_eq!((passed.claimed, passed.failed), (1, 1), "{passed:?}");
    let calls = seen.lock().unwrap();
    let record = calls.iter().find(|a| a.get(1).map(String::as_str) == Some("record")).expect("the end is recorded");
    assert_eq!(record[2..4], ["incident:a", "unread"], "{record:?}");
    drop(calls);

    let mut no_batch = config.clone();
    no_batch.insert(incident::intake::BATCH_KEY.into(), "0".into());
    assert!(run_file(&effects, "k", &spec(RUN, &no_batch)).unwrap_err().contains(incident::intake::BATCH_KEY));
}
