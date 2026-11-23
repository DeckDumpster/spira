//! `tsd_keys` — the ops pane's flow headlines, read in-process through `tsd::query` (`where`, `rework`) and `tsd::testrun`.
//! A query that fails, prints nothing parseable or names no known fields is `?`, never `0`.

use super::{push, Kv};
use crate::io;
use serde_json::Value;
use std::sync::Mutex;

pub fn tsd_keys() -> Kv {
    match io::try_run_dir() {
        Ok(root) => tsd_keys_in(&root, spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref(), io::now()),
        Err(_) => tsd_keys_with(|_| None),
    }
}

/// The headlines from the run directory's family files and the metrics store at `endpoint`,
/// read in-process the way `tsd-read` reads them.
fn tsd_keys_in(root: &std::path::Path, endpoint: Option<&str>, now: i64) -> Kv {
    static HELD: Mutex<TestsHeld> = Mutex::new(TestsHeld { entry: None, reads: 0 });
    tsd_keys_held(&mut HELD.lock().unwrap_or_else(|e| e.into_inner()), root, endpoint, now)
}

/// The inventory text last computed, and what it was computed from.
struct TestsHeld {
    entry: Option<(TestsKey, String)>,
    reads: usize,
}

/// A file's identity and extent, and the hour: the inventory changes only when one of them does.
#[derive(PartialEq)]
struct TestsKey {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: (i64, i64),
    hour: u64,
}

fn tests_key(path: &std::path::Path, now: u64) -> Option<TestsKey> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path).ok()?;
    Some(TestsKey { dev: m.dev(), ino: m.ino(), len: m.len(), mtime: (m.mtime(), m.mtime_nsec()), hour: now / 3600 })
}

fn tsd_keys_held(held: &mut TestsHeld, root: &std::path::Path, endpoint: Option<&str>, now: i64) -> Kv {
    let reader = tsd::store::Reader::new(endpoint).ok();
    let src = tsd::query::Source { reader: reader.as_ref(), root };
    let now = now.max(0) as u64;
    let held = std::cell::RefCell::new(held);
    let stage = std::cell::OnceCell::new();
    let stage_rows = || stage.get_or_init(|| src.rows(tsd::query::STAGE, tsd::query::since(now, QUERY_HOURS)).ok()).as_ref();
    tsd_keys_with(|q| match q {
        "where" => stage_rows().map(|r| tsd::query::wip_of(r, now).to_string()),
        "rework" => stage_rows().map(|r| tsd::query::rework_of(r).to_string()),
        _ => {
            let path = tsd::family_path(root, tsd::testrun::FAMILY);
            let mut held = held.borrow_mut();
            let Some(key) = tests_key(&path, now) else {
                held.entry = None;
                return None;
            };
            if let Some((k, text)) = &held.entry {
                if *k == key {
                    return Some(text.clone());
                }
            }
            held.entry = None;
            held.reads += 1;
            let since = now.saturating_sub(TESTS_WINDOW_HOURS * 3600);
            let rows = src.rows(tsd::testrun::FAMILY, since).ok()?;
            let text = tsd::testrun::inventory(&rows, 10).to_string();
            held.entry = Some((key, text.clone()));
            Some(text)
        }
    })
}

fn tsd_keys_with(run: impl Fn(&str) -> Option<String>) -> Kv {
    let mut out = Kv::new();
    push(&mut out, "SP_TSD_WHERE", rows(run("where")).and_then(|r| where_line(&r)).unwrap_or_else(unread));
    push(&mut out, "SP_TSD_REWORK", rows(run("rework")).and_then(|r| rework_line(&r)).unwrap_or_else(unread));
    let tests = run("tests").and_then(|raw| serde_json::from_str::<Value>(raw.trim()).ok()).and_then(|v| tests_line(&v));
    push(&mut out, "SP_TSD_TESTS", tests.unwrap_or_else(unread));
    out
}

const TESTS_WINDOW_HOURS: u64 = 72;
const QUERY_HOURS: u64 = 720;

