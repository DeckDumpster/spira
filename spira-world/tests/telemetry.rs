//! Each of the three binaries pushes its invocation, duration and outcome to the metrics endpoint the config declares.

use std::path::Path;

fn run(bin: &str, args: &[&str], tag: &str) -> String {
    let sink = spira_otel::fixture::Sink::start();
    let tmp = testkit::TempDir::new(tag);
    let run_dir = tmp.join("run");
    std::fs::create_dir_all(&run_dir).unwrap();
    let toml = spira_config::process::fixture_toml(
        &tmp,
        &[("SPIRA_RUN", &run_dir.display().to_string()), ("SPIRA_METRICS_ENDPOINT", sink.addr())],
    );
    let stub = tmp.join("systemctl");
    testkit::write_exe(&stub, "#!/bin/sh\nexit 0\n");
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    let seen = sink.run(bin, args, &toml, &home.display().to_string(), &[("HOME", &tmp.display().to_string()), ("SPIRA_SYSTEMCTL", &stub.display().to_string())]);
    seen.iter().map(|p| format!("{} {:?}", p.name, p.labels)).collect::<Vec<_>>().join("\n")
}

fn expect(pushed: &str, needles: &[&str]) {
    for needle in needles {
        assert!(pushed.contains(needle), "{needle} missing from the push: {pushed:?}");
    }
}

#[test]
fn world_pushes_its_run_by_subcommand() {
    expect(&run(env!("CARGO_BIN_EXE_world"), &["status"], "world-telemetry"), &["spira_component_invocations", "spira_component_duration_seconds", "spira-world", "status"]);
}

#[test]
fn aeons_pushes_its_run_by_subcommand() {
    expect(&run(env!("CARGO_BIN_EXE_aeons"), &["status"], "aeons-telemetry"), &["spira_component_invocations", "spira-aeons", "status"]);
}

#[test]
fn slay_pushes_an_error_outcome_when_it_refuses() {
    expect(&run(env!("CARGO_BIN_EXE_slay"), &["--bead", "sp-nosuch"], "slay-telemetry"), &["spira_component_invocations", "spira-slay", "error", "exit_2"]);
}
