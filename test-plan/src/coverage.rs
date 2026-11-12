//! The round's coverage report: the use-case report derived from the matrix, and the delta
//! between two reports. Reports and never gates.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use spira_otel::metrics::Series;

use crate::MatrixDoc;

pub const REPORT_API_VERSION: &str = "test-plan-coverage/v1";

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
    pub use_cases: UcReport,
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

pub fn build_report(round: Option<&str>, m: &MatrixDoc) -> CoverageReport {
    CoverageReport { api_version: REPORT_API_VERSION.into(), round: round.map(str::to_string), use_cases: uc_report(m) }
}

fn pct(n: u64, d: u64) -> f64 {
    if d == 0 { 0.0 } else { n as f64 * 100.0 / d as f64 }
}

/// The report's short text: totals, then every area's declared/covered with its uncovered ids.
pub fn render_text(r: &CoverageReport) -> String {
    let mut out = String::new();
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

/// What moved since `prev`: use cases gained or lost. Reports only; a drop is a fact for the round to loop on, not a verdict.
pub fn render_delta(prev: &CoverageReport, cur: &CoverageReport) -> String {
    let mut out = String::new();
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
    let (pc, cc) = (covered_set(&prev.use_cases), covered_set(&cur.use_cases));
    format!(
        "use cases {}/{} (+{} gained, -{} lost)",
        cur.use_cases.covered, cur.use_cases.declared, cc.difference(&pc).count(), pc.difference(&cc).count()
    )
}

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

    #[test]
    fn use_cases_count_covered_and_list_the_uncovered_ids() {
        let r = uc_report(&matrix(&[("UC-dispatch-01", true), ("UC-dispatch-02", false)]));
        assert_eq!((r.declared, r.covered), (2, 1));
        assert_eq!(r.areas[0].uncovered_ids, vec!["UC-dispatch-02"]);
    }

    #[test]
    fn metrics_carry_one_series_per_area() {
        let r = build_report(None, &matrix(&[("UC-dispatch-01", true)]));
        let m = metric_series(&r, std::time::SystemTime::now());
        assert!(m.iter().any(|s| s.name == METRIC_UC_COVERED && s.labels == vec![("area".to_string(), "dispatch".to_string())]));
    }
}
