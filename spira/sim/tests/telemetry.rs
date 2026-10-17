//! `sim` and `sim-agent` push their invocation, duration and outcome to the metrics endpoint the config declares.

use std::path::Path;
use std::process::Command;

fn run(bin: &str, args: &[&str], tag: &str) -> String {
    let sink = testkit::OtlpSink::start();
    let tmp = testkit::TempDir::new(tag);
    let toml = spira_config::process::fixture_toml(&tmp, &[("SPIRA_METRICS_ENDPOINT", &sink.addr)]);
    Command::new(bin)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", tmp.path())
        .env("SPIRA_TOML", &toml)
        .env("SPIRA_HOME", Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spira"))
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
fn sim_pushes_an_error_outcome_for_a_refused_call() {
    let pushed = run(env!("CARGO_BIN_EXE_sim"), &["bogus"], "sim-telemetry");
    expect(&pushed, &["spira_component_invocations", "spira_component_duration_seconds", "spira-sim", "bogus", "error", "exit_2"]);
}

#[test]
fn sim_agent_pushes_an_ok_run() {
    let pushed = run(env!("CARGO_BIN_EXE_sim-agent"), &[], "sim-agent-telemetry");
    expect(&pushed, &["spira_component_invocations", "spira-sim-agent", "play", "ok"]);
}
