//! Fire-and-forget OTLP metrics for `sending`: one run and its seconds per invocation, by operation and outcome.

use spira_otel::metrics::{self, MetricsSink};

pub const COMPONENT: &str = "sending";
pub const OPERATIONS: &[&str] = &["reap-terminal", "destroy-worktree", "destroy-branch", "reap-landed-branch", "prune", "reap-stale", "salvage", "witness", "hold-alive", "holder-alive", "stop-holder", "worktree-of", "caller", "reaplog"];

pub fn sink() -> MetricsSink {
    metrics::sink_for(spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref(), COMPONENT)
}

pub fn operation(first_arg: Option<&str>) -> &'static str {
    match first_arg {
        Some(a) if a.starts_with('-') => "sweep",
        a => metrics::operation(a, OPERATIONS),
    }
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
        let (code, got) = fixture::capture(|s| run(s, Some("reap-terminal"), || 1));
        assert_eq!(code, 1);
        let runs = got.iter().find(|c| c.name == metrics::RUNS).expect("run count pushed");
        assert_eq!((runs.label("component"), runs.label("operation"), runs.label("outcome")), (Some("sending"), Some("reap-terminal"), Some("error")));
        assert!(got.iter().any(|c| c.name == metrics::RUN_SECONDS), "run seconds pushed");
    }

    #[test]
    fn an_argument_that_is_no_operation_is_other() {
        assert_eq!(operation(Some("sp-abc123")), "other");
        assert_eq!(operation(None), "other");
    }

    #[test]
    fn a_sweep_flag_is_the_sweep_operation() {
        assert_eq!(operation(Some("--all")), "sweep");
    }
}
