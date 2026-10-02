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

/// Builds one workspace binary and returns its path, parsed from cargo's own
/// `--message-format=json` rather than guessed from `target/debug/...` (the target
/// directory is whatever `CARGO_TARGET_DIR` says) — same approach
/// `release/tests/session_hook_minimal_env.rs::build_bin` already uses for the identical need.
fn build_bin(package: &str, bin: &str) -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(&cargo)
        .args(["build", "--message-format=json", "-p", package, "--bin", bin])
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

/// `(release_root, run_dir, legacy_conf_path)`.
fn build_fixture_release(tmp: &Path) -> (PathBuf, PathBuf, PathBuf) {
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
    (release_root, run_dir, conf)
}

/// `env -i HOME=<home> PATH=/usr/bin:/bin SPIRA_CONF=<conf> <exe>` — the exact bare-shell
/// repro this bead names, carrying NONE of this test process's own PATH, cargo env, or
/// release env.
fn run_under_bare_shell(exe: &Path, home: &Path, conf: &Path) -> Child {
    Command::new("env")
        .arg("-i")
        .arg(format!("HOME={}", home.display()))
        .arg("PATH=/usr/bin:/bin")
        .arg(format!("SPIRA_CONF={}", conf.display()))
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
    let (release_root, run_dir, conf) = build_fixture_release(&tmp);
    let exe = release_root.join("bin/inbox-triage");

    let mut child = run_under_bare_shell(&exe, &home, &conf);
    // conf.sh's own default: SPIRA_CONCIERGE_INBOX := $SPIRA_RUN/watchd/concierge-inbox.log
    // (spira/conf.d/SPIRA_CONCIERGE_INBOX) — inbox-triage creates this file (and its parent
    // dir) the moment load_config() succeeds, right before it starts tailing it forever. Its
    // existence is this test's "got past conf.sh" signal.
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
