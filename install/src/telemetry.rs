use std::process::ExitCode;

pub const COMPONENT: &str = "install";
pub const OPERATIONS: &[&str] = &["install", "units-install", "unit-ensure", "render-unit"];

pub fn endpoint() -> Option<String> {
    spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok()
}

/// `operation` is one of [`OPERATIONS`]: the binary that ran.
pub fn metered_exit(operation: &str, f: impl FnOnce() -> ExitCode) -> ExitCode {
    let operation = spira_otel::metrics::run::operation_label(Some(operation), OPERATIONS);
    spira_otel::metrics::run::metered_exit(endpoint, COMPONENT, operation, f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_install_pushes_its_count_duration_and_error_outcome() {
        let (addr, h) = spira_otel::metrics::run::fixture_sink();
        let m = spira_otel::metrics::run::RunMeter::start(COMPONENT, "units-install");
        m.finish(|| Some(addr), "error", &[]);
        let body = h.join().unwrap();
        for want in ["spira_component_invocations", "spira_component_duration_seconds", "install", "units-install", "error"] {
            assert!(body.contains(want), "{want} missing from the push");
        }
    }

    #[test]
    fn a_run_through_the_wrapper_keeps_its_exit_code() {
        assert!(format!("{:?}", metered_exit("nope", || ExitCode::from(2))) == format!("{:?}", ExitCode::from(2)));
    }
}
