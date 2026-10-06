//! sp-ivfu3: `world status`, run from a genuinely bare shell — `env -i HOME=<home>
//! PATH=<bin>:/usr/bin:/bin SPIRA_HOME=<home> SPIRA_TOML=<toml>`, nothing else — must find
//! the resolved config document and report HALTED when that config's own `run` directory
//! holds `world.halted`.
//!
//! `SPIRA_TOML` IS pinned explicitly now: the "ambient tier" this module originally
//! exercised (`$HOME/.config/spira/spira.toml`, no `SPIRA_TOML` set at all) is itself
//! retired (per Ryan 2026-10-05: one source of config — `spira_config::resolve::
//! resolve_process` requires `$SPIRA_TOML` unconditionally now, with no ambient fallback;
//! `spira-config locate`'s own XDG search is a different, narrower door this binary does
//! not go through). `SPIRA_HOME` is the checkout's own `spira/` (where conf.d — the key
//! registry — lives), not a synthetic empty one: `cfg()` refuses a key with no
//! `conf.d/<KEY>` entry, and the old "an existing-but-empty conf.d is fine" tolerance for
//! THIS resolution path is gone with it.
//!
//! BEFORE THIS BEAD: `spira_world::spira_run()` defaulted to the literal `/tmp/spira`
//! whenever `$SPIRA_RUN` itself was unset — it never consulted that document at all — so
//! this exact invocation read (and `world start`/`stop` would have written) the wrong run
//! directory while a real halt sat, unreported, in the fixture's own one. This test is
//! RED against that old code (the fixture's `world.halted` is never found, so `world
//! status` prints "spira: not halted by world.sh" instead) and GREEN now that `spira_run`
//! resolves the DECLARED value from the pinned config, in-process, never a guess.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A complete fixture config (every registered key declared), `run` pointed at a fixture
/// directory that already holds `world.halted`. `instance` stays the fixture's own
/// default, "prod", so the per-timer loop names the real, instance-qualified unit forms.
/// Returns `(home, toml, run)`.
fn build_fixture(tmp: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let home = tmp.join("home");
    let run = tmp.join("run");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(
        run.join("world.halted"),
        "2026-10-02T00:00:00Z\nwhy: sp-ivfu3 fixture\n",
    )
    .unwrap();
    let toml = spira_config::process::fixture_toml(tmp, &[("SPIRA_RUN", &run.display().to_string())]);
    (home, toml, run)
}

/// `env -i HOME=<home> PATH=<bin_dir>:/usr/bin:/bin SPIRA_HOME=<real spira/> SPIRA_TOML=
/// <toml> world status`.
fn run_world_status(home: &Path, toml: &Path) -> std::process::Output {
    let bin_dir = PathBuf::from(env!("CARGO_BIN_EXE_world")).parent().unwrap().to_path_buf();
    let real_home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    Command::new("env")
        .arg("-i")
        .arg(format!("HOME={}", home.display()))
        .arg(format!("PATH={}:/usr/bin:/bin", bin_dir.display()))
        .arg(format!("SPIRA_HOME={}", real_home.display()))
        .arg(format!("SPIRA_TOML={}", toml.display()))
        .arg("world")
        .arg("status")
        .output()
        .expect("cannot spawn env -i world status")
}

#[test]
fn world_status_under_a_bare_shell_reports_halted_from_the_fixture_config() {
    let tmp = testkit::TempDir::new("ivfu3-world-status-bare-shell");
    let (home, toml, _run) = build_fixture(&tmp);

    let out = run_world_status(&home, &toml);

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
    std::fs::create_dir_all(&home).unwrap();
    let toml = tmp.join("spira.toml");
    std::fs::write(&toml, "this is not [valid toml").unwrap();

    let out = run_world_status(&home, &toml);

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
    let (home, toml, _run) = build_fixture(&tmp);
    let stub = tmp.join("systemctl");
    testkit::write_exe(&stub, "#!/bin/sh\necho 'Failed to connect to user scope bus via local transport' >&2\nexit 1\n");
    let bin_dir = PathBuf::from(env!("CARGO_BIN_EXE_world")).parent().unwrap().to_path_buf();
    let real_home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");

    let out = Command::new("env")
        .arg("-i")
        .arg(format!("HOME={}", home.display()))
        .arg(format!("PATH={}:/usr/bin:/bin", bin_dir.display()))
        .arg(format!("SPIRA_HOME={}", real_home.display()))
        .arg(format!("SPIRA_TOML={}", toml.display()))
        .arg(format!("SPIRA_SYSTEMCTL={}", stub.display()))
        .args(["world", "status"])
        .output()
        .unwrap();

    let all = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!out.status.success(), "must exit nonzero:\n{all}");
    assert!(all.contains("cannot reach the systemd user bus"), "{all}");
    assert!(!all.contains("MISSING"), "{all}");
}
