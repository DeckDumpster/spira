// covers: strand/src/check.rs
//! A strand escalation, a real writer of the inbox, against a real broker.
#[macro_use]
extern crate spira_test_id;
#[path = "../../inbox-rules/tests/support/announced.rs"]
mod support;

use support::{announced, sink};
use strand::check::{check_into, Classified};
use strand::config::Config;
use strand::model::{Disposition, PartRow, Row};
use testkit::broker::Broker;

#[test_id(trai8nh)]
#[test]
fn an_unrostered_escalation_is_announced_as_strand() {
    let (broker, d) = (Broker::up(), testkit::TempDir::new("strand-page"));
    let inbox = sink(&d, &broker, "strand");
    let cfg = Config { run: Some(d.path().to_path_buf()), strand_grace: 0, ..Config::test_fixture() };
    let row = Row::new("unrostered", "sp-x", Disposition::Escalate, "no persona".into(), "roster it".into());
    let rows = vec![PartRow { part: strand::unrostered::PART.into(), row }];
    let door = Err("no store in this test".to_string());

    let rc = check_into(&cfg, &door, &Ok(inbox.clone()), Classified { rows, watching: Vec::new(), live: Vec::new() }, false);

    assert_eq!(rc, 0);
    let line = announced(&broker, &inbox);
    assert!(line.ends_with("Z [strand] Spira: sp-x is stranded (unrostered): no persona — roster it"), "{line}");
}
