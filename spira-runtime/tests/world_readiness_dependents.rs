//! `World::readiness_dependents` with the control store configured, against a spira-lc double on
//! a socket that speaks `lc_call`'s protocol: the first pass takes the head and judges nothing,
//! a later one reads the changes since the carried revision, and a failed judgement fails the pass.

use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};

use runtime::RuntimeError;
use spira_runtime::watchers::readiness_dependents::Pass;
use spira_runtime::world::World;

struct Double {
    _dir: testkit::TempDir,
    _env: testkit::EnvGuard,
    world: World,
    sent: Arc<Mutex<Vec<String>>>,
}

fn double(keys: &'static [&'static str]) -> Double {
    let dir = testkit::TempDir::new("world-readiness-dependents");
    let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_LC_STORE_PATH", "/fixture/control.db")]);
    let env = testkit::env(&[("SPIRA_TOML", toml.to_str())]);
    let path = dir.path().join("lc.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let log = sent.clone();
    std::thread::spawn(move || {
        runtime::socket::answer(&listener, &|argv| {
            log.lock().unwrap().push(argv.join(" "));
            match argv[0].as_str() {
                "event-seq" => Ok(serde_json::json!({ "seq": 90, "keys": keys }).to_string()),
                other => Err(RuntimeError::new(format!("the double has no {other}"))),
            }
        })
    });
    Double { _dir: dir, _env: env, world: World { lc_socket: path, ..World::default() }, sent }
}

fn input(through: Option<u64>) -> String {
    serde_json::to_string(&Pass { through, changed: 0, keys: vec![] }).unwrap()
}

#[test]
fn the_first_pass_takes_the_head_as_its_baseline_and_judges_nothing() {
    let d = double(&[]);
    let out: Pass = serde_json::from_str(&d.world.readiness_look(&input(None)).unwrap()).unwrap();
    assert_eq!(out, Pass { through: Some(90), changed: 0, keys: vec![] });
    assert_eq!(*d.sent.lock().unwrap(), ["event-seq --since 18446744073709551615"]);
}

#[test]
fn a_later_pass_reads_since_the_carried_revision() {
    let d = double(&[]);
    d.world.readiness_look(&input(Some(41))).unwrap();
    assert_eq!(*d.sent.lock().unwrap(), ["event-seq --since 41"]);
}

#[test]
fn a_look_names_the_changed_keys_and_judges_nothing() {
    let d = double(&["sp-a", "sp-b"]);
    let out: Pass = serde_json::from_str(&d.world.readiness_look(&input(Some(41))).unwrap()).unwrap();
    assert_eq!(out, Pass { through: Some(90), changed: 0, keys: vec!["sp-a".into(), "sp-b".into()] });
    assert_eq!(*d.sent.lock().unwrap(), ["event-seq --since 41"]);
}

#[test]
fn a_pass_with_keys_reads_no_event_seq_and_its_failed_judgement_fails_the_pass() {
    let d = double(&[]);
    let keyed = serde_json::to_string(&Pass { through: Some(41), changed: 0, keys: vec!["sp-w".into()] }).unwrap();
    let err = d.world.readiness_dependents(&keyed).unwrap_err();
    assert!(err.contains("republish-dependents"), "{err}");
    assert!(d.sent.lock().unwrap().is_empty());
}

fn serve(replies: fn(&[String]) -> Option<String>) -> (testkit::TempDir, std::path::PathBuf, Arc<Mutex<Vec<String>>>) {
    let dir = testkit::TempDir::new("event-seq-head");
    let path = dir.path().join("lc.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let log = sent.clone();
    std::thread::spawn(move || {
        runtime::socket::answer(&listener, &|argv| {
            log.lock().unwrap().push(argv.join(" "));
            replies(argv).ok_or_else(|| RuntimeError::new("the double has no such verb"))
        })
    });
    (dir, path, sent)
}

#[test]
fn reconcile_content_reads_only_the_head_of_the_event_log() {
    let (dir, path, sent) = serve(|argv| match argv[0].as_str() {
        "reconcile-content" => Some(String::new()),
        "event-seq" => Some(serde_json::json!({ "seq": 7, "keys": [] }).to_string()),
        _ => None,
    });
    let lc = spira_store::lc_call::LcClient::socket(path);
    let out = spira_runtime::tasks::b::reconcile_content::observe(&lc, dir.path(), "").unwrap();
    assert!(out.contains("\"through_seq\":7"), "{out}");
    assert_eq!(*sent.lock().unwrap(), ["event-seq --head"]);
}

#[test]
fn reconcile_content_since_asks_the_store_from_the_cursor_and_reads_its_through_seq() {
    let (dir, path, sent) = serve(|argv| match argv[0].as_str() {
        "reconcile-content" => Some("would close sp-a (done)\nthrough-seq: 52\n".into()),
        _ => None,
    });
    let lc = spira_store::lc_call::LcClient::socket(path);
    let out = spira_runtime::tasks::b::reconcile_content::observe(&lc, dir.path(), "since:40").unwrap();
    assert!(out.contains("\"through_seq\":52") && out.contains("\"pass\":\"since\""), "{out}");
    assert_eq!(*sent.lock().unwrap(), ["reconcile-content --dry-run --since-seq 40"]);
}

#[test]
fn reconcile_content_full_reads_the_head_then_the_whole_dry_run() {
    let (dir, path, sent) = serve(|argv| match argv[0].as_str() {
        "reconcile-content" => Some(String::new()),
        "event-seq" => Some(serde_json::json!({ "seq": 9, "keys": [] }).to_string()),
        _ => None,
    });
    let lc = spira_store::lc_call::LcClient::socket(path);
    let out = spira_runtime::tasks::b::reconcile_content::observe(&lc, dir.path(), "full").unwrap();
    assert!(out.contains("\"through_seq\":9"), "{out}");
    assert_eq!(*sent.lock().unwrap(), ["event-seq --head", "reconcile-content --dry-run"]);
}

#[test]
fn reconcile_content_observe_names_an_unknown_input() {
    let (dir, path, _) = serve(|_| None);
    let lc = spira_store::lc_call::LcClient::socket(path);
    let err = spira_runtime::tasks::b::reconcile_content::observe(&lc, dir.path(), "weekly").unwrap_err();
    assert!(err.contains("weekly"), "{err}");
}

#[test]
fn reconcile_content_observe_reports_a_halted_world_without_asking_the_store() {
    let (dir, path, sent) = serve(|_| None);
    std::fs::write(dir.path().join("world.halted"), "").unwrap();
    let lc = spira_store::lc_call::LcClient::socket(path);
    let out = spira_runtime::tasks::b::reconcile_content::observe(&lc, dir.path(), "").unwrap();
    assert!(out.contains("\"world_halted\":true"), "{out}");
    assert!(sent.lock().unwrap().is_empty());
}
