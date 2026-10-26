//! The binary, run for real under a bare environment, pushes the standard run metrics to the
//! store its config names.

use spira_otel::metrics::fixture::{bare, Capture};
use std::path::Path;

fn run(args: &[&str]) -> Capture {
    let cap = Capture::start();
    let tmp = testkit::TempDir::new("cockpit-collect-telemetry");
    let toml = spira_config::process::fixture_toml(tmp.path(), &[("SPIRA_METRICS_ENDPOINT", &cap.addr), ("SPIRA_RUN", &tmp.path().display().to_string())]);
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    let mut c = bare(Path::new(env!("CARGO_BIN_EXE_cockpit-collect")), &toml, &home);
    c.args(args).output().expect("cockpit-collect runs");
    cap
}

#[test]
fn a_pass_is_one_run_labelled_by_its_subcommand() {
    let cap = run(&["sweep-temps"]);
    cap.assert_ran("cockpit-collect", "sweep-temps");
}

#[test]
fn collect_started_while_the_lock_is_held_skips_at_once() {
    let tmp = testkit::TempDir::new("cockpit-collect-lock");
    let toml = spira_config::process::fixture_toml(tmp.path(), &[("SPIRA_RUN", &tmp.path().display().to_string())]);
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    let held = strand::state::try_lock(&tmp.path().join("cockpit-collect.lock")).unwrap().expect("first holder");
    let mut c = bare(Path::new(env!("CARGO_BIN_EXE_cockpit-collect")), &toml, &home);
    let out = c.arg("collect").output().expect("cockpit-collect runs");
    assert!(out.status.success(), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("previous collect still running"), "{err}");
    drop(held);
}
