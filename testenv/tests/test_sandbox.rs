//! Every test binary runs under cargo's target runner, `tools/test-sandbox/runner`, which
//! `.cargo/config.toml` names: a bubblewrap sandbox in which the caller's installation is out of
//! reach (sp-bpenvh). What a binary can reach inside it is the test-sandbox suite's to prove
//! with a probe binary; these tests read files and spawn at most the runner's own `--print-args`.

#[macro_use]
extern crate spira_test_id;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn workspace() -> PathBuf {
    spira_config::resolve::checkout_root_for_process().unwrap()
}

#[test_id(tx9xe9v)]
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
        .arg(system_paths::USR_BIN_ENV)
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
        conf_default("SPIRA_UNIT_TEST_DEADLINE_SECS"),
        conf_default("SPIRA_UNIT_TEST_DEADLINE_PERIODS"),
        &spira_config::unit_tier::DenyList::default(),
    );
    assert_eq!(fs::read_to_string(workspace().join(".config/nextest.toml")).unwrap(), want);
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

struct Fixture {
    _tmp: testkit::TempDir,
    root: PathBuf,
}

fn bounded_fixture() -> Fixture {
    let tmp = testkit::TempDir::new("bounded-runner");
    let root = fs::canonicalize(tmp.path()).unwrap();
    fs::create_dir_all(root.join("tools/test-sandbox")).unwrap();
    fs::create_dir_all(root.join(".config")).unwrap();
    testkit::write_exe(root.join("tools/test-sandbox/runner"), &fs::read_to_string(workspace().join("tools/test-sandbox/runner")).unwrap());
    fs::write(root.join(".config/nextest.toml"), "[profile.default]\nslow-timeout = { period = \"1s\", terminate-after = 1 }\n").unwrap();
    Fixture { _tmp: tmp, root }
}

impl Fixture {
    /// A stream test is about bytes, not silence: a starved watcher on a loaded box outlasts one period.
    fn streaming(self) -> Self {
        fs::write(self.root.join(".config/nextest.toml"), "[profile.default]\nslow-timeout = { period = \"60s\", terminate-after = 1 }\n").unwrap();
        self
    }

    fn run(&self, name: &str, script: &str, nextest: bool) -> std::process::Output {
        let bin = self.root.join(name);
        testkit::write_exe(&bin, &format!("#!/bin/bash\n{script}\n"));
        let mut cmd = std::process::Command::new("bash");
        cmd.arg(self.root.join("tools/test-sandbox/runner"));
        cmd.arg(&bin).env_remove("NEXTEST");
        if nextest {
            cmd.env("NEXTEST", "1");
        }
        cmd.output().expect("the runner spawns")
    }

    fn assert_killed(&self, name: &str, script: &str, test: &str) {
        let out = self.run(name, script, false);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(124), "{err}");
        assert!(err.contains(&format!("test-sandbox: HUNG test={test} binary={name}")), "{err}");
        let pid: String = fs::read_to_string(self.root.join("grandchild")).unwrap().trim().to_string();
        assert!(!Path::new(system_paths::PROC).join(&pid).exists(), "grandchild {pid} outlived the runner");
    }
}

const GRANDCHILD: &str = "sleep 6 & echo $! > \"$(dirname \"$0\")/grandchild\"";

#[test_id(tt2sybr)]
#[test]
fn a_binary_that_prints_libtests_hung_line_and_sleeps_is_killed_named_and_its_children_die() {
    let f = bounded_fixture();
    f.assert_killed("hangs", &format!("{GRANDCHILD}\necho 'test mod::slow has been running for over 60 seconds'\nwait"), "mod::slow");
}

#[test_id(tfv15vy)]
#[test]
fn a_silent_binary_is_killed_after_the_declared_period_and_its_children_die() {
    let f = bounded_fixture();
    f.assert_killed("silent", &format!("{GRANDCHILD}\nwait"), "-");
}

#[test_id(t6gqur3)]
#[test]
fn a_finishing_binary_keeps_its_exit_code_stdout_and_stderr() {
    let f = bounded_fixture();
    let out = f.run("done", "echo out; echo err >&2; exit 7", false);
    assert_eq!((out.status.code(), &out.stdout[..], &out.stderr[..]), (Some(7), &b"out\n"[..], &b"err\n"[..]));
}

