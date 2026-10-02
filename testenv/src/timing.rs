//! run/tsd families this runner produces and reads (DESIGN.md §3.4): `suite-timing` (one row
//! per executed suite plus a `__batch__` row) and, for a round's corpus build, `round`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::Path;

pub const SUITE_TIMING: &str = "suite-timing";
pub const ROUND: &str = "round";
pub const CASE_TIMING: &str = "case-timing";
pub const BATCH_ROW: &str = "__batch__";

/// One `suite-timing` row's producer fields (the envelope — ts, host, family — is tsd's).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuiteTimingRow {
    pub run_id: String,
    pub branch: String,
    pub suite: String,
    pub rc: i64,
    pub wall_secs: u64,
    pub bd_calls: u64,
    pub bd_ms: u64,
    pub mode: String,
    /// The suite's `# tier:` at measurement time; empty when undeclared.
    pub tier: String,
    /// `__batch__` only (DESIGN.md §11.2, D11): seconds from start to the first suite's launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_secs: Option<u64>,
    /// `__batch__` only: `<phase>:<secs>,…` in order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phases: Option<String>,
    /// `__batch__` only: `spare` (claimed a pre-booted container), `cold` (booted one on a
    /// warm slot), `off` (no warm path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warm: Option<String>,
}

impl SuiteTimingRow {
    /// A row with the per-suite fields; the `__batch__`-only ones empty.
    pub fn new(
        run_id: &str,
        branch: &str,
        suite: &str,
        rc: i64,
        wall_secs: u64,
        mode: &str,
    ) -> Self {
        SuiteTimingRow {
            run_id: run_id.into(),
            branch: branch.into(),
            suite: suite.into(),
            rc,
            wall_secs,
            bd_calls: 0,
            bd_ms: 0,
            mode: mode.into(),
            tier: String::new(),
            setup_secs: None,
            phases: None,
            warm: None,
        }
    }
}

/// One `case-timing` row: a testlib case's wall milliseconds, keyed to the suite, the case
/// name and the use cases the suite covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseTimingRow {
    pub run_id: String,
    pub branch: String,
    pub suite: String,
    pub case: String,
    pub status: String,
    pub ms: u64,
    pub tier: String,
    pub uc: Vec<String>,
}

/// The cases of a suite's captured TAP output that carry a testlib `#ms=<n>` line, in order.
/// A suite not on testlib.sh emits none, and so yields no rows.
pub fn case_rows(
    run_id: &str,
    branch: &str,
    suite: &str,
    tier: &str,
    uc: &[String],
    out: &str,
) -> Vec<CaseTimingRow> {
    let mut rows = Vec::new();
    let mut open: Option<(String, &str)> = None;
    for line in out.lines() {
        if let Some(case) = crate::tap::case_of(line, "not ok") {
            open = Some((case, "fail"));
        } else if let Some(case) = crate::tap::case_of(line, "ok") {
            open = Some((case, "pass"));
        } else if let Some(ms) = line.strip_prefix("#ms=").and_then(|n| n.trim().parse().ok()) {
            if let Some((case, status)) = open.take() {
                rows.push(CaseTimingRow {
                    run_id: run_id.into(),
                    branch: branch.into(),
                    suite: suite.into(),
                    case,
                    status: status.into(),
                    ms,
                    tier: tier.into(),
                    uc: uc.to_vec(),
                });
            }
        }
    }
    rows
}

/// One `round` row: the corpus build phase of a round (only with SPIRA_ROUND_BATCH_ID).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundPhaseRow {
    pub batch_id: String,
    pub phase: String,
    pub secs: u64,
    pub members: u64,
    pub reds: u64,
}

fn fields_of<T: Serialize>(row: &T) -> Vec<(String, Value)> {
    match serde_json::to_value(row) {
        Ok(Value::Object(m)) => m.into_iter().collect(),
        _ => Vec::new(),
    }
}

/// Render one JSONL line for `family` through tsd's own row builder.
pub fn render<T: Serialize>(family: &str, ts: &str, host: &str, row: &T) -> Result<String, String> {
    tsd::build_row(ts, host, family, &fields_of(row))
}