fn thousands(n: i64) -> String {
    let d = n.to_string();
    let mut out = String::new();
    for (i, c) in d.chars().enumerate() {
        if i > 0 && (d.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `10,914 tests  +12/-3 on 2026-11-06  4 red  slowest a.sh::x 4.2s` from `tsd::testrun::inventory`. The count is
/// the latest round's own; no suite count is ever added to it.
fn tests_line(inv: &Value) -> Option<String> {
    let total = inv.get("total")?.as_i64().filter(|t| *t > 0)?;
    let mut line = format!("{} tests", thousands(total));
    if let Some(d) = inv.get("days")?.as_array()?.last() {
        line.push_str(&format!("  +{}/-{} on {}", d.get("added")?.as_i64()?, d.get("removed")?.as_i64()?, d.get("day")?.as_str()?));
    }
    line.push_str(&format!("  {} red", inv.get("red")?.as_i64()?));
    if let Some(s) = inv.get("slowest")?.as_array()?.first() {
        line.push_str(&format!("  slowest {} {:.1}s", s.get("test")?.as_str()?, s.get("seconds")?.as_f64()?));
    }
    Some(line)
}

fn unread() -> String {
    "?".to_string()
}

fn rows(raw: Option<String>) -> Option<Vec<Value>> {
    match serde_json::from_str::<Value>(raw?.trim()).ok()? {
        Value::Array(a) => Some(a),
        _ => None,
    }
}

fn mins(secs: i64) -> String {
    if secs >= 3600 { format!("{}h", secs / 3600) } else { format!("{}m", secs / 60) }
}

fn where_line(rows: &[Value]) -> Option<String> {
    let mut parts = Vec::new();
    for r in rows {
        let state = r.get("state")?.as_str()?;
        let wip = r.get("wip")?.as_i64()?;
        let dwell = r.get("dwell_p50_s")?.as_i64()?;
        parts.push(format!("{state} {wip} ({})", mins(dwell)));
    }
    Some(if parts.is_empty() { "nothing in flight".to_string() } else { parts.join(" \u{b7} ") })
}

fn rework_line(rows: &[Value]) -> Option<String> {
    let total = rows.iter().find(|r| r.get("reason").and_then(Value::as_str) == Some("*"))?;
    let reopens = total.get("reopens")?.as_i64()?;
    let landed = total.get("landed")?.as_i64()?;
    let mut line = format!("{reopens} reopened / {landed} landed");
    let top: Vec<String> = rows
        .iter()
        .filter(|r| r.get("reason").and_then(Value::as_str) != Some("*"))
        .take(3)
        .filter_map(|r| Some(format!("{} {}", r.get("reason")?.as_str()?, r.get("reopens")?.as_i64()?)))
        .collect();
    if !top.is_empty() {
        line.push_str(&format!("  {}", top.join(", ")));
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get<'a>(kv: &'a Kv, k: &str) -> &'a str {
        &kv.iter().find(|(key, _)| key == k).unwrap().1
    }

    #[test_id(tr0wdw8)]
    #[test]
    fn known_rows_render_the_headlines() {
        let kv = tsd_keys_with(|q| {
            Some(match q {
                "where" => r#"[{"state":"WORKING","wip":3,"dwell_p50_s":720,"dwell_max_s":900},{"state":"REWORK","wip":1,"dwell_p50_s":7300,"dwell_max_s":7300}]"#,
                _ => r#"[{"reason":"*","reopens":3,"landed":9},{"reason":"gate-red","reopens":2,"landed":9},{"reason":"ejected","reopens":1,"landed":9}]"#,
            }
            .to_string())
        });
        assert_eq!(get(&kv, "SP_TSD_WHERE"), "WORKING 3 (12m) \u{b7} REWORK 1 (2h)");
        assert_eq!(get(&kv, "SP_TSD_REWORK"), "3 reopened / 9 landed  gate-red 2, ejected 1");
    }

    #[test_id(tucgoil)]
    #[test]
    fn the_tests_headline_is_the_latest_rounds_test_count_alone() {
        let kv = tsd_keys_with(|q| {
            Some(if q == "tests" {
                r#"{"total":10914,"red":4,"days":[{"day":"2026-11-05","total":10900,"added":5,"removed":0},{"day":"2026-11-06","total":10914,"added":12,"removed":3}],"slowest":[{"test":"a.sh::x","seconds":4.25}],"flakiest":[]}"#.to_string()
            } else {
                "[]".to_string()
            })
        });
        assert_eq!(get(&kv, "SP_TSD_TESTS"), "10,914 tests  +12/-3 on 2026-11-06  4 red  slowest a.sh::x 4.2s");
        assert_eq!(get(&tsd_keys_with(|_| None), "SP_TSD_TESTS"), "?");
    }

    #[test_id(tcbhzrw)]
    #[test]
    fn a_failed_query_is_a_question_mark_never_zero() {
        let kv = tsd_keys_with(|_| None);
        assert_eq!(get(&kv, "SP_TSD_WHERE"), "?");
        assert_eq!(get(&kv, "SP_TSD_REWORK"), "?");
    }

    #[test_id(t7vdn3h)]
    #[test]
    fn unparseable_or_foreign_output_is_a_question_mark() {
        let kv = tsd_keys_with(|q| Some(if q == "where" { "Conversion Error: nope".into() } else { r#"[{"x":1}]"#.into() }));
        assert_eq!(get(&kv, "SP_TSD_WHERE"), "?");
        assert_eq!(get(&kv, "SP_TSD_REWORK"), "?");
    }

    #[test_id(t3ttwpy)]
    #[test]
    fn an_empty_pipeline_says_so() {
        let kv = tsd_keys_with(|_| Some("[]".to_string()));
        assert_eq!(get(&kv, "SP_TSD_WHERE"), "nothing in flight");
        assert_eq!(get(&kv, "SP_TSD_REWORK"), "?");
    }

    fn push_stage(store: &spira_otel::fixture::FakeStore, ts: &str) {
        let pusher = tsd::store::Pusher::new(Some(store.addr().to_string()));
        let fields = vec![("machine".to_string(), "bead".into()), ("applied".to_string(), true.into()), ("key".to_string(), "sp-a".into()), ("seq".to_string(), 1.into()), ("to_state".to_string(), "WORKING".into())];
        pusher.push("bead-stage", ts, &fields).unwrap();
    }

    fn run_row(ts: &str, round: &str) -> String {
        let f = |k: &str, v: Value| (k.to_string(), v);
        tsd::build_row(ts, "h", tsd::testrun::FAMILY, &[f("round", round.into()), f("test", "a.sh::x".into()), f("suite", "a.sh".into()), f("status", "pass".into()), f("seconds", 1.5.into())]).unwrap()
    }

    #[test_id(tr0wcnt)]
    #[test]
    fn the_tests_file_is_reread_only_when_it_or_the_hour_changes_and_the_stage_is_fetched_once_a_pass() {
        let root = testkit::TempDir::new("cockpit-collect-tsd-held");
        let store = spira_otel::fixture::FakeStore::start();
        let now = 1_700_000_000_i64;
        let ts = tsd::iso_utc(now as u64 - 600);
        push_stage(&store, &ts);
        let path = tsd::family_path(root.path(), tsd::testrun::FAMILY);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut held = TestsHeld { entry: None, reads: 0 };
        let pass = |held: &mut TestsHeld, now: i64| tsd_keys_held(held, root.path(), Some(store.addr()), now);

        assert_eq!(get(&pass(&mut held, now), "SP_TSD_TESTS"), "?", "a missing file is unread");
        std::fs::write(&path, format!("{}\n", run_row(&ts, "r1"))).unwrap();
        let before = store.export_requests();
        let first = pass(&mut held, now);
        assert_eq!(store.export_requests() - before, 1, "one bead-stage request a pass");
        let reads = held.reads;
        let again = pass(&mut held, now + 60);
        assert_eq!(held.reads, reads, "an unchanged file is not read again");
        assert_eq!(get(&first, "SP_TSD_TESTS"), get(&again, "SP_TSD_TESTS"));

        std::fs::write(&path, format!("{}\n{}\n", run_row(&ts, "r1"), run_row(&ts, "r2"))).unwrap();
        pass(&mut held, now + 60);
        assert_eq!(held.reads, reads + 1, "an appended row is read");
        pass(&mut held, now + 3600 + 60);
        assert_eq!(held.reads, reads + 2, "a later hour is read");
        std::fs::remove_file(&path).unwrap();
        assert_eq!(get(&pass(&mut held, now + 3700), "SP_TSD_TESTS"), "?");
        assert!(held.entry.is_none(), "a missing file is never held");
    }

    #[test_id(tfnxf1h)]
    #[test]
    fn the_headlines_are_read_in_process_from_the_store_and_the_family_files() {
        let root = testkit::TempDir::new("cockpit-collect-tsd-keys");
        let store = spira_otel::fixture::FakeStore::start();
        let now = 1_700_000_000_i64;
        let ts = tsd::iso_utc(now as u64 - 600);
        let pusher = tsd::store::Pusher::new(Some(store.addr().to_string()));
        let fields = |pairs: &[(&str, Value)]| pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect::<Vec<_>>();
        pusher
            .push("bead-stage", &ts, &fields(&[("machine", "bead".into()), ("applied", true.into()), ("key", "sp-a".into()), ("seq", 1.into()), ("to_state", "WORKING".into())]))
            .unwrap();
        pusher
            .push("bead-stage", &ts, &fields(&[("machine", "bead".into()), ("applied", true.into()), ("key", "sp-b".into()), ("seq", 1.into()), ("to_state", "LANDED".into())]))
            .unwrap();
        pusher
            .push("bead-stage", &ts, &fields(&[("machine", "bead".into()), ("applied", true.into()), ("key", "sp-a".into()), ("seq", 2.into()), ("to_state", "REWORK".into()), ("reason", "gate".into())]))
            .unwrap();
        let row = tsd::build_row(&ts, "h", tsd::testrun::FAMILY, &fields(&[("round", "r1".into()), ("test", "a.sh::x".into()), ("suite", "a.sh".into()), ("status", "pass".into()), ("seconds", 1.5.into())])).unwrap();
        std::fs::create_dir_all(root.path().join("tsd")).unwrap();
        std::fs::write(tsd::family_path(root.path(), tsd::testrun::FAMILY), format!("{row}\n")).unwrap();

        let kv = tsd_keys_in(root.path(), Some(store.addr()), now);
        assert_eq!(get(&kv, "SP_TSD_WHERE"), "REWORK 1 (10m)");
        assert!(get(&kv, "SP_TSD_REWORK").contains("landed"), "{}", get(&kv, "SP_TSD_REWORK"));
        assert!(get(&kv, "SP_TSD_TESTS").starts_with("1 tests"), "{}", get(&kv, "SP_TSD_TESTS"));
        let unread = tsd_keys_in(root.path(), None, now);
        assert_eq!(get(&unread, "SP_TSD_WHERE"), "?", "a moved family with no store is unread, not empty");
    }
}
