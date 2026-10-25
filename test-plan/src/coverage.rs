//! The round's coverage report: line coverage read from `cargo llvm-cov report --json
//! --summary-only`, the use-case report derived from the matrix, and the delta between two
//! reports. Reports and never gates.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use spira_otel::metrics::Series;

use crate::MatrixDoc;

pub const REPORT_API_VERSION: &str = "test-plan-coverage/v1";
const MOVED_FILES_SHOWN: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lines {
    pub covered: u64,
    pub count: u64,
}

impl Lines {
    pub fn percent(&self) -> f64 {
        if self.count == 0 { 0.0 } else { self.covered as f64 * 100.0 / self.count as f64 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LineReport {
    pub totals: Lines,
    pub files: BTreeMap<String, Lines>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AreaUc {
    pub area: String,
    pub declared: u64,
    pub covered: u64,
    pub uncovered_ids: Vec<String>,
    #[serde(default)]
    pub covered_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UcReport {
    pub declared: u64,
    pub covered: u64,
    pub areas: Vec<AreaUc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoverageReport {
    pub api_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<String>,
    /// `None` is "lines: unavailable": the round's image had no llvm-cov, or the run failed.
    pub lines: Option<LineReport>,
    pub use_cases: UcReport,
}

/// Reads llvm-cov's export JSON; file names lose `root` so reports from two checkouts compare.
pub fn parse_llvm_cov(json: &str, root: &str) -> Result<LineReport, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("llvm-cov json: {e}"))?;
    let data = v.get("data").and_then(|d| d.get(0)).ok_or("llvm-cov json: no data[0]")?;
    let lines_of = |o: &serde_json::Value| -> Option<Lines> {
        let l = o.get("lines")?;
        Some(Lines { covered: l.get("covered")?.as_u64()?, count: l.get("count")?.as_u64()? })
    };
    let totals = data.get("totals").and_then(lines_of).ok_or("llvm-cov json: no totals.lines")?;
    let prefix = format!("{}/", root.trim_end_matches('/'));
    let mut files = BTreeMap::new();
    for f in data.get("files").and_then(|f| f.as_array()).ok_or("llvm-cov json: no files")? {
        let name = f.get("filename").and_then(|n| n.as_str()).ok_or("llvm-cov json: file without filename")?;
        let Some(l) = f.get("summary").and_then(lines_of) else { continue };
        let name = if root.is_empty() { name } else { name.strip_prefix(prefix.as_str()).unwrap_or(name) };
        files.insert(name.to_string(), l);
    }
    Ok(LineReport { totals, files })
}

/// A use case is covered when a suite or test names it; an `uncovered` marker is a declared gap.
pub fn uc_report(m: &MatrixDoc) -> UcReport {
    let mut areas = Vec::new();
    for a in &m.areas {
        let (covered_ids, uncovered_ids): (Vec<_>, Vec<_>) =
            a.use_cases.iter().partition(|u| !u.covering_suites.is_empty());
        areas.push(AreaUc {
            area: a.area.clone(),
            declared: a.use_cases.len() as u64,
            covered: covered_ids.len() as u64,
            uncovered_ids: uncovered_ids.iter().map(|u| u.id.clone()).collect(),
            covered_ids: covered_ids.iter().map(|u| u.id.clone()).collect(),
        });
    }
    UcReport { declared: areas.iter().map(|a| a.declared).sum(), covered: areas.iter().map(|a| a.covered).sum(), areas }
}

pub fn build_report(round: Option<&str>, lines: Option<LineReport>, m: &MatrixDoc) -> CoverageReport {
    CoverageReport { api_version: REPORT_API_VERSION.into(), round: round.map(str::to_string), lines, use_cases: uc_report(m) }
}

fn pct(n: u64, d: u64) -> f64 {
    if d == 0 { 0.0 } else { n as f64 * 100.0 / d as f64 }
}

/// The report's short text: totals, then every area's declared/covered with its uncovered ids.
pub fn render_text(r: &CoverageReport) -> String {
    let mut out = String::new();
    match &r.lines {
        Some(l) => out.push_str(&format!("lines: {:.2}% ({}/{})\n", l.totals.percent(), l.totals.covered, l.totals.count)),
        None => out.push_str("lines: unavailable\n"),
    }
    let u = &r.use_cases;
    out.push_str(&format!("use cases: {}/{} covered ({:.1}%)\n", u.covered, u.declared, pct(u.covered, u.declared)));
    for a in &u.areas {
        out.push_str(&format!("  {}: {}/{} ({:.0}%)\n", a.area, a.covered, a.declared, pct(a.covered, a.declared)));
        if !a.uncovered_ids.is_empty() {
            out.push_str(&format!("    uncovered: {}\n", a.uncovered_ids.join(" ")));
        }
    }
    out
}

fn covered_set(r: &UcReport) -> BTreeSet<&str> {
    r.areas.iter().flat_map(|a| a.covered_ids.iter().map(String::as_str)).collect()
}

/// What moved since `prev`: lines, use cases gained or lost, and the files whose line coverage
/// moved most. Reports only; a drop is a fact for the round to loop on, not a verdict.
pub fn render_delta(prev: &CoverageReport, cur: &CoverageReport) -> String {
    let mut out = String::new();
    match (&prev.lines, &cur.lines) {
        (Some(p), Some(c)) => {
            let d = c.totals.percent() - p.totals.percent();
            out.push_str(&format!(
                "lines: {:.2}% -> {:.2}% ({:+.2} pts; covered {:+}, total {:+})\n",
                p.totals.percent(), c.totals.percent(), d,
                c.totals.covered as i64 - p.totals.covered as i64,
                c.totals.count as i64 - p.totals.count as i64
            ));
            let mut moved: Vec<(f64, &str, f64, f64)> = Vec::new();
            for name in p.files.keys().chain(c.files.keys()).collect::<BTreeSet<_>>() {
                let before = p.files.get(name).map_or(0.0, Lines::percent);
                let after = c.files.get(name).map_or(0.0, Lines::percent);
                if p.files.get(name) != c.files.get(name) && (after - before).abs() >= 0.005 {
                    moved.push((after - before, name, before, after));
                }
            }
            moved.sort_by(|a, b| b.0.abs().total_cmp(&a.0.abs()).then(a.1.cmp(b.1)));
            for (d, name, b, a) in moved.iter().take(MOVED_FILES_SHOWN) {
                out.push_str(&format!("  file {name}: {b:.1}% -> {a:.1}% ({d:+.1})\n"));
            }
            if moved.len() > MOVED_FILES_SHOWN {
                out.push_str(&format!("  ... and {} more files moved\n", moved.len() - MOVED_FILES_SHOWN));
            }
        }
        (None, Some(_)) => out.push_str("lines: previous round had none\n"),
        (_, None) => out.push_str("lines: unavailable this round\n"),
    }
    let (pu, cu) = (&prev.use_cases, &cur.use_cases);
    out.push_str(&format!("use cases: {}/{} -> {}/{} covered\n", pu.covered, pu.declared, cu.covered, cu.declared));
    let (pc, cc) = (covered_set(pu), covered_set(cu));
    let gained: Vec<&&str> = cc.difference(&pc).collect();
    let lost: Vec<&&str> = pc.difference(&cc).collect();
    if !gained.is_empty() {
        out.push_str(&format!("  gained: {}\n", gained.iter().map(|s| **s).collect::<Vec<_>>().join(" ")));
    }
    if !lost.is_empty() {
        out.push_str(&format!("  lost: {}\n", lost.iter().map(|s| **s).collect::<Vec<_>>().join(" ")));
    }
    out
}

/// One line for the round's summary.
pub fn delta_line(prev: &CoverageReport, cur: &CoverageReport) -> String {
    let lines = match (&prev.lines, &cur.lines) {
        (Some(p), Some(c)) => format!("lines {:.2}% ({:+.2} pts)", c.totals.percent(), c.totals.percent() - p.totals.percent()),
        (_, Some(c)) => format!("lines {:.2}% (no previous)", c.totals.percent()),
        _ => "lines unavailable".to_string(),
    };
    let (pc, cc) = (covered_set(&prev.use_cases), covered_set(&cur.use_cases));
    format!(
        "{lines}; use cases {}/{} (+{} gained, -{} lost)",
        cur.use_cases.covered, cur.use_cases.declared, cc.difference(&pc).count(), pc.difference(&cc).count()
    )
}

pub const METRIC_LINE_PERCENT: &str = "spira_coverage_line_percent";
pub const METRIC_LINES_COVERED: &str = "spira_coverage_lines_covered";
pub const METRIC_LINES_TOTAL: &str = "spira_coverage_lines_total";
pub const METRIC_UC_DECLARED: &str = "spira_coverage_uc_declared";
pub const METRIC_UC_COVERED: &str = "spira_coverage_uc_covered";

pub fn metric_series(r: &CoverageReport, at: std::time::SystemTime) -> Vec<Series> {
    let s = |name: &str, value: f64, labels: &[(&str, &str)]| Series {
        name: name.into(),
        value,
        labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        at,
    };
    let mut out = Vec::new();
    if let Some(l) = &r.lines {
        out.push(s(METRIC_LINE_PERCENT, l.totals.percent(), &[]));
        out.push(s(METRIC_LINES_COVERED, l.totals.covered as f64, &[]));
        out.push(s(METRIC_LINES_TOTAL, l.totals.count as f64, &[]));
    }
    out.push(s(METRIC_UC_DECLARED, r.use_cases.declared as f64, &[("area", "all")]));
    out.push(s(METRIC_UC_COVERED, r.use_cases.covered as f64, &[("area", "all")]));
    for a in &r.use_cases.areas {
        out.push(s(METRIC_UC_DECLARED, a.declared as f64, &[("area", &a.area)]));
        out.push(s(METRIC_UC_COVERED, a.covered as f64, &[("area", &a.area)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AreaMatrix, SuiteRef, Tier, UseCaseMatrix, MATRIX_API_VERSION};

    fn uc(id: &str, covered: bool) -> UseCaseMatrix {
        UseCaseMatrix {
            id: id.into(),
            tier: Tier::T1,
            statement: "s".into(),
            covering_suites: if covered { vec![SuiteRef { path: "spira/test-a.sh".into(), tier: None, p50_ms: None, over_budget: false }] } else { vec![] },
            launcher: None,
            uncovered: None,
        }
    }

    fn matrix(covered: &[(&str, bool)]) -> MatrixDoc {
        MatrixDoc {
            api_version: MATRIX_API_VERSION.into(),
            areas: vec![AreaMatrix { area: "dispatch".into(), use_cases: covered.iter().map(|(i, c)| uc(i, *c)).collect() }],
        }
    }

    const LLVM: &str = r#"{"data":[{"totals":{"lines":{"count":10,"covered":5,"percent":50.0}},"files":[
        {"filename":"/w/a/src/x.rs","summary":{"lines":{"count":4,"covered":4,"percent":100.0}}},
        {"filename":"/w/a/src/y.rs","summary":{"lines":{"count":6,"covered":1,"percent":16.6}}}]}],"type":"llvm.coverage.json.export"}"#;

    #[test]
    fn llvm_cov_export_is_read_and_paths_lose_the_root() {
        let l = parse_llvm_cov(LLVM, "/w/a").unwrap();
        assert_eq!(l.totals, Lines { covered: 5, count: 10 });
        assert_eq!(l.files["src/x.rs"], Lines { covered: 4, count: 4 });
        assert!(parse_llvm_cov("{}", "").is_err());
    }

    #[test]
    fn use_cases_count_covered_and_list_the_uncovered_ids() {
        let r = uc_report(&matrix(&[("UC-dispatch-01", true), ("UC-dispatch-02", false)]));
        assert_eq!((r.declared, r.covered), (2, 1));
        assert_eq!(r.areas[0].uncovered_ids, vec!["UC-dispatch-02"]);
    }

    #[test]
    fn the_delta_names_lines_files_and_use_cases_that_moved() {
        let prev = build_report(Some("r1"), Some(parse_llvm_cov(LLVM, "/w/a").unwrap()), &matrix(&[("UC-dispatch-01", true), ("UC-dispatch-02", false)]));
        let better = LLVM.replace("\"covered\":1,", "\"covered\":3,").replace("\"covered\":5,", "\"covered\":7,");
        let cur = build_report(Some("r2"), Some(parse_llvm_cov(&better, "/w/a").unwrap()), &matrix(&[("UC-dispatch-01", false), ("UC-dispatch-02", true)]));
        let d = render_delta(&prev, &cur);
        assert!(d.contains("50.00% -> 70.00% (+20.00 pts"), "{d}");
        assert!(d.contains("file src/y.rs"), "{d}");
        assert!(d.contains("gained: UC-dispatch-02") && d.contains("lost: UC-dispatch-01"), "{d}");
        assert!(delta_line(&prev, &cur).contains("+1 gained, -1 lost"));
    }

    #[test]
    fn a_round_without_lines_says_so_and_still_reports_use_cases() {
        let r = build_report(None, None, &matrix(&[("UC-dispatch-01", true)]));
        assert!(render_text(&r).starts_with("lines: unavailable\n"));
        assert!(render_delta(&r, &r).contains("lines: unavailable this round"));
        assert!(!metric_series(&r, std::time::SystemTime::now()).iter().any(|s| s.name == METRIC_LINE_PERCENT));
    }

    #[test]
    fn metrics_carry_one_series_per_area_and_the_line_percent() {
        let r = build_report(None, Some(parse_llvm_cov(LLVM, "").unwrap()), &matrix(&[("UC-dispatch-01", true)]));
        let m = metric_series(&r, std::time::SystemTime::now());
        assert!(m.iter().any(|s| s.name == METRIC_LINE_PERCENT && s.value == 50.0));
        assert!(m.iter().any(|s| s.name == METRIC_UC_COVERED && s.labels == vec![("area".to_string(), "dispatch".to_string())]));
    }
}
