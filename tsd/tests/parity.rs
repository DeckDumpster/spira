//! tsd-read, reading moved families from the metrics store, answers what the DuckDB queries over
//! the families' old JSONL files answered (tests/legacy-tsd-query.sh) for the same fixture, and
//! refuses by name, with a non-zero exit, when the store is stopped, unconfigured or empty.

use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tsd::store::push_row;

struct Lcg(u64);

impl Lcg {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn env(family: &str, ts: u64, fields: Value) -> Value {
    let mut v = json!({"ts": tsd::iso_utc(ts), "host": "h", "family": family});
    for (k, x) in fields.as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

fn stage_rows(rng: &mut Lcg, now: u64) -> Vec<Value> {
    let mut rows: Vec<(u64, Value)> = Vec::new();
    let path = ["READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"];
    let row = |t: u64, machine: &str, key: &str, from: &str, to: &str, applied: bool, reason: Option<&str>| {
        let mut f = json!({"seq": 0, "machine": machine, "key": key, "from_state": from, "to_state": to, "applied": applied});
        if let Some(r) = reason {
            f["reason"] = json!(r);
        }
        (t, env("bead-stage", t, f))
    };
    for b in 0..60u64 {
        let mut t = now - 70 * 3600 + rng.next(66 * 3600);
        let key = format!("sp-{b}");
        let mut from = "NONE";
        for st in path.iter().take(1 + rng.next(6) as usize + 1) {
            rows.push(row(t, "bead", &key, from, st, rng.next(9) != 0, None));
            from = st;
            t += 90 + rng.next(5000);
            if *st == "SUBMITTED" && rng.next(3) == 0 {
                rows.push(row(t, "bead", &key, st, "REWORK", true, [Some("red"), Some("conflict"), None][rng.next(3) as usize]));
                from = "REWORK";
                t += 700 + rng.next(3000);
            }
        }
    }
    rows.retain(|r| r.0 < now - 120);
    rows.sort_by_key(|r| r.0);
    rows.into_iter().enumerate().map(|(i, (_, mut v))| {
        v["seq"] = json!(i + 1);
        v
    })
    .collect()
}

fn fixtures() -> Vec<(&'static str, Vec<Value>)> {
    let mut rng = Lcg(11);
    let n = now();
    let dwarf: Vec<Value> = (0..90).map(|i| {
        let wall = if i % 11 == 0 { json!("?") } else { json!(30 + rng.next(900)) };
        let (at, bead, rune, status, turns) = (n - 120 - rng.next(70 * 3600), format!("sp-{}", rng.next(60)), ["builder", "reviewer"][rng.next(2) as usize], ["submitted", "ok", "failed"][rng.next(3) as usize], rng.next(40));
        env("dwarf-session", at, json!({"bead": bead, "rune": rune, "rc": 0, "status": status, "wall_s": wall, "turns": turns}))
    }).collect();
    let sentinel: Vec<Value> = (0..40).flat_map(|p| (1..=5).map(move |c| (p, c))).map(|(p, c)| {
        env("sentinel-phase", n - 300 - (40 - p) * 1500 + c, json!({"pass": format!("pass-{p}"), "check": format!("CHECK{c}"), "secs": (p * 7 + c * 13) % 90}))
    }).collect();
    vec![("bead-stage", stage_rows(&mut rng, n)), ("dwarf-session", dwarf), ("sentinel-phase", sentinel)]
}

fn kept_fixtures() -> Vec<(&'static str, Vec<Value>)> {
    let mut rng = Lcg(5);
    let n = now();
    let gate: Vec<Value> = (0..50).map(|_| {
        let (at, bead, status, secs) = (n - 200 - rng.next(70 * 3600), format!("sp-{}", rng.next(60)), ["green", "red"][rng.next(2) as usize], 20 + rng.next(400));
        env("gate-run", at, json!({"bead": bead, "status": status, "reason": "", "ran_secs": secs}))
    }).collect();
    let slots: Vec<Value> = (0..400).map(|i| {
        let live = if i % 37 == 0 { json!("?") } else { json!(rng.next(4)) };
        env("slots", n - 60 - i * 290, json!({"live": live, "ceiling": 4, "ready": rng.next(3), "capacity_paused": 0}))
    }).collect();
    let rounds: Vec<Value> = (0..20).map(|_| env("batch-round", n - 300 - rng.next(70 * 3600), json!({"duration_ms": 60000 + rng.next(900000)}))).collect();
    vec![("gate-run", gate), ("slots", slots), ("batch-round", rounds)]
}

fn write_file(run: &Path, family: &str, rows: &[Value]) {
    let p = tsd::family_path(run, family);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, rows.iter().map(|r| format!("{r}\n")).collect::<String>()).unwrap();
}

fn have_duckdb() -> bool {
    Command::new("duckdb").arg("--version").output().is_ok()
}

fn legacy(run: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new("bash").arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/legacy-tsd-query.sh")).args(args).env("SPIRA_RUN", run).env("TZ", "UTC").output().unwrap();
    (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned())
}

fn read(run: &Path, endpoint: Option<&str>, args: &[&str]) -> (i32, String, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tsd-read"));
    c.args(args).env_clear().env("SPIRA_RUN", run).env("PATH", "/nonexistent");
    if let Some(e) = endpoint {
        c.env("SPIRA_METRICS_ENDPOINT", e);
    }
    let out = c.output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The instant a `ts` names, from the row's ISO form or DuckDB's `YYYY-MM-DD HH:MM:SS+HH` display.
fn instant(v: &Value) -> Option<u64> {
    let s = v.as_str()?;
    if let Some(t) = tsd::parse_ts(s) {
        return Some(t);
    }
    let (stamp, off) = s.split_at(s.rfind(['+', '-'])?);
    let hours: i64 = off.parse().ok()?;
    let utc = tsd::parse_ts(&format!("{}Z", stamp.replace(' ', "T")))?;
    Some((utc as i64 - hours * 3600) as u64)
}

fn same(a: &Value, b: &Value, tol: f64, what: &str) {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => assert!((x.as_f64().unwrap() - y.as_f64().unwrap()).abs() <= tol, "{what}: {a} vs {b}"),
        (Value::Array(x), Value::Array(y)) => {
            assert_eq!(x.len(), y.len(), "{what}: {a} vs {b}");
            x.iter().zip(y).for_each(|(p, q)| same(p, q, tol, what));
        }
        (Value::Object(x), Value::Object(y)) => {
            assert_eq!(x.keys().collect::<Vec<_>>(), y.keys().collect::<Vec<_>>(), "{what}: {a} vs {b}");
            for (k, v) in x {
                if k == "ts" {
                    assert_eq!(instant(v), instant(&y[k]), "{what}: {a} vs {b}");
                } else {
                    same(v, &y[k], tol, what);
                }
            }
        }
        _ => assert_eq!(a, b, "{what}"),
    }
}

#[test]
fn every_moved_family_query_answers_what_the_file_query_answered() {
    if !have_duckdb() {
        eprintln!("skipped: no duckdb on PATH");
        return;
    }
    let dir = testkit::TempDir::new("tsd-parity");
    let store = spira_otel::fixture::FakeStore::start();
    for (family, rows) in fixtures() {
        write_file(&dir, family, &rows);
        for r in &rows {
            push_row(Some(store.addr()), family, &r.to_string()).unwrap();
        }
        let stored = tsd::store::Reader::new(Some(store.addr())).unwrap().rows(family, 0).unwrap();
        assert_eq!(stored.len(), rows.len(), "{family}: every row is stored");
    }
    for (family, rows) in kept_fixtures() {
        write_file(&dir, family, &rows);
    }
    let cases: Vec<(Vec<&str>, f64)> = vec![
        (vec!["where"], 2.0),
        (vec!["where", "30"], 2.0),
        (vec!["rework", "24"], 0.0),
        (vec!["rework"], 0.0),
        (vec!["sentinel", "24"], 0.0),
        (vec!["time", "24"], 1.0),
        (vec!["bead", "sp-3"], 0.0),
        (vec!["bead", "sp-17"], 0.0),
        (vec!["baseline", "sentinel-phase", "secs", "24"], 1e-9),
        (vec!["baseline", "dwarf-session", "turns", "30"], 1e-9),
        (vec!["rate", "dwarf-session", "24"], 1e-9),
        (vec!["rate", "dwarf-session", "30"], 1e-9),
        (vec!["rate", "dwarf-session", "60"], 1e-9),
        (vec!["dwell", "sentinel-phase", "secs", "0.9", "24"], 1e-9),
        (vec!["dwell", "dwarf-session", "turns", "0.5"], 1e-9),
    ];
    for (args, tol) in cases {
        let (ok, want) = legacy(&dir, &args);
        assert!(ok, "{args:?}: the oracle failed");
        let (code, got, err) = read(&dir, Some(store.addr()), &args);
        assert_eq!(code, 0, "{args:?}: {err}");
        let (want, got): (Value, Value) = (serde_json::from_str(&want).unwrap(), serde_json::from_str(&got).unwrap());
        assert!(want.as_array().is_some_and(|a| !a.is_empty() && a.iter().any(|r| r.as_object().is_some_and(|o| o.values().any(|v| v.as_f64().is_some_and(|f| f != 0.0))))), "{args:?}: the fixture must exercise the query: {want}");
        same(&got, &want, tol, &args.join(" "));
    }
    for args in [vec!["count-by", "dwarf-session", "status", "24"], vec!["by-group", "dwarf-session", "rune", "turns", "48"], vec!["count-by", "bead-stage", "to_state"]] {
        let (ok, want) = legacy(&dir, &args);
        assert!(ok, "{args:?}");
        let (code, got, err) = read(&dir, Some(store.addr()), &args);
        assert_eq!(code, 0, "{args:?}: {err}");
        let parse = |t: &str| t.lines().map(|l| l.split_once('\t').map(|(g, n)| (g.to_string(), n.parse::<f64>().unwrap())).unwrap()).collect::<Vec<_>>();
        let (w, g) = (parse(&want), parse(&got));
        assert!(!w.is_empty(), "{args:?}: nothing to compare");
        assert_eq!(w.len(), g.len(), "{args:?}: {want} vs {got}");
        for ((wg, wn), (gg, gn)) in w.iter().zip(&g) {
            assert!(wg == gg || args[0] == "by-group", "{args:?}: {want} vs {got}");
            assert!((wn - gn).abs() < 1e-6, "{args:?}: {want} vs {got}");
        }
    }
}

#[test]
fn a_moved_family_is_never_answered_from_a_file() {
    let dir = testkit::TempDir::new("tsd-parity-decoy");
    write_file(&dir, "sentinel-phase", &[env("sentinel-phase", now() - 60, json!({"pass": "p", "check": "C1", "secs": 5}))]);
    let empty = spira_otel::fixture::FakeStore::start();
    let (code, out, err) = read(&dir, Some(empty.addr()), &["sentinel", "24"]);
    assert_ne!(code, 0, "{out}");
    assert!(err.contains("metrics-store-no-series"), "{err}");
}

#[test]
fn every_moved_query_refuses_by_name_when_the_store_cannot_answer() {
    let dir = testkit::TempDir::new("tsd-parity-refuse");
    for (family, rows) in kept_fixtures() {
        write_file(&dir, family, &rows);
    }
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let stopped = l.local_addr().unwrap().to_string();
    drop(l);
    let empty = spira_otel::fixture::FakeStore::start();
    let queries: Vec<Vec<&str>> = vec![
        vec!["where"], vec!["rework"], vec!["sentinel"], vec!["time"], vec!["bead", "sp-1"],
        vec!["baseline", "sentinel-phase", "secs", "24"], vec!["rate", "dwarf-session", "24"],
        vec!["dwell", "dwarf-session", "wall_s", "0.5"], vec!["by-group", "dwarf-session", "rune", "wall_s"], vec!["count-by", "bead-stage", "to_state"], vec!["rows", "bead-stage"],
    ];
    for args in queries {
        for (endpoint, name) in [(Some(stopped.as_str()), "metrics-store-unreachable"), (Some(empty.addr()), "metrics-store-no-series"), (None, "metrics-store-not-configured")] {
            let (code, out, err) = read(&dir, endpoint, &args);
            assert_ne!(code, 0, "{args:?} against {name} must refuse, said {out:?}");
            assert!(out.trim().is_empty(), "{args:?} printed an answer: {out}");
            assert!(err.contains(name), "{args:?}: wanted {name}, got {err}");
        }
    }
}
