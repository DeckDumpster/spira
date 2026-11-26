//! A run enters the test sandbox once, through `tools/test-sandbox/runner --around`, and every test
//! binary inside it is launched directly: the cargo config a test sees names no target runner. What
//! a test can reach inside is the test-sandbox suite's to prove with a probe binary; these tests
//! read files and spawn at most the runner itself.

#[macro_use]
extern crate spira_test_id;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn workspace() -> PathBuf {
    spira_config::resolve::checkout_root_for_process().unwrap()
}

/// The workspace's `.cargo/config.toml` as committed: a test sees the sandbox's runner-less
/// overlay at that path, so the file itself is read from the tree's own commit.
fn committed_cargo_config() -> toml::Table {
    let out = std::process::Command::new("git").arg("-C").arg(workspace()).args(["show", "HEAD:.cargo/config.toml"]).output().expect("git spawns");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().parse().expect(".cargo/config.toml parses")
}

#[test_id(tx9xe9v)]
#[test]
fn cargo_names_the_sandbox_runner_outside_the_sandbox_and_the_overlay_is_that_config_without_it() {
    let ws = workspace();
    let overlay: toml::Table = fs::read_to_string(ws.join("tools/test-sandbox/cargo-config.toml")).unwrap().parse().expect("cargo-config.toml parses");
    let mode = fs::metadata(ws.join("tools/test-sandbox/runner")).expect("the runner exists").permissions().mode();
    assert_ne!(mode & 0o111, 0, "the runner is executable");
    let conf = committed_cargo_config();
    let target = conf.get("target").and_then(toml::Value::as_table).expect("a [target] table");
    for triple in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let runner = target.get(triple).and_then(|t| t.get("runner")).and_then(toml::Value::as_str);
        assert_eq!(runner, Some("tools/test-sandbox/runner"), "[target.{triple}] names the sandbox runner");
    }
    assert!(target.keys().all(|k| !k.starts_with("cfg(")), "a cfg(...) key loses to a triple key in ~/.cargo/config.toml: {target:?}");
    let mut without = conf.clone();
    without.remove("target");
    assert_eq!(overlay, without, "cargo-config.toml is .cargo/config.toml minus its target tables");
}

#[test_id(tw5vhoi)]
#[test]
fn a_test_reads_no_target_runner_in_the_cargo_config_of_its_own_run() {
    let text = fs::read_to_string(workspace().join(".cargo/config.toml")).expect("the workspace's .cargo/config.toml");
    let conf: toml::Table = text.parse().expect(".cargo/config.toml parses");
    let runners: Vec<_> = conf.get("target").and_then(toml::Value::as_table).into_iter().flatten().filter_map(|(k, v)| v.get("runner").map(|r| format!("{k}: {r}"))).collect();
    assert!(runners.is_empty(), "cargo in the sandbox would launch each test through {runners:?}");
}

