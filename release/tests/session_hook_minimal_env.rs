//! sp-7jr34 requirement 3: `session.sh` and `ctx-meter.sh`, run through the REGISTERED
//! COMMAND `session_hook::install` writes, under the same minimal environment the client
//! gives a hook (`env -i HOME PATH=/usr/bin:/bin`) — never the launcher's own PATH. This is
//! the regression itself: the old registration was a bare path with no `env`/`PATH` prefix,
//! so it ran under the CLIENT's environment and `conf.sh` could not find `spira-config`
//! (fails closed), exiting 1 on every one of the 27 accumulated entries. The fix makes the
//! registered command self-contained, so it must succeed under an environment that carries
//! nothing of the launcher's at all.
//!
//! Builds a fixture release root: `bin/spira-config` (a real build — conf.sh's own
//! `spira-config convert`/`export` calls need it, and that dependency is the whole defect),
//! `spira/` symlinked to this checkout's own (session.sh and ctx-meter.sh are unmodified by
//! this bead, so the real ones are the ones to prove).

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("release/ has a parent").to_path_buf()
}

/// Builds `spira-config`'s own binary (not built by `cargo test -p release` alone: it is a
/// path dependency's *library*, and cargo only builds a dependency's `[[bin]]` on request)
/// and returns its path, parsed from `--message-format=json` rather than guessed from
/// `target/debug/...` — the target directory is whatever `CARGO_TARGET_DIR` says, or the
/// gate's own sccache-backed one (release/DESIGN.md "Build IO"), never assumed.
fn build_spira_config() -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(&cargo)
        .args(["build", "--message-format=json", "-p", "spira-config", "--bin", "spira-config"])
        .current_dir(workspace_root())
        .output()
        .expect("cannot run cargo build -p spira-config");
    assert!(out.status.success(), "cargo build -p spira-config failed:\n{}", String::from_utf8_lossy(&out.stderr));
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v.get("reason").and_then(Value::as_str) == Some("compiler-artifact") && v.get("target").and_then(|t| t.get("name")).and_then(Value::as_str) == Some("spira-config") {
            if let Some(exe) = v.get("executable").and_then(Value::as_str) {
                return PathBuf::from(exe);
            }
        }
    }
    panic!("spira-config's binary artifact did not appear in cargo's own build output");
}

/// Runs `command` (the registered hook/meter command, already self-contained: `env
/// SPIRA_RELEASE=... PATH=... <script>`) exactly as the client would — `stdin`, and an
/// OUTER environment reset to the bare minimum (`env -i HOME PATH=/usr/bin:/bin`), carrying
/// NONE of this test process's own PATH, cargo env, or release env.
fn run_under_minimal_env(command: &str, stdin: &str) -> (i32, String) {
    let home = PathBuf::from(std::env::var("HOME").expect("HOME must be set to run this test at all"));
    run_under_minimal_env_as(&home, command, stdin)
}

/// [`run_under_minimal_env`], against a caller-chosen `HOME` rather than this test process's
/// real one — so a fixture can be isolated from the real host's own `spira.toml` and
/// transcripts while still exercising the client's real minimal environment shape.
fn run_under_minimal_env_as(home: &Path, command: &str, stdin: &str) -> (i32, String) {
    let home = home.display().to_string();
    let mut child = Command::new("env")
        .arg("-i")
        .arg(format!("HOME={home}"))
        .arg("PATH=/usr/bin:/bin")
        .arg("bash")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("cannot spawn env -i bash -c");
    use std::io::Write;
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).expect("write stdin");
    let out = child.wait_with_output().expect("wait for the hook");
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).to_string())
}

