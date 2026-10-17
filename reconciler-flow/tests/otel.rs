use spira_otel::fixture::Sink;
use spira_otel::metrics::{INVOCATIONS, RUN_SECONDS};

#[test]
fn a_run_pushes_its_invocation_duration_and_outcome() {
    let sink = Sink::start();
    let dir = testkit::TempDir::new("reconciler-flow-otel");
    let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_METRICS_ENDPOINT", sink.addr())]);
    let got = sink.run(env!("CARGO_BIN_EXE_reconciler-flow"), &["--pass"], &toml, concat!(env!("CARGO_MANIFEST_DIR"), "/../spira"), &[]);
    let inv = got.iter().find(|s| s.name == INVOCATIONS).expect("an invocation count was pushed");
    assert_eq!(
        (inv.service.as_str(), inv.label("component"), inv.label("operation")),
        ("spira-reconciler-flow", Some("reconciler-flow"), Some("pass"))
    );
    let outcome = inv.label("outcome").expect("an outcome label");
    assert!(outcome == "ok" || outcome.starts_with("exit_"), "outcome {outcome}");
    assert!(got.iter().any(|s| s.name == RUN_SECONDS && s.label("operation") == Some("pass")));
}