/// Append a row under an exclusive flock, as tsd-write does. Best-effort by contract: a
/// timing row never fails the suite it measures; the caller logs the error and moves on.
pub fn append<T: Serialize>(
    run_root: &Path,
    family: &str,
    ts: &str,
    host: &str,
    row: &T,
) -> Result<(), String> {
    let line = render(family, ts, host, row)?;
    let path = tsd::family_path(run_root, family);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let fd = f.as_raw_fd();
    // SAFETY: flock on a file descriptor we own for the duration of the call.
    if unsafe { libc::flock(fd, libc::LOCK_EX) } != 0 {
        return Err(format!("{}: flock failed", path.display()));
    }
    let r = writeln!(f, "{line}").map_err(|e| format!("{}: {e}", path.display()));
    unsafe { libc::flock(fd, libc::LOCK_UN) };
    r
}

#[derive(Debug, Clone, PartialEq)]
struct Obs {
    ts: String,
    order: usize,
    suite: String,
    wall: f64,
}

fn observations(text: &str) -> Vec<Obs> {
    text.lines()
        .enumerate()
        .filter_map(|(order, l)| {
            let v: Value = serde_json::from_str(l).ok()?;
            let suite = v.get("suite")?.as_str()?.to_string();
            let wall = v.get("wall_secs")?.as_f64()?;
            let ts = v
                .get("ts")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Some(Obs {
                ts,
                order,
                suite,
                wall,
            })
        })
        .collect()
}

