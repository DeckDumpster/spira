//! testenv-cap against real processes: a stand-in for a testenv run is killed past
//! cap + grace and logged once; one under the cap is left alone.

use std::path::Path;
use std::process::{Child, Command, Stdio};

fn config(dir: &Path, suite: &str, gate: &str) -> std::path::PathBuf {
    spira_config::process::fixture_toml(
        dir,
        &[
            ("SPIRA_TESTENV_SUITE_CAP", suite),
            ("SPIRA_TESTENV_GATE_CAP", gate),
            ("SPIRA_CONCIERGE_INBOX", &dir.join("inbox").display().to_string()),
        ],
    )
}

/// A process whose cmdline is `testenv -c "sleep 300" <args>`: a shell under another name.
fn stand_in(dir: &Path, args: &[&str]) -> Child {
    let exe = dir.join("testenv");
    if !exe.exists() {
        let sleep = String::from_utf8(Command::new("sh").args(["-c", "command -v sh"]).output().unwrap().stdout).unwrap();
        std::fs::copy(sleep.trim(), &exe).unwrap();
    }
    Command::new(&exe).args(["-c", "sleep 300"]).args(args).stdout(Stdio::null()).spawn().unwrap()
}

fn scan(dir: &Path, cfg: &Path, grace: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_testenv-cap"))
        .arg("scan")
        .args(["--grace", grace, "--exe"])
        .arg(dir.join("testenv"))
        .env_clear()
        .env("SPIRA_TOML", cfg)
        .env("SPIRA_HOME", dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_run_past_cap_plus_grace_is_killed_and_reported_once_and_one_under_it_is_not() {
    let t = testkit::TempDir::new("cap-scan");
    let mut run = stand_in(t.path(), &[]);
    let mut gate = stand_in(t.path(), &["--deadline", "300"]);
    std::thread::sleep(std::time::Duration::from_millis(2200));

    let under = scan(t.path(), &config(t.path(), "900", "300"), "0");
    assert_eq!(under, "", "nothing is killed under the cap");
    assert!(run.try_wait().unwrap().is_none() && gate.try_wait().unwrap().is_none());

    let within_grace = scan(t.path(), &config(t.path(), "1", "1"), "60");
    assert_eq!(within_grace, "", "nothing is killed inside the grace");
    assert!(run.try_wait().unwrap().is_none());

    let over = scan(t.path(), &config(t.path(), "1", "0"), "0");
    assert_eq!(over.lines().count(), 1, "only the suite run is over: {over}");
    assert!(over.contains("tier=suite") && over.contains(&format!("pid {}", run.id())), "{over}");
    assert!(run.wait().is_ok_and(|s| !s.success()), "the over-cap run was killed");
    assert!(gate.try_wait().unwrap().is_none(), "the gate run (cap 0 + settle) is still inside its cap");

    let inbox = std::fs::read_to_string(t.path().join("inbox")).unwrap();
    assert_eq!(inbox.lines().count(), 1, "one inbox line: {inbox}");
    assert!(inbox.contains("testenv over-cap killed") && inbox.contains("tier=suite") && inbox.contains("wall="));

    assert_eq!(scan(t.path(), &config(t.path(), "1", "0"), "0"), "", "reported once");
    gate.kill().ok();
}
