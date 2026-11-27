// covers: incident/src/alarm.rs
//! An incident alarm, a real writer of the inbox, against a real broker.
#[macro_use]
extern crate spira_test_id;
#[path = "../../inbox-rules/tests/support/announced.rs"]
mod support;

use support::{announced, sink};
use incident::alarm::note;
use testkit::broker::Broker;

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
