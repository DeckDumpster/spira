use std::process::Command;
use std::time::Duration;

use testkit::otlp::Sink;
use testkit::TempDir;

#[test]
fn a_run_pushes_its_standard_metrics_to_the_configured_endpoint() {
    let dir = TempDir::new("otel-archivist");
    let sink = Sink::start();
    let home = dir.join("home");
    std::fs::create_dir(&home).expect("home dir");
    std::os::unix::fs::symlink(concat!(env!("CARGO_MANIFEST_DIR"), "/../spira/conf.d"), home.join("conf.d")).expect("conf.d link");
    let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_METRICS_ENDPOINT", &sink.addr)]);
    let out = Command::new(env!("CARGO_BIN_EXE_archivist"))
        .args(["bogus"])
        .env_clear()
        .env("SPIRA_TOML", &toml)
        .env("SPIRA_HOME", &home)
        .output()
        .expect("the binary runs");
    assert!(!out.status.success(), "the fixture invocation is a failing one");
    sink.expect(&["spira_component_runs", "spira_component_run_seconds", "archivist", "outcome", "error", "exit_"], Duration::from_secs(5));
}
