//! sp-ivfu3: `world status`, run from a genuinely bare shell — `env -i HOME=<home>
//! PATH=<bin>:/usr/bin:/bin`, nothing else, no `SPIRA_RUN`/`SPIRA_TOML` at all — must
//! still find the resolved config document under `$HOME/.config/spira/` (the ambient
//! tier `spira_config::locate` searches with no pin set) and report HALTED when that
//! config's own `run` directory holds `world.halted`.
//!
//! `SPIRA_HOME` IS set, to a fixture harness directory holding nothing but an empty
//! `conf.d/` — `resolve_for_process` needs a real, readable `conf.d` to resolve ANY key
//! at all (an existing-but-empty directory is fine; a missing one is a named refusal —
//! `spira_config::registry::load`'s own doc). On a real release this is `locate_home`'s
//! own ancestor search finding the release's shipped `spira/conf.d` beside the binary;
//! pinning it here keeps the test's answer independent of exactly where cargo happens to
//! place `CARGO_BIN_EXE_world` for a given build (the gate's own tmpfs target directory is
//! not nested under this checkout's `spira/` at a depth that ancestor search would find).
//!
//! BEFORE THIS BEAD: `spira_world::spira_run()` defaulted to the literal `/tmp/spira`
//! whenever `$SPIRA_RUN` itself was unset — it never consulted that document at all — so
//! this exact invocation read (and `world start`/`stop` would have written) the wrong run
//! directory while a real halt sat, unreported, in the fixture's own one. This test is
//! RED against that old code (the fixture's `world.halted` is never found, so `world
//! status` prints "spira: not halted by world.sh" instead) and GREEN now that `spira_run`
//! resolves it in-process.

use std::path::{Path, PathBuf};
use std::process::Command;

/// `$HOME/.config/spira/`'s own config document, pointing `run` at a fixture directory
/// that already holds `world.halted`, plus `instance = "prod"` so the per-timer loop names
/// the real, instance-qualified unit forms rather than the unqualified ones. Returns
/// `(home, harness_home, run)`.
fn build_fixture(tmp: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let home = tmp.join("home");
    let harness_home = tmp.join("harness-home");
    let run = tmp.join("run");
    std::fs::create_dir_all(home.join(".config/spira")).unwrap();
    std::fs::create_dir_all(harness_home.join("conf.d")).unwrap();
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(
        run.join("world.halted"),
        "2026-10-02T00:00:00Z\nwhy: sp-ivfu3 fixture\n",
    )
    .unwrap();
    std::fs::write(
        home.join(".config/spira").join(spira_config::FILE_NAME),
        format!("[spira]\nrun = {:?}\ninstance = \"prod\"\n", run.display().to_string()),
    )
    .unwrap();
    (home, harness_home, run)
}

/// `env -i HOME=<home> PATH=<bin_dir>:/usr/bin:/bin SPIRA_HOME=<harness_home> world status`
/// — see the module doc for why `SPIRA_HOME` is pinned rather than left for `locate_home`'s
/// own ancestor search to find.
fn run_world_status(home: &Path, harness_home: &Path) -> std::process::Output {
    let bin_dir = PathBuf::from(env!("CARGO_BIN_EXE_world")).parent().unwrap().to_path_buf();
    Command::new("env")
        .arg("-i")
        .arg(format!("HOME={}", home.display()))
        .arg(format!("PATH={}:/usr/bin:/bin", bin_dir.display()))
        .arg(format!("SPIRA_HOME={}", harness_home.display()))
        .arg("world")
        .arg("status")
        .output()
        .expect("cannot spawn env -i world status")
}

#[test]
fn world_status_under_a_bare_shell_reports_halted_from_the_fixture_config() {
    let tmp = testkit::TempDir::new("ivfu3-world-status-bare-shell");
    let (home, harness_home, _run) = build_fixture(&tmp);

    let out = run_world_status(&home, &harness_home);

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stdout.lines().next().is_some_and(|l| l.starts_with("spira: HALTED since ")),
        "world status did not report HALTED from the fixture config under a bare shell\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// The companion refusal: a config document that fails to parse must make `world status`
/// name the refusal and exit nonzero — never silently fall back to `/tmp/spira` and carry
/// on as if nothing were configured.
#[test]
fn world_status_refuses_named_when_the_only_config_is_malformed() {
    let tmp = testkit::TempDir::new("ivfu3-world-status-bad-toml");
    let home = tmp.join("home");
    let harness_home = tmp.join("harness-home");
    std::fs::create_dir_all(home.join(".config/spira")).unwrap();
    std::fs::create_dir_all(harness_home.join("conf.d")).unwrap();
    std::fs::write(home.join(".config/spira").join(spira_config::FILE_NAME), "this is not [valid toml").unwrap();

    let out = run_world_status(&home, &harness_home);

    assert!(!out.status.success(), "a malformed config document must not exit 0");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stderr.contains("FATAL world") && stderr.contains("cannot resolve spira.run"),
        "expected a named refusal, got:\n{stderr}"
    );
    assert!(!stderr.contains("/tmp/spira"), "must never guess /tmp/spira:\n{stderr}");
}

/// A control that cannot check must refuse: when systemctl cannot connect to the user
/// bus, `world status` names that, never classifying the timers as MISSING.
#[test]
fn world_status_refuses_when_the_user_bus_is_unreachable() {
    let tmp = testkit::TempDir::new("wf3gc-world-status-no-bus");
    let (home, harness_home, _run) = build_fixture(&tmp);
    let stub = tmp.join("systemctl");
    testkit::write_exe(&stub, "#!/bin/sh\necho 'Failed to connect to user scope bus via local transport' >&2\nexit 1\n");
    let bin_dir = PathBuf::from(env!("CARGO_BIN_EXE_world")).parent().unwrap().to_path_buf();

    let out = Command::new("env")
        .arg("-i")
        .arg(format!("HOME={}", home.display()))
        .arg(format!("PATH={}:/usr/bin:/bin", bin_dir.display()))
        .arg(format!("SPIRA_HOME={}", harness_home.display()))
        .arg(format!("SPIRA_SYSTEMCTL={}", stub.display()))
        .args(["world", "status"])
        .output()
        .unwrap();

    let all = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!out.status.success(), "must exit nonzero:\n{all}");
    assert!(all.contains("cannot reach the systemd user bus"), "{all}");
    assert!(!all.contains("MISSING"), "{all}");
}