#[test_id(tf36ekh)]
#[test]
fn under_nextest_a_silent_binary_is_not_killed() {
    let f = bounded_fixture();
    let out = f.run("quiet", "sleep 1.5; echo fin", true);
    assert_eq!((out.status.code(), &out.stdout[..]), (Some(0), &b"fin\n"[..]));
}

#[test_id(txpbx5s)]
#[test]
fn a_burst_of_output_on_both_streams_arrives_whole_and_separate() {
    let f = bounded_fixture().streaming();
    let out = f.run("burst", "for i in $(seq 1 3000); do echo \"o$i\"; echo \"e$i\" >&2; done", false);
    let lines = |b: &[u8], p: char| String::from_utf8_lossy(b).lines().map(String::from).collect::<Vec<_>>().eq(&(1..=3000).map(|i| format!("{p}{i}")).collect::<Vec<_>>());
    assert!(lines(&out.stdout, 'o') && lines(&out.stderr, 'e'), "streams arrive whole and in order");
}

/// uutils tee dies mid-stream when the kernel sizes its private pipe down; a stream must not
/// depend on a `tee` on the caller's PATH.
#[test_id(tppacx1)]
#[test]
fn a_tee_that_dies_mid_stream_cannot_cut_a_stream() {
    let f = bounded_fixture().streaming();
    let shim = f.root.join("shim");
    fs::create_dir_all(&shim).unwrap();
    testkit::write_exe(shim.join("tee"), "#!/bin/bash\nexec 3>\"$1\"\nfor _ in 1 2; do IFS= read -r l && printf '%s\\n' \"$l\" && printf '%s\\n' \"$l\" >&3; done\nexit 101\n");
    let bin = f.root.join("burst");
    testkit::write_exe(&bin, "#!/bin/bash\nfor i in $(seq 1 50); do echo \"o$i\"; echo \"e$i\" >&2; done\n");
    let path = format!("{}:{}", shim.display(), std::env::var("PATH").unwrap_or_default());
    let out = std::process::Command::new("bash")
        .arg(f.root.join("tools/test-sandbox/runner"))
        .arg(&bin)
        .env_remove("NEXTEST")
        .env("PATH", path)
        .output()
        .expect("the runner spawns");
    let want = |p: char| (1..=50).map(|i| format!("{p}{i}\n")).collect::<String>();
    assert_eq!(
        (out.status.code(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned()),
        (Some(0), want('o'), want('e'))
    );
}

#[test_id(tztmwrt)]
#[test]
fn silence_is_bounded_by_the_file_period_times_terminate_after_not_by_one_period() {
    let f = bounded_fixture();
    fs::write(f.root.join(".config/nextest.toml"), "[profile.default]\nslow-timeout = { period = \"1s\", terminate-after = 3 }\n").unwrap();
    let out = f.run("pause", "sleep 1.5; echo fin", false);
    assert_eq!((out.status.code(), &out.stdout[..]), (Some(0), &b"fin\n"[..]));
}

#[test_id(tav3w5w)]
#[test]
fn silence_is_bounded_by_the_longest_deadline_in_the_file_wherever_it_sits() {
    let f = bounded_fixture();
    fs::write(f.root.join(".config/nextest.toml"), "[profile.default]\nslow-timeout = { period = \"1s\", terminate-after = 3 }\n[[profile.default.overrides]]\nfilter = \"kind(lib)\"\nslow-timeout = { period = \"1s\", terminate-after = 1 }\n").unwrap();
    let out = f.run("pause", "sleep 1.5; echo fin", false);
    assert_eq!((out.status.code(), &out.stdout[..]), (Some(0), &b"fin\n"[..]));
}

#[test_id(tlmmajp)]
#[test]
fn print_args_prints_the_command_line_and_never_runs_the_binary() {
    let f = bounded_fixture();
    let marker = f.root.join("ran");
    let bin = f.root.join("probe");
    testkit::write_exe(&bin, &format!("#!/bin/bash\ntouch {}\n", marker.display()));
    let out = std::process::Command::new("bash")
        .arg(f.root.join("tools/test-sandbox/runner"))
        .arg("--print-args")
        .arg(&bin)
        .output()
        .unwrap();
    assert!(out.status.success() && String::from_utf8_lossy(&out.stdout).contains("bwrap"));
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
