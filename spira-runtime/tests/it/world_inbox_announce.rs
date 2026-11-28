//! A runtime watcher's inbox line is recorded and announced on `concierge/inbox`.

use inbox_rules::Bus;
use spira_runtime::world::World;

#[test_id(t209z08)]
#[test]
fn an_append_is_announced_once_past_watchds_cursor_as_the_inbox_writer() {
    let broker = testkit::broker::Broker::up();
    let d = testkit::TempDir::new("world-inbox-announce");
    let cursor = inbox_rules::concierge_cursor(d.path().to_str().unwrap());
    std::fs::create_dir_all(cursor.parent().unwrap()).unwrap();
    std::fs::write(d.join("inbox.log"), "t0 read already\n").unwrap();
    std::fs::write(&cursor, "1\n").unwrap();
    let w = World { run_dir: d.path().to_path_buf(), inbox: d.join("inbox.log"), inbox_bus: Bus::At(broker.socket().to_path_buf()), ..World::default() };

    w.inbox_append("t1 one\nt2 two").unwrap();

    let held = broker.retained("concierge/inbox");
    let [(_, value)] = held.as_slice() else { panic!("one retained value: {held:?}") };
    assert_eq!(serde_json::from_str::<serde_json::Value>(value).unwrap(), serde_json::json!({"unread": 2, "oldest": "t1 one"}));
    assert_eq!(broker.retained("publisher/spira-runtime-inbox").len(), 1, "named for the writer");
    assert!(broker.retained("publisher/spira-runtime").is_empty(), "the watch rows' resident publisher is not said gone by an append");
}
