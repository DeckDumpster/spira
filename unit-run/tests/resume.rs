//! A listener restarted against the metrics store resumes from what the store holds, and rows the
//! store refused are held and delivered in order once it answers again.

use serde_json::json;
use unit_run::watch::last_ended_us;
use unit_run::{written_ids, Backlog};

fn row(inv: &str, ended: &str) -> String {
    json!({"ts": ended, "host": "h", "family": "unit-run-dbus", "unit": "x.service", "invocation_id": inv, "ended_at": ended, "result": "success"}).to_string()
}

#[test]
fn the_resume_point_and_the_dedupe_set_come_from_the_store() {
    let store = spira_otel::fixture::FakeStore::start();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    for (inv, back) in [("a", 300u64), ("b", 200), ("c", 100)] {
        tsd::store::push_row(Some(store.addr()), "unit-run-dbus", &row(inv, &tsd::iso_utc(now - back))).unwrap();
    }
    let rows = tsd::store::Reader::new(Some(store.addr())).unwrap().rows_or_empty("unit-run-dbus", now - 3600).unwrap();
    assert_eq!(written_ids(&rows).into_iter().collect::<Vec<_>>(), ["a", "b", "c"]);
    assert_eq!(last_ended_us(&rows), Some((now - 100) * 1_000_000));
}

#[test]
fn rows_a_stopped_store_refused_are_delivered_in_order_when_it_returns() {
    let first = spira_otel::fixture::FakeStore::start();
    let addr = first.addr().to_string();
    drop(first);
    std::thread::sleep(std::time::Duration::from_millis(50));
    let pusher = tsd::store::Pusher::new(Some(addr.clone())).suspending_for(0);
    let mut backlog = Backlog::new(3);
    let field = |inv: &str| vec![("invocation_id".to_string(), json!(inv)), ("unit".to_string(), json!("x.service"))];
    for (i, inv) in ["a", "b", "c", "d"].iter().enumerate() {
        let (dropped, stored) = backlog.offer(&pusher, "unit-run-dbus", tsd::iso_utc(1_792_000_000 + i as u64), field(inv));
        assert!(stored.is_err(), "the store is stopped: {stored:?}");
        assert_eq!(dropped, i == 3, "only the fourth row overflows a backlog of three");
    }
    assert_eq!(backlog.len(), 3);

    let back = spira_otel::fixture::FakeStore::start_on(&addr);
    let (_, stored) = backlog.offer(&pusher, "unit-run-dbus", tsd::iso_utc(1_792_000_010), field("e"));
    assert_eq!(stored, Ok(()));
    assert!(backlog.is_empty());
    let rows = tsd::store::Reader::new(Some(back.addr())).unwrap().rows("unit-run-dbus", 0).unwrap();
    let ids: Vec<&str> = rows.iter().filter_map(|r| r["invocation_id"].as_str()).collect();
    assert_eq!(ids, ["c", "d", "e"], "the oldest was dropped, the rest kept in order");
}
