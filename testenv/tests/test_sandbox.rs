//! Every test binary runs under cargo's target runner, `tools/test-sandbox/runner`, which
//! `.cargo/config.toml` names: a bubblewrap sandbox in which the caller's installation is out of
//! reach (sp-bpenvh). What a binary can reach inside it is the test-sandbox suite's to prove
//! with a probe binary; these tests read files and spawn at most the runner's own `--print-args`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn workspace() -> PathBuf {
    spira_config::resolve::checkout_root_for_process().unwrap()
}

#[test]
fn cargo_runs_every_test_binary_through_the_sandbox_runner() {
    let ws = workspace();
    let text = fs::read_to_string(ws.join(".cargo/config.toml")).expect("the workspace's .cargo/config.toml");
    let conf: toml::Table = text.parse().expect(".cargo/config.toml parses");
    let target = conf.get("target").and_then(toml::Value::as_table).expect("a [target] table");
    for triple in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let runner = target.get(triple).and_then(|t| t.get("runner")).and_then(toml::Value::as_str);
        assert_eq!(runner, Some("tools/test-sandbox/runner"), "[target.{triple}] names the sandbox runner");
    }
    assert!(target.keys().all(|k| !k.starts_with("cfg(")), "a cfg(...) key loses to a triple key in ~/.cargo/config.toml: {target:?}");
    let mode = fs::metadata(ws.join("tools/test-sandbox/runner")).expect("the runner exists").permissions().mode();
    assert_ne!(mode & 0o111, 0, "the runner is executable");
}

/// The repository's own git directory, which both sandboxes bind back read-only so a test can
/// run git against its checkout: a worktree's `.git` names `<common>/worktrees/<name>`.
fn git_common_dir() -> Option<PathBuf> {
    let dotgit = workspace().join(".git");
    let text = fs::read_to_string(&dotgit).ok()?;
    let gitdir = PathBuf::from(text.strip_prefix("gitdir:")?.trim());
    let gitdir = if gitdir.is_absolute() { gitdir } else { workspace().join(gitdir) };
    Some(match gitdir.parent() {
        Some(p) if p.file_name().is_some_and(|n| n == "worktrees") => p.parent()?.to_path_buf(),
        _ => gitdir,
    })
}

/// Every path under `dir` a test can see, except the way down to each of `keep` and the kept
/// paths themselves.
fn visible_beyond(dir: &Path, keep: &[PathBuf], out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for e in entries {
        let p = e.unwrap().path();
        if keep.contains(&p) {
            continue;
        }
        if keep.iter().any(|k| k.starts_with(&p)) {
            visible_beyond(&p, keep, out);
        } else {
            out.push(p);
        }
    }
}

/// What the runner binds back on purpose: the git common dir (read-only), the workspace and
/// CARGO_TARGET_DIR. When a checkout or its target lives under $HOME/spira (a worktree under
/// run/worktree/, a gate's own target under run/), the way down to it is mount scaffolding,
/// not the installation.
fn bound_back() -> Vec<PathBuf> {
    let canon = |p: PathBuf| fs::canonicalize(&p).unwrap_or(p);
    let mut keep: Vec<PathBuf> = git_common_dir().into_iter().map(canon).collect();
    keep.push(canon(workspace()));
    if let Some(t) = std::env::var_os("CARGO_TARGET_DIR") {
        keep.push(canon(PathBuf::from(t)));
    }
    keep
}

#[test]
fn a_test_finds_no_installation_under_its_home() {
    let home = PathBuf::from(std::env::var_os("HOME").expect("the runner sets HOME"));
    let mut seen = Vec::new();
    visible_beyond(&home.join("spira"), &bound_back(), &mut seen);
    assert!(seen.is_empty(), "under $HOME/spira a test sees {seen:?}; only the repository's git directory, workspace and target are bound in");
    for p in [".config/spira", ".local/bin/work"] {
        assert!(!home.join(p).exists(), "$HOME/{p} is visible inside the sandbox");
    }
}

fn print_args(args: &[&str]) -> Vec<String> {
    let out = std::process::Command::new(workspace().join("tools/test-sandbox/runner"))
        .arg("--print-args")
        .args(args)
        .output()
        .expect("the runner spawns");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().lines().map(String::from).collect()
}

#[test]
fn around_binds_each_target_link_destination_writable_and_keeps_the_environment() {
    let ws = workspace();
    let scratch = testkit::TempDir::new("around-dest");
    let dest = scratch.path().to_path_buf();
    let (target, link) = (ws.join("target"), ws.join("target/around-probe"));
    let made_target = !target.exists();
    fs::create_dir_all(&target).unwrap();
    let _ = fs::remove_file(&link);
    std::os::unix::fs::symlink(&dest, &link).unwrap();
    let args = print_args(&["--around", system_paths::USR_BIN_TRUE, "nextest", "run"]);
    fs::remove_file(&link).unwrap();
    if made_target {
        let _ = fs::remove_dir(&target);
    }
    let want = fs::canonicalize(&dest).unwrap().display().to_string();
    assert!(args.windows(3).any(|w| w[0] == "--bind" && w[1] == want && w[2] == want), "{want} bound writable: {args:?}");
    assert!(!args.iter().any(|a| a == "--clearenv"), "the caller's environment is kept: {args:?}");
    assert!(args.windows(3).any(|w| w[0] == "--setenv" && w[1] == "GIT_CONFIG_SYSTEM"), "{args:?}");
    assert_eq!(&args[args.len() - 4..], ["--", system_paths::USR_BIN_TRUE, "nextest", "run"]);
}

