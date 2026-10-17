//! A run of the binary pushes its invocation, duration and outcome to the metrics sink.

#[test]
fn a_run_pushes_its_invocation_duration_and_outcome() {
    let sink = testkit::OtlpSink::start();
    let dir = testkit::TempDir::new("gate-run-telemetry");
    let env = spira_config::process::fixture_metrics_env(&dir, &sink.addr, &[]);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_gate-run"))
        .arg("--status")
        .env_clear()
        .envs(env)
        .output()
        .unwrap();
    assert!(!out.status.success(), "no branch is a usage error");
    let body = sink.body();
    for want in ["spira_component_invocations", "spira_component_duration_seconds", "gate-run", "status", "error"] {
        assert!(body.contains(want), "{want} missing from the push: {body:?}");
    }
}
