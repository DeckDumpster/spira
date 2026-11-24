//! `spira/uninstall.sh` is the subject: its stray sweep runs under `bash` against a throwaway
//! unit directory and a stub `systemctl`. The install-to-uninstall round trip on real systemd
//! stays acceptance's (`release/src/acceptance/phases.rs`).

#[macro_use]
extern crate spira_test_id;
use std::process::Command;

// covers: UC-instance-lifecycle-40
#[test_id(th7p4ec)]
#[test]
fn the_stray_sweep_reports_a_planted_unit_it_does_not_own_and_leaves_it_in_place() {
    let dir = testkit::TempDir::new("uninstall-stray");
    let d = dir.path();
    let spira = testkit::manifest_dir().parent().unwrap().join("spira");
    let release = d.join("release");
    std::fs::create_dir_all(release.join("bin")).unwrap();
    std::os::unix::fs::symlink(testkit::bin_exe("spira-config"), release.join("bin/spira-config")).unwrap();
    let units = d.join(".config/systemd/user");
    std::fs::create_dir_all(&units).unwrap();
    std::fs::write(units.join("spira-legacy-stray.service"), "[Unit]\nDescription=stray\n").unwrap();
    std::fs::write(units.join("dolt-beads.service"), "[Unit]\nDescription=owned\n").unwrap();
    testkit::write_exe(d.join("systemctl"), "#!/bin/bash\nexit 0\n");
    testkit::write_exe(d.join("loginctl"), "#!/bin/bash\nexit 0\n");
    let toml = spira_config::process::fixture_toml(d, &[]);
    let home = spira_config::resolve::locate_home_for_process().unwrap();
    let o = Command::new("bash")
        .env_clear()
        .arg(spira.join("uninstall.sh"))
        .arg("--yes")
        .env("PATH", format!("{}:{}:/usr/bin:/bin", release.join("bin").display(), spira.display()))
        .env("HOME", d)
        .env("SPIRA_RELEASE", &release)
        .env("SPIRA_HOME", home)
        .env("SPIRA_TOML", toml)
        .env("SPIRA_INSTANCE", "t1")
        .env("SPIRA_SYSTEMCTL", d.join("systemctl"))
        .env("SPIRA_LOGINCTL", d.join("loginctl"))
        .output()
        .unwrap();
    let out = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert_eq!(o.status.code(), Some(0), "{out}");
    assert!(out.contains("STRAY"), "{out}");
    assert!(out.contains("spira-legacy-stray.service"), "{out}");
    assert!(!out.contains("STRAY  dolt-beads.service"), "an owned unit is not a stray: {out}");
    assert!(units.join("spira-legacy-stray.service").exists(), "a stray is reported, never deleted");
}
