//! Fire-and-forget OTLP metrics for `queue`: one run and its seconds per invocation, by operation and outcome.

use spira_otel::metrics::{self, MetricsSink};

pub const COMPONENT: &str = "queue";
pub const OPERATIONS: &[&str] = &["submit", "protect", "stats", "churn", "flush", "step", "verdict", "eject", "abandon", "open-batch", "claim", "release", "resubmit", "absolve", "rework", "land-local", "publish", "publish-settle", "to-forge", "to-local", "rollback-local", "rebase-waiting", "round"];

pub fn sink() -> MetricsSink {
    metrics::sink_for(spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref(), COMPONENT)
}

pub fn operation(first_arg: Option<&str>) -> &'static str {
    metrics::operation(first_arg, OPERATIONS)
}

pub fn run(sink: &MetricsSink, first_arg: Option<&str>, f: impl FnOnce() -> i32) -> i32 {
    let op = operation(first_arg);
    let code = metrics::timed(sink, COMPONENT, op, f);
    if let Some(event) = batch_event(op).filter(|_| code == 0) {
        sink.push(&[metrics::Point::Count { name: "spira_queue_batch_events", value: 1 }], &[("event", event)]);
    }
    code
}

/// The batch lifecycle event a successful `queue` operation is.
pub fn batch_event(op: &str) -> Option<&'static str> {
    match op {
        "open-batch" => Some("opened"),
        "abandon" => Some("abandoned"),
        "land-local" => Some("landed"),
        "rollback-local" => Some("rolled_back"),
        "eject" => Some("ejected"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_otel::metrics::fixture;

    #[test]
    fn a_run_pushes_its_count_and_seconds_labelled_by_component_operation_and_outcome() {
        let (code, got) = fixture::capture(|s| run(s, Some("submit"), || 1));
        assert_eq!(code, 1);
        let runs = got.iter().find(|c| c.name == metrics::RUNS).expect("run count pushed");
        assert_eq!((runs.label("component"), runs.label("operation"), runs.label("outcome")), (Some("queue"), Some("submit"), Some("error")));
        assert!(got.iter().any(|c| c.name == metrics::RUN_DURATION), "run seconds pushed");
    }

    #[test]
    fn an_argument_that_is_no_operation_is_other() {
        assert_eq!(operation(Some("sp-abc123")), "other");
        assert_eq!(operation(None), "other");
    }

    #[test]
    fn a_successful_open_batch_counts_a_batch_opened_and_a_failed_one_does_not() {
        let (_, got) = fixture::capture(|s| run(s, Some("open-batch"), || 0));
        let ev = got.iter().find(|c| c.name == "spira_queue_batch_events").expect("batch event pushed");
        assert_eq!(ev.label("event"), Some("opened"));
        let (_, got) = fixture::capture(|s| run(s, Some("open-batch"), || 1));
        assert!(got.iter().all(|c| c.name != "spira_queue_batch_events"));
    }
}
