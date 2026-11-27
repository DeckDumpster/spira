// covers: testenv/src/cap.rs
//! An over-cap kill, a real writer of the inbox, against a real broker.
#[macro_use]
extern crate spira_test_id;

use testenv::cap::{report, Breach, Proc};
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