/// Mean wall seconds per suite over every row (what LPT orders by). Unparseable rows are
/// skipped, never fatal; the `__batch__` row is not a suite.
pub fn mean_wall_by_suite(text: &str) -> HashMap<String, f64> {
    let mut acc: HashMap<String, (f64, u64)> = HashMap::new();
    for o in observations(text) {
        if o.suite == BATCH_ROW {
            continue;
        }
        let e = acc.entry(o.suite).or_insert((0.0, 0));
        e.0 += o.wall;
        e.1 += 1;
    }
    acc.into_iter()
        .map(|(k, (s, n))| (k, s / n as f64))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuiteMedian {
    pub suite: String,
    pub median: f64,
    pub n: u64,
}

/// `--report N`: each suite's median wall_secs over its last N rows (newest by ts, then by
/// file position), every host counted together; sorted by suite. Interpolated median, as
/// DuckDB's quantile_cont(…, 0.5).
pub fn suite_medians(text: &str, n: usize) -> Vec<SuiteMedian> {
    let mut by: HashMap<String, Vec<Obs>> = HashMap::new();
    for o in observations(text) {
        by.entry(o.suite.clone()).or_default().push(o);
    }
    let mut out: Vec<SuiteMedian> = by
        .into_iter()
        .filter_map(|(suite, mut obs)| {
            obs.sort_by(|a, b| b.ts.cmp(&a.ts).then(b.order.cmp(&a.order)));
            obs.truncate(n);
            if obs.is_empty() {
                return None;
            }
            let mut w: Vec<f64> = obs.iter().map(|o| o.wall).collect();
            w.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let pos = (w.len() - 1) as f64 * 0.5;
            let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
            let median = w[lo] + (w[hi] - w[lo]) * (pos - lo as f64);
            Some(SuiteMedian {
                suite,
                median,
                n: w.len() as u64,
            })
        })
        .collect();
    out.sort_by(|a, b| a.suite.cmp(&b.suite));
    out
}

/// `<suite>\t<calls>\t<ms>` from a bd call log: one line per call, first field the ms.
pub fn bd_totals(log: &str) -> (u64, u64) {
    let mut calls = 0;
    let mut ms = 0.0;
    for l in log.lines() {
        calls += 1;
        ms += l
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
    }
    (calls, ms as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(suite: &str, wall: u64) -> SuiteTimingRow {
        SuiteTimingRow {
            run_id: "local-1".into(),
            branch: "b".into(),
            suite: suite.into(),
            rc: 0,
            wall_secs: wall,
            bd_calls: 2,
            bd_ms: 30,
            mode: "parallel".into(),
            tier: String::new(),
            setup_secs: None,
            phases: None,
            warm: None,
        }
    }

    #[test]
    fn case_rows_pair_each_case_with_the_ms_line_after_it() {
        let out = "TAP version 14\nok 1 - fast\n#ms=3\nnot ok 2 - slow\n# wanted x\n#ms=4100\nok 3 - untimed\n";
        let uc = vec!["UC-gate-01".to_string()];
        let rows = case_rows("r", "b", "test-x.sh", "T1", &uc, out);
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].case.as_str(), rows[0].status.as_str(), rows[0].ms), ("fast", "pass", 3));
        assert_eq!((rows[1].case.as_str(), rows[1].status.as_str(), rows[1].ms), ("slow", "fail", 4100));
        assert_eq!(rows[1].uc, uc);
        assert!(case_rows("r", "b", "test-old.sh", "", &[], "ok 1 - a\n").is_empty());
        let line = render(CASE_TIMING, "2026-10-02T00:00:00Z", "h", &rows[1]).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["family"], "case-timing");
        assert_eq!(v["ms"], 4100);
        assert_eq!(v["uc"][0], "UC-gate-01");
    }

    #[test]
    fn the_batch_row_carries_its_phases_and_a_suite_row_does_not() {
        let plain = render(
            "suite-timing",
            "2026-09-30T00:00:00Z",
            "h",
            &row("test-a.sh", 3),
        )
        .unwrap();
        assert!(!plain.contains("setup_secs") && !plain.contains("phases"));
        let b = SuiteTimingRow {
            setup_secs: Some(31),
            phases: Some("resolve:1,up:0".into()),
            warm: Some("spare".into()),
            ..SuiteTimingRow::new("r", "b", BATCH_ROW, 0, 90, "parallel")
        };
        let line = render("suite-timing", "2026-09-30T00:00:00Z", "h", &b).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["setup_secs"], 31);
        assert_eq!(v["warm"], "spare");
        assert_eq!(v["phases"], "resolve:1,up:0");
        // readers of the per-suite fields are unaffected
        assert!(mean_wall_by_suite(&line).is_empty());
    }

    #[test]
    fn rows_carry_the_envelope_and_typed_fields() {
        let line = render(
            SUITE_TIMING,
            "2026-09-28T00:00:00Z",
            "h1",
            &row("test-a.sh", 12),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["family"], "suite-timing");
        assert_eq!(v["host"], "h1");
        assert_eq!(v["ts"], "2026-09-28T00:00:00Z");
        assert_eq!(v["wall_secs"], 12);
        assert_eq!(v["rc"], 0);
        assert_eq!(v["run_id"], "local-1");
        assert_eq!(v["tier"], "");
    }

    #[test]
    fn append_writes_jsonl_under_run_tsd() {
        let dir = testkit::TempDir::new("testenv-timing");
        append(&dir, SUITE_TIMING, "t", "h", &row("a", 1)).unwrap();
        append(&dir, SUITE_TIMING, "t", "h", &row("b", 2)).unwrap();
        let text = fs::read_to_string(dir.join("tsd/suite-timing.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn means_skip_batch_rows_and_garbage() {
        let text = "{\"suite\":\"a\",\"wall_secs\":10}\n{\"suite\":\"a\",\"wall_secs\":20}\nnot json\n{\"suite\":\"__batch__\",\"wall_secs\":999}\n{\"suite\":\"b\"}\n";
        let m = mean_wall_by_suite(text);
        assert_eq!(m.get("a"), Some(&15.0));
        assert!(!m.contains_key("__batch__"));
        assert!(!m.contains_key("b"));
    }

    #[test]
    fn medians_take_the_newest_n_rows() {
        let text = "{\"ts\":\"2026-01-01T00:00:01Z\",\"suite\":\"a\",\"wall_secs\":100}\n\
                    {\"ts\":\"2026-01-01T00:00:02Z\",\"suite\":\"a\",\"wall_secs\":10}\n\
                    {\"ts\":\"2026-01-01T00:00:03Z\",\"suite\":\"a\",\"wall_secs\":20}\n\
                    {\"ts\":\"2026-01-01T00:00:03Z\",\"suite\":\"b\",\"wall_secs\":7}\n";
        let m = suite_medians(text, 2);
        assert_eq!(
            m,
            vec![
                SuiteMedian {
                    suite: "a".into(),
                    median: 15.0,
                    n: 2
                },
                SuiteMedian {
                    suite: "b".into(),
                    median: 7.0,
                    n: 1
                }
            ]
        );
    }

    #[test]
    fn bd_totals_sum_the_first_field() {
        assert_eq!(bd_totals("12 bd list\n30 bd show\n"), (2, 42));
        assert_eq!(bd_totals(""), (0, 0));
    }
}
