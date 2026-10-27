//! The binary, run for real under a bare environment, pushes the standard run metrics to the
//! store its config names.

use spira_otel::metrics::fixture::{bare, Capture};
use std::path::Path;

fn run(args: &[&str]) -> Capture {
    let cap = Capture::start();
    let tmp = testkit::TempDir::new("heimdall-telemetry");
    let toml = spira_config::process::fixture_toml(tmp.path(), &[("SPIRA_METRICS_ENDPOINT", &cap.addr), ("SPIRA_RUN", &tmp.path().display().to_string())]);
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    let mut c = bare(Path::new(env!("CARGO_BIN_EXE_heimdall")), &toml, &home);
    c.args(args).output().expect("heimdall runs");
    cap
}

#[test]
fn a_pass_pushes_its_run_metrics_and_the_firing_count() {
    let cap = run(&["--report"]);
    cap.assert_ran("heimdall", "pass");
    assert!(cap.samples(3).iter().any(|s| s.name == "spira_heimdall_firing_alerts"));
}
