//! `spira-config` and `spira-admit` push their invocation, duration and outcome; a config that fails to validate also counts a resolve failure.

use std::path::Path;
use std::process::Command;

fn run(bin: &str, args: &[&str], tag: &str, file: &str) -> String {
    let sink = testkit::OtlpSink::start();
    let tmp = testkit::TempDir::new(tag);
    let run_dir = tmp.join("run");
    std::fs::create_dir_all(&run_dir).unwrap();
    let toml = spira_config::process::fixture_toml(&tmp, &[("SPIRA_RUN", &run_dir.display().to_string()), ("SPIRA_METRICS_ENDPOINT", &sink.addr)]);
    let target = if file == "good" { toml.clone() } else { let bad = tmp.join("bad.toml"); std::fs::write(&bad, "this is not [valid toml").unwrap(); bad };
    Command::new("env")
        .arg("-i")
        .arg(format!("HOME={}", tmp.display()))
        .arg("PATH=/usr/bin:/bin")
        .arg(format!("SPIRA_HOME={}", Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira").display()))
        .arg(format!("SPIRA_TOML={}", toml.display()))
        .arg(bin)
        .args(args)
        .arg(target)
        .output()
        .unwrap();
    sink.body()
}

fn expect(pushed: &str, needles: &[&str]) {
    for needle in needles {
        assert!(pushed.contains(needle), "{needle} missing from the push: {pushed:?}");
    }
}

#[test]
fn a_valid_config_pushes_an_ok_run() {
    let pushed = run(env!("CARGO_BIN_EXE_spira-config"), &["validate"], "cfg-telemetry-ok", "good");
    expect(&pushed, &["spira_component_invocations", "spira_component_duration_seconds", "spira-config", "validate", "ok"]);
    assert!(!pushed.contains("spira_config_resolve_failures"), "{pushed:?}");
}

#[test]
fn a_config_that_does_not_validate_counts_a_resolve_failure() {
    let pushed = run(env!("CARGO_BIN_EXE_spira-config"), &["validate"], "cfg-telemetry-bad", "bad");
    expect(&pushed, &["spira_component_invocations", "error", "spira_config_resolve_failures"]);
}

#[test]
fn admit_status_pushes_its_run() {
    let pushed = run(env!("CARGO_BIN_EXE_spira-admit"), &["status"], "admit-telemetry", "good");
    expect(&pushed, &["spira_component_invocations", "spira-admit", "status"]);
}
