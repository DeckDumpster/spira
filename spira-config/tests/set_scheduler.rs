//! `spira-config set|unset` says whether the running scheduler took the change, and
//! `get --running` reads what it holds. The scheduler is a socket answering the control wire.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

const KEY: &str = "SPIRA_MAX_DWARVES";

/// Answers `scheduler repin|running|status` for KEY: it holds `held` and `wanted` is the file's.
fn scheduler(sock: &Path, held: &str, wanted: &str, takes: bool) -> Arc<Mutex<Vec<Vec<String>>>> {
    let listener = UnixListener::bind(sock).unwrap();
    let seen: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let (log, held, wanted) = (seen.clone(), held.to_string(), wanted.to_string());
    std::thread::spawn(move || {
        let mut repinned = false;
        for conn in listener.incoming().flatten() {
            let mut line = String::new();
            BufReader::new(&conn).read_line(&mut line).unwrap();
            let argv: Vec<String> = serde_json::from_str(&line).unwrap();
            log.lock().unwrap().push(argv.clone());
            let body = match argv.get(1).map(String::as_str) {
                Some("repin") => {
                    repinned = takes;
                    serde_json::json!({ "state": "pending", "held": held, "wanted": wanted }).to_string()
                }
                Some("running") => (if repinned { &wanted } else { &held }).clone(),
                _ => serde_json::json!({ "repin": { "waits": "the batcher to finish the round it is driving" } }).to_string(),
            };
            writeln!(&conn, "{}", serde_json::json!({ "exit_code": 0, "len": body.len() })).unwrap();
            (&conn).write_all(body.as_bytes()).unwrap();
        }
    });
    seen
}

struct Box_ {
    dir: testkit::TempDir,
    toml: std::path::PathBuf,
    env: Vec<(&'static str, String)>,
}

fn setup(name: &str, sock: &Path) -> Box_ {
    let dir = testkit::TempDir::new(name);
    let env = spira_config::process::fixture_metrics_env(dir.path(), "127.0.0.1:9", &[("SPIRA_RUNTIME_SOCKET", &sock.display().to_string())]);
    let toml = std::path::PathBuf::from(&env.iter().find(|(k, _)| *k == "SPIRA_TOML").unwrap().1);
    Box_ { dir, toml, env }
}

fn run(b: &Box_, args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_spira-config"));
    c.env_clear().args(args);
    for (k, v) in &b.env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn a_set_the_scheduler_takes_says_it_re_pinned_the_key() {
    let d = testkit::TempDir::new("set-sched-ok");
    let sock = d.path().join("c.sock");
    let seen = scheduler(&sock, "0", "4", true);
    let b = setup("set-sched-ok-box", &sock);
    let o = run(&b, &["set", "spira.max_dwarves", "4", b.toml.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(out(&o).lines().any(|l| l == format!("scheduler re-pinned {KEY}")), "{}", out(&o));
    assert!(seen.lock().unwrap().iter().any(|a| a == &["scheduler", "repin", KEY]));
    drop(b.dir);
}

#[test]
fn a_set_the_scheduler_has_not_taken_says_what_it_still_holds_and_what_it_waits_for() {
    let d = testkit::TempDir::new("set-sched-wait");
    let sock = d.path().join("c.sock");
    scheduler(&sock, "0", "4", false);
    let b = setup("set-sched-wait-box", &sock);
    let o = run(&b, &["set", "spira.max_dwarves", "4", b.toml.to_str().unwrap()]);
    assert!(o.status.success());
    assert!(out(&o).contains(&format!("written; the scheduler still holds 0 for {KEY} and takes 4 at its next safe point (the batcher to finish the round it is driving)")), "{}", out(&o));
}

#[test]
fn get_running_prints_the_value_the_scheduler_holds_and_not_the_files() {
    let d = testkit::TempDir::new("get-running");
    let sock = d.path().join("c.sock");
    scheduler(&sock, "0", "4", false);
    let b = setup("get-running-box", &sock);
    let o = run(&b, &["get", "--running", KEY]);
    assert_eq!(out(&o).trim(), "0");
}

#[test]
fn a_set_with_no_scheduler_listening_still_writes_and_says_so() {
    let d = testkit::TempDir::new("set-sched-down");
    let b = setup("set-sched-down-box", &d.path().join("nobody.sock"));
    let o = run(&b, &["set", "spira.max_dwarves", "4", b.toml.to_str().unwrap()]);
    assert!(o.status.success());
    assert!(out(&o).contains("written; the scheduler is not answering"), "{}", out(&o));
    assert!(std::fs::read_to_string(&b.toml).unwrap().contains("max_dwarves = 4") || std::fs::read_to_string(&b.toml).unwrap().contains("max_dwarves = \"4\""));
}

#[test]
fn a_write_typed_against_a_staged_release_says_nothing_about_the_scheduler() {
    let d = testkit::TempDir::new("set-sched-release");
    let sock = d.path().join("c.sock");
    let seen = scheduler(&sock, "0", "4", true);
    let b = setup("set-sched-release-box", &sock);
    let home = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    let o = run(&b, &["set", "--release", home.to_str().unwrap(), "spira.max_dwarves", "4", b.toml.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!out(&o).contains("scheduler") && seen.lock().unwrap().is_empty(), "{}", out(&o));
}

#[test]
fn an_unset_reports_the_scheduler_the_same_way() {
    let d = testkit::TempDir::new("unset-sched");
    let sock = d.path().join("c.sock");
    scheduler(&sock, "4", "0", true);
    let b = setup("unset-sched-box", &sock);
    assert!(run(&b, &["set", "spira.max_dwarves", "4", b.toml.to_str().unwrap()]).status.success());
    let o = run(&b, &["unset", "spira.max_dwarves", b.toml.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(out(&o).lines().any(|l| l == format!("scheduler re-pinned {KEY}")), "{}", out(&o));
}
