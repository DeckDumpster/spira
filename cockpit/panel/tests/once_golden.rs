//! Gap #5 of docs/test-plan/cockpit-observability.md: `tests/fixture.json` was read by
//! nothing — no cargo test, no shell suite — and the `--once` path (`PANEL_FIXTURE=...
//! --once`, capture.sh's own recipe for a human-free screenshot) had no automated coverage
//! either. This exercises both at once: PANEL_FIXTURE plus PANEL_NOW make the frame
//! deterministic, so the assertions are about the render, not about which second it ran.

use std::process::Command;

fn run_once(size: &str) -> (std::process::ExitStatus, String, String) {
    let exe = env!("CARGO_BIN_EXE_panel");
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixture.json");
    let out = Command::new(exe)
        .args(["--once", "--size", size])
        .env("PANEL_FIXTURE", fixture)
        .env("PANEL_NOW", "2026-09-05T16:30:00Z")
        .output()
        .expect("panel --once failed to spawn");
    (
        out.status,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn once_renders_the_fixture_deterministically() {
    let (status, stdout, stderr) = run_once("107x19");
    assert!(status.success(), "panel --once exited {status:?}: {stderr}");

    let lines: Vec<&str> = stdout.lines().collect();
    // `--size WxH` sets the frame height exactly: a frame that painted fewer or more rows
    // than the pane's actual geometry is the untested defect capture.sh's own comment warns
    // about (a frame correct on stdout but wrong on screen).
    assert_eq!(
        lines.len(),
        19,
        "expected exactly 19 rows for --size 107x19, got {}:\n{stdout}",
        lines.len()
    );

    // The default view is Decisions: an OPEN decision from the fixture must appear...
    assert!(
        stdout.contains("connection-pool"),
        "expected an open decision's title in the Decisions frame:\n{stdout}"
    );
    // ...a CLOSED one must not (closed asks are excluded from the open Decisions view) — the
    // fixture's own `_note` calls this out as one of the shapes the pane has to get right.
    assert!(
        !stdout.contains("retry budget"),
        "a closed decision's title leaked into the open Decisions frame:\n{stdout}"
    );
    // An Alerts-tab row must not appear on the Decisions tab shown by default.
    assert!(
        !stdout.contains("sentinel has not completed a pass"),
        "an Alerts-tab row leaked into the default Decisions frame:\n{stdout}"
    );
}

#[test]
fn once_selects_the_requested_item() {
    // `--sel N` starts on the Nth item — main.rs's own comment says layout depends on which
    // item is selected, so a render change "cannot be measured without being able to pick
    // the item under test". Two different selections over the same fixture must not render
    // byte-identical frames, or --sel would be dead on arrival.
    let exe = env!("CARGO_BIN_EXE_panel");
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixture.json");
    let run = |sel: &str| {
        Command::new(exe)
            .args(["--once", "--size", "107x19", "--sel", sel])
            .env("PANEL_FIXTURE", fixture)
            .env("PANEL_NOW", "2026-09-05T16:30:00Z")
            .output()
            .expect("panel --once --sel failed to spawn")
    };
    let a = run("0");
    let b = run("1");
    assert!(a.status.success() && b.status.success());
    assert_ne!(
        String::from_utf8_lossy(&a.stdout),
        String::from_utf8_lossy(&b.stdout),
        "selecting a different item produced an identical frame"
    );
}
