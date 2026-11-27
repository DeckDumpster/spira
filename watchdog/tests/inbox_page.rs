// covers: watchdog/src/lib.rs
//! The scheduler watchdog's page, a real writer of the inbox, against a real broker.
#[macro_use]
extern crate spira_test_id;

use watchdog::{InboxPager, Pager};
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

#[test_id(thdtk9f)]
#[test]
fn a_scheduler_down_page_is_announced_as_the_watchdog() {
    let (broker, d) = (Broker::up(), testkit::TempDir::new("watchdog-page"));
    let pager = InboxPager(sink(&d, &broker, "spira-watchdog"));

    pager.page("scheduler down", "no turn").unwrap();

    let line = announced(&broker, &pager.0);
    assert!(line.ends_with("Z scheduler down: no turn"), "{line}");
}

#[test_id(tlvi6x6)]
#[test]
fn with_the_broker_stopped_a_page_still_reaches_the_inbox() {
    let (mut broker, d) = (Broker::up(), testkit::TempDir::new("watchdog-nobroker"));
    let pager = InboxPager(sink(&d, &broker, "spira-watchdog"));
    broker.stop();

    assert_eq!(pager.page("scheduler down", "no turn"), Ok(()));

    let inbox = std::fs::read_to_string(&pager.0.inbox).unwrap();
    assert!(inbox.ends_with("Z scheduler down: no turn\n"), "{inbox}");
}
