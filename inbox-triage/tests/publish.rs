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
    std::fs::write(inbox_rules::concierge_cursor(&run), "1\n").unwrap();
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
