// testrun — the per-test record. One `test-run` row per case per round, keyed by the test's id;
// the suite a case lives in is a label on the row and nothing is aggregated by it.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::query::{num, quantile, text, ts_of};

pub const FAMILY: &str = "test-run";
const VERDICT_CASE: &str = "(verdict)";
const SECS_PER_DAY: u64 = 86_400;

/// A test's id: the file it lives in, then its case name. The id is unique; the file is only
/// the first half of it.
pub fn test_id(suite: &str, case: &str) -> String {
    format!("{suite}::{case}")
}

/// `pass`, `fail` or `skip`: the three outcomes a rate is taken over.
fn outcome(status: &str) -> &'static str {
    match status {
        "pass" => "pass",
        "skip" | "unreached" | "bail" => "skip",
        _ => "fail",
    }
}

/// The rows of one round from its `results.jsonl`. `(verdict)` rows are the retry
/// classifier's, not tests, and a row with no suite or no case names nothing.
pub fn rows_from_results(jsonl: &str, round: &str, ts: &str, host: &str) -> Vec<String> {
    jsonl
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| {
            let (suite, case) = (text(&v, "suite")?, text(&v, "case")?);
            if suite.is_empty() || case.is_empty() || case == VERDICT_CASE {
                return None;
            }
            let fields = vec![
                ("round".to_string(), json!(round)),
                ("test".to_string(), json!(test_id(suite, case))),
                ("suite".to_string(), json!(suite)),
                ("status".to_string(), json!(outcome(text(&v, "status").unwrap_or("")))),
                ("seconds".to_string(), json!(num(&v, "seconds").unwrap_or(0.0))),
            ];
            crate::build_row(ts, host, FAMILY, &fields).ok()
        })
        .collect()
}

struct Round<'a> {
    at: u64,
    tests: BTreeMap<&'a str, &'a Value>,
}

/// Rounds, oldest first, each with its tests.
fn rounds(rows: &[Value]) -> Vec<Round<'_>> {
    let mut by: BTreeMap<&str, Round> = BTreeMap::new();
    for r in rows {
        let (Some(round), Some(test), Some(at)) = (text(r, "round"), text(r, "test"), ts_of(r)) else { continue };
        let e = by.entry(round).or_insert(Round { at, tests: BTreeMap::new() });
        e.at = e.at.min(at);
        e.tests.insert(test, r);
    }
    let mut out: Vec<Round> = by.into_values().collect();
    out.sort_by_key(|r| r.at);
    out
}

/// One test over every round it ran in: runs, duration p50/p90, pass/red/skip/flip rates.
/// A flip is a run whose outcome differs from the test's previous run.
pub fn test_stats(rows: &[Value], test: &str) -> Value {
    let mut runs: Vec<&Value> = rows.iter().filter(|r| text(r, "test") == Some(test)).collect();
    runs.sort_by_key(|r| ts_of(r));
    let n = runs.len();
    let status = |r: &Value| outcome(text(r, "status").unwrap_or(""));
    let count = |s: &str| runs.iter().filter(|r| status(r) == s).count();
    let flips = runs.windows(2).filter(|w| status(w[0]) != status(w[1])).count();
    let secs: Vec<f64> = runs.iter().filter_map(|r| num(r, "seconds")).collect();
    let rate = |k: usize, d: usize| (d > 0).then(|| k as f64 / d as f64);
    let covered: BTreeSet<&str> = runs.iter().filter_map(|r| text(r, "covered")).flat_map(|c| c.split(',')).filter(|u| !u.is_empty()).collect();
    json!({
        "test": test,
        "runs": n,
        "p50_s": quantile(&secs, 0.5),
        "p90_s": quantile(&secs, 0.9),
        "pass_rate": rate(count("pass"), n),
        "red_rate": rate(count("fail"), n),
        "flip_rate": rate(flips, n.saturating_sub(1)),
        "covered": covered,
    })
}

