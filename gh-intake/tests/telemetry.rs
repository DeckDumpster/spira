//! The binary, run for real under a bare environment, pushes the standard run metrics to the
//! store its config names.

use spira_otel::metrics::fixture::{bare, Capture};
use std::path::Path;

fn run(args: &[&str]) -> Capture {
    let cap = Capture::start();
    let tmp = testkit::TempDir::new("gh-intake-telemetry");
    let toml = spira_config::process::fixture_toml(tmp.path(), &[("SPIRA_METRICS_ENDPOINT", &cap.addr), ("SPIRA_RUN", &tmp.path().display().to_string())]);
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    let mut c = bare(Path::new(env!("CARGO_BIN_EXE_gh-intake")), &toml, &home);
    c.args(args).output().expect("gh-intake runs");
    cap
}

#[test]
fn a_refused_run_is_still_one_failed_run() {
    let cap = run(&["--no-such-flag"]);
    cap.assert_run("gh-intake", "intake", "error");
}
