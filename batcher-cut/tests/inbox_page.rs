// covers: batcher-cut/src/pace.rs
//! The batcher's round pages, a real writer of the inbox, against a real broker.
#[macro_use]
extern crate spira_test_id;

use batcher_cut::pace::{self, Files, Seams};
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

#[test_id(tkasbxp)]
#[test]
fn a_stale_pause_page_is_announced_as_the_batcher() {
    let (broker, d) = (Broker::up(), testkit::TempDir::new("batcher-stale-pause"));
    let inbox = sink(&d, &broker, "batcher");
    let files = Files::new(d.path(), "a");
    std::fs::create_dir_all(files.pause_file().parent().unwrap()).unwrap();
    std::fs::write(files.pause_file(), "red: the round was red\n").unwrap();
    let since = files.pause_since().unwrap();
    let stamp = |_| "T1".to_string();
    let sm = Seams { repo: "a", now: since + 3601, inbox: &inbox, stamp: &stamp, redrive_after: 1, pause_lift_after: 1, landing_head: None };

    pace::page_stale_pause(&files, &sm, 3600, || true).unwrap();

    let line = announced(&broker, &inbox);
    assert!(line.starts_with("T1 [watch:rounds] PAUSE UNATTENDED a: a red round, held 3601s"), "{line}");
}

#[test_id(tmvqt0y)]
#[test]
fn a_held_repo_page_is_announced_and_marked_as_said() {
    let (broker, d) = (Broker::up(), testkit::TempDir::new("batcher-held"));
    let inbox = sink(&d, &broker, "batcher");

    pace::page_held(d.path(), Some(&inbox), "a", &["sp-a", "sp-b"], "T1");

    assert_eq!(announced(&broker, &inbox), "T1 [watch:rounds] BATCHES HELD a: SPIRA_BATCHER_FORGE_ENABLE=0 with CERTIFIED work waiting: sp-a,sp-b");
    assert_eq!(std::fs::read_to_string(d.join("rounds-held/a")).unwrap(), "sp-a,sp-b");
}

#[test_id(t161eh5)]
#[test]
fn a_held_page_that_misses_the_inbox_is_not_marked_as_said() {
    let (broker, d) = (Broker::up(), testkit::TempDir::new("batcher-held-unwritable"));
    let mut inbox = sink(&d, &broker, "batcher");
    inbox.inbox = d.path().to_path_buf();

    pace::page_held(d.path(), Some(&inbox), "a", &["sp-a"], "T1");

    assert!(!d.join("rounds-held/a").exists());
}

#[test_id(t19k45b)]
#[test]
fn a_repeated_refusal_page_is_announced_as_the_batcher() {
    let (broker, d) = (Broker::up(), testkit::TempDir::new("batcher-repeat"));
    let inbox = sink(&d, &broker, "batcher");

    for now in [100, 101] {
        pace::stop_on_repeat(d.path(), Some(&inbox), "a", "b-1", "refused: no binaries", now, "T1");
    }

    let line = announced(&broker, &inbox);
    assert!(line.starts_with("T1 [watch:rounds] ROUNDS STOPPED a: 2 consecutive rounds refused"), "{line}");
}