#[test]
fn the_held_decision_comes_before_any_work_a_held_call_does_not_need() {
    let text = fs::read_to_string(workspace().join("tools/test-sandbox/runner")).unwrap();
    let held = text.find("[ -z \"$print\" ] && held").expect("the runner decides held");
    for later in ["readlink -f", "sed -n", "/bwrap"] { // not a path: a substring searched for in the runner script
        let first_use = text.find(later).unwrap();
        assert!(held < first_use, "`{later}` runs at byte {first_use}, before the held decision at {held}");
    }
}

#[test]
fn print_args_for_a_binary_is_what_the_landing_ref_printed() {
    let ws = workspace();
    let old = std::process::Command::new("git").arg("-C").arg(&ws).args(["show", "local/main:tools/test-sandbox/runner"]).output();
    let Some(old) = old.ok().filter(|o| o.status.success()) else { return };
    let copy = ws.join("tools/test-sandbox/runner.landing-ref");
    testkit::write_exe(&copy, &String::from_utf8(old.stdout).unwrap());
    let before = std::process::Command::new(&copy).args(["--print-args", system_paths::USR_BIN_TRUE, "a", "b"]).output();
    fs::remove_file(&copy).unwrap();
    let before = String::from_utf8(before.unwrap().stdout).unwrap();
    let strip = |mut args: Vec<String>| {
        for name in [".cargo/config.toml", ".cargo/config"] {
            if let Some(i) = args.windows(3).position(|w| w[0] == "--ro-bind" && w[1].ends_with(name) && w[1] == w[2]) {
                args.drain(i..i + 3);
            }
        }
        if let Some(i) = args.windows(3).position(|w| w[0] == "--setenv" && w[1] == "RUSTC_WRAPPER" && w[2].is_empty()) {
            args.drain(i..i + 3);
        }
        args
    };
    let now = strip(print_args(&[system_paths::USR_BIN_TRUE, "a", "b"]));
    let before = strip(before.lines().map(String::from).collect());
    assert_eq!(now, before);
}

fn gate_environment_runner(fixture_home: &Path, release: &Path, tail: &str, args: &[&str]) -> std::process::Output {
    let path = spira_config::release_path_from_env_with_tail(Some(release.to_str().unwrap()), tail).unwrap();
    std::process::Command::new(workspace().join("tools/test-sandbox/runner"))
        .args(args)
        .env_clear()
        .env("PATH", path)
        .env("HOME", fixture_home)
        .output()
        .expect("the runner spawns")
}

