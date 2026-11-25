//! The one definition of a heavy operation's log record: what a gate (and later the round,
//! store, testenv and release) writes when an operation ends, and the query that totals them.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::logs::{Attr, LogSink};

pub const EVENT: &str = "op.done";

#[derive(Clone, Debug, PartialEq)]
pub struct Cost {
    pub write_bytes: u64,
    pub cpu_seconds: f64,
}

#[derive(Clone, Debug)]
pub struct Op {
    pub operation: String,
    pub phase: Option<String>,
    pub subject: String,
    pub repo: Option<String>,
    pub start: SystemTime,
    pub duration: Duration,
    pub result: String,
    pub cost: Option<Cost>,
    pub tests: Option<u64>,
    pub place: Option<String>,
}

/// Best effort, like `rounds::emit`: a failed write is counted by the sink and never returned.
pub fn emit(run: &Path, service: &str, op: &Op) {
    let s = |v: &str| Attr::Str(v.to_string());
    let ms = |d: Duration| Attr::Int(d.as_millis() as i64);
    let mut attrs = vec![("operation".to_string(), s(&op.operation))];
    let mut put = |k: &str, v: Option<Attr>| attrs.extend(v.map(|v| (k.to_string(), v)));
    put("phase", op.phase.as_deref().map(s));
    put("subject", Some(s(&op.subject)));
    put("repo", op.repo.as_deref().map(s));
    put("start_ms", Some(ms(op.start.duration_since(UNIX_EPOCH).unwrap_or_default())));
    put("duration_ms", Some(ms(op.duration)));
    put("result", Some(s(&op.result)));
    put("write_bytes", op.cost.as_ref().map(|c| Attr::Int(c.write_bytes as i64)));
    put("cpu_seconds", op.cost.as_ref().map(|c| Attr::Float(c.cpu_seconds)));
    put("tests", op.tests.map(|t| Attr::Int(t as i64)));
    put("place", op.place.as_deref().map(s));
    let _ = LogSink::new(crate::dir_of(run), service).write(EVENT, &attrs, SystemTime::now());
}

pub fn ops_query() -> String {
    format!("_msg:\"{EVENT}\" | stats by (operation, phase) count() ops, quantile(0.5, duration_ms) p50_ms, sum(write_bytes) write_bytes, sum(cpu_seconds) cpu_seconds")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op() -> Op {
        Op {
            operation: "gate".into(),
            phase: Some("build".into()),
            subject: "sp-a".into(),
            repo: Some("r".into()),
            start: UNIX_EPOCH + Duration::from_secs(1000),
            duration: Duration::from_millis(2500),
            result: "PASS".into(),
            cost: Some(Cost { write_bytes: 4096, cpu_seconds: 1.5 }),
            tests: Some(7),
            place: Some("host".into()),
        }
    }

    fn read(d: &Path) -> Vec<(String, serde_json::Value)> {
        let recs = crate::logs::read_all(&crate::dir_of(d), "svc");
        assert_eq!(recs.len(), 1);
        recs.into_iter().next().unwrap()
    }

    #[test_id(t282k8e)]
    #[test]
    fn emit_writes_one_record_that_reads_back_with_every_attribute_typed() {
        let d = testkit::TempDir::new("otel-ops");
        emit(d.path(), "svc", &op());
        let r = read(d.path());
        let get = |k: &str| r.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_else(|| panic!("{k} missing"));
        for (k, want) in [("operation", "gate"), ("phase", "build"), ("subject", "sp-a"), ("repo", "r"), ("result", "PASS"), ("place", "host")] {
            assert_eq!(get(k)["stringValue"], want, "{k}");
        }
        for (k, want) in [("start_ms", "1000000"), ("duration_ms", "2500"), ("write_bytes", "4096"), ("tests", "7")] {
            assert_eq!(get(k)["intValue"].to_string().trim_matches('"'), want, "{k}");
        }
        assert_eq!(get("cpu_seconds")["doubleValue"], 1.5);
    }

    #[test_id(t9pycev)]
    #[test]
    fn an_op_without_cost_tests_or_phase_writes_none_of_those_attributes() {
        let d = testkit::TempDir::new("otel-ops-bare");
        emit(d.path(), "svc", &Op { cost: None, tests: None, phase: None, repo: None, place: None, ..op() });
        let r = read(d.path());
        for k in ["write_bytes", "cpu_seconds", "tests", "phase", "repo", "place"] {
            assert!(!r.iter().any(|(n, _)| n == k), "{k}");
        }
    }

    #[test_id(tftdud3)]
    #[test]
    fn the_heavy_operations_panel_runs_the_pinned_query() {
        let dash: serde_json::Value = serde_json::from_str(include_str!("../../spira/metrics/grafana/dashboards/harness-cost.json")).unwrap();
        let panel = dash["panels"].as_array().unwrap().iter().find(|p| p["title"] == "heavy operations").expect("the panel");
        assert_eq!(panel["targets"][0]["expr"], ops_query());
        assert_eq!(panel["datasource"]["uid"], "${DS_VL}");
    }
}
