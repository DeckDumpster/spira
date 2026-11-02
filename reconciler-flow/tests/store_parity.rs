//! The stage queries read from the metrics store and give the answers the DuckDB queries over
//! the old `bead-stage.jsonl` gave: the same fixture goes down both paths.


use reconciler_flow::io::sentinel_wall;
use serde_json::json;
use tsd::store::{push_row, Reader};

const NOW: u64 = 1_792_700_000;
#[test]
fn the_sentinel_wall_is_the_summed_latest_pass_as_it_was() {
    let mut rows = Vec::new();
    for (pass, base) in [("p1", 1000u64), ("p2", 2000)] {
        for (i, check) in ["CHECK1", "CHECK2", "CHECK3"].iter().enumerate() {
            rows.push(json!({"ts": tsd::iso_utc(NOW - 5000 + base + i as u64), "host": "h", "family": "sentinel-phase", "pass": pass, "check": check, "secs": 10 * (i as u64 + 1)}));
        }
    }
    let store = spira_otel::fixture::FakeStore::start();
    for r in &rows {
        push_row(Some(store.addr()), "sentinel-phase", &r.to_string()).unwrap();
    }
    let got = Reader::new(Some(store.addr())).unwrap().rows("sentinel-phase", 0).unwrap();
    assert_eq!(sentinel_wall(&got), Some(60));
    let file_answer = {
        let latest = rows.iter().max_by_key(|r| r["ts"].as_str().unwrap().to_string()).unwrap()["pass"].clone();
        rows.iter().filter(|r| r["pass"] == latest).map(|r| r["secs"].as_u64().unwrap()).sum::<u64>()
    };
    assert_eq!(file_answer, 60);
}

#[test]
fn a_stopped_store_makes_every_stage_query_a_named_error() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    drop(l);
    let e = reconciler_flow::io::stage_rows(Some(&addr)).unwrap_err();
    assert!(e.starts_with("metrics-store-unreachable"), "{e}");
    let e = reconciler_flow::io::sentinel_pass_wall_seconds(Some(&addr), NOW, 3600).unwrap_err();
    assert!(e.starts_with("metrics-store-unreachable"), "{e}");
    let e = reconciler_flow::io::stage_rows(None).unwrap_err();
    assert!(e.starts_with("metrics-store-not-configured"), "{e}");
    let empty = spira_otel::fixture::FakeStore::start();
    let e = reconciler_flow::io::stage_rows(Some(empty.addr())).unwrap_err();
    assert!(e.starts_with("metrics-store-no-series"), "an empty store is not an all-clear: {e}");
}
