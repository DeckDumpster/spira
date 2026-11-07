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
    serde_json::to_string(&Pass { through, changed: 0 }).unwrap()
}

#[test]
fn the_first_pass_takes_the_head_as_its_baseline_and_judges_nothing() {
    let d = double(&[]);
    let out: Pass = serde_json::from_str(&d.world.readiness_dependents(&input(None)).unwrap()).unwrap();
    assert_eq!(out, Pass { through: Some(90), changed: 0 });
    assert_eq!(*d.sent.lock().unwrap(), ["event-seq --since 18446744073709551615"]);
}

#[test]
fn a_later_pass_reads_since_the_carried_revision() {
    let d = double(&[]);
    d.world.readiness_dependents(&input(Some(41))).unwrap();
    assert_eq!(*d.sent.lock().unwrap(), ["event-seq --since 41"]);
}

#[test]
fn a_pass_whose_judgement_fails_fails_so_the_revision_is_not_taken() {
    let d = double(&["sp-w"]);
    let err = d.world.readiness_dependents(&input(Some(41))).unwrap_err();
    assert!(err.contains("republish-dependents"), "{err}");
}
