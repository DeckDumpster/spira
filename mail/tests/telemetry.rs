//! The binary, run for real under a bare environment, pushes the standard run metrics to the
//! store its config names.

use spira_otel::metrics::fixture::{bare, Capture};
use std::path::Path;

fn run(args: &[&str]) -> Capture {
    let cap = Capture::start();
    let tmp = testkit::TempDir::new("mail-telemetry");
    let toml = spira_config::process::fixture_toml(tmp.path(), &[("SPIRA_METRICS_ENDPOINT", &cap.addr), ("SPIRA_RUN", &tmp.path().display().to_string())]);
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    let mut c = bare(Path::new(env!("CARGO_BIN_EXE_mail")), &toml, &home);
    c.args(args).output().expect("mail runs");
    cap
}

#[test]
fn an_unknown_command_is_one_failed_run_under_a_bounded_operation() {
    let cap = run(&["frobnicate-9f3a"]);
    cap.assert_run("mail", "unknown", "error");
}

#[test]
fn a_command_is_one_run_labelled_by_its_name() {
    let cap = run(&["unread-age", "no-such-mailbox"]);
    cap.assert_ran("mail", "unread-age");
}
