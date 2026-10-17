//! `sim-fit` pushes its invocation, duration and outcome to the metrics endpoint the config declares.

use std::path::Path;
use std::process::Command;

#[test]
fn a_failed_fit_pushes_an_error_outcome() {
    let sink = testkit::OtlpSink::start();
    let tmp = testkit::TempDir::new("sim-fit-telemetry");
    let toml = spira_config::process::fixture_toml(&tmp, &[("SPIRA_METRICS_ENDPOINT", &sink.addr)]);
    let missing = tmp.join("missing.log").display().to_string();
    let out = tmp.join("durations.toml").display().to_string();
    Command::new(env!("CARGO_BIN_EXE_sim-fit"))
        .args(["--gate-log", &missing, "--land-times", &missing, "--out", &out])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", tmp.path())
        .env("SPIRA_TOML", &toml)
        .env("SPIRA_HOME", Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira"))
        .output()
        .unwrap();
    let pushed = sink.body();
    for needle in ["spira_component_invocations", "spira_component_duration_seconds", "spira-sim-fit", "fit", "error"] {
        assert!(pushed.contains(needle), "{needle} missing from the push: {pushed:?}");
    }
}
