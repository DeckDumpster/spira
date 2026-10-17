//! `rule` pushes its invocation, duration and outcome to the metrics endpoint the config declares.

use std::path::Path;
use std::process::Command;

fn run(args: &[&str], tag: &str) -> String {
    let sink = testkit::OtlpSink::start();
    let tmp = testkit::TempDir::new(tag);
    let toml = spira_config::process::fixture_toml(&tmp, &[("SPIRA_DB", &tmp.join("nodb").display().to_string()), ("SPIRA_METRICS_ENDPOINT", &sink.addr)]);
    Command::new(env!("CARGO_BIN_EXE_rule"))
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", tmp.path())
        .env("SPIRA_TOML", &toml)
        .env("SPIRA_HOME", Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira"))
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
fn a_run_pushes_its_invocation_duration_and_outcome() {
    let pushed = run(&["system-prompt-split"], "rule-telemetry-ok");
    expect(&pushed, &["spira_component_invocations", "spira_component_duration_seconds", "spira-rule", "system-prompt-split"]);
}

#[test]
fn an_enact_against_no_store_pushes_an_error_outcome() {
    let pushed = run(&["enact", "law-x"], "rule-telemetry-err");
    expect(&pushed, &["spira_component_invocations", "enact", "error", "exit_1"]);
    assert!(!pushed.contains("spira_statutes_changed"), "{pushed:?}");
}
