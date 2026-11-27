use std::process::ExitCode;

pub const COMPONENT: &str = "install";
pub const OPERATIONS: &[&str] = &["install", "units-install", "unit-ensure", "render-unit"];

pub fn endpoint() -> Option<String> {
    spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok()
}

/// Where one run's metrics go.
pub enum Endpoint {
    Process,
    /// The endpoint of a config the run derived itself; never falls back to the process config.
    Derived(Option<String>),
}

impl Endpoint {
    fn resolve(self, process: impl FnOnce() -> Option<String>) -> Option<String> {
        match self {
            Endpoint::Process => process(),
            Endpoint::Derived(e) => e,
        }
    }
}

/// [`metered_exit`] for a run that names its endpoint when it ends.
pub fn metered_exit_to(operation: &str, f: impl FnOnce() -> (ExitCode, Endpoint)) -> ExitCode {
    metered_with(endpoint, operation, f)
}

fn metered_with(process: impl FnOnce() -> Option<String>, operation: &str, f: impl FnOnce() -> (ExitCode, Endpoint)) -> ExitCode {
    let operation = spira_otel::metrics::run::operation_label(Some(operation), OPERATIONS);
    let m = spira_otel::metrics::run::RunMeter::start(COMPONENT, operation);
    let (code, to) = f();
    m.finish(|| to.resolve(process), spira_otel::metrics::outcome_of_exit(code), &[]);
    code
}

/// `operation` is one of [`OPERATIONS`]: the binary that ran.
pub fn metered_exit(operation: &str, f: impl FnOnce() -> ExitCode) -> ExitCode {
    let operation = spira_otel::metrics::run::operation_label(Some(operation), OPERATIONS);
    spira_otel::metrics::run::metered_exit(endpoint, COMPONENT, operation, f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tuljuny)]
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

    #[test_id(tden4h2)]
    #[test]
    fn a_derived_endpoint_is_the_only_one_a_run_pushes_to() {
        use std::time::Duration;
        let sink = |ms| spira_otel::metrics::run::fixture_sink_within(Duration::from_millis(ms));
        let ran = |to| move || (ExitCode::SUCCESS, to);

        let (addr, derived) = sink(2000);
        metered_with(|| None, "install", ran(Endpoint::Derived(Some(addr))));
        assert!(derived.join().unwrap().is_some_and(|b| b.contains("spira_component_invocations")));

        let (addr, process) = sink(50);
        metered_with(|| Some(addr), "install", ran(Endpoint::Derived(None)));
        assert_eq!(process.join().unwrap(), None, "a derived run fell back to the process config");
    }

    #[test_id(tqktz6p)]
    #[test]
    fn a_process_run_pushes_to_the_process_endpoint() {
        let (addr, sink) = spira_otel::metrics::run::fixture_sink_within(std::time::Duration::from_secs(2));
        metered_with(|| Some(addr), "install", || (ExitCode::SUCCESS, Endpoint::Process));
        assert!(sink.join().unwrap().is_some_and(|b| b.contains("spira_component_invocations")));
    }

    #[test_id(t8ajy35)]
    #[test]
    fn a_run_through_the_wrapper_keeps_its_exit_code() {
        assert!(format!("{:?}", metered_exit("nope", || ExitCode::from(2))) == format!("{:?}", ExitCode::from(2)));
    }
}
