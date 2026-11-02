//! A run of the binary pushes its invocation, duration and outcome to the metrics sink.

#[test]
fn a_run_pushes_its_invocation_duration_and_outcome() {
    let sink = testkit::OtlpSink::start();
    let dir = testkit::TempDir::new("gate-check-telemetry");
    let bin = testkit::TempDir::new("gate-check-telemetry-bdq");
    testkit::write_exe(bin.join("bdq"), "#!/bin/sh\necho '[]'\n");
    let env = spira_config::process::fixture_metrics_env(&dir, &sink.addr, &[("SPIRA_FLAKY_GH_REPO", "")]);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_gate-check"))
        .env_clear()
        .envs(env)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.path().display()))
        .output()
        .unwrap();
    let body = sink.body();
    for want in ["spira_component_invocations", "spira_component_duration_seconds", "gate-check", "check"] {
        assert!(body.contains(want), "{want} missing from the push: {body:?} (stderr {})", String::from_utf8_lossy(&out.stderr));
    }
}
