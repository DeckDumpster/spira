//! sp-kgzql: `inbox-triage` shells into `conf.sh` via a bash child, and the child must find
//! `spira-config` on PATH even when the PARENT process itself was launched from a bare shell
//! with nothing of a release on PATH — exactly how the SessionStart compact hook arms the
//! Concierge's durable inbox Monitor (`env -i HOME=$HOME PATH=/usr/bin:/bin
//! /home/.../spira-releases/current/bin/inbox-triage`). Before this bead's fix, the bash
//! child inherited that same bare PATH, `command -v spira-config` inside `conf.sh` failed,
//! and the whole process died with "conf.sh could not be sourced ... (exit 97)" before it
//! ever read a config value.
//!
//! Builds a fixture release root: `bin/inbox-triage` COPIED from this crate's own just-built
//! test binary (never symlinked — `inbox-triage::home_dir` walks up from its resolved
//! `current_exe()`, and a symlink would resolve into cargo's own target directory, which has
//! no `spira/` sibling at all; `release/tests/session_hook_minimal_env.rs` names the same
//! hazard for `watchd`), `bin/spira-config` built fresh and copied in, and `spira/` symlinked
//! to this checkout's own (conf.sh/conf.d are unmodified by this fix — the real ones are
//! what has to be proven against). `SPIRA_DB` points at a directory that is never created,
//! so `spira-config check-bd`'s own "no store yet" skip applies and no real `bd` binary is
//! needed at all.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("inbox-triage/ has a parent").to_path_buf()
}
/// The cargo profile this test binary was itself built in, from its own path
/// (`<target>/<profile-dir>/deps/<exe>`; `debug` is the `dev` profile). Building the fixture
/// binary in the SAME profile reuses what is already built — without it, a gate testing under
/// its own profile cold-built the whole dependency tree a second time, minutes per gate (sp-0umtv).
fn own_profile() -> String {
    let exe = std::env::current_exe().expect("test binary has a path");
    let dir = exe.parent().and_then(|d| d.parent()).and_then(|d| d.file_name()).and_then(|n| n.to_str()).unwrap_or("debug").to_string();
    if dir == "debug" { "dev".to_string() } else { dir }
}


/// Builds one workspace binary and returns its path, parsed from cargo's own
/// `--message-format=json` rather than guessed from `target/debug/...` (the target
/// directory is whatever `CARGO_TARGET_DIR` says) — same approach
/// `release/tests/session_hook_minimal_env.rs::build_bin` already uses for the identical need.
fn build_bin(package: &str, bin: &str) -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(&cargo)
        .args(["build", "--message-format=json", "--profile", &own_profile(), "-p", package, "--bin", bin])
        .current_dir(workspace_root())
        .output()
        .unwrap_or_else(|e| panic!("cannot run cargo build -p {package}: {e}"));
    assert!(out.status.success(), "cargo build -p {package} failed:\n{}", String::from_utf8_lossy(&out.stderr));
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v.get("reason").and_then(Value::as_str) == Some("compiler-artifact")
            && v.get("target").and_then(|t| t.get("name")).and_then(Value::as_str) == Some(bin)
        {
            if let Some(exe) = v.get("executable").and_then(Value::as_str) {
                return PathBuf::from(exe);
            }
        }
    }
    panic!("{bin}'s binary artifact did not appear in cargo's own build output");
}

