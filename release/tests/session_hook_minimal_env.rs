//! sp-7jr34 requirement 3: `session.sh` and `ctx-meter.sh`, run through the REGISTERED
//! COMMAND `session_hook::install` writes, under the same minimal environment the client
//! gives a hook (`env -i HOME PATH=/usr/bin:/bin`) — never the launcher's own PATH. This is
//! the regression itself: the old registration was a bare path with no `env`/`PATH` prefix,
//! so it ran under the CLIENT's environment and `conf.sh` could not find `spira-config`. The
//! fix makes the registered command self-contained, so it must succeed under an environment
//! that carries nothing of the launcher's at all.
//!
//! The bare form's OWN failure shape moved under us (sp-ubcgo, 225aeafae, "conf.sh's resolve
//! refusal is return, not exit", landed the same day this test does): conf.sh now `return`s
//! out of its failed resolve instead of `exit`ing the whole process, deliberately, so a
//! handful of OTHER callers that guard their own `. conf.sh || true` degrade instead of
//! dying. `session.sh` is not one of those callers, but it is not guarded by `set -e` either,
//! so it was never going to die from that `return` — it just carries on with every derived
//! key unset, reaches its own `WATCHD status` call with `watchd` not on the bare `PATH`
//! either, and takes the "no watchers configured" empty-output exit built for a harness
//! nobody has turned on (session.sh's own "IT ALWAYS EXITS 0" rule). So the bare form no
//! longer dies with a visible hook error; it goes quiet instead — rc 0, no output — which is
//! what this test now has to demonstrate instead of a nonzero exit.
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
    build_bin("spira-config", "spira-config")
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


/// Builds one workspace binary the hook execs and returns its path (see [`build_spira_config`]).
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
        if v.get("reason").and_then(Value::as_str) == Some("compiler-artifact") && v.get("target").and_then(|t| t.get("name")).and_then(Value::as_str) == Some(bin) {
            if let Some(exe) = v.get("executable").and_then(Value::as_str) {
                return PathBuf::from(exe);
            }
        }
    }
    panic!("{bin}'s binary artifact did not appear in cargo's own build output");
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
/// real one — so a fixture can be isolated from the real host's own config document and
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
    let home = tmp.path().join("home"); // fresh — no real host config document, no real transcripts
    std::fs::create_dir_all(&home).unwrap();
    let release_root = tmp.path().join("release");
    std::fs::create_dir_all(release_root.join("bin")).unwrap();
    std::os::unix::fs::symlink(&spira_config, release_root.join("bin/spira-config")).expect("symlink spira-config");
    // session.sh reads its watcher rows from the release's own `watchd` binary (sp-48f6g);
    // without it `watchd status` is not found, prints nothing, and the hook stays silent.
    // COPIED, as a release copies it, never symlinked: watchd finds conf.sh by walking up
    // from its resolved current_exe(), and a symlink resolves into the target directory —
    // outside any spira/ when the gate builds on tmpfs.
    std::fs::copy(build_bin("watchd", "watchd"), release_root.join("bin/watchd")).expect("copy watchd");
    std::os::unix::fs::symlink(workspace.join("spira"), release_root.join("spira")).expect("symlink spira/");

    let paths = release::session_hook::resolve(&release_root, "", tmp.path().join("settings.json")).expect("resolve paths");

    // `SPIRA_CONF` (the legacy pre-toml override) is DEAD: spira/conf.sh (per Ryan
    // 2026-10-05) no longer reads it at all — "no legacy spira.conf, no conversion, no
    // default for any key" (conf.sh's own top-of-file comment). Every value this fixture
    // needs now goes through `$SPIRA_TOML` instead, via `fixture_toml`'s declare list:
    //   - `SPIRA_PROD` — session.sh's own guard ("SPIRA_PROD IS THE HARNESS systemd
    //     ExecStarts FROM") silences the hook whenever its physically-derived SPIRA_HOME
    //     disagrees with SPIRA_PROD; must name this fixture's own release as the one in
    //     force, or the hook goes silent exactly as it must in a second, inactive checkout.
    //   - `SPIRA_RUN` — matches what this test writes session state under.
    //   - `SPIRA_WATCHERS`/`SPIRA_WATCHERS_OVERLAY` — watchd's own manifest read; the
    //     complete fixture's placeholder paths for both do not exist, which is
    //     indistinguishable from "no watchers configured" (this test's whole point is a
    //     REAL watcher row, not that case) — pointed at this fixture's own manifest, and a
    //     nonexistent overlay dir (matching an operator who configured none).
    let run_dir = tmp.path().join("run");
    let manifest = tmp.path().join("watchers");
    std::fs::write(&manifest, "probe|daemon|/bin/true\n").unwrap();
    let prod_s = release_root.join("spira").display().to_string();
    let run_s = run_dir.display().to_string();
    let watchers_s = manifest.display().to_string();
    let overlay_s = tmp.path().join("watchers-overlay-unset").display().to_string();
    let toml = spira_config::process::fixture_toml(
        tmp.path(),
        &[("SPIRA_PROD", &prod_s), ("SPIRA_RUN", &run_s), ("SPIRA_WATCHERS", &watchers_s), ("SPIRA_WATCHERS_OVERLAY", &overlay_s)],
    );
    // `spira_config::resolve::locate_home` no longer walks up from the exe's own location
    // (per Ryan 2026-10-05: named, never searched for) — it needs SPIRA_HOME outright or
    // SPIRA_RELEASE (every real unit's own shape, release/src/units.rs's `release_values`).
    // `hook_command()` already carries `SPIRA_RELEASE=<release_root>`, which should derive
    // the same place, but watchd is handed SPIRA_HOME directly here too, named outright
    // rather than relying on that derivation chain reaching it unbroken through session.sh.
    let home_dir = release_root.join("spira").display().to_string();

    let hook_cmd = format!("SPIRA_TOML={} SPIRA_HOME={} {}", toml.display(), home_dir, paths.hook_command());
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
    // prefix at all — must still fail to do the hook's job under this same minimal env,
    // because `conf.sh` cannot find `spira-config` on a bare PATH. This is what proves the
    // fix above is a fix and not a fixture that would have passed either way.
    //
    // NOT a nonzero exit code (see the module doc): session.sh never dies for this, by its
    // own "IT ALWAYS EXITS 0" rule, both before and after sp-ubcgo's 225aeafae. What the bug
    // actually costs is the watcher line this hook exists to print — so that is what a
    // revert of the env/PATH wrapper has to be caught losing, against the sibling test above
    // that proves the real, wrapped command still produces it.
    let workspace = workspace_root();
    let tmp = testkit::TempDir::new("session-hook-regression");
    let release_root = tmp.path().join("release");
    std::fs::create_dir_all(&release_root).unwrap();
    std::os::unix::fs::symlink(workspace.join("spira"), release_root.join("spira")).expect("symlink spira/");
    // Deliberately NO bin/spira-config on this release root, and no env/PATH wrapper — the
    // bare path the old script wrote.
    let bare_command = release_root.join("spira/hooks/session.sh").display().to_string();
    let (rc, out) = run_under_minimal_env(&bare_command, "");
    assert_eq!(rc, 0, "a SessionStart hook exits 0 even for this failure shape (session.sh's own rule) — a nonzero code here would be a different bug, not this one; got rc={rc}, output:\n{out}");
    assert!(out.trim().is_empty(), "the bare, unwrapped registration must produce no watcher output under a minimal env — if it printed the watcher line the wrapped command does, this test no longer demonstrates the defect it exists to guard against; got output:\n{out}");
}
