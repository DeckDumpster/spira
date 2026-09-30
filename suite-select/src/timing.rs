//! Predicted suite cost: the P90 of `wall_secs` over each suite's last N rows of
//! `run/tsd/suite-timing.jsonl` — what `tsd-query.sh suite-p90s` computed with duckdb
//! (`quantile_cont`, linear interpolation), newest first by `ts`, ties by file order (a
//! later line is newer).

use crate::{refuse, Refusal};
use std::collections::HashMap;
use std::path::Path;

/// P90 per suite, and the count of rows that could not be read (skipped).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct P90s {
    pub by_suite: HashMap<String, f64>,
    pub skipped: usize,
}

/// `quantile_cont(xs, q)`: linear interpolation between the closest ranks.
pub fn quantile_cont(xs: &[f64], q: f64) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    let mut v = xs.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pos = q * (v.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    Some(v[lo] + (v[hi] - v[lo]) * (pos - lo as f64))
}

/// Parse the family's text: one JSON object per line with `suite`, `ts` and a numeric
/// `wall_secs`. A line that does not have all three is skipped and counted.
pub fn p90s(text: &str, runs: usize) -> P90s {
    let mut rows: HashMap<String, Vec<(String, usize, f64)>> = HashMap::new();
    let mut skipped = 0;
    for (i, l) in text.lines().enumerate() {
        if l.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(l) else {
            skipped += 1;
            continue;
        };
        let (Some(s), Some(ts), Some(w)) = (
            v.get("suite").and_then(|x| x.as_str()),
            v.get("ts").and_then(|x| x.as_str()),
            v.get("wall_secs").and_then(|x| x.as_f64()),
        ) else {
            skipped += 1;
            continue;
        };
        rows.entry(s.to_string()).or_default().push((ts.to_string(), i, w));
    }
    let runs = runs.max(1);
    let mut by_suite = HashMap::new();
    for (s, mut r) in rows {
        r.sort_by(|a, b| (&b.0, b.1).cmp(&(&a.0, a.1)));
        let xs: Vec<f64> = r.iter().take(runs).map(|x| x.2).collect();
        if let Some(p) = quantile_cont(&xs, 0.9) {
            by_suite.insert(s, p);
        }
    }
    P90s { by_suite, skipped }
}

/// P90 of the test runner's measured setup (`setup_secs` on its `__batch__` rows, testenv
/// DESIGN.md §11) over the newest `runs` rows that carry it; None when no row does (every
/// row written before sp-govet).
pub fn setup_p90(text: &str, runs: usize) -> Option<f64> {
    let mut rows: Vec<(String, usize, f64)> = text
        .lines()
        .enumerate()
        .filter_map(|(i, l)| {
            let v = serde_json::from_str::<serde_json::Value>(l).ok()?;
            if v.get("suite")?.as_str()? != "__batch__" {
                return None;
            }
            Some((v.get("ts")?.as_str()?.to_string(), i, v.get("setup_secs")?.as_f64()?))
        })
        .collect();
    rows.sort_by(|a, b| (&b.0, b.1).cmp(&(&a.0, a.1)));
    let xs: Vec<f64> = rows.iter().take(runs.max(1)).map(|r| r.2).collect();
    quantile_cont(&xs, 0.9)
}

/// [`setup_p90`] of `<run>/tsd/suite-timing.jsonl`; a missing file is no measurement.
pub fn load_setup(run: &Path, runs: usize) -> Result<Option<f64>, Refusal> {
    let p = run.join("tsd").join("suite-timing.jsonl");
    match std::fs::read(&p) {
        Ok(b) => Ok(setup_p90(&String::from_utf8_lossy(&b), runs)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => refuse(format!("cannot read {}: {e}", p.display())),
    }
}

/// `<run>/tsd/suite-timing.jsonl`. No file: no P90s (every suite costs its tier cap). A file
/// that exists and cannot be read is a refusal.
pub fn load(run: &Path, runs: usize) -> Result<P90s, Refusal> {
    let p = run.join("tsd").join("suite-timing.jsonl");
    match std::fs::read(&p) {
        Ok(b) => Ok(p90s(&String::from_utf8_lossy(&b), runs)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(P90s::default()),
        Err(e) => refuse(format!("cannot read {}: {e}", p.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantile_cont_interpolates_like_duckdb() {
        assert_eq!(quantile_cont(&[5.0], 0.9), Some(5.0));
        // 10 values 1..10: pos 8.1 → 9 + 0.1 = 9.1
        let xs: Vec<f64> = (1..=10).map(f64::from).collect();
        assert!((quantile_cont(&xs, 0.9).unwrap() - 9.1).abs() < 1e-9);
        assert!((quantile_cont(&[1.0, 2.0], 0.9).unwrap() - 1.9).abs() < 1e-9);
        assert_eq!(quantile_cont(&[], 0.9), None);
    }

    #[test]
    fn only_the_last_n_rows_by_ts_count_and_bad_rows_are_skipped() {
        let mut t = String::new();
        // Older rows are huge; the newest three are 1, 2, 3.
        for (ts, w) in [("2026-09-01T00:00:00Z", 100), ("2026-09-02T00:00:00Z", 1), ("2026-09-03T00:00:00Z", 2), ("2026-09-04T00:00:00Z", 3)] {
            t.push_str(&format!("{{\"suite\":\"test-a.sh\",\"ts\":\"{ts}\",\"wall_secs\":{w}}}\n"));
        }
        t.push_str("{\"suite\":\"test-b.sh\",\"ts\":\"x\"}\nnot json\n{\"suite\":\"test-b.sh\",\"ts\":\"x\",\"wall_secs\":4.5}\n");
        let p = p90s(&t, 3);
        assert!((p.by_suite["test-a.sh"] - 2.8).abs() < 1e-9);
        assert_eq!(p.by_suite["test-b.sh"], 4.5);
        assert_eq!(p.skipped, 2);
    }

    #[test]
    fn setup_p90_reads_only_batch_rows_that_carry_it() {
        let t = "{\"suite\":\"__batch__\",\"ts\":\"2026-09-30T00:00:01Z\",\"wall_secs\":90}\n\
                 {\"suite\":\"test-a.sh\",\"ts\":\"2026-09-30T00:00:02Z\",\"wall_secs\":9,\"setup_secs\":500}\n\
                 {\"suite\":\"__batch__\",\"ts\":\"2026-09-30T00:00:03Z\",\"wall_secs\":90,\"setup_secs\":20}\n\
                 {\"suite\":\"__batch__\",\"ts\":\"2026-09-30T00:00:04Z\",\"wall_secs\":99,\"setup_secs\":30}\n";
        assert!((setup_p90(t, 20).unwrap() - 29.0).abs() < 1e-9);
        assert_eq!(setup_p90(t, 1), Some(30.0), "newest first");
        assert_eq!(setup_p90("{\"suite\":\"__batch__\",\"ts\":\"x\",\"wall_secs\":1}", 20), None);
    }

    #[test]
    fn a_missing_family_is_no_timings_not_a_refusal() {
        let d = testkit::TempDir::new("suite-select-timing");
        assert_eq!(load(d.path(), 20).unwrap(), P90s::default());
    }
}
