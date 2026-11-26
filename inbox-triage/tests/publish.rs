// covers: inbox-triage/src/append.rs inbox-rules/src/lib.rs
//! The built `inbox-triage append`, a real writer of the inbox, against a real broker.
#[macro_use]
extern crate spira_test_id;

use std::process::{Command, Stdio};
use std::time::Duration;

use rumqttc::{Client, Event, MqttOptions, Packet, QoS, Transport};
use serde_json::{json, Value};
use testkit::broker::Broker;

/// Bounds each wait on the broker so a broken one fails the test instead of hanging.
const FAILSAFE: Duration = Duration::from_secs(30);

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

    let mut options = MqttOptions::new("watching", broker.socket().to_str().unwrap(), 0);
    options.set_transport(Transport::Unix);
    options.set_keep_alive(Duration::ZERO);
    let (client, mut connection) = Client::new(options, 8);
    client.subscribe("concierge/inbox", QoS::AtLeastOnce).unwrap();
    client.subscribe("publisher/inbox-triage", QoS::AtLeastOnce).unwrap();
    let mut next = || -> (String, Value) {
        loop {
            match connection.recv_timeout(FAILSAFE) {
                Ok(Ok(Event::Incoming(Packet::Publish(p)))) => return (p.topic, serde_json::from_slice(&p.payload).unwrap()),
                Ok(Ok(_)) => {}
                other => panic!("nothing retained arrived: {other:?}"),
            }
        }
    };
    let (topic, value) = next();
    assert_eq!(topic, "concierge/inbox");
    assert_eq!(value["unread"], 1, "{value}");
    assert!(value["oldest"].as_str().is_some_and(|l| l.ends_with("a new thing")), "{value}");
    assert_eq!(next(), ("publisher/inbox-triage".to_string(), json!({"state": "gone"})));
}
