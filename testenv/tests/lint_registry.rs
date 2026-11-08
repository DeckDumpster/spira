//! `testenv lint` needs one setting, the run directory. A key the live config declares and the
//! registry it resolves against predates must not refuse a lint that never reads that key.

use std::fs;
use std::process::Command;

#[test]
fn lint_does_not_refuse_a_key_the_live_config_declares_and_an_older_registry_lacks() {
    let tmp = testkit::TempDir::new("lint-registry");
    let dir = tmp.path().to_path_buf();
    let home = spira_config::process::fixture_registry_home(&dir);
    fs::remove_file(home.join("conf.d/SPIRA_TESTENV_MAX_RUNS"))
        .expect("the release registers the key");
    let run = dir.join("run");
    fs::create_dir_all(run.join("lint-pass")).unwrap();
    let tip = "0123456789abcdef0123456789abcdef01234567";
    fs::write(run.join("lint-pass").join(tip), "none\n").unwrap();
    let toml = spira_config::process::fixture_toml(
        &dir,
        &[
            ("SPIRA_TESTENV_MAX_RUNS", "3"),
            ("SPIRA_RUN", run.to_str().unwrap()),
        ],
    );
    let lint = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_testenv"))
            .args(args)
            .env_clear()
            .env("SPIRA_HOME", &home)
            .env("SPIRA_TOML", &toml)
            .output()
            .expect("testenv starts");
        (
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    };
    let (ok, text) = lint(&["lint", "--verify", tip]);
    assert!(ok, "{text}");
    assert!(text.contains("already passed"), "{text}");
    let (ok, text) = lint(&["shots", "--help"]);
    assert!(
        !ok && text.contains(&format!("registry read: {}", home.join("conf.d").display())),
        "{text}"
    );
}
