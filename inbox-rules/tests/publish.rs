// covers: inbox-rules/src/lib.rs
//! `record` against a real broker.
#[macro_use]
extern crate spira_test_id;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use inbox_rules::{concierge_cursor, record, record_lines};
use ports::{SeededRng, VirtualClock};
use serde_json::{json, Value};
use spira_pub::{Options, PubError, Publisher};
use testkit::broker::Broker;

fn publisher(socket: &Path) -> impl FnOnce() -> Result<Publisher, PubError> + '_ {
    move || Publisher::with("t", Options::new(socket, Arc::new(VirtualClock::new(7_000)), Box::new(SeededRng::new(1))))
}

/// What a subscriber arriving now is handed for `concierge/inbox`.
fn retained(broker: &Broker) -> Value {
    let held = broker.retained("concierge/inbox");
    let [(_, payload)] = held.as_slice() else { panic!("one retained concierge/inbox value: {held:?}") };
    serde_json::from_str(payload).unwrap()
}

fn no_flusher() -> Result<PathBuf, String> {
    Err("this line pushes nothing".to_string())
}

#[test_id(tiuswvo)]
#[test]
fn record_publishes_how_many_lines_are_unread_and_the_oldest_of_them() {
    let broker = Broker::up();
    let dir = testkit::TempDir::new("inbox-pub");
    let inbox = dir.join("concierge-inbox.log");
    let cursor = concierge_cursor(dir.path().to_str().unwrap());
    std::fs::create_dir_all(cursor.parent().unwrap()).unwrap();
    let write = |line: &str| record(&inbox, line, "", "", no_flusher, &cursor, publisher(broker.socket())).unwrap();

    write("t1 one");
    write("t2 two");
    assert_eq!(retained(&broker), json!({"unread": 2, "oldest": "t1 one"}));

    // The reader's cursor, nowhere near the log: the first two are read.
    std::fs::write(&cursor, "2\n").unwrap();
    write("t3 three");
    assert_eq!(retained(&broker), json!({"unread": 1, "oldest": "t3 three"}));
}

#[test_id(tw7urkd)]
#[test]
fn with_the_broker_stopped_record_still_appends_and_returns_ok() {
    let mut broker = Broker::up();
    broker.stop();
    let dir = testkit::TempDir::new("inbox-nobroker");
    let inbox = dir.join("concierge-inbox.log");

    let sent = record(&inbox, "t1 one", "", "", no_flusher, &dir.join("no.cursor"), publisher(broker.socket()));

    assert_eq!(sent, Ok(false));
    assert_eq!(std::fs::read_to_string(&inbox).unwrap(), "t1 one\n");
}

#[test_id(tyys9rx)]
#[test]
fn a_batch_is_published_once_after_every_line_is_on_disk() {
    let broker = Broker::up();
    let dir = testkit::TempDir::new("inbox-batch");
    let inbox = dir.join("concierge-inbox.log");
    let made = std::cell::Cell::new(0);
    let counted = || {
        made.set(made.get() + 1);
        publisher(broker.socket())()
    };

    record_lines(&inbox, &["t1 one", "t2 two", "t3 three"], "", "", no_flusher, &dir.join("no.cursor"), counted).unwrap();

    assert_eq!(made.get(), 1);
    assert_eq!(retained(&broker), json!({"unread": 3, "oldest": "t1 one"}));
}
