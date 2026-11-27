// covers: perf-watch/src/lib.rs
//! A perf alarm, a real writer of the inbox, against a real broker.
#[macro_use]
extern crate spira_test_id;

use perf_watch::{append_inbox, inbox_line};
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

#[test_id(tc1wype)]
#[test]
fn a_perf_alarm_is_announced_as_perf_watch() {
    let (broker, d) = (Broker::up(), testkit::TempDir::new("perf-watch-page"));
    let inbox = sink(&d, &broker, "perf-watch");

    append_inbox(&inbox, &inbox_line(0, "PERF SLOW x 9ms"));

    assert_eq!(announced(&broker, &inbox), "1970-01-01T00:00:00Z [watch:perf-watch] PERF SLOW x 9ms");
}
