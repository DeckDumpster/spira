//! `tsd_keys` — the ops pane's flow headlines, read in-process through `tsd::query` (`where`, `rework`), and
//! the tests headline from the `test.result` events.
//! A query that fails, prints nothing parseable or names no known fields is `?`, never `0`.

use super::{push, Kv};
use crate::io;
use serde_json::Value;
use spira_otel::testresult::Summary;
use spira_otel::vlogs::Logs;

pub fn tsd_keys() -> Kv {
    let logs = spira_config::process::cfg("SPIRA_VICTORIALOGS_URL").ok();
    match io::try_run_dir() {
        Ok(root) => tsd_keys_in(&root, spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref(), logs.as_deref(), io::now()),
        Err(_) => tsd_keys_with(|_| None, test_summary(logs.as_deref())),
    }
}

/// The flow headlines from the run directory's family files and the metrics store at `endpoint`,
/// read in-process the way `tsd-read` reads them; the tests headline from the log store at `logs`.
fn tsd_keys_in(root: &std::path::Path, endpoint: Option<&str>, logs: Option<&str>, now: i64) -> Kv {
    let reader = tsd::store::Reader::new(endpoint).ok();
    let src = tsd::query::Source { reader: reader.as_ref(), root };
    let now = now.max(0) as u64;
    let stage = std::cell::OnceCell::new();
    let stage_rows = || stage.get_or_init(|| src.rows(tsd::query::STAGE, tsd::query::since(now, QUERY_HOURS)).ok()).as_ref();
    tsd_keys_with(
        |q| match q {
            "where" => stage_rows().map(|r| tsd::query::wip_of(r, now).to_string()),
            _ => stage_rows().map(|r| tsd::query::rework_of(r).to_string()),
        },
        test_summary(logs),
    )
}

fn test_summary(logs: Option<&str>) -> Option<Summary> {
    Logs::new(logs?).ok()?.test_summary(TESTS_WINDOW_HOURS * 3600).ok()
}

fn tsd_keys_with(run: impl Fn(&str) -> Option<String>, tests: Option<Summary>) -> Kv {
    let mut out = Kv::new();
    push(&mut out, "SP_TSD_WHERE", rows(run("where")).and_then(|r| where_line(&r)).unwrap_or_else(unread));
    push(&mut out, "SP_TSD_REWORK", rows(run("rework")).and_then(|r| rework_line(&r)).unwrap_or_else(unread));
    push(&mut out, "SP_TSD_TESTS", tests.and_then(|s| tests_line(&s)).unwrap_or_else(unread));
    out
}

const TESTS_WINDOW_HOURS: u64 = 72;
const QUERY_HOURS: u64 = 720;