/// `(release_root, run_dir, legacy_conf_path, spira_toml_path)`. `inbox-triage` now reads
/// `SPIRA_CONCIERGE_INBOX`/`SPIRA_CONCIERGE_INBOX_DEDUP` through
/// `spira_config::process::cfg` (per Ryan 2026-10-05: one source of config) — a plain
/// `$SPIRA_TOML` file parse, not the legacy `spira.conf`/`conf.sh` seam — so the fixture
/// needs a real `spira.toml` declaring every registered key, with `SPIRA_CONCIERGE_INBOX`
/// pinned at the path this test watches for.
fn build_fixture_release(tmp: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let workspace = workspace_root();
    let release_root = tmp.join("release");
    std::fs::create_dir_all(release_root.join("bin")).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_inbox-triage"), release_root.join("bin/inbox-triage")).expect("copy inbox-triage");
    std::fs::copy(build_bin("spira-config", "spira-config"), release_root.join("bin/spira-config")).expect("copy spira-config");
    std::os::unix::fs::symlink(workspace.join("spira"), release_root.join("spira")).expect("symlink spira/");

    let run_dir = tmp.join("run");
    std::fs::create_dir_all(&run_dir).unwrap();
    let db_dir = tmp.join("db"); // deliberately never created
    let conf = tmp.join("spira.conf");
    std::fs::write(
        &conf,
        format!(
            "SPIRA_ID_PREFIX = sp\nSPIRA_RUN = {}\nSPIRA_DB = {}\n",
            run_dir.display(),
            db_dir.display(),
        ),
    )
    .unwrap();
    let inbox_log = run_dir.join("watchd/concierge-inbox.log");
    let toml = spira_config::process::fixture_toml(
        tmp,
        &[
            ("SPIRA_ID_PREFIX", "sp"),
            ("SPIRA_RUN", &run_dir.display().to_string()),
            ("SPIRA_DB", &db_dir.display().to_string()),
            ("SPIRA_CONCIERGE_INBOX", &inbox_log.display().to_string()),
        ],
    );
    (release_root, run_dir, conf, toml)
}

/// `env -i HOME=<home> PATH=/usr/bin:/bin SPIRA_HOME=<release>/spira SPIRA_CONF=<conf>
/// SPIRA_TOML=<toml> <exe>` — the exact bare-shell repro this bead names, carrying NONE of
/// this test process's own PATH, cargo env, or release env. `SPIRA_HOME` has to be
/// explicit now: `spira_config::resolve::locate_home` no longer walks up from the
/// executable looking for a `spira/` sibling (it is SPIRA_HOME, else `$SPIRA_RELEASE/
/// spira`, else refuse) — `exe`'s own release root supplies it.
fn run_under_bare_shell(exe: &Path, home: &Path, conf: &Path, toml: &Path) -> Child {
    let spira_home = exe.parent().and_then(|bin| bin.parent()).expect("exe has a release root").join("spira");
    Command::new("env")
        .arg("-i")
        .arg(format!("HOME={}", home.display()))
        .arg("PATH=/usr/bin:/bin")
        .arg(format!("SPIRA_HOME={}", spira_home.display()))
        .arg(format!("SPIRA_CONF={}", conf.display()))
        .arg(format!("SPIRA_TOML={}", toml.display()))
        .arg(exe)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("cannot spawn env -i <exe>")
}

#[test]
fn gets_past_conf_sh_under_a_bare_shell_with_no_launcher_path() {
    let tmp = testkit::TempDir::new("inbox-triage-release-path");
    let home = tmp.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let (release_root, run_dir, conf, toml) = build_fixture_release(&tmp);
    let exe = release_root.join("bin/inbox-triage");

    let mut child = run_under_bare_shell(&exe, &home, &conf, &toml);
    // `SPIRA_CONCIERGE_INBOX`, pinned explicitly in the fixture toml above (its own
    // declared default — spira/conf.d/SPIRA_CONCIERGE_INBOX — is `$SPIRA_RUN/watchd/
    // concierge-inbox.log`, which `fixture_toml` cannot re-derive from our `SPIRA_RUN`
    // override, since the complete fixture bakes concrete literals). inbox-triage creates
    // this file (and its parent dir) the moment `load_config()` succeeds, right before it
    // starts tailing it forever — its existence is this test's "config resolved" signal.
    let inbox_log = run_dir.join("watchd/concierge-inbox.log");
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut exited = None;
    while Instant::now() < deadline {
        if inbox_log.is_file() {
            break;
        }
        if let Ok(Some(status)) = child.try_wait() {
            exited = Some(status);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let created = inbox_log.is_file();
    if exited.is_none() {
        let _ = child.kill();
    }
    let out = child.wait_with_output().expect("wait for inbox-triage");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        created,
        "inbox-triage never created its own inbox log — conf.sh did not resolve under a bare shell; stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("exit 97") && !stderr.contains("could not be sourced"),
        "still hit the exit-97 spira-config-not-found guard this bead fixes:\n{stderr}"
    );
}
