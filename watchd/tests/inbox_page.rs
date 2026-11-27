// covers: watchd/src/ops.rs
//! A watchd page, a real writer of the inbox, against a real broker.
#[macro_use]
extern crate spira_test_id;

use std::sync::OnceLock;

use watchd::ops::{Ops, Real};
use inbox_rules::{Bus, Sink};
use testkit::broker::Broker;

fn sink(d: &testkit::TempDir, broker: &Broker, name: &str) -> Sink {
    Sink {
        inbox: d.join("inbox.log"),
        cursor: d.join("inbox.cursor"),
        bus: Bus::At(broker.socket().to_path_buf()),
        wake: String::new(),
        pattern: String::new(),
        name: name.into(),
    }
}

/// The one line the inbox holds, which `concierge/inbox` must carry under `name`.
fn announced(broker: &Broker, sink: &Sink) -> String {
    let held = broker.retained("concierge/inbox");
    let [(_, value)] = held.as_slice() else { panic!("one retained value: {held:?}") };
    let value: serde_json::Value = serde_json::from_str(value).unwrap();
    let inbox = std::fs::read_to_string(&sink.inbox).unwrap();
    let line = inbox.strip_suffix('\n').expect("a finished line");
    assert_eq!(value["unread"], 1, "{value}");
    assert_eq!(value["oldest"], serde_json::json!(line));
    assert_eq!(broker.retained(&format!("publisher/{}", sink.name)).len(), 1, "named for the writer");
    line.to_string()
}

#[test_id(tsgdcfj)]
#[test]
fn a_line_that_does_not_push_is_announced_as_watchd() {
    let (broker, d) = (Broker::up(), testkit::TempDir::new("watchd-page"));
    let expected = sink(&d, &broker, "watchd");
    let real = Real { systemctl: "nonexistent/systemctl".into(), units: runtime::units::unreachable("down"), systemd_run: "nonexistent".into(), sink: OnceLock::from(expected.clone()) };

    real.inbox_append("[watch:sample] a quiet page").unwrap();

    let line = announced(&broker, &expected);
    assert!(line.ends_with("Z [watch:sample] a quiet page"), "{line}");
}