/// The live inventory: the latest round's test count, how many tests the last round of each
/// day added and removed against the previous day's, and the slowest and flakiest tests.
pub fn inventory(rows: &[Value], top: usize) -> Value {
    let rs = rounds(rows);
    let Some(latest) = rs.last() else { return json!({"total": 0, "round": null, "days": [], "slowest": [], "flakiest": []}) };
    let mut last_of_day: BTreeMap<u64, &Round> = BTreeMap::new();
    for r in &rs {
        last_of_day.insert(r.at / SECS_PER_DAY, r);
    }
    let mut days = Vec::new();
    let mut prev: Option<&Round> = None;
    for (day, r) in &last_of_day {
        let (added, removed) = match prev {
            Some(p) => (r.tests.keys().filter(|t| !p.tests.contains_key(*t)).count(), p.tests.keys().filter(|t| !r.tests.contains_key(*t)).count()),
            None => (r.tests.len(), 0),
        };
        days.push(json!({"day": &crate::iso_utc(day * SECS_PER_DAY)[..10], "total": r.tests.len(), "added": added, "removed": removed}));
        prev = Some(r);
    }
    let mut slow: Vec<(&str, f64)> = latest.tests.iter().map(|(t, r)| (*t, num(r, "seconds").unwrap_or(0.0))).collect();
    slow.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(b.0)));
    let mut flaky: Vec<(&str, f64)> = latest.tests.keys().filter_map(|t| {
        let s = test_stats(rows, t);
        Some((*t, s["flip_rate"].as_f64().filter(|f| *f > 0.0)?))
    }).collect();
    flaky.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(b.0)));
    let reds = latest.tests.values().filter(|r| outcome(text(r, "status").unwrap_or("")) == "fail").count();
    json!({
        "total": latest.tests.len(),
        "round": text(latest.tests.values().next().copied().unwrap_or(&Value::Null), "round"),
        "red": reds,
        "days": days,
        "slowest": slow.iter().take(top).map(|(t, s)| json!({"test": t, "seconds": s})).collect::<Vec<_>>(),
        "flakiest": flaky.iter().take(top).map(|(t, f)| json!({"test": t, "flip_rate": f})).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows_of(round: &str, ts: &str, cases: &[(&str, &str, &str, f64)]) -> Vec<Value> {
        let jsonl: String = cases
            .iter()
            .map(|(s, c, st, secs)| format!("{{\"suite\":\"{s}\",\"case\":\"{c}\",\"status\":\"{st}\",\"seconds\":{secs}}}\n"))
            .collect();
        rows_from_results(&jsonl, round, ts, "h").iter().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    #[test]
    fn a_verdict_row_is_not_a_test_and_a_case_is_keyed_by_file_and_name() {
        let jsonl = "{\"suite\":\"a.sh\",\"case\":\"(verdict)\",\"status\":\"red-red\",\"seconds\":0}\n{\"suite\":\"a.sh\",\"case\":\"x\",\"status\":\"ok\",\"seconds\":2}\n";
        let rows = rows_from_results(jsonl, "r1", "2026-11-01T00:00:00Z", "h");
        assert_eq!(rows.len(), 1);
        let v: Value = serde_json::from_str(&rows[0]).unwrap();
        assert_eq!((v["test"].as_str(), v["suite"].as_str(), v["family"].as_str()), (Some("a.sh::x"), Some("a.sh"), Some(FAMILY)));
    }

    #[test]
    fn the_total_counts_tests_not_suites_and_two_suites_with_one_case_name_stay_two_tests() {
        let rows = rows_of("r1", "2026-11-01T00:00:00Z", &[("a.sh", "t", "pass", 1.0), ("b.sh", "t", "pass", 1.0), ("b.sh", "u", "pass", 1.0)]);
        assert_eq!(inventory(&rows, 5)["total"], 3);
    }

    #[test]
    fn a_day_reports_the_tests_added_and_removed_against_the_day_before() {
        let mut rows = rows_of("r1", "2026-11-01T01:00:00Z", &[("a.sh", "t", "pass", 1.0), ("a.sh", "gone", "pass", 1.0)]);
        rows.extend(rows_of("r2", "2026-11-02T01:00:00Z", &[("a.sh", "t", "pass", 1.0), ("a.sh", "new1", "pass", 1.0), ("a.sh", "new2", "pass", 1.0)]));
        let inv = inventory(&rows, 5);
        assert_eq!(inv["total"], 3);
        let d = &inv["days"][1];
        assert_eq!((d["added"].as_i64(), d["removed"].as_i64(), d["day"].as_str()), (Some(2), Some(1), Some("2026-11-02")));
    }

    #[test]
    fn a_test_reports_its_median_red_rate_and_flips_over_its_own_runs() {
        let mut rows = rows_of("r1", "2026-11-01T00:00:00Z", &[("a.sh", "t", "pass", 1.0)]);
        rows.extend(rows_of("r2", "2026-11-01T01:00:00Z", &[("a.sh", "t", "fail", 3.0)]));
        rows.extend(rows_of("r3", "2026-11-01T02:00:00Z", &[("a.sh", "t", "pass", 2.0)]));
        rows.extend(rows_of("r3", "2026-11-01T02:00:00Z", &[("a.sh", "other", "fail", 9.0)]));
        let s = test_stats(&rows, "a.sh::t");
        assert_eq!((s["runs"].as_u64(), s["p50_s"].as_f64()), (Some(3), Some(2.0)));
        assert!((s["red_rate"].as_f64().unwrap() - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(s["flip_rate"].as_f64(), Some(1.0));
    }

    #[test]
    fn a_missing_inventory_is_zero_tests_not_a_panic() {
        assert_eq!(inventory(&[], 5)["total"], 0);
    }
}
