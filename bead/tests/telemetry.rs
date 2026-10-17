//! `bead` and `bdq` push their invocation, duration and outcome to the metrics endpoint the config declares.

use std::path::Path;
use std::process::Command;

fn run(bin: &str, args: &[&str], tag: &str) -> String {
    let sink = testkit::OtlpSink::start();
    let tmp = testkit::TempDir::new(tag);
    let run_dir = tmp.join("run");
    std::fs::create_dir_all(&run_dir).unwrap();
    let toml = spira_config::process::fixture_toml(&tmp, &[("SPIRA_RUN", &run_dir.display().to_string()), ("SPIRA_METRICS_ENDPOINT", &sink.addr)]);
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    Command::new(bin)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", tmp.path())
        .env("SPIRA_TOML", &toml)
        .env("SPIRA_HOME", &home)
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
fn bead_pushes_an_ok_run_by_subcommand() {
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    let pushed = run(env!("CARGO_BIN_EXE_bead"), &["--home", &home.display().to_string(), "contract"], "bead-telemetry-ok");
    expect(&pushed, &["spira_component_invocations", "spira_component_duration_seconds", "spira-bead", "contract", "ok"]);
}

#[test]
fn bead_pushes_an_error_outcome_for_a_refused_call() {
    let pushed = run(env!("CARGO_BIN_EXE_bead"), &["bogus"], "bead-telemetry-err");
    expect(&pushed, &["spira_component_invocations", "bogus", "error", "exit_2"]);
}

#[test]
fn bdq_pushes_its_run() {
    let pushed = run(env!("CARGO_BIN_EXE_bdq"), &["__latency"], "bdq-telemetry");
    expect(&pushed, &["spira_component_invocations", "spira-bdq", "__latency"]);
}
