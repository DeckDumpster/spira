//! Every periodic check stands down, through the real binary, while the world is halted or the
//! detectors' lane is stopped on every registered repo, and says which.

use std::process::Command;

const CHECKS: [&str; 12] = [
    "throttle-check",
    "czar-outcome-check",
    "pr-stall-check",
    "lock-holders-check",
    "release-skew-check",
    "deploy-fault-check",
    "sccache-wedge-check",
    "conditions-check",
    "dolt-drop-check",
    "slow-query-check",
    "drift-check",
    "sweep",
];

fn run_check(d: &testkit::TempDir, run: &std::path::Path, check: &str) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_watchtower"));
    if check != "sweep" {
        cmd.arg(format!("--{check}"));
    }
    cmd.env_clear().env("PATH", std::env::var("PATH").unwrap_or_default());
    for (k, v) in spira_config::process::fixture_world_env(d.path(), run, &["spira", "other"]) {
        if let Some(v) = v {
            cmd.env(k, v);
        }
    }
    let out = cmd.output().expect("watchtower runs");
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn world(tag: &str) -> (testkit::TempDir, std::path::PathBuf) {
    let d = testkit::TempDir::new(tag);
    let run = d.path().join("run");
    std::fs::create_dir_all(&run).unwrap();
    (d, run)
}

fn stop(run: &std::path::Path, repo: &str) {
    spira_config::scoped_stop::pause_repo(run, repo, &["heimdall".to_string()], "cutover-mode", "cutover", "world stop", 1).unwrap();
}

#[test]
fn every_check_stands_down_when_the_detectors_lane_is_stopped_on_every_repo() {
    let (d, run) = world("watchtower-lane");
    stop(&run, "spira");
    stop(&run, "other");
    for check in CHECKS {
        let out = run_check(&d, &run, check);
        assert!(out.contains(&format!("watchtower: {check}: skipped — the heimdall lane of spira is stopped on purpose by cutover-mode")) && out.contains("world stop"), "{check}: {out}");
    }
}

#[test]
fn a_check_does_not_stand_down_while_one_repo_still_takes_the_detectors_lane() {
    let (d, run) = world("watchtower-lane-partial");
    stop(&run, "spira");
    for check in CHECKS.iter().filter(|c| **c != "sweep") {
        let out = run_check(&d, &run, check);
        assert!(!out.contains("the heimdall lane of") && !out.contains("stopped on purpose"), "{check}: {out}");
    }
}

// covers: UC-ops-detection-remediation-25
#[test]
fn every_periodic_check_stands_down_while_the_world_is_halted() {
    let (d, run) = world("watchtower-halted");
    std::fs::write(run.join("world.halted"), "").unwrap();
    for check in CHECKS.iter().filter(|c| **c != "sweep") {
        let out = run_check(&d, &run, check);
        assert!(out.contains(&format!("watchtower: {check} skipped — world is halted")), "{check}: {out}");
    }
}
