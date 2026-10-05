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

/// THE ONE SOURCE: `$SPIRA_TOML` is the file. Found → printed, exit 0.
#[test]
fn prints_the_pinned_path_and_exits_0() {
    let dir = scratch_dir("found");
    let p = write_toml(&dir, "anywhere/spira.toml");
    let out = locate(&[("SPIRA_TOML", p.to_str().unwrap())]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_trimmed(&out));
    assert_eq!(stdout_trimmed(&out), p.display().to_string());
}

/// Unset → exit 1, naming SPIRA_TOML — even with a spira.toml (and a legacy spira.conf) in
/// every place the old search looked: XDG, HOME/.config.
#[test]
fn unset_refuses_and_nothing_is_discovered() {
    let dir = scratch_dir("unset");
    write_toml(&dir, "xdg/spira/spira.toml");
    write_toml(&dir, "home/.config/spira/spira.toml");
    let conf = write_legacy_conf(&dir, "xdg/spira/spira.conf");
    let out = locate(&[
        ("HOME", dir.join("home").to_str().unwrap()),
        ("XDG_CONFIG_HOME", dir.join("xdg").to_str().unwrap()),
        ("SPIRA_CONF", conf.to_str().unwrap()),
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout_trimmed(&out).is_empty(), "nothing may be printed: {}", stdout_trimmed(&out));
    assert!(stderr_trimmed(&out).contains("SPIRA_TOML is not set"), "{}", stderr_trimmed(&out));
}

/// A pin to a missing file → exit 1, naming that path.
#[test]
fn a_pin_to_a_missing_file_refuses_naming_it() {
    let dir = scratch_dir("missing");
    let p = dir.join("nope.toml");
    let out = locate(&[("SPIRA_TOML", p.to_str().unwrap())]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr_trimmed(&out).contains(&p.display().to_string()), "{}", stderr_trimmed(&out));
}

/// `get` with no file argument reads the one file, and refuses without it rather than
/// reading the working directory or blocking on stdin.
#[test]
fn get_with_no_file_arg_reads_spira_toml_or_refuses() {
    let dir = scratch_dir("get");
    let p = write_toml(&dir, "s/spira.toml");
    let out = run(&["get", "spira.operator"], &[("SPIRA_TOML", p.to_str().unwrap())]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_trimmed(&out));
    assert_eq!(stdout_trimmed(&out), "fixture");
    let out = run(&["get", "spira.operator"], &[]);
    assert_ne!(out.status.code(), Some(0));
    assert!(stdout_trimmed(&out).is_empty());
}
