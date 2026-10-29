//! `spira-submission` refuses to start without its config key registry, naming where it looked, and
//! answers `check-config` against a release that has one.

use std::path::Path;
use std::process::{Command, Output};

fn run(args: &[&str], release: &Path, toml: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_spira-submission"))
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("SPIRA_RELEASE", release)
        .env("SPIRA_TOML", toml)
        .output()
        .expect("the binary runs")
}

#[test]
fn a_release_without_its_registry_is_refused_by_path_at_check_config_and_at_start() {
    let dir = testkit::TempDir::new("spira-submission-check-config");
    let toml = spira_config::process::fixture_toml(&dir, &[]);
    let release = spira_config::process::fixture_release(&dir, false);
    let want = release.join("spira/conf.d").display().to_string();
    for args in [&["check-config"][..], &["serve"][..]] {
        let out = run(args, &release, &toml);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{args:?} started without a registry: {stderr}");
        assert!(stderr.contains(&want), "{args:?} did not name {want}: {stderr}");
    }

    std::fs::remove_dir_all(dir.join("release")).unwrap();
    let release = spira_config::process::fixture_release(&dir, true);
    let out = run(&["check-config"], &release, &toml);
    assert!(out.status.success(), "positive control: {}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("ok "), "{}", String::from_utf8_lossy(&out.stdout));
}