fn thousands(n: u64) -> String {
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

/// `10,914 tests  4 red  slowest a.sh::x 4.2s`. No events at all is unread, not zero tests.
fn tests_line(s: &Summary) -> Option<String> {
    if s.tests == 0 {
        return None;
    }
    let mut line = format!("{} tests  {} red", thousands(s.tests), s.red);
    if let Some((test, ms)) = &s.slowest {
        line.push_str(&format!("  slowest {test} {:.1}s", *ms as f64 / 1000.0));
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
    use spira_otel::vlogs::fake::{FakeLogs, Script};

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
        }, None);
        assert_eq!(get(&kv, "SP_TSD_WHERE"), "WORKING 3 (12m) \u{b7} REWORK 1 (2h)");
        assert_eq!(get(&kv, "SP_TSD_REWORK"), "3 reopened / 9 landed  gate-red 2, ejected 1");
    }

    fn scripted(counts: &str, slowest: &str) -> FakeLogs {
        let fake = FakeLogs::start();
        fake.answer("count_uniq", Script { status: 200, body: counts.into(), hang: false });
        fake.answer("max(duration_ms)", Script { status: 200, body: slowest.into(), hang: false });
        fake
    }

    fn tests_row(url: Option<&str>) -> String {
        let root = testkit::TempDir::new("cockpit-collect-tsd-tests");
        get(&tsd_keys_in(root.path(), None, url, 1_700_000_000), "SP_TSD_TESTS").to_string()
    }

    #[test_id(t22qdz0)]
    #[test]
    fn the_tests_headline_is_built_from_the_test_result_events_of_the_last_72_hours() {
        let fake = scripted("{\"tests\":\"10914\",\"red\":\"4\"}\n", "{\"test_id\":\"a.sh::x\",\"ms\":\"4250\"}\n");
        assert_eq!(tests_row(Some(&fake.url())), "10,914 tests  4 red  slowest a.sh::x 4.2s");
        let queries = fake.queries();
        assert!(!queries.is_empty() && queries.iter().all(|q| q.starts_with("_time:259200s _msg:\"test.result\" ")), "{queries:?}");
    }

    #[test_id(tuom063)]
    #[test]
    fn the_tests_headline_is_a_question_mark_when_the_events_cannot_be_read() {
        assert_eq!(tests_row(None), "?", "no address");
        let down = FakeLogs::start();
        down.answer(spira_otel::testresult::EVENT, Script { status: 500, body: "down".into(), hang: false });
        assert_eq!(tests_row(Some(&down.url())), "?", "a refused read");
        let none = scripted("{\"tests\":\"0\",\"red\":\"0\"}\n", "");
        assert_eq!(tests_row(Some(&none.url())), "?", "no events is not a green zero");
    }

    #[test_id(tcbhzrw)]
    #[test]
    fn a_failed_query_is_a_question_mark_never_zero() {
        let kv = tsd_keys_with(|_| None, None);
        assert_eq!(get(&kv, "SP_TSD_WHERE"), "?");
        assert_eq!(get(&kv, "SP_TSD_REWORK"), "?");
    }

    #[test_id(t7vdn3h)]
    #[test]
    fn unparseable_or_foreign_output_is_a_question_mark() {
        let kv = tsd_keys_with(|q| Some(if q == "where" { "Conversion Error: nope".into() } else { r#"[{"x":1}]"#.into() }), None);
        assert_eq!(get(&kv, "SP_TSD_WHERE"), "?");
        assert_eq!(get(&kv, "SP_TSD_REWORK"), "?");
    }

    #[test_id(t3ttwpy)]
    #[test]
    fn an_empty_pipeline_says_so() {
        let kv = tsd_keys_with(|_| Some("[]".to_string()), None);
        assert_eq!(get(&kv, "SP_TSD_WHERE"), "nothing in flight");
        assert_eq!(get(&kv, "SP_TSD_REWORK"), "?");
    }

    fn push_stage(store: &spira_otel::fixture::FakeStore, ts: &str) {
        let pusher = tsd::store::Pusher::new(Some(store.addr().to_string()));
        let fields = vec![("machine".to_string(), "bead".into()), ("applied".to_string(), true.into()), ("key".to_string(), "sp-a".into()), ("seq".to_string(), 1.into()), ("to_state".to_string(), "WORKING".into())];
        pusher.push("bead-stage", ts, &fields).unwrap();
    }

    #[test_id(tekb7ql)]
    #[test]
    fn the_stage_rows_are_fetched_once_a_pass() {
        let root = testkit::TempDir::new("cockpit-collect-tsd-stage");
        let store = spira_otel::fixture::FakeStore::start();
        let now = 1_700_000_000_i64;
        push_stage(&store, &tsd::iso_utc(now as u64 - 600));
        let before = store.export_requests();
        tsd_keys_in(root.path(), Some(store.addr()), None, now);
        assert_eq!(store.export_requests() - before, 1, "one bead-stage request answers both headlines");
    }

    #[test_id(tfnxf1h)]
    #[test]
    fn the_headlines_are_read_in_process_from_the_metrics_and_log_stores() {
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
        let logs = scripted("{\"tests\":\"1\",\"red\":\"0\"}\n", "{\"test_id\":\"a.sh::x\",\"ms\":\"1500\"}\n");
        let kv = tsd_keys_in(root.path(), Some(store.addr()), Some(&logs.url()), now);
        assert_eq!(get(&kv, "SP_TSD_WHERE"), "REWORK 1 (10m)");
        assert!(get(&kv, "SP_TSD_REWORK").contains("landed"), "{}", get(&kv, "SP_TSD_REWORK"));
        assert!(get(&kv, "SP_TSD_TESTS").starts_with("1 tests"), "{}", get(&kv, "SP_TSD_TESTS"));
        let unread = tsd_keys_in(root.path(), None, Some(&logs.url()), now);
        assert_eq!(get(&unread, "SP_TSD_WHERE"), "?", "a moved family with no store is unread, not empty");
    }
}
