// covers: watchd/src/ops.rs
//! A watchd page, a real writer of the inbox, against a real broker.
#[macro_use]
extern crate spira_test_id;
#[path = "../../inbox-rules/tests/support/announced.rs"]
mod support;

use support::{announced, sink};
use std::sync::OnceLock;

use watchd::ops::{Ops, Real};
use testkit::broker::Broker;

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
