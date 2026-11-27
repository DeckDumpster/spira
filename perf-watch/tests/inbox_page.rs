// covers: perf-watch/src/lib.rs
//! A perf alarm, a real writer of the inbox, against a real broker.
#[macro_use]
extern crate spira_test_id;
#[path = "../../inbox-rules/tests/support/announced.rs"]
mod support;

use support::{announced, sink};
use perf_watch::{append_inbox, inbox_line};
use testkit::broker::Broker;

#[test_id(tc1wype)]
#[test]
fn a_perf_alarm_is_announced_as_perf_watch() {
    let (broker, d) = (Broker::up(), testkit::TempDir::new("perf-watch-page"));
    let inbox = sink(&d, &broker, "perf-watch");

    append_inbox(&inbox, &inbox_line(0, "PERF SLOW x 9ms"));

    assert_eq!(announced(&broker, &inbox), "1970-01-01T00:00:00Z [watch:perf-watch] PERF SLOW x 9ms");
}
