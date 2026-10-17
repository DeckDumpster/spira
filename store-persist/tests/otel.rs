use spira_otel::fixture::Sink;
use spira_otel::metrics::{INVOCATIONS, RUN_SECONDS};

#[test]
fn a_run_pushes_its_invocation_duration_and_outcome() {
    let sink = Sink::start();
    let dir = testkit::TempDir::new("store-persist-otel");
    let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_METRICS_ENDPOINT", sink.addr())]);
    let got = sink.run(env!("CARGO_BIN_EXE_store-persist"), &["age"], &toml, concat!(env!("CARGO_MANIFEST_DIR"), "/../spira"), &[]);
    let inv = got.iter().find(|s| s.name == INVOCATIONS).expect("an invocation count was pushed");
    assert_eq!(
        (inv.service.as_str(), inv.label("component"), inv.label("operation"), inv.label("outcome")),
        ("spira-store-persist", Some("store-persist"), Some("age"), Some("exit_1"))
    );
    assert!(got.iter().any(|s| s.name == RUN_SECONDS && s.label("operation") == Some("age")));
}
