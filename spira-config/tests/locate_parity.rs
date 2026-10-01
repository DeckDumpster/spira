//! Parity proof (sp-hconl, DESIGN-locate.md §4): `spira-config locate` must name the same
//! `spira.toml` (or the same absence) as `conf.sh`'s own `spira_toml_file`, across every tier
//! it searches. This test extracts the ACTUAL function bodies of `spira_toml_file` and
//! `spira_conf_file` out of this worktree's `spira/conf.sh` — never a hand-copied
//! restatement of them — into a throwaway bash harness, so a future edit to `conf.sh`'s
//! search makes this test fail instead of silently drifting from the Rust side.
//!
//! Read-only: this test never sources the rest of `conf.sh` (which has side effects —
//! containment checks, a bd schema preflight that can `exit 1`, PATH mutation), and never
//! touches this box's real `~/.config/spira/spira.toml`; every scenario runs against a
//! scratch `$HOME`/`$XDG_CONFIG_HOME` the test creates itself with `env_clear()`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<repo>/spira-config`; conf.sh lives at `<repo>/spira/conf.sh`.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

/// Pulls the verbatim body of `name() { ... }` out of `text` — the first line matching
/// `name() {` through the next line that is exactly `}`. Panics (loudly, this is a test
/// fixture, not production code) if `conf.sh` no longer defines `name` in this shape, which
/// is itself a signal this parity test needs attention before anything else does.
fn extract_function(text: &str, name: &str) -> String {
    let start_marker = format!("{name}() {{");
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim_end() == start_marker)
        .unwrap_or_else(|| panic!("conf.sh no longer defines `{start_marker}` verbatim"));
    let end = lines[start..]
        .iter()
        .position(|l| l.trim_end() == "}")
        .unwrap_or_else(|| panic!("no closing brace found for `{name}`"));
    lines[start..=start + end].join("\n")
}

/// Builds the throwaway bash harness: the two extracted functions, plus a final call to
/// whichever one `call` names, printing its stdout unindented.
fn harness_script(conf_sh: &str, call: &str) -> String {
    let toml_fn = extract_function(conf_sh, "spira_toml_file");
    let conf_fn = extract_function(conf_sh, "spira_conf_file");
    format!("#!/usr/bin/env bash\nset -u\n{conf_fn}\n\n{toml_fn}\n\n{call}\n")
}

fn scratch_dir(tag: &str) -> testkit::TempDir {
    testkit::TempDir::new(&format!("spira-config-test-locate-parity-{tag}"))
}

