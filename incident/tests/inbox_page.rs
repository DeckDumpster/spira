// covers: incident/src/alarm.rs
//! An incident alarm, a real writer of the inbox, against a real broker.
#[macro_use]
extern crate spira_test_id;

use incident::alarm::note;
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

#[test_id(t912pxr)]
#[test]
fn a_due_alarm_is_announced_as_incident_and_remembered() {
    let (broker, d) = (Broker::up(), testkit::TempDir::new("incident-alarm"));
    let inbox = sink(&d, &broker, "incident");
    let state = d.join("alarms.tsv");

    assert_eq!(note(&inbox, &state, "incident:x", "the thing is down"), Ok(true));

    let line = announced(&broker, &inbox);
    assert!(line.ends_with("Z ALARM the thing is down [incident:x]"), "{line}");
    assert!(std::fs::read_to_string(&state).unwrap().starts_with("incident:x\t"));
}

#[test_id(t21o7w9)]
#[test]
fn an_alarm_that_misses_the_inbox_is_not_remembered() {
    let (broker, d) = (Broker::up(), testkit::TempDir::new("incident-alarm-unwritable"));
    let mut inbox = sink(&d, &broker, "incident");
    inbox.inbox = d.path().to_path_buf();
    let state = d.join("alarms.tsv");

    assert!(note(&inbox, &state, "incident:x", "t").is_err());

    assert!(!state.exists());
}
