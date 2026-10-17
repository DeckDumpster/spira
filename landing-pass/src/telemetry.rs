//! Fire-and-forget OTLP metrics for `landing-pass`: one run and its seconds per invocation, by operation and outcome.

use spira_otel::metrics::{self, MetricsSink};

pub const COMPONENT: &str = "landing-pass";
pub const OPERATIONS: &[&str] = &["pass", "land", "halt", "noverdict", "ask-rebase-loop", "land-subject", "pr-merged", "conflict-note", "other-beads", "is-work-type"];

pub fn sink() -> MetricsSink {
    metrics::sink_for(spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref(), COMPONENT)
}

pub fn operation(first_arg: Option<&str>) -> &'static str {
    metrics::operation(first_arg.map(|a| a.trim_start_matches('-')), OPERATIONS)
}

pub fn run(sink: &MetricsSink, first_arg: Option<&str>, f: impl FnOnce() -> i32) -> i32 {
    let op = operation(first_arg);
    metrics::timed(sink, COMPONENT, op, f)
}

/// Beads a landing pass moved onto the landing ref.
pub fn moved(sink: &MetricsSink, n: u64) {
    sink.push(&[metrics::Point::Count { name: "spira_landing_moved", value: n }], &[]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_otel::metrics::fixture;

    #[test]
    fn a_run_pushes_its_count_and_seconds_labelled_by_component_operation_and_outcome() {
        let (code, got) = fixture::capture(|s| run(s, Some("pass"), || 1));
        assert_eq!(code, 1);
        let runs = got.iter().find(|c| c.name == metrics::RUNS).expect("run count pushed");
        assert_eq!((runs.label("component"), runs.label("operation"), runs.label("outcome")), (Some("landing-pass"), Some("pass"), Some("error")));
        assert!(got.iter().any(|c| c.name == metrics::RUN_DURATION), "run seconds pushed");
    }

    #[test]
    fn an_argument_that_is_no_operation_is_other() {
        assert_eq!(operation(Some("sp-abc123")), "other");
        assert_eq!(operation(None), "other");
    }

    #[test]
    fn the_pr_flag_is_the_pass_operation() {
        assert_eq!(operation(Some("--pass")), "pass");
    }

    #[test]
    fn landing_movements_are_counted_by_the_beads_moved() {
        let (_, got) = fixture::capture(|s| moved(s, 3));
        let m = got.iter().find(|c| c.name == "spira_landing_moved").expect("movements pushed");
        assert_eq!(m.value, 3.0);
    }
}
