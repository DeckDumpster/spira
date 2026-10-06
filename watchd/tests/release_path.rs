//! sp-kgzql: `watchd` shells into `conf.sh` via a bash child (`context::load`), the same seam
//! shape `inbox-triage` has — and the same scar: a bare shell with no launcher PATH
//! (`env -i HOME=$HOME PATH=/usr/bin:/bin <release>/bin/watchd manifest`) left the bash
//! child's PATH bare too, so `command -v spira-config` inside `conf.sh` failed and the whole
//! process died with "conf.sh could not be sourced ... (exit 97)" before `watchd` ever read
//! a watcher row.
//!
//! Builds a fixture release root the same way `inbox-triage/tests/release_path.rs` does:
//! `bin/watchd` COPIED (never symlinked — `watchd::context::home_dir` walks up from its
//! resolved `current_exe()`, and a symlink would resolve into cargo's own target directory,
//! which has no `spira/` sibling), `bin/spira-config` built fresh and copied in, `spira/`
//! symlinked to this checkout's own. `SPIRA_DB` points at a directory that is never created,
//! so `spira-config check-bd`'s "no store yet" skip applies — no real `bd` binary needed.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("watchd/ has a parent").to_path_buf()
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


/// See `inbox-triage/tests/release_path.rs::build_bin` — identical approach, duplicated
/// rather than shared because the two crates' test suites do not otherwise depend on each
/// other and a test-only cross-crate dependency would be a stranger addition than repeating
/// twenty lines.
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

/// `(release_root, legacy_conf_path, spira_toml_path)`. `watchd::context::load` now reads
/// `SPIRA_RUN`/`SPIRA_DB`/`SPIRA_WATCHERS`/`SPIRA_ID_PREFIX` (and the rest of its
/// `CONFIG_VARS`) through `spira_config::process::cfg` (per Ryan 2026-10-05: one source of
/// config), not off this legacy `spira.conf` — so the fixture needs a real config file
/// too, declaring every registered key `fixture_toml` knows about, with these four pinned.
fn build_fixture_release(tmp: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let workspace = workspace_root();
    let release_root = tmp.join("release");
    std::fs::create_dir_all(release_root.join("bin")).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_watchd"), release_root.join("bin/watchd")).expect("copy watchd");
    std::fs::copy(build_bin("spira-config", "spira-config"), release_root.join("bin/spira-config")).expect("copy spira-config");
    std::os::unix::fs::symlink(workspace.join("spira"), release_root.join("spira")).expect("symlink spira/");

    let run_dir = tmp.join("run");
    std::fs::create_dir_all(&run_dir).unwrap();
    let db_dir = tmp.join("db"); // deliberately never created
    let watchers = tmp.join("watchers");
    std::fs::write(&watchers, "probe|extern|ok\n").unwrap();
    let conf = tmp.join("spira.conf");
    std::fs::write(
        &conf,
        format!(
            "SPIRA_ID_PREFIX = sp\nSPIRA_RUN = {}\nSPIRA_DB = {}\nSPIRA_WATCHERS = {}\n",
            run_dir.display(),
            db_dir.display(),
            watchers.display(),
        ),
    )
    .unwrap();
    let toml = spira_config::process::fixture_toml(
        tmp,
        &[
            ("SPIRA_ID_PREFIX", "sp"),
            ("SPIRA_RUN", &run_dir.display().to_string()),
            ("SPIRA_DB", &db_dir.display().to_string()),
            ("SPIRA_WATCHERS", &watchers.display().to_string()),
        ],
    );
    (release_root, conf, toml)
}

#[test]
fn gets_past_conf_sh_under_a_bare_shell_with_no_launcher_path() {
    let tmp = testkit::TempDir::new("watchd-release-path");
    let home = tmp.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let (release_root, conf, toml) = build_fixture_release(&tmp);
    let exe = release_root.join("bin/watchd");

    // SPIRA_HOME must be explicit now — `spira_config::resolve::locate_home` no longer
    // walks up from the executable looking for a `spira/` sibling (per the Concierge:
    // locate_home is SPIRA_HOME, else $SPIRA_RELEASE/spira, else refuse).
    let out = Command::new("env")
        .arg("-i")
        .arg(format!("HOME={}", home.display()))
        .arg("PATH=/usr/bin:/bin")
        .arg(format!("SPIRA_HOME={}", release_root.join("spira").display()))
        .arg(format!("SPIRA_CONF={}", conf.display()))
        .arg(format!("SPIRA_TOML={}", toml.display()))
        .arg(&exe)
        .arg("manifest")
        .stdin(Stdio::null())
        .output()
        .expect("cannot spawn env -i watchd manifest");

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !stderr.contains("exit 97") && !stderr.contains("could not be sourced"),
        "still hit the exit-97 spira-config-not-found guard this bead fixes:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "watchd manifest should succeed once conf.sh resolves; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("probe"), "expected the fixture's own watcher row in the manifest output:\n{stdout}");
}
