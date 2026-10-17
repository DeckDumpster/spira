//! `sop` pushes its invocation, duration and outcome to the metrics endpoint the config declares.

use std::path::Path;
use std::process::Command;

fn run(args: &[&str], tag: &str) -> String {
    let sink = testkit::OtlpSink::start();
    let tmp = testkit::TempDir::new(tag);
    let toml = spira_config::process::fixture_toml(&tmp, &[("SPIRA_METRICS_ENDPOINT", &sink.addr)]);
    Command::new(env!("CARGO_BIN_EXE_sop"))
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
fn a_refused_call_pushes_an_error_outcome_by_subcommand() {
    let pushed = run(&["bogus"], "sop-telemetry-err");
    expect(&pushed, &["spira_component_invocations", "spira_component_duration_seconds", "spira-sop", "bogus", "error", "exit_nonzero"]);
}

#[test]
fn a_clean_call_pushes_an_ok_outcome() {
    let pushed = run(&["validate", "sop-x"], "sop-telemetry-ok");
    expect(&pushed, &["spira_component_invocations", "spira-sop", "validate"]);
}
