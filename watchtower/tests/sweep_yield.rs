//! The sweep's yield columns come from the records and the meter under its run directory,
//! through the real binary.

#[macro_use]
extern crate spira_test_id;
use std::process::Command;

// covers: UC-gate-verdict-24
#[test_id(txundu5)]
#[test]
fn the_shown_sweep_counts_the_reds_recorded_under_its_run_directory() {
    let d = testkit::TempDir::new("watchtower-yield");
    let run = d.path().join("run");
    std::fs::create_dir_all(run.join("gate-yield")).unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    for (name, verdict) in [("a", "DEFECT"), ("b", "GATE_FAULT"), ("c", "GATE_FAULT")] {
        std::fs::write(run.join("gate-yield").join(name), format!("at={now}\nsuite=test-box.sh\nverdict={verdict}\n")).unwrap();
    }
    let stamp = strand::timefmt::utc_stamp(now as i64 - 60);
    std::fs::write(run.join("gate.log"), format!("{stamp} repo spira/sp-a waited=0s ran=73s rc=0 pass\n")).unwrap();

    let mut cmd = Command::new(testkit::bin_exe("watchtower"));
    cmd.arg("--show").env_clear().env("PATH", std::env::var("PATH").unwrap_or_default());
    let release = spira_config::process::fixture_release(d.path(), true);
    std::fs::create_dir(release.join("bin")).unwrap();
    for absent in ["resolve", "spira-lc"] {
        testkit::write_exe(release.join("bin").join(absent), "#!/bin/sh\nexit 1\n");
    }
    cmd.env("SPIRA_RELEASE", release);
    for (k, v) in spira_config::process::fixture_world_env(d.path(), &run, &["spira"]) {
        if let Some(v) = v {
            cmd.env(k, v);
        }
    }
    let out = cmd.output().expect("watchtower runs");
    let shown = String::from_utf8_lossy(&out.stdout).into_owned();
    let line = |start: &str| shown.lines().find(|l| l.trim_start().starts_with(start)).unwrap_or_else(|| panic!("no {start:?} line in:\n{shown}\n{}", String::from_utf8_lossy(&out.stderr))).to_string();
    assert!(line("gate reds, last 1d").ends_with(" 3"), "{shown}");
    assert!(shown.contains("worst: test-box.sh x2"), "{shown}");
    assert!(shown.contains("73s"), "the meter's cost reaches the sweep:\n{shown}");
}