#[test]
fn hook_and_meter_run_clean_under_the_clients_own_minimal_env() {
    let workspace = workspace_root();
    let spira_config = build_spira_config();

    let tmp = testkit::TempDir::new("session-hook-minimal-env");
    let home = tmp.path().join("home"); // fresh — no real spira.toml, no real transcripts
    std::fs::create_dir_all(&home).unwrap();
    let release_root = tmp.path().join("release");
    std::fs::create_dir_all(release_root.join("bin")).unwrap();
    std::os::unix::fs::symlink(&spira_config, release_root.join("bin/spira-config")).expect("symlink spira-config");
    std::os::unix::fs::symlink(workspace.join("spira"), release_root.join("spira")).expect("symlink spira/");

    let paths = release::session_hook::resolve(&release_root, "", tmp.path().join("settings.json")).expect("resolve paths");

    // session.sh's own guard (hooks/session.sh: "SPIRA_PROD IS THE HARNESS systemd
    // ExecStarts FROM") silences it whenever SPIRA_HOME (derived from its own, physically
    // resolved location) disagrees with SPIRA_PROD — exactly right in production, where a
    // second checkout must never print into the operator's real sessions, but it means a
    // fixture must say, explicitly, that ITS OWN release IS the one in force. `SPIRA_CONF`
    // (the legacy pre-toml override conf.sh still reads) says so without touching the real
    // host's spira.toml, and gives the hook a manifest with one real row rather than an
    // empty one — the "no watchers means no output" case this is not trying to exercise.
    let run_dir = tmp.path().join("run");
    let manifest = tmp.path().join("watchers");
    std::fs::write(&manifest, "probe|daemon|/bin/true\n").unwrap();
    let conf = tmp.path().join("spira.conf");
    std::fs::write(&conf, format!("SPIRA_ID_PREFIX = sp\nSPIRA_PROD = {}\nSPIRA_RUN = {}\nSPIRA_WATCHERS = {}\n", release_root.join("spira").display(), run_dir.display(), manifest.display())).unwrap();

    let hook_cmd = format!("SPIRA_CONF={} {}", conf.display(), paths.hook_command());
    let (rc, out) = run_under_minimal_env_as(&home, &hook_cmd, "{\"hook_event_name\":\"SessionStart\",\"source\":\"startup\"}");
    assert_eq!(rc, 0, "session.sh must exit 0 through its registered command under a minimal env; got rc={rc}, output:\n{out}");
    assert!(!out.trim().is_empty(), "session.sh produced no output at all, with a real watcher row in its manifest");

    // context_window.current_usage set (even empty) is the client's own "a real API response
    // has happened" signal (ctx-meter.sh: "THE SUPPLIED FIELD WINS") — it needs no real
    // transcript under this fresh HOME to produce its headline.
    let (rc, out) = run_under_minimal_env_as(&home, &paths.meter_command(), "{\"context_window\": {\"current_usage\": {}, \"total_input_tokens\": 12345}}");
    assert_eq!(rc, 0, "ctx-meter.sh must exit 0 through its registered command under a minimal env; got rc={rc}, output:\n{out}");
    assert!(!out.trim().is_empty(), "ctx-meter.sh produced no output at all");
}

#[test]
fn the_old_bare_path_form_is_the_regression_this_replaces() {
    // POSITIVE CONTROL (law-absence-needs-a-positive-control): the exact shape sp-7jr34
    // found still live in the operator's real settings.json — a command with no `env`/`PATH`
    // prefix at all — fails under this same minimal env, because `conf.sh` cannot find
    // `spira-config` on a bare PATH. This is what proves the fix above is a fix and not a
    // fixture that would have passed either way.
    let workspace = workspace_root();
    let tmp = testkit::TempDir::new("session-hook-regression");
    let release_root = tmp.path().join("release");
    std::fs::create_dir_all(&release_root).unwrap();
    std::os::unix::fs::symlink(workspace.join("spira"), release_root.join("spira")).expect("symlink spira/");
    // Deliberately NO bin/spira-config on this release root, and no env/PATH wrapper — the
    // bare path the old script wrote.
    let bare_command = release_root.join("spira/hooks/session.sh").display().to_string();
    let (rc, _out) = run_under_minimal_env(&bare_command, "");
    assert_ne!(rc, 0, "the bare, unwrapped registration must fail under a minimal env — if it didn't, this test no longer demonstrates the defect it exists to guard against");
}
