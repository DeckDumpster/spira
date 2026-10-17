//! Every `ctrl` run pushes its invocation, duration and outcome to the metrics endpoint the config declares.

use std::path::Path;
use std::process::Command;

fn run_ctrl(args: &[&str], tag: &str) -> (std::process::Output, String) {
    let sink = testkit::OtlpSink::start();
    let tmp = testkit::TempDir::new(tag);
    let ctrl_file = tmp.join("control.json");
    let toml = spira_config::process::fixture_toml(
        &tmp,
        &[("SPIRA_CTRL", &ctrl_file.display().to_string()), ("SPIRA_METRICS_ENDPOINT", &sink.addr)],
    );
    let out = Command::new("env")
        .arg("-i")
        .arg(format!("HOME={}", tmp.display()))
        .arg("PATH=/usr/bin:/bin")
        .arg(format!("SPIRA_HOME={}", Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira").display()))
        .arg(format!("SPIRA_TOML={}", toml.display()))
        .arg(env!("CARGO_BIN_EXE_ctrl"))
        .args(args)
        .output()
        .unwrap();
    (out, sink.body())
}

#[test]
fn a_run_pushes_its_invocation_duration_and_ok_outcome() {
    let (out, pushed) = run_ctrl(&["list"], "ctrl-telemetry-ok");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    for needle in ["spira_component_invocations", "spira_component_duration_seconds", "spira-ctrl", "list", "ok"] {
        assert!(pushed.contains(needle), "{needle} missing from the push: {pushed:?}");
    }
}

#[test]
fn a_failed_run_pushes_an_error_outcome_with_its_class() {
    let (out, pushed) = run_ctrl(&["suspend"], "ctrl-telemetry-err");
    assert!(!out.status.success());
    for needle in ["spira_component_invocations", "error", "exit_nonzero", "suspend"] {
        assert!(pushed.contains(needle), "{needle} missing from the push: {pushed:?}");
    }
}
