// covers: watchdog/src/lib.rs
//! The scheduler watchdog's page, a real writer of the inbox, against a real broker.
#[macro_use]
extern crate spira_test_id;
#[path = "../../inbox-rules/tests/support/announced.rs"]
mod support;

use support::{announced, sink};
use watchdog::{InboxPager, Pager};
use testkit::broker::Broker;

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