#[test]
fn around_finds_the_cargo_the_gate_path_names_and_binds_its_toolchain_under_a_hidden_home() {
    let real = PathBuf::from(std::env::var("CARGO").expect("cargo sets CARGO for a test"));
    let tmp = testkit::TempDir::new("around-gate-env");
    let home = fs::canonicalize(tmp.path()).unwrap().join("home");
    let release = tmp.path().join("release");
    fs::create_dir_all(home.join(".cargo/bin")).unwrap();
    fs::create_dir_all(&release).unwrap();
    std::os::unix::fs::symlink(&real, home.join(".cargo/bin/cargo")).unwrap();
    let tail = format!("{}/.cargo/bin", home.display());
    let out = gate_environment_runner(&home, &release, &tail, &["--print-args", "--around", "cargo", "--version"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let args: Vec<String> = String::from_utf8(out.stdout).unwrap().lines().map(String::from).collect();
    let cargo = format!("{}/.cargo/bin/cargo", home.display());
    assert_eq!(&args[args.len() - 3..], ["--", cargo.as_str(), "--version"], "{args:?}");
    let hidden = home.display().to_string();
    let shadow = args.iter().position(|a| a == "--tmpfs").and_then(|_| args.windows(2).position(|w| w[0] == "--tmpfs" && w[1] == hidden)).expect("the home is shadowed");
    let toolchain = fs::canonicalize(&real).unwrap();
    let bound = |dir: &str| args.windows(3).enumerate().any(|(i, w)| i > shadow && w[0] == "--ro-bind" && w[1] == dir && w[2] == dir);
    let bin = format!("{hidden}/.cargo/bin");
    let root = toolchain.parent().unwrap().parent().unwrap().display().to_string();
    assert!(bound(&bin) || bound(&root), "cargo's directory is bound back after the home tmpfs: {args:?}");
    if toolchain.file_name().is_some_and(|n| n == "cargo") {
        assert!(bound(&root), "the toolchain cargo links to is bound back: {args:?}");
    }
}

fn cargo_config_fixture(name: &str) -> (testkit::TempDir, PathBuf) {
    let tmp = testkit::TempDir::new(name);
    let home = fs::canonicalize(tmp.path()).unwrap().join("home");
    fs::create_dir_all(home.join(".cargo/bin")).unwrap();
    for f in ["config.toml", "config", "credentials.toml", "credentials"] {
        fs::write(home.join(".cargo").join(f), "").unwrap();
    }
    (tmp, home)
}

fn assert_config_bound_after_shadow_and_credentials_not(args: &[String], home: &Path) {
    let h = home.display().to_string();
    let shadow = args.windows(2).position(|w| w[0] == "--tmpfs" && w[1] == h).expect("the home is shadowed");
    let config = format!("{h}/.cargo/config.toml");
    let at = args.windows(3).position(|w| w[0] == "--ro-bind" && w[1] == config && w[2] == config).expect("config.toml is bound");
    assert!(at > shadow, "{args:?}");
    assert!(args.iter().all(|a| !a.contains("credentials")), "{args:?}");
    let legacy = format!("{h}/.cargo/config");
    let at = args.windows(3).position(|w| w[0] == "--ro-bind" && w[1] == legacy && w[2] == legacy).expect("the legacy config is bound");
    assert!(at > shadow, "{args:?}");
    assert!(args.windows(3).any(|w| w[0] == "--setenv" && w[1] == "RUSTC_WRAPPER" && w[2].is_empty()), "an empty wrapper overrides the config's: {args:?}");
    let home_cargo = format!("{h}/.cargo");
    assert!(args.windows(2).all(|w| !(w[0].ends_with("bind") && w[1] == home_cargo)), "{args:?}");
}

#[test]
fn around_binds_the_cargo_config_back_read_only_and_never_the_credentials() {
    let (tmp, home) = cargo_config_fixture("around-cargo-config");
    let release = tmp.path().join("release");
    fs::create_dir_all(&release).unwrap();
    std::os::unix::fs::symlink(std::env::var("CARGO").unwrap(), home.join(".cargo/bin/cargo")).unwrap();
    let tail = format!("{}/.cargo/bin", home.display());
    let out = gate_environment_runner(&home, &release, &tail, &["--print-args", "--around", "cargo", "--version"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let args: Vec<String> = String::from_utf8(out.stdout).unwrap().lines().map(String::from).collect();
    assert_config_bound_after_shadow_and_credentials_not(&args, &home);
    assert!(!args.iter().any(|a| a == "--unsetenv"), "{args:?}");
}

#[test]
fn a_binary_run_binds_the_declared_cargo_homes_config_back_read_only_and_never_the_credentials() {
    let (_tmp, home) = cargo_config_fixture("binary-cargo-config");
    let out = std::process::Command::new(workspace().join("tools/test-sandbox/runner"))
        .args(["--print-args", system_paths::USR_BIN_TRUE])
        .env_clear()
        .env("PATH", format!("{}:{}", system_paths::USR_BIN, system_paths::BIN))
        .env("HOME", &home)
        .env("CARGO_HOME", home.join(".cargo"))
        .output()
        .expect("the runner spawns");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let args: Vec<String> = String::from_utf8(out.stdout).unwrap().lines().map(String::from).collect();
    assert_config_bound_after_shadow_and_credentials_not(&args, &home);
}

#[test]
fn around_without_the_command_on_path_is_no_verdict_and_names_the_path() {
    let tmp = testkit::TempDir::new("around-no-cargo");
    let home = fs::canonicalize(tmp.path()).unwrap().join("home");
    let release = tmp.path().join("release");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&release).unwrap();
    let out = gate_environment_runner(&home, &release, "", &["--around", "cargo", "--version"]);
    assert_eq!(out.status.code(), Some(75), "{}", String::from_utf8_lossy(&out.stderr));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("cargo is not on PATH") && err.contains(&release.display().to_string()), "{err}");
}

#[test]
fn a_held_run_keeps_the_identity_an_enclosing_sandbox_handed_in_when_run_is_unwritable() {
    let tmp = testkit::TempDir::new("held-identity");
    let base = fs::canonicalize(tmp.path()).unwrap();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    let identity = base.join("handed.gitconfig");
    fs::write(&identity, "[user]\n\tname = handed\n\temail = handed@localhost\n").unwrap();
    let mut cmd = std::process::Command::new("bwrap");
    cmd.args(["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--tmpfs"])
        .arg(&home)
        .args(["--tmpfs", "/run", "--remount-ro", "/run", "--unshare-net"])
        .arg(workspace().join("tools/test-sandbox/runner"))
        .arg("/usr/bin/printenv")
        .arg("GIT_CONFIG_SYSTEM")
        .env("HOME", &home)
        .env("GIT_CONFIG_SYSTEM", &identity);
    let out = cmd.output().expect("bwrap spawns");
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("bwrap:") {
        eprintln!("skipped: no nested sandbox to hold the runner in: {stderr}");
        return;
    }
    assert!(out.status.success(), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), identity.display().to_string());
}
