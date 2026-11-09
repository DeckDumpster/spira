//! The heimdall lane, through the real binary: a pass whose lane is stopped on every registered
//! repo says so and stops before it reads or writes any state.

use std::process::Command;

fn pass(d: &testkit::TempDir, run: &std::path::Path, args: &[&str]) -> (i32, String) {
    let release = d.path().join("release");
    std::fs::create_dir_all(release.join("bin")).unwrap();
    testkit::write_exe(release.join("bin/spira-lc"), "#!/bin/sh\nexit 1\n");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_heimdall"));
    cmd.args(args).env_clear().env("PATH", std::env::var("PATH").unwrap_or_default());
    for (k, v) in spira_config::process::fixture_world_env(d.path(), run, &["spira", "other"]) {
        if let Some(v) = v {
            cmd.env(k, v);
        }
    }
    let out = cmd.env("SPIRA_RELEASE", &release).output().expect("heimdall runs");
    (out.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

fn stop(run: &std::path::Path, repo: &str) {
    spira_config::scoped_stop::pause_repo(run, repo, &["heimdall".to_string()], "cutover-mode", "cutover", "world stop", 1).unwrap();
}

#[test]
fn a_pass_with_the_heimdall_lane_stopped_on_every_repo_skips_and_writes_no_state() {
    let d = testkit::TempDir::new("heimdall-lane");
    let run = d.path().join("run");
    std::fs::create_dir_all(&run).unwrap();
    stop(&run, "spira");
    stop(&run, "other");
    let (code, out) = pass(&d, &run, &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("heimdall: skipped — the heimdall lane of spira is stopped on purpose by cutover-mode") && out.contains("world stop"), "{out}");
    assert!(!run.join("heimdall.state").exists() && !run.join("heimdall.status").exists(), "a stopped pass wrote state");
}

#[test]
fn a_pass_goes_on_while_one_repo_still_takes_the_heimdall_lane() {
    let d = testkit::TempDir::new("heimdall-lane-partial");
    let run = d.path().join("run");
    std::fs::create_dir_all(&run).unwrap();
    stop(&run, "spira");
    let (_, out) = pass(&d, &run, &[]);
    assert!(!out.contains("skipped — the heimdall lane"), "{out}");
    assert!(run.join("heimdall.state").exists() || run.join("heimdall.status").exists(), "the pass never got past its lane check: {out}");
}

#[test]
fn a_report_reads_through_a_stopped_lane() {
    let d = testkit::TempDir::new("heimdall-lane-report");
    let run = d.path().join("run");
    std::fs::create_dir_all(&run).unwrap();
    stop(&run, "spira");
    stop(&run, "other");
    let (_, out) = pass(&d, &run, &["--report"]);
    assert!(!out.contains("skipped — the heimdall lane"), "{out}");
    assert!(out.contains("heimdall"), "a report said nothing: {out:?}");
}
