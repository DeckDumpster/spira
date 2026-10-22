//! The stage queries read from the metrics store and give the answers the DuckDB queries over
//! the old `bead-stage.jsonl` gave: the same fixture goes down both paths.

use std::process::Command;

use reconciler_flow::io::{dwell_metrics, dwell_regression_metrics, rework_metrics, round_health_metrics, sentinel_wall, velocity_metrics, waiting_to_land};
use serde_json::{json, Value};
use tsd::store::{push_row, Reader};

const NOW: u64 = 1_792_700_000;
const NOW_SQL: &str = "TIMESTAMP '2026-10-22 20:13:20'";
const WINDOW: f64 = 6.0;
const BASELINE: f64 = 24.0;

struct Lcg(u64);

impl Lcg {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

fn row(ts: u64, seq: u64, machine: &str, key: &str, from: &str, to: &str, applied: bool, reason: Option<&str>) -> Value {
    let mut v = json!({"ts": tsd::iso_utc(ts), "host": "h", "family": "bead-stage", "seq": seq, "machine": machine, "key": key, "from_state": from, "to_state": to, "applied": applied});
    if let Some(r) = reason {
        v["reason"] = json!(r);
    }
    v
}

fn fixture() -> Vec<Value> {
    let mut rng = Lcg(7);
    let mut rows: Vec<(u64, Value)> = Vec::new();
    let path = ["READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"];
    for b in 0..70u64 {
        let mut t = NOW - 40 * 3600 + rng.next(38 * 3600);
        let key = format!("sp-{b}");
        let stop = 1 + rng.next(6) as usize;
        let mut from = "NONE";
        for (i, st) in path.iter().enumerate().take(stop + 1) {
            rows.push((t, row(t, 0, "bead", &key, from, st, i != 2 || rng.next(5) != 0, None)));
            from = st;
            t += 60 + rng.next(4000);
            if *st == "SUBMITTED" && rng.next(4) == 0 {
                rows.push((t, row(t, 0, "bead", &key, st, "REWORK", true, Some(["red", "conflict", "review"][rng.next(3) as usize]))));
                from = "REWORK";
                t += 600 + rng.next(3000);
            }
        }
    }
    for b in 0..14u64 {
        let mut t = NOW - 38 * 3600 + rng.next(37 * 3600);
        let key = format!("batch-{b}");
        let mut from = "NONE";
        for st in ["OPEN", "CI_RUNNING", "ATTRIBUTING", "REBUILDING", "CI_RUNNING", "GREEN", "LANDED"].iter().take(3 + rng.next(5) as usize) {
            rows.push((t, row(t, 0, "batch", &key, from, st, true, None)));
            from = st;
            t += 120 + rng.next(2500);
        }
    }
    rows.retain(|r| r.0 < NOW - 30);
    rows.sort_by_key(|r| r.0);
    rows.into_iter().enumerate().map(|(i, (_, mut v))| {
        v["seq"] = json!(i + 1);
        v
    })
    .collect()
}

fn duckdb(file: &std::path::Path, sql: &str) -> Option<Vec<Value>> {
    let out = Command::new("duckdb").args(["-json", "-c", sql]).output().ok()?;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    let _ = file;
    Some(serde_json::from_str(text.trim()).unwrap_or_default())
}

fn f(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}

fn close(a: f64, b: f64, what: &str) {
    assert!((a - b).abs() < 1e-6, "{what}: store {a} vs file {b}");
}

#[test]
fn every_stage_query_gives_the_answer_the_file_query_gave() {
    if Command::new("duckdb").arg("--version").output().is_err() {
        eprintln!("skipped: no duckdb on PATH");
        return;
    }
    let rows = fixture();
    let dir = testkit::TempDir::new("flow-parity");
    let file = dir.join("bead-stage.jsonl");
    std::fs::write(&file, rows.iter().map(|r| format!("{r}\n")).collect::<String>()).unwrap();
    let p = file.to_string_lossy().to_string();

    let store = spira_otel::fixture::FakeStore::start();
    for r in &rows {
        push_row(Some(store.addr()), "bead-stage", &r.to_string()).unwrap();
    }
    let got = Reader::new(Some(store.addr())).unwrap().rows("bead-stage", 0).unwrap();
    assert_eq!(got.len(), rows.len(), "every row stored");

    // waiting_to_land
    let old = duckdb(&file, &format!("WITH latest AS (SELECT to_state, row_number() OVER (PARTITION BY key ORDER BY seq DESC) AS rn FROM read_ndjson_auto('{p}') WHERE machine = 'bead' AND applied) SELECT count(*) AS n FROM latest WHERE rn = 1 AND to_state IN ('SUBMITTED', 'CERTIFIED', 'IN_DELIVERY');")).unwrap();
    assert!(f(&old[0], "n") > 0.0, "the fixture must exercise the query");
    close(waiting_to_land(&got) as f64, f(&old[0], "n"), "waiting_to_land");

    // velocity
    let old = duckdb(&file, &format!("SELECT count(*) FILTER (WHERE machine = 'bead' AND applied AND to_state = 'LANDED' AND CAST(ts AS TIMESTAMP) >= {NOW_SQL} - INTERVAL '{WINDOW} hours') AS cur_n, count(*) FILTER (WHERE machine = 'bead' AND applied AND to_state = 'LANDED' AND CAST(ts AS TIMESTAMP) >= {NOW_SQL} - INTERVAL '{BASELINE} hours') AS base_n FROM read_ndjson_auto('{p}');")).unwrap();
    let (cur, base) = velocity_metrics(&got, NOW, WINDOW, BASELINE);
    assert!(f(&old[0], "cur_n") > 0.0 && f(&old[0], "base_n") > f(&old[0], "cur_n"), "{old:?}");
    close(cur, f(&old[0], "cur_n") / WINDOW, "velocity current");
    close(base, f(&old[0], "base_n") / BASELINE, "velocity baseline");

    // dwell
    let states = "'READY', 'WORKING', 'SUBMITTED', 'IN_DELIVERY', 'REWORK', 'OPEN', 'CI_RUNNING', 'GREEN', 'ATTRIBUTING', 'REBUILDING'";
    let old = duckdb(&file, &format!("WITH ordered AS (SELECT machine, key, to_state, CAST(ts AS TIMESTAMP) AS ts, LAG(to_state) OVER (PARTITION BY machine, key ORDER BY seq) AS prev_state, LAG(CAST(ts AS TIMESTAMP)) OVER (PARTITION BY machine, key ORDER BY seq) AS prev_ts FROM read_ndjson_auto('{p}') WHERE applied), dwells AS (SELECT ts, epoch(ts) - epoch(prev_ts) AS dwell_s FROM ordered WHERE prev_state IN ({states})) SELECT quantile_cont(dwell_s, 0.95) FILTER (WHERE ts >= {NOW_SQL} - INTERVAL '{WINDOW} hours') AS cur_p95, count(*) FILTER (WHERE ts >= {NOW_SQL} - INTERVAL '{WINDOW} hours') AS cur_n, quantile_cont(dwell_s, 0.95) FILTER (WHERE ts >= {NOW_SQL} - INTERVAL '{BASELINE} hours') AS base_p95, count(*) FILTER (WHERE ts >= {NOW_SQL} - INTERVAL '{BASELINE} hours') AS base_n FROM dwells;")).unwrap();
    let (cp, cn, bp, bn) = dwell_metrics(&got, NOW, WINDOW, BASELINE);
    assert!(f(&old[0], "cur_n") > 3.0, "{old:?}");
    close(cp, f(&old[0], "cur_p95"), "dwell cur p95");
    close(cn as f64, f(&old[0], "cur_n"), "dwell cur n");
    close(bp, f(&old[0], "base_p95"), "dwell base p95");
    close(bn as f64, f(&old[0], "base_n"), "dwell base n");

    // round health
    let old = duckdb(&file, &format!("WITH ordered AS (SELECT key, to_state, CAST(ts AS TIMESTAMP) AS ts, LAG(to_state) OVER (PARTITION BY key ORDER BY seq) AS prev_state FROM read_ndjson_auto('{p}') WHERE machine = 'batch' AND applied) SELECT count(*) FILTER (WHERE prev_state IS NOT NULL AND to_state = 'ATTRIBUTING' AND prev_state = 'CI_RUNNING' AND ts >= {NOW_SQL} - INTERVAL '{WINDOW} hours') AS cur_flips, count(*) FILTER (WHERE prev_state IS NOT NULL AND ts >= {NOW_SQL} - INTERVAL '{WINDOW} hours') AS cur_transitions, count(*) FILTER (WHERE prev_state IS NOT NULL AND to_state = 'ATTRIBUTING' AND prev_state = 'CI_RUNNING' AND ts >= {NOW_SQL} - INTERVAL '{BASELINE} hours') AS base_flips, count(*) FILTER (WHERE prev_state IS NOT NULL AND ts >= {NOW_SQL} - INTERVAL '{BASELINE} hours') AS base_transitions FROM ordered;")).unwrap();
    let (cf, ct, bf, bt) = round_health_metrics(&got, NOW, WINDOW, BASELINE);
    assert!(f(&old[0], "base_flips") > 0.0, "{old:?}");
    for (name, new) in [("cur_flips", cf), ("cur_transitions", ct), ("base_flips", bf), ("base_transitions", bt)] {
        close(new as f64, f(&old[0], name), name);
    }

    // rework
    let old = duckdb(&file, &format!("SELECT count(*) FILTER (WHERE applied AND to_state = 'REWORK' AND CAST(ts AS TIMESTAMP) >= {NOW_SQL} - INTERVAL '{BASELINE} hours') AS reopens, count(*) FILTER (WHERE applied AND to_state = 'LANDED' AND CAST(ts AS TIMESTAMP) >= {NOW_SQL} - INTERVAL '{BASELINE} hours') AS landed, epoch({NOW_SQL}) - epoch(min(CAST(ts AS TIMESTAMP))) AS history_secs FROM read_ndjson_auto('{p}');")).unwrap();
    let (reopens, landed, hist) = rework_metrics(&got, NOW, BASELINE);
    assert!(f(&old[0], "reopens") > 0.0, "{old:?}");
    close(reopens as f64, f(&old[0], "reopens"), "reopens");
    close(landed as f64, f(&old[0], "landed"), "landed");
    close(hist, f(&old[0], "history_secs") / 3600.0, "history hours");

    // dwell regression
    let old = duckdb(&file, &format!("WITH ordered AS (SELECT \"key\", to_state AS state, CAST(ts AS TIMESTAMP) AS ts, LAG(to_state) OVER (PARTITION BY \"key\" ORDER BY seq) AS prev_state, LAG(CAST(ts AS TIMESTAMP)) OVER (PARTITION BY \"key\" ORDER BY seq) AS prev_ts FROM read_ndjson_auto('{p}') WHERE applied), dwells AS (SELECT prev_state AS state, ts, epoch(ts) - epoch(prev_ts) AS dwell_s FROM ordered WHERE prev_state IS NOT NULL) SELECT state, quantile_cont(dwell_s, 0.9) FILTER (WHERE ts >= {NOW_SQL} - INTERVAL '{WINDOW} hours') AS cur_p90, count(*) FILTER (WHERE ts >= {NOW_SQL} - INTERVAL '{WINDOW} hours') AS cur_n, quantile_cont(dwell_s, 0.9) FILTER (WHERE ts >= {NOW_SQL} - INTERVAL '{BASELINE} hours') AS base_p90, count(*) FILTER (WHERE ts >= {NOW_SQL} - INTERVAL '{BASELINE} hours') AS base_n FROM dwells GROUP BY state;")).unwrap();
    let new = dwell_regression_metrics(&got, NOW, WINDOW, BASELINE);
    assert_eq!(new.len(), old.len(), "{new:?} vs {old:?}");
    for o in &old {
        let state = o["state"].as_str().unwrap();
        let n = new.iter().find(|n| n.0 == state).unwrap_or_else(|| panic!("{state} missing"));
        close(n.1, f(o, "cur_p90"), "regression cur p90");
        close(n.2 as f64, f(o, "cur_n"), "regression cur n");
        close(n.3, f(o, "base_p90"), "regression base p90");
        close(n.4 as f64, f(o, "base_n"), "regression base n");
    }
}

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
