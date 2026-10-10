//! `classify` on a box configured only by the one config document: no flag beyond the repository, and no
//! legacy spira.conf consulted.

use std::process::Command;

fn classify(dir: &std::path::Path, extra: &[&str]) -> (i32, String, String) {
    let bd = dir.join("bd");
    testkit::write_exe(&bd, "#!/bin/sh\necho '[]'\n");
    let run = dir.join("run");
    std::fs::create_dir_all(&run).unwrap();
    let toml = spira_config::process::fixture_toml(
        dir,
        &[
            ("SPIRA_RUN", run.to_str().unwrap()),
            ("SPIRA_BD", bd.to_str().unwrap()),
            ("SPIRA_DB", dir.to_str().unwrap()),
            ("SPIRA_QUEUE_DIR", run.join("queue").to_str().unwrap()),
            ("SPIRA_LC_PASSWORD_FILE", ""),
        ],
    );
    let o = Command::new(env!("CARGO_BIN_EXE_spira-lc"))
        .arg("classify")
        .args(extra)
        .env("SPIRA_LC_PASSWORD", "")
        .env("SPIRA_HOME", concat!(env!("CARGO_MANIFEST_DIR"), "/../spira"))
        .env("SPIRA_TOML", &toml)
        .output()
        .unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}

#[test]
fn classify_dry_run_needs_only_the_repository_on_a_toml_only_box() {
    let t = testkit::TempDir::new("spira-lc-classify-toml");
    let d = t.path();
    // A legacy config that would answer differently if it were read.
    std::fs::write(d.join("spira.conf"), "SPIRA_HOME_REPO=legacy\n").unwrap();

    let (code, out, err) = classify(d, &["--dry-run", "--repo", "spira"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains(&format!("\"source\": \"{}\"", spira_config::FILE_NAME)) && out.contains("\"beads\": 0"), "{out}");

    let (code, out, _) = classify(d, &["--dry-run", "--repo", "legacy"]);
    assert_eq!(code, 2);
    assert!(out.contains(&format!("legacy: not present in {}", spira_config::FILE_NAME)), "{out}");
}
