//! Fire-and-forget OTLP metrics for `rebase-stale`: one run and its seconds per invocation, by operation and outcome.

use spira_otel::metrics::{self, MetricsSink};

pub const COMPONENT: &str = "rebase-stale";
pub const OPERATIONS: &[&str] = &["rebase-branch", "recut-onto"];

pub fn sink() -> MetricsSink {
    metrics::sink_for(spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref(), COMPONENT)
}

pub fn operation(first_arg: Option<&str>) -> &'static str {
    match first_arg {
        Some(a) if OPERATIONS.contains(&a) => metrics::operation(Some(a), OPERATIONS),
        _ => "rebase",
    }
}

pub fn run(sink: &MetricsSink, first_arg: Option<&str>, f: impl FnOnce() -> i32) -> i32 {
    let op = operation(first_arg);
    let code = metrics::timed_as(sink, COMPONENT, op, |c| outcome(op, c), f);
    if outcome(op, code) == "conflict" {
        sink.push(&[metrics::Point::Count { name: "spira_rebase_conflicts", value: 1 }], &[]);
    }
    code
}

/// `rebase-stale <bead>` exits 0 ok, 1 real conflict, 2 red gate, 3 not attempted.
pub fn outcome(op: &str, code: i32) -> &'static str {
    match (op, code) {
        ("rebase", 1) => "conflict",
        ("rebase", 2) => "red_gate",
        ("rebase", 3) => "not_attempted",
        _ => metrics::outcome_of(code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_otel::metrics::fixture;

    #[test]
    fn a_run_pushes_its_count_and_seconds_labelled_by_component_operation_and_outcome() {
        let (code, got) = fixture::capture(|s| run(s, Some("rebase-branch"), || 1));
        assert_eq!(code, 1);
        let runs = got.iter().find(|c| c.name == metrics::RUNS).expect("run count pushed");
        assert_eq!((runs.label("component"), runs.label("operation"), runs.label("outcome")), (Some("rebase-stale"), Some("rebase-branch"), Some("error")));
        assert!(got.iter().any(|c| c.name == metrics::RUN_DURATION), "run seconds pushed");
    }

    #[test]
    fn a_bead_id_is_the_rebase_operation() {
        assert_eq!(operation(Some("sp-abc123")), "rebase");
        assert_eq!(operation(None), "rebase");
    }

    #[test]
    fn a_conflict_exit_is_an_outcome_and_counts_a_rebase_conflict() {
        let (_, got) = fixture::capture(|s| run(s, Some("sp-abc"), || 1));
        let runs = got.iter().find(|c| c.name == metrics::RUNS).unwrap();
        assert_eq!((runs.label("operation"), runs.label("outcome")), (Some("rebase"), Some("conflict")));
        assert!(got.iter().any(|c| c.name == "spira_rebase_conflicts"));
    }
}
