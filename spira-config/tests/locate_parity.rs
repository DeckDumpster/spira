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
use std::path::PathBuf;

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

/// The extraction pulls the real function out of this tree's conf.sh.
#[test]
fn extraction_pulls_the_real_current_conf_sh_text() {
    let conf_sh = fs::read_to_string(repo_root().join("spira/conf.sh")).unwrap();
    let f = extract_function(&conf_sh, "spira_toml_file");
    assert!(f.contains("SPIRA_TOML") && !f.contains("XDG_CONFIG_HOME"), "{f}");
}
