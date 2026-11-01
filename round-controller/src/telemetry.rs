//! Fire-and-forget OTLP metrics for `round-controller`: one run and its seconds per invocation,
//! by verb and outcome (law-every-component-sends-otlp).

use spira_otel::metrics::{self, MetricsSink};

pub const COMPONENT: &str = "round-controller";
pub const OPERATIONS: &[&str] = &["version", "pass"];

pub fn sink() -> MetricsSink {
    metrics::sink_for(
        spira_config::process::cfg("SPIRA_METRICS_ENDPOINT")
            .ok()
            .as_deref(),
        COMPONENT,
    )
}

pub fn operation(first_arg: Option<&str>) -> &'static str {
    metrics::operation(first_arg, OPERATIONS)
}

pub fn run(sink: &MetricsSink, first_arg: Option<&str>, f: impl FnOnce() -> i32) -> i32 {
    metrics::timed(sink, COMPONENT, operation(first_arg), f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_otel::metrics::fixture;

    #[test]
    fn a_run_pushes_its_count_and_seconds_labelled_by_component_verb_and_outcome() {
        let (code, got) = fixture::capture(|s| run(s, Some("pass"), || 1));
        assert_eq!(code, 1);
        let runs = got
            .iter()
            .find(|c| c.name == metrics::RUNS)
            .expect("run count pushed");
        assert_eq!(
            (
                runs.label("component"),
                runs.label("operation"),
                runs.label("outcome")
            ),
            (Some(COMPONENT), Some("pass"), Some("error"))
        );
        assert!(
            got.iter().any(|c| c.name == metrics::RUN_DURATION),
            "run seconds pushed"
        );
    }

    #[test]
    fn each_verb_is_its_own_operation_and_anything_else_is_other() {
        assert_eq!(operation(Some("version")), "version");
        assert_eq!(operation(Some("--repo")), "other");
        assert_eq!(operation(None), "other");
    }
}
