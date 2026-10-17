//! The binary, run for real under a bare environment, pushes the standard run metrics to the
//! store its config names.

use spira_otel::metrics::fixture::{bare, Capture};
use std::path::Path;

fn run(args: &[&str]) -> Capture {
    let cap = Capture::start();
    let tmp = testkit::TempDir::new("intent-report-telemetry");
    let toml = spira_config::process::fixture_toml(tmp.path(), &[("SPIRA_METRICS_ENDPOINT", &cap.addr), ("SPIRA_RUN", &tmp.path().display().to_string())]);
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    let mut c = bare(Path::new(env!("CARGO_BIN_EXE_intent-report")), &toml, &home);
    c.args(args).output().expect("intent-report runs");
    cap
}

#[test]
fn a_report_is_one_ok_run() {
    let dir = testkit::TempDir::new("intent-report-run");
    let cap = run(&["--run", &dir.path().display().to_string()]);
    cap.assert_run("intent-report", "report", "ok");
}
