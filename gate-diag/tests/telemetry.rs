//! A run of the binary pushes its invocation, duration, suites and reds by class to the metrics sink.

#[test]
fn a_run_pushes_its_invocation_duration_suites_and_reds() {
    let sink = testkit::OtlpSink::start();
    let dir = testkit::TempDir::new("gate-diag-telemetry");
    let root = dir.join("results");
    std::fs::create_dir_all(&root).unwrap();
    let env = spira_config::process::fixture_metrics_env(&dir, &sink.addr, &[]);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_gate-diag"))
        .arg(&root)
        .env_clear()
        .envs(env)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let body = sink.body();
    for want in ["spira_component_invocations", "spira_component_duration_seconds", "spira_gate_diag_suites", "gate-diag", "diagnose", "ok"] {
        assert!(body.contains(want), "{want} missing from the push: {body:?}");
    }
}
