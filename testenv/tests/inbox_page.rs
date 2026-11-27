// covers: testenv/src/cap.rs
//! An over-cap kill, a real writer of the inbox, against a real broker.
#[macro_use]
extern crate spira_test_id;
#[path = "../../inbox-rules/tests/support/announced.rs"]
mod support;

use support::{announced, sink};
use testenv::cap::{report, Breach, Proc};
use testkit::broker::Broker;

#[test_id(tp7thcn)]
#[test]
fn an_over_cap_kill_is_announced_as_testenv_cap() {
    let (broker, d) = (Broker::up(), testkit::TempDir::new("testenv-cap-page"));
    let inbox = sink(&d, &broker, "testenv-cap");
    let proc = Proc { pid: 7, start_ticks: 1, argv: vec!["testenv".into(), "run".into()], age_secs: 100, ledgered: false };

    report(&inbox, &Breach { proc, tier: "suite", cap: 50 });

    let line = announced(&broker, &inbox);
    assert!(line.ends_with("Z testenv over-cap killed: pid 7 tier=suite wall=100s cap=50s cmd=testenv run"), "{line}");
}