#[test_id(tbf8ca5)]
#[test]
fn the_runner_called_as_a_target_runner_refuses_and_names_around() {
    let out = std::process::Command::new(workspace().join("tools/test-sandbox/runner")).arg(system_paths::USR_BIN_TRUE).output().expect("the runner spawns");
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--around"), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test_id(ttrajft)]
#[test]
fn a_cargo_run_artifact_beside_fingerprint_runs_as_it_is_while_a_harness_in_deps_is_refused() {
    let tmp = testkit::TempDir::new("cargo-run-artifact");
    let profile = tmp.path().join("debug");
    fs::create_dir_all(profile.join(".fingerprint")).unwrap();
    fs::create_dir_all(profile.join("deps")).unwrap();
    let run = |bin: PathBuf| {
        testkit::write_exe(&bin, "#!/bin/sh\necho \"ran $1\"\n");
        std::process::Command::new(workspace().join("tools/test-sandbox/runner")).arg(&bin).arg("a").output().expect("the runner spawns")
    };
    let artifact = run(profile.join("tool"));
    assert_eq!((artifact.status.code(), &artifact.stdout[..]), (Some(0), &b"ran a\n"[..]), "{}", String::from_utf8_lossy(&artifact.stderr));
    let harness = run(profile.join("deps/tool-0123"));
    assert_eq!((harness.status.code(), &harness.stdout[..]), (Some(2), &b""[..]));
}

#[test_id(trhutlx)]
#[test]
fn around_applies_the_environment_allow_list_once_keeping_rust_build_flags() {
    let printed = std::process::Command::new(workspace().join("tools/test-sandbox/runner"))
        .args(["--print-args", "--around", system_paths::USR_BIN_ENV])
        .env("SPIRA_RELEASE", "x")
        .env("RUSTFLAGS", "y")
        .env("CARGO_REGISTRY_TOKEN", "secret")
        .env("CARGO_REGISTRIES_X_TOKEN", "secret")
        .output()
        .expect("the runner spawns");
    let printed = String::from_utf8_lossy(&printed.stdout);
    assert!(printed.lines().any(|l| l == "RUSTFLAGS") && printed.lines().any(|l| l == "y"), "{printed}");
    assert!(!printed.contains("SPIRA_RELEASE") && !printed.contains("secret"), "{printed}");
    let out = std::process::Command::new(workspace().join("tools/test-sandbox/runner"))
        .args(["--around", system_paths::USR_BIN_ENV])
        .env("SPIRA_RELEASE", "x")
        .env("RUSTFLAGS", "y")
        .output()
        .expect("the runner spawns");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let env = String::from_utf8_lossy(&out.stdout);
    assert!(env.lines().any(|l| l == "RUSTFLAGS=y"), "{env}");
    assert!(!env.contains("SPIRA_RELEASE"), "{env}");
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

#[test_id(tev042r)]
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

#[test_id(tdbmfid)]
#[test]
fn around_binds_each_target_link_destination_writable_and_the_runner_less_cargo_config_over_the_workspaces() {
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
    assert!(args.iter().any(|a| a == "--clearenv"), "the environment is cleared to the allow-list: {args:?}");
    let overlay = ws.join("tools/test-sandbox/cargo-config.toml").display().to_string();
    let config = ws.join(".cargo/config.toml").display().to_string();
    assert!(args.windows(3).any(|w| w[0] == "--ro-bind" && w[1] == overlay && w[2] == config), "the runner-less config is bound over the workspace's: {args:?}");
    assert!(args.windows(3).any(|w| w[0] == "--setenv" && w[1] == "GIT_CONFIG_SYSTEM"), "{args:?}");
    assert_eq!(&args[args.len() - 4..], ["--", system_paths::USR_BIN_TRUE, "nextest", "run"]);
}

#[test_id(tdlszcq)]
#[test]
fn the_held_decision_comes_before_any_work_a_held_call_does_not_need() {
    let text = fs::read_to_string(workspace().join("tools/test-sandbox/runner")).unwrap();
    let held = text.find("[ -z \"$print\" ] && held").expect("the runner decides held");
    for later in ["readlink -f", "gitdir:", "/bwrap"] { // not a path: a substring searched for in the runner script
        let first_use = text.find(later).unwrap();
        assert!(held < first_use, "`{later}` runs at byte {first_use}, before the held decision at {held}");
    }
}

#[test_id(t4iqnrz)]
#[test]
fn print_args_for_a_command_is_what_the_landing_ref_printed_but_for_the_environment_and_the_config_overlay() {
    let ws = workspace();
    let old = std::process::Command::new("git").arg("-C").arg(&ws).args(["show", "local/main:tools/test-sandbox/runner"]).output();
    let Some(old) = old.ok().filter(|o| o.status.success()) else { return };
    let copy = ws.join("tools/test-sandbox/runner.landing-ref");
    testkit::write_exe(&copy, &String::from_utf8(old.stdout).unwrap());
    let before = std::process::Command::new(&copy).args(["--print-args", "--around", system_paths::USR_BIN_TRUE, "a", "b"]).output();
    fs::remove_file(&copy).unwrap();
    let before = String::from_utf8(before.unwrap().stdout).unwrap();
    let landing_ref_kept_the_environment = !before.lines().any(|l| l == "--clearenv");
    let strip = |mut args: Vec<String>| {
        for name in [".cargo/config.toml", ".cargo/config"] {
            if let Some(i) = args.windows(3).position(|w| w[0] == "--ro-bind" && w[1].ends_with(name) && w[1] == w[2]) {
                args.drain(i..i + 3);
            }
        }
        if let Some(i) = args.windows(3).position(|w| w[0] == "--ro-bind" && w[2].ends_with(".cargo/config.toml") && w[1].ends_with("test-sandbox/cargo-config.toml")) {
            args.drain(i..i + 3);
        }
        while let Some(i) = args.windows(3).position(|w| w[0] == "--bind" && w[1].contains("/around-dest")) {
            args.drain(i..i + 3);
        }
        if landing_ref_kept_the_environment {
            while let Some(i) = args.iter().position(|a| a == "--setenv") {
                args.drain(i..i + 3);
            }
            args.retain(|a| a != "--clearenv");
        }
        // The carried-over broker directory is absent from a landing ref's runner that predates it.
        if let Some(i) = args.windows(3).position(|w| w[0] == "--setenv" && w[1] == "PATH") {
            let kept: Vec<&str> = args[i + 2].split(':').filter(|d| !(d.starts_with("/nix/store/") && d.contains("-mosquitto-"))).collect();
            args[i + 2] = kept.join(":");
        }
        args
    };
    let now = strip(print_args(&["--around", system_paths::USR_BIN_TRUE, "a", "b"]));
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

const AROUND_CMD: &str = "spira-sandbox-fixture-cmd";

fn fixture_command(home: &Path) {
    let stub = home.join("toolchain/bin").join(AROUND_CMD);
    fs::create_dir_all(stub.parent().unwrap()).unwrap();
    fs::create_dir_all(home.join(".cargo/bin")).unwrap();
    testkit::write_exe(&stub, "#!/bin/sh\nexit 0\n");
    std::os::unix::fs::symlink(&stub, home.join(".cargo/bin").join(AROUND_CMD)).unwrap();
}

#[test_id(tg6blr3)]
#[test]
fn around_finds_the_command_the_gate_path_names_and_binds_its_toolchain_under_a_hidden_home() {
    let tmp = testkit::TempDir::new("around-gate-env");
    let home = fs::canonicalize(tmp.path()).unwrap().join("home");
    let release = tmp.path().join("release");
    fs::create_dir_all(&release).unwrap();
    fixture_command(&home);
    let tail = format!("{}/.cargo/bin", home.display());
    let out = gate_environment_runner(&home, &release, &tail, &["--print-args", "--around", AROUND_CMD, "--version"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let args: Vec<String> = String::from_utf8(out.stdout).unwrap().lines().map(String::from).collect();
    let cmd = format!("{}/.cargo/bin/{AROUND_CMD}", home.display());
    assert_eq!(&args[args.len() - 3..], ["--", cmd.as_str(), "--version"], "{args:?}");
    let hidden = home.display().to_string();
    let shadow = args.windows(2).position(|w| w[0] == "--tmpfs" && w[1] == hidden).expect("the home is shadowed");
    let root = format!("{hidden}/toolchain");
    let at = args.windows(3).position(|w| w[0] == "--ro-bind" && w[1] == root && w[2] == root).expect("the toolchain is bound back");
    assert!(at > shadow, "{args:?}");
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

#[test_id(tkobgq3)]
#[test]
fn around_binds_the_cargo_config_back_read_only_and_never_the_credentials() {
    let (tmp, home) = cargo_config_fixture("around-cargo-config");
    let release = tmp.path().join("release");
    fs::create_dir_all(&release).unwrap();
    fixture_command(&home);
    let tail = format!("{}/.cargo/bin", home.display());
    let out = gate_environment_runner(&home, &release, &tail, &["--print-args", "--around", AROUND_CMD, "--version"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let args: Vec<String> = String::from_utf8(out.stdout).unwrap().lines().map(String::from).collect();
    assert_config_bound_after_shadow_and_credentials_not(&args, &home);
    assert!(!args.iter().any(|a| a == "--unsetenv"), "{args:?}");
}

#[test_id(tc9c93q)]
#[test]
fn a_binary_run_binds_the_declared_cargo_homes_config_back_read_only_and_never_the_credentials() {
    let (_tmp, home) = cargo_config_fixture("binary-cargo-config");
    let out = std::process::Command::new(workspace().join("tools/test-sandbox/runner"))
        .args(["--print-args", "--around", system_paths::USR_BIN_TRUE])
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

#[test_id(trg288p)]
#[test]
fn a_rounds_own_spira_env_keeps_its_mosquitto_on_the_sandbox_path_and_no_other_tool_of_it() {
    let own = std::env::var_os("PATH").unwrap();
    let store = std::env::split_paths(&own)
        .map(|d| d.join("mosquitto"))
        .find(|p| p.is_file())
        .map(|p| fs::canonicalize(p).unwrap())
        .expect("mosquitto is on PATH, which this test requires");
    let tmp = testkit::TempDir::new("around-round-env");
    let home = fs::canonicalize(tmp.path()).unwrap().join("home");
    let env_bin = home.join("spira-env/bin");
    fs::create_dir_all(&env_bin).unwrap();
    std::os::unix::fs::symlink(&store, env_bin.join("mosquitto")).unwrap();
    let out = std::process::Command::new(workspace().join("tools/test-sandbox/runner"))
        .args(["--print-args", "--around", "bash"])
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", env_bin.display()))
        .env("HOME", &home)
        .output()
        .expect("the runner spawns");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    let args: Vec<&str> = stdout.lines().collect();
    let path = args.windows(3).find(|w| w[0] == "--setenv" && w[1] == "PATH").expect("a PATH is set")[2];
    let dirs: Vec<&str> = path.split(':').collect();
    assert_eq!(dirs.last().copied(), store.parent().unwrap().to_str(), "{path}");
    assert!(!dirs.iter().any(|d| Path::new(d) == env_bin), "{path}");
}

#[test_id(tj4gn43)]
#[test]
fn around_without_the_command_on_path_is_no_verdict_and_names_the_path() {
    let tmp = testkit::TempDir::new("around-no-cargo");
    let home = fs::canonicalize(tmp.path()).unwrap().join("home");
    let release = tmp.path().join("release");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&release).unwrap();
    let out = gate_environment_runner(&home, &release, "", &["--around", AROUND_CMD, "--version"]);
    assert_eq!(out.status.code(), Some(75), "{}", String::from_utf8_lossy(&out.stderr));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains(&format!("{AROUND_CMD} is not on PATH")) && err.contains(&release.display().to_string()), "{err}");
}

#[test_id(tznf3mn)]
#[test]
fn a_held_run_keeps_the_identity_an_enclosing_sandbox_handed_in_when_run_is_unwritable() {
    let tmp = testkit::TempDir::new("held-identity");
    let base = fs::canonicalize(tmp.path()).unwrap();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    let identity = base.join("handed.gitconfig");
    fs::write(&identity, "[user]\n\tname = handed\n\temail = handed@localhost\n").unwrap();
    let mut cmd = std::process::Command::new("bwrap");
    cmd.args(["--ro-bind", system_paths::ROOT, system_paths::ROOT, "--dev", system_paths::DEV, "--proc", system_paths::PROC, "--tmpfs"])
        .arg(&home)
        .args(["--tmpfs", system_paths::RUN, "--remount-ro", system_paths::RUN, "--unshare-net"])
        .arg(workspace().join("tools/test-sandbox/runner"))
        .args(["--around", system_paths::USR_BIN_ENV])
        .env("HOME", &home)
        .env("GIT_CONFIG_SYSTEM", &identity);
    let out = cmd.output().expect("bwrap spawns");
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("bwrap:") {
        eprintln!("skipped: no nested sandbox to hold the runner in: {stderr}");
        return;
    }
    assert!(out.status.success(), "{stderr}");
    let seen = format!("GIT_CONFIG_SYSTEM={}", identity.display());
    assert!(String::from_utf8_lossy(&out.stdout).lines().any(|l| l == seen), "{}", String::from_utf8_lossy(&out.stdout));
}

fn conf_default(key: &str) -> u64 {
    let text = fs::read_to_string(workspace().join("spira/conf.d.defaults.generated.sh")).unwrap();
    let marker = format!("${{{key}:=");
    let at = text.find(&marker).unwrap_or_else(|| panic!("{key} has a default")) + marker.len();
    text[at..].split('}').next().unwrap().parse().unwrap()
}

#[test_id(tpzzf50)]
#[test]
fn the_nextest_config_is_the_unit_deadline_defaults() {
    let want = spira_config::unit_tier::nextest_config(
        (conf_default("SPIRA_UNIT_TEST_DEADLINE_SECS"), conf_default("SPIRA_UNIT_TEST_DEADLINE_PERIODS")),
        (conf_default("SPIRA_INTEGRATION_TEST_DEADLINE_SECS"), conf_default("SPIRA_INTEGRATION_TEST_DEADLINE_PERIODS")),
        &spira_config::unit_tier::DenyList::default(),
    );
    assert_eq!(fs::read_to_string(workspace().join(".config/nextest.toml")).unwrap(), want);
}

#[test_id(ths23zl)]
#[test]
fn the_rounds_config_carries_the_unit_deadline_defaults() {
    let spira = spira_config::load(&workspace().join("spira/testlib/complete.toml")).unwrap().spira.unwrap();
    for key in [
        "SPIRA_UNIT_TEST_DEADLINE_SECS",
        "SPIRA_UNIT_TEST_DEADLINE_PERIODS",
        "SPIRA_INTEGRATION_TEST_DEADLINE_SECS",
        "SPIRA_INTEGRATION_TEST_DEADLINE_PERIODS",
    ] {
        let declared = spira.get_u32(key).unwrap().map(u64::from);
        assert_eq!(declared, Some(conf_default(key)), "complete.toml's {key} differs from the conf.d default");
    }
}

#[test_id(tewojri)]
#[test]
fn the_integration_and_sim_targets_keep_a_two_minute_deadline_in_the_nextest_config() {
    let config: toml::Table = fs::read_to_string(workspace().join(".config/nextest.toml")).unwrap().parse().unwrap();
    let overrides = config["profile"]["default"].get("overrides").and_then(|o| o.as_array()).expect("an override scopes the tests/ targets");
    let tests = overrides.iter().find(|o| o["filter"].as_str() == Some("kind(test)")).expect("an override for kind(test)");
    assert_eq!(tests["slow-timeout"]["period"].as_str(), Some("60s"));
    assert_eq!(tests["slow-timeout"]["terminate-after"].as_integer(), Some(2));
}

#[test_id(tlmmajp)]
#[test]
fn print_args_prints_the_command_line_and_never_runs_the_command() {
    let tmp = testkit::TempDir::new("print-args-probe");
    let marker = tmp.path().join("ran");
    let bin = tmp.path().join("probe");
    testkit::write_exe(&bin, &format!("#!/bin/bash\ntouch {}\n", marker.display()));
    let out = std::process::Command::new(workspace().join("tools/test-sandbox/runner")).args(["--print-args", "--around"]).arg(&bin).output().unwrap();
    assert!(out.status.success() && String::from_utf8_lossy(&out.stdout).contains("bwrap"), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!marker.exists());
}

#[test_id(tkt2rbj)]
#[test]
fn the_built_testenv_supervisor_records_who_signalled_it_and_passes_the_signal_on() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let dir = testkit::TempDir::new("supervise-record");
    let record = dir.path().join("rec");
    let mut sup = Command::new(testkit::bin_exe("testenv"))
        .args(["sandbox-supervise"])
        .arg(&record)
        .args(["sh", "-c", "echo ready; exec sleep 29.5"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = String::new();
    BufReader::new(sup.stdout.take().unwrap()).read_line(&mut ready).unwrap();
    assert_eq!(ready.trim(), "ready");
    let mut killer = Command::new("kill").args(["-TERM", &sup.id().to_string()]).spawn().unwrap();
    let status = sup.wait().unwrap();
    let text = fs::read_to_string(&record).unwrap();
    assert!(text.starts_with(&format!("testenv: stopped from outside: sig=15 from pid={} cmd=", killer.id())), "{text}");
    assert_eq!(status.code(), Some(143));
    killer.wait().unwrap();
}

#[test_id(tzvk5ed)]
#[test]
fn around_binds_a_relative_config_file_argument_at_its_absolute_path() {
    let tmp = testkit::TempDir::new("around-config-file-relative");
    fs::write(tmp.path().join("gate-unit-nextest.toml"), "").unwrap();
    let out = std::process::Command::new(workspace().join("tools/test-sandbox/runner"))
        .current_dir(tmp.path())
        .args(["--print-args", "--around", system_paths::USR_BIN_TRUE, "--config-file", "gate-unit-nextest.toml"])
        .output()
        .unwrap();
    let printed = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = printed.lines().collect();
    let path = fs::canonicalize(tmp.path().join("gate-unit-nextest.toml")).unwrap().display().to_string();
    assert!(lines.windows(3).any(|w| w[0] == "--ro-bind" && w[1] == path && w[2] == path), "{printed}");
    assert!(!lines.windows(2).any(|w| w[0] == "--ro-bind" && w[1] == "gate-unit-nextest.toml"), "{printed}");
}

#[test_id(t4nmj1z)]
#[test]
fn around_binds_a_config_file_argument_read_only_so_a_config_in_the_hidden_home_stays_readable() {
    let tmp = testkit::TempDir::new("around-config-file");
    let home = fs::canonicalize(tmp.path()).unwrap().join("home");
    fs::create_dir_all(&home).unwrap();
    let cfg = home.join("round-nextest.toml");
    fs::write(&cfg, "").unwrap();
    let out = std::process::Command::new(workspace().join("tools/test-sandbox/runner")).args(["--print-args", "--around", system_paths::USR_BIN_TRUE, "--config-file"]).arg(&cfg).env("HOME", &home).output().unwrap();
    let printed = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = printed.lines().collect();
    let (home, path) = (home.display().to_string(), cfg.display().to_string());
    let hidden = lines.windows(2).position(|w| w[0] == "--tmpfs" && w[1] == home).unwrap_or_else(|| panic!("the home is hidden: {printed}"));
    let bound = lines.windows(3).position(|w| w[0] == "--ro-bind" && w[1] == path && w[2] == path).unwrap_or_else(|| panic!("the config is bound: {printed}"));
    assert!(hidden < bound, "the config is bound on top of the hidden home: {printed}");
}
