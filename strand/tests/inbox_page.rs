// covers: strand/src/check.rs
//! A strand escalation, a real writer of the inbox, against a real broker.
#[macro_use]
extern crate spira_test_id;

use strand::check::{check_into, Classified};
use strand::config::Config;
use strand::model::{Disposition, PartRow, Row};
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