/// Runs `conf.sh`'s real `spira_toml_file` (bash) in a fully cleared environment plus
/// `env`, returning its stdout trimmed — empty string means "no file", matching the
/// function's own documented contract.
fn bash_spira_toml_file(env: &[(&str, &str)]) -> String {
    let conf_sh = fs::read_to_string(repo_root().join("spira/conf.sh")).expect("read conf.sh");
    let script = harness_script(&conf_sh, "spira_toml_file");
    let script_dir = scratch_dir("script");
    let script_path = script_dir.join("harness.sh");
    fs::write(&script_path, script).unwrap();
    let mut cmd = Command::new("bash");
    cmd.env_clear();
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.arg(&script_path);
    let out = cmd.output().expect("bash runs");
    assert!(out.status.success(), "harness script failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The compiled `spira-config locate`'s answer under the same environment: `Some(path)` on a
/// clean exit (Found), `None` on exit 1 (NotFound) or exit 2 (LegacyOnly) — collapsed the
/// same way `discover()` collapses them, since `spira_toml_file` alone (never
/// `spira_toml_resolve`'s auto-convert) is bash's side of this comparison, and
/// `spira_toml_file` only ever says "a path" or "empty", never "ambiguous".
fn rust_locate(env: &[(&str, &str)]) -> Option<String> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_spira-config"));
    cmd.env_clear();
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.arg("locate");
    let out = cmd.output().expect("spira-config runs");
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

fn assert_parity(scenario: &str, env: &[(&str, &str)]) {
    let bash = bash_spira_toml_file(env);
    let rust = rust_locate(env);
    let bash_opt = if bash.is_empty() { None } else { Some(bash) };
    assert_eq!(
        bash_opt, rust,
        "{scenario}: conf.sh's spira_toml_file said {bash_opt:?}, spira-config locate said {rust:?} (env: {env:?})"
    );
}

fn etc_spira_real() -> bool {
    Path::new("/etc/spira/spira.toml").is_file()
}

#[test]
fn parity_explicit_spira_toml_set_and_present() {
    let dir = scratch_dir("set-present");
    let p = dir.join("spira.toml");
    fs::write(&p, "[spira]\n").unwrap();
    assert_parity("SPIRA_TOML set, file exists", &[("SPIRA_TOML", p.to_str().unwrap())]);
}

#[test]
fn parity_explicit_spira_toml_set_and_missing() {
    let dir = scratch_dir("set-missing");
    let p = dir.join("nonexistent.toml");
    assert_parity(
        "SPIRA_TOML set, file missing (no fallthrough)",
        &[("SPIRA_TOML", p.to_str().unwrap())],
    );
}

#[test]
fn parity_xdg_config_home_tier() {
    let dir = scratch_dir("xdg");
    fs::create_dir_all(dir.join("xdg/spira")).unwrap();
    fs::write(dir.join("xdg/spira/spira.toml"), "[spira]\n").unwrap();
    let home = dir.join("home-empty");
    fs::create_dir_all(&home).unwrap();
    assert_parity(
        "XDG_CONFIG_HOME tier",
        &[
            ("XDG_CONFIG_HOME", dir.join("xdg").to_str().unwrap()),
            ("HOME", home.to_str().unwrap()),
        ],
    );
}

#[test]
fn parity_home_dot_config_tier_when_xdg_unset() {
    let dir = scratch_dir("home");
    fs::create_dir_all(dir.join("home/.config/spira")).unwrap();
    fs::write(dir.join("home/.config/spira/spira.toml"), "[spira]\n").unwrap();
    assert_parity(
        "HOME/.config fallback (XDG unset)",
        &[("HOME", dir.join("home").to_str().unwrap())],
    );
}

#[test]
fn parity_nothing_found() {
    if etc_spira_real() {
        eprintln!("skipping: this machine has a real /etc/spira/spira.toml");
        return;
    }
    let dir = scratch_dir("none");
    let home = dir.join("home-empty");
    fs::create_dir_all(&home).unwrap();
    assert_parity("nothing at any tier", &[("HOME", home.to_str().unwrap())]);
}

#[test]
fn parity_spira_repo_is_ignored_by_both() {
    // THE CASE THIS BEAD EXISTS FOR: both sides must agree $SPIRA_REPO is not a candidate —
    // conf.sh since sp-9hwim, spira-config as of this bead.
    if etc_spira_real() {
        eprintln!("skipping: this machine has a real /etc/spira/spira.toml");
        return;
    }
    let dir = scratch_dir("repo-ignored");
    fs::create_dir_all(dir.join("repo")).unwrap();
    fs::write(dir.join("repo/spira.toml"), "[spira]\n").unwrap();
    let home = dir.join("home-empty");
    fs::create_dir_all(&home).unwrap();
    assert_parity(
        "SPIRA_REPO set with a real spira.toml beside it, but unconsulted by either side",
        &[("SPIRA_REPO", dir.join("repo").to_str().unwrap()), ("HOME", home.to_str().unwrap())],
    );
}

/// Positive control for the test harness itself: proves `extract_function` really is reading
/// `conf.sh`'s current text (not a cached or hand-copied body) by asserting the doc comment
/// directly above `spira_toml_file` still carries the citation conf.sh's own author attached
/// to the SPIRA_REPO removal — if sp-9hwim's change were ever reverted without this test
/// noticing, this is what would catch it independently of the behavioural comparisons above.
/// Also asserts the function BODY itself never names `SPIRA_REPO` — the actual parity claim,
/// not just the comment above it.
#[test]
fn extraction_pulls_the_real_current_conf_sh_text() {
    let conf_sh = fs::read_to_string(repo_root().join("spira/conf.sh")).expect("read conf.sh");
    let toml_fn = extract_function(&conf_sh, "spira_toml_file");
    assert!(!toml_fn.contains("SPIRA_REPO"), "{toml_fn}");
    let doc_line = conf_sh
        .lines()
        .take_while(|l| !l.trim_end().starts_with("spira_toml_file() {"))
        .last()
        .expect("a line precedes spira_toml_file's definition");
    assert!(doc_line.contains("sp-9hwim"), "{doc_line}");
}
