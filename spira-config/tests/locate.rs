//! `spira-config locate`, and the fail-closed no-file-argument path of `get`/`validate`/
//! `export --sh` (sp-hconl). Every case runs the real binary with a fully cleared
//! environment (never the ambient one this test process inherits), so a candidate that
//! happens to exist on the machine running the suite can't make a case pass for the wrong
//! reason, and a real operator `~/.config/spira/spira.toml` on this box never leaks in.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn scratch_dir(tag: &str) -> testkit::TempDir {
    testkit::TempDir::new(&format!("spira-config-test-locate-{tag}"))
}

fn write_toml(dir: &Path, rel: &str) -> std::path::PathBuf {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, "[spira]\noperator = \"fixture\"\n").unwrap();
    p
}

fn write_legacy_conf(dir: &Path, rel: &str) -> std::path::PathBuf {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, "SPIRA_HOME=/x\n").unwrap();
    p
}

/// Runs the real binary in a fully cleared environment plus exactly the vars given.
fn run(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_spira-config"));
    cmd.env_clear();
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.args(args);
    cmd.output().expect("spira-config runs")
}

fn locate(env: &[(&str, &str)]) -> Output {
    run(&["locate"], env)
}

fn stdout_trimmed(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
fn stderr_trimmed(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).trim().to_string()
}

fn etc_spira_real() -> bool {
    Path::new("/etc/spira/spira.toml").is_file() || Path::new("/etc/spira/spira.conf").is_file()
}

#[test]
fn prints_the_path_and_exits_0_when_found() {
    let dir = scratch_dir("found");
    let xdg_toml = write_toml(&dir, "xdg/spira/spira.toml");
    let out = locate(&[
        ("HOME", dir.join("home-empty").to_str().unwrap()),
        ("XDG_CONFIG_HOME", dir.join("xdg").to_str().unwrap()),
    ]);
    assert!(out.status.success());
    assert_eq!(stdout_trimmed(&out), xdg_toml.to_str().unwrap());
}

#[test]
fn exits_1_naming_tried_paths_when_nothing_is_found() {
    if etc_spira_real() {
        eprintln!("skipping: this machine has a real /etc/spira config");
        return;
    }
    let dir = scratch_dir("notfound");
    let home = dir.join("home-empty");
    fs::create_dir_all(&home).unwrap();
    let out = locate(&[("HOME", home.to_str().unwrap())]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout_trimmed(&out), "");
    let err = stderr_trimmed(&out);
    assert!(err.contains("no spira.toml found"), "{err}");
    assert!(err.contains(".config/spira/spira.toml"), "{err}");
    assert!(err.contains("/etc/spira/spira.toml"), "{err}");
}

