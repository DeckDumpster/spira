use std::process::ExitCode;

pub const COMPONENT: &str = "desired-state";
pub const OPERATIONS: &[&str] = &["apply", "compose", "schema"];

pub fn endpoint() -> Option<String> {
    spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok()
}

pub fn operation(argv: &[String]) -> &'static str {
    spira_otel::metrics::run::operation_label(spira_otel::metrics::run::verb(argv), OPERATIONS)
}

pub fn metered_exit(argv: &[String], f: impl FnOnce() -> ExitCode) -> ExitCode {
    spira_otel::metrics::run::metered_exit(endpoint, COMPONENT, operation(argv), f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_pushes_its_count_duration_and_outcome_for_its_operation() {
        let (addr, h) = spira_otel::metrics::run::fixture_sink();
        let argv = vec!["apply".to_string()];
        let m = spira_otel::metrics::run::RunMeter::start(COMPONENT, operation(&argv));
        m.finish(|| Some(addr), "error", &[]);
        let body = h.join().unwrap();
        for want in ["spira_component_runs", "spira_component_run_seconds", COMPONENT, "apply", "error"] {
            assert!(body.contains(want), "{want} missing from the push");
        }
    }

    #[test]
    fn an_unlisted_verb_is_labelled_unknown() {
        assert_eq!(operation(&["--x".into(), "nope".into()]), "unknown");
    }
}
