//! Fire-and-forget OTLP metrics for `forge`: one run and its seconds per invocation, by operation and outcome.

use spira_otel::metrics::{self, MetricsSink};

pub const COMPONENT: &str = "forge";
pub const OPERATIONS: &[&str] = &["pr-create", "pr-number", "pr-list-queue", "pr-list-open", "pr-mergeability", "pr-state", "pr-merge-commit", "pr-red", "pr-automerge", "pr-arm-queue", "check-status", "run-id", "runs-for-branch", "runs-queue-branches", "batch-ci-status", "queued-since", "runs-active", "stranded-runners", "run-metadata", "run-cancel", "force-cancel", "workflow-rerun", "pr-close", "pr-comment", "dispatch", "fail-lines", "branch-protect", "branch-protection-status"];

pub fn sink() -> MetricsSink {
    metrics::sink_for(spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref(), COMPONENT)
}

pub fn operation(first_arg: Option<&str>) -> &'static str {
    metrics::operation(first_arg, OPERATIONS)
}

pub fn run(sink: &MetricsSink, first_arg: Option<&str>, f: impl FnOnce() -> i32) -> i32 {
    let op = operation(first_arg);
    metrics::timed(sink, COMPONENT, op, f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_otel::metrics::fixture;

    #[test]
    fn a_run_pushes_its_count_and_seconds_labelled_by_component_operation_and_outcome() {
        let (code, got) = fixture::capture(|s| run(s, Some("pr-create"), || 1));
        assert_eq!(code, 1);
        let runs = got.iter().find(|c| c.name == metrics::RUNS).expect("run count pushed");
        assert_eq!((runs.label("component"), runs.label("operation"), runs.label("outcome")), (Some("forge"), Some("pr-create"), Some("error")));
        assert!(got.iter().any(|c| c.name == metrics::RUN_SECONDS), "run seconds pushed");
    }

    #[test]
    fn an_argument_that_is_no_operation_is_other() {
        assert_eq!(operation(Some("sp-abc123")), "other");
        assert_eq!(operation(None), "other");
    }
}
