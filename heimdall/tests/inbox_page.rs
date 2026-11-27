// covers: heimdall/src/pass.rs
//! A heimdall page, a real writer of the inbox, against a real broker.
#[macro_use]
extern crate spira_test_id;

use heimdall::pass::{page_inbox, PassPaths};
use inbox_rules::Bus;
use serde_json::{json, Value};
use testkit::broker::Broker;

#[test_id(thaq9r9)]
#[test]
fn a_page_is_announced_past_watchds_cursor_as_heimdall() {
    let broker = Broker::up();
    let d = testkit::TempDir::new("heimdall-page");
    let inbox_cursor = inbox_rules::concierge_cursor(d.path().to_str().unwrap());
    std::fs::create_dir_all(inbox_cursor.parent().unwrap()).unwrap();
    std::fs::write(d.join("inbox.log"), "t0 read already\n").unwrap();
    std::fs::write(&inbox_cursor, "1\n").unwrap();
    let paths = PassPaths {
        fallback: d.join("fallback"),
        heartbeat: d.join("heartbeat"),
        inbox: d.join("inbox.log"),
        inbox_cursor,
        inbox_bus: Bus::At(broker.socket().to_path_buf()),
    };

    page_inbox(&paths, 0, "stalled", "BEADS sp-a").unwrap();

    let held = broker.retained("concierge/inbox");
    let [(_, value)] = held.as_slice() else { panic!("one retained value: {held:?}") };
    let value: Value = serde_json::from_str(value).unwrap();
    assert_eq!(value["unread"], 1, "{value}");
    assert_eq!(value["oldest"], json!("1970-01-01T00:00:00Z [heimdall] stalled; beads sp-a"));
    assert_eq!(broker.retained("publisher/heimdall").len(), 1, "named for the writer");
}
