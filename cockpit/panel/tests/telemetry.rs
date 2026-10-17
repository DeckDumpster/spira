//! `panel --once`, run for real, pushes the standard run metrics to the store its config names.

use spira_otel::metrics::fixture::{bare, Capture};
use std::path::Path;

#[test]
fn a_once_render_is_one_ok_run() {
    let cap = Capture::start();
    let tmp = testkit::TempDir::new("panel-telemetry");
    let toml = spira_config::process::fixture_toml(tmp.path(), &[("SPIRA_METRICS_ENDPOINT", &cap.addr), ("SPIRA_RUN", &tmp.path().display().to_string())]);
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spira");
    let out = bare(Path::new(env!("CARGO_BIN_EXE_panel")), &toml, &home)
        .args(["--once"])
        .env("PANEL_FIXTURE", concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixture.json"))
        .env("PANEL_NOW", "2026-09-05T16:30:00Z")
        .output()
        .expect("panel runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    cap.assert_run("panel", "once", "ok");
}
