// covers: inbox-triage/src/append.rs inbox-rules/src/lib.rs
//! The built `inbox-triage append`, a real writer of the inbox, against a real broker.
#[macro_use]
extern crate spira_test_id;

use std::process::{Command, Stdio};

use serde_json::{json, Value};
use testkit::broker::Broker;

#[test_id(tjzfbv2)]
#[test]
fn append_publishes_the_unread_count_past_watchds_cursor_under_its_own_name() {
    let broker = Broker::up();
    let dir = testkit::TempDir::new("triage-pub");
    let (run, inbox) = (dir.join("run"), dir.join("elsewhere/inbox.log"));
    std::fs::create_dir_all(run.join("watchd")).unwrap();
    std::fs::create_dir_all(dir.join("elsewhere")).unwrap();
    std::fs::write(&inbox, "t0 read already\n").unwrap();
    std::fs::write(inbox_rules::concierge_cursor(run.to_str().unwrap()), "1\n").unwrap();
    let declare = [
        ("SPIRA_BROKER_SOCKET", broker.socket().to_str().unwrap()),
        ("SPIRA_RUN", run.to_str().unwrap()),
        ("SPIRA_CONCIERGE_INBOX", inbox.to_str().unwrap()),
        ("SPIRA_CONCIERGE_PUSH_WAKE", ""),
    ];
    let env = spira_config::process::fixture_metrics_env(&dir, "", &declare);

    let out = Command::new(testkit::bin_exe("inbox-triage")).args(["append", "a new thing"]).env_clear().envs(env).stdin(Stdio::null()).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let held = |topic: &str| -> Value {
        let held = broker.retained(topic);
        let [(_, payload)] = held.as_slice() else { panic!("one retained {topic} value: {held:?}") };
        serde_json::from_str(payload).unwrap()
    };
    let value = held("concierge/inbox");
    assert_eq!(value["unread"], 1, "{value}");
    assert!(value["oldest"].as_str().is_some_and(|l| l.ends_with("a new thing")), "{value}");
    assert_eq!(held("publisher/inbox-triage"), json!({"state": "gone"}));
}

/// An environment declaring `inbox` and the broker, with no concierge push key at all.
fn env_without_push_keys(dir: &std::path::Path, broker: &Broker, inbox: &std::path::Path) -> Vec<(&'static str, String)> {
    let run = dir.join("run");
    let declare = [
        ("SPIRA_BROKER_SOCKET", broker.socket().to_str().unwrap()),
        ("SPIRA_RUN", run.to_str().unwrap()),
        ("SPIRA_CONCIERGE_INBOX", inbox.to_str().unwrap()),
    ];
    let env = spira_config::process::fixture_metrics_env(dir, "", &declare);
    let toml = &env.iter().find(|(k, _)| *k == "SPIRA_TOML").unwrap().1;
    let kept: String = std::fs::read_to_string(toml).unwrap().lines().filter(|l| !l.starts_with("concierge_push_")).map(|l| format!("{l}\n")).collect();
    std::fs::write(toml, kept).unwrap();
    env
}

#[test_id(t4p6dim)]
#[test]
fn an_urgent_append_leaves_stderr_empty_and_no_spool_and_is_announced() {
    let broker = Broker::up();
    let dir = testkit::TempDir::new("triage-urgent");
    let inbox = dir.join("inbox.log");
    let env = env_without_push_keys(&dir, &broker, &inbox);

    let out = Command::new(testkit::bin_exe("inbox-triage")).args(["append", "x P0 down"]).env_clear().envs(env).stdin(Stdio::null()).output().unwrap();

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
    assert!(!dir.join("inbox.log.push").exists(), "no spool beside the inbox");
    assert!(std::fs::read_to_string(&inbox).unwrap().ends_with(" x P0 down\n"));
    let held = broker.retained("concierge/inbox");
    let [(_, payload)] = held.as_slice() else { panic!("one retained value: {held:?}") };
    assert_eq!(serde_json::from_str::<Value>(payload).unwrap()["unread"], 1, "{payload}");
}

#[test_id(tpokhbb)]
#[test]
fn flush_is_refused_as_an_unknown_verb() {
    let broker = Broker::up();
    let dir = testkit::TempDir::new("triage-flush");
    let inbox = dir.join("inbox.log");
    let declare = [("SPIRA_CONCIERGE_INBOX", inbox.to_str().unwrap()), ("SPIRA_BROKER_SOCKET", broker.socket().to_str().unwrap())];
    let env = spira_config::process::fixture_metrics_env(&dir, "", &declare);

    let out = Command::new(testkit::bin_exe("inbox-triage")).arg("flush").env_clear().envs(env).stdin(Stdio::null()).output().unwrap();

    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown verb \"flush\""), "{}", String::from_utf8_lossy(&out.stderr));
}