#[test]
fn exits_2_naming_the_legacy_conf_when_only_it_exists() {
    let dir = scratch_dir("legacy");
    let home = dir.join("home");
    let conf = write_legacy_conf(&home, ".config/spira/spira.conf");
    let out = locate(&[("HOME", home.to_str().unwrap())]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(stdout_trimmed(&out), "");
    let err = stderr_trimmed(&out);
    assert!(err.contains(conf.to_str().unwrap()), "{err}");
    assert!(err.contains("spira-config convert"), "{err}");
}

#[test]
fn pinned_spira_toml_wins_over_a_real_xdg_file() {
    let dir = scratch_dir("pin-priority");
    let explicit = write_toml(&dir, "explicit/spira.toml");
    let xdg_toml = write_toml(&dir, "xdg/spira/spira.toml");
    let out = locate(&[
        ("SPIRA_TOML", explicit.to_str().unwrap()),
        ("XDG_CONFIG_HOME", dir.join("xdg").to_str().unwrap()),
    ]);
    assert!(out.status.success());
    assert_eq!(stdout_trimmed(&out), explicit.to_str().unwrap());
    // positive control that the XDG tier the pin pre-empted really is reachable on its own:
    let out2 = locate(&[("XDG_CONFIG_HOME", dir.join("xdg").to_str().unwrap())]);
    assert_eq!(stdout_trimmed(&out2), xdg_toml.to_str().unwrap());
}

#[test]
fn pinned_spira_toml_missing_does_not_fall_through_to_xdg() {
    let dir = scratch_dir("pin-miss");
    write_toml(&dir, "xdg/spira/spira.toml");
    let out = locate(&[
        ("SPIRA_TOML", dir.join("nonexistent.toml").to_str().unwrap()),
        ("XDG_CONFIG_HOME", dir.join("xdg").to_str().unwrap()),
    ]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr_trimmed(&out);
    assert!(err.contains("nonexistent.toml"), "{err}");
    // It must name ONLY the pinned path, never the XDG file it never consulted:
    assert!(!err.contains("xdg/spira/spira.toml"), "{err}");
}

#[test]
fn spira_repo_is_never_a_candidate() {
    // THE REGRESSION sp-hconl FIXES, exercised through the CLI: a spira.toml sitting beside
    // $SPIRA_REPO must not be found, matching conf.sh's current (post sp-9hwim) search.
    if etc_spira_real() {
        eprintln!("skipping: this machine has a real /etc/spira config");
        return;
    }
    let dir = scratch_dir("repo-ignored");
    let repo = dir.join("repo");
    write_toml(&repo, "spira.toml");
    let home = dir.join("home-empty");
    fs::create_dir_all(&home).unwrap();
    let out = locate(&[("SPIRA_REPO", repo.to_str().unwrap()), ("HOME", home.to_str().unwrap())]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout_trimmed(&out), "");
}

// Proves the no-file-argument path of get/validate/export is wired through the SAME search,
// not just exposed standalone as its own subcommand.
#[test]
fn get_with_no_file_arg_finds_it_via_the_xdg_tier() {
    let dir = scratch_dir("get-wiring");
    write_toml(&dir, "xdg/spira/spira.toml");
    let out = run(
        &["get", "spira.operator"],
        &[
            ("HOME", dir.join("home-empty").to_str().unwrap()),
            ("XDG_CONFIG_HOME", dir.join("xdg").to_str().unwrap()),
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr_trimmed(&out));
    assert_eq!(stdout_trimmed(&out), "fixture");
}

// The fail-closed fix itself: before this bead, `get`/`validate`/`export --sh` with no file
// argument and no resolvable config either read whatever `./spira.toml` happened to be in the
// CURRENT DIRECTORY (never checked against the real search tiers) or blocked forever on
// stdin. Now it refuses by name instead of guessing either way.
#[test]
fn get_with_no_file_arg_and_nothing_resolvable_refuses_instead_of_reading_cwd_or_blocking() {
    if etc_spira_real() {
        eprintln!("skipping: this machine has a real /etc/spira config");
        return;
    }
    let dir = scratch_dir("get-refuse");
    let home = dir.join("home-empty");
    fs::create_dir_all(&home).unwrap();
    // A decoy spira.toml in a DIFFERENT directory from the one the process runs in: if the
    // old cwd-guessing behaviour regressed back in, this would need to be where the process
    // actually runs to matter, so this alone doesn't prove anything — the regression this
    // guards is cwd-vs-tiers, not "exists somewhere".
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_spira-config"));
    cmd.env_clear();
    cmd.env("HOME", &home);
    cmd.current_dir(&dir);
    cmd.args(["get", "spira.operator"]);
    // Plant the decoy AFTER setting current_dir's target, directly in the cwd the child runs
    // in — proving a real ./spira.toml in the cwd is no longer read as a fallback.
    write_toml(&dir, "spira.toml");
    let out = cmd.output().expect("spira-config runs");
    assert!(!out.status.success());
    assert_eq!(stdout_trimmed(&out), "");
    let err = stderr_trimmed(&out);
    assert!(err.contains("no spira.toml found") || err.contains("tried:"), "{err}");
}
