//! The one place `watchd` shells into `conf.sh` — the same "seam" pattern `gate` and
//! `queue-watch` already use for `lib.sh` (DESIGN.md "Non-goals": conf.sh/lib.sh stay bash
//! until the config/store-core rewrite group). Everything else in this crate is native Rust
//! reading the key=value pairs this one call captured.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Every value `watchd` needs that `conf.sh` computes (a default, a derived path, or a
/// `command -v` resolution) rather than merely passing an ambient environment variable
/// through unchanged.
const VARS: &[&str] = &[
    "SPIRA_RUN",
    "SPIRA_HOME",
    "SPIRA_REPO",
    "SPIRA_COCKPIT",
    "SPIRA_DB",
    "SPIRA_WORKSPACES",
    "SPIRA_TOWN",
    "SPIRA_WIKI",
    "SPIRA_VIEW",
    "SPIRA_VIEW_SESSION",
    "SPIRA_WATCHERS",
    "SPIRA_WATCHERS_OVERLAY",
    "SPIRA_CONF_FILE",
    "SPIRA_ACTIONABLE",
    "SPIRA_HEALTH_TIMEOUT",
    "SPIRA_NOTIFY_AGE",
    "SPIRA_ID_PREFIX",
    "SPIRA_BD",
];

const SCRIPT: &str = r#"set -uo pipefail
HERE="$1"; shift
. "$HERE/conf.sh" >/dev/null 2>&1 || exit 97
for v in "$@"; do printf '%s=%s\0' "$v" "${!v-}"; done
exit 0
"#;

#[derive(Debug, Clone)]
pub struct Context {
    pub run: String,
    pub watchers: String,
    pub watchers_overlay: String,
    pub conf_file: String,
    pub actionable: String,
    pub health_timeout: String,
    pub notify_age: String,
    pub id_prefix: String,
    pub bd: String,
    pub db: String,
    /// The ten `WATCHD_KEYS` placeholder values, keyed by name — everything an `@KEY@` in a
    /// manifest row may reference.
    pub placeholders: HashMap<String, String>,
    /// `SPIRA_SYSTEMCTL`, `SPIRA_INSTANCE` — ambient overrides `conf.sh` never defaults, so
    /// read straight from this process's own environment rather than through the seam.
    pub systemctl: String,
    pub instance: String,
    /// `SPIRA_NOW` captured once, here, when `load()` ran — never re-read per call. A
    /// dynamic per-call read of a process environment variable is a data race the moment
    /// more than one test in this crate wants a different clock at once (`cargo test` runs
    /// tests in parallel threads of one process); a value captured at construction is not.
    pub now: i64,
}

/// `bash -c '. "$HERE/conf.sh" >/dev/null 2>&1 || exit 97; …' <home> <vars…>`. `home` is the
/// directory holding `conf.sh` — `SPIRA_HOME` if already known, else this binary's own
/// directory (mirrors how the bash resolved `BASH_SOURCE[0]`'s directory before sourcing
/// itself).
pub fn load(home: &Path) -> Result<Context, String> {
    let out = Command::new("bash")
        .arg("-c")
        .arg(SCRIPT)
        .arg("watchd-context")
        .arg(home)
        .args(VARS)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("cannot run bash: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "watchd: conf.sh could not be sourced from {} — refusing to judge any watcher (exit {})",
            home.display(),
            out.status.code().unwrap_or(-1)
        ));
    }
    let mut kv: HashMap<String, String> = HashMap::new();
    for rec in String::from_utf8_lossy(&out.stdout).split('\0') {
        if let Some((k, v)) = rec.split_once('=') {
            kv.insert(k.to_string(), v.to_string());
        }
    }
    let mut take = |k: &str| kv.remove(k).unwrap_or_default();
    let mut placeholders = HashMap::new();
    for key in crate::manifest::KEYS {
        placeholders.insert(key.to_string(), take(key));
    }
    Ok(Context {
        run: take("SPIRA_RUN"),
        watchers: take("SPIRA_WATCHERS"),
        watchers_overlay: take("SPIRA_WATCHERS_OVERLAY"),
        conf_file: take("SPIRA_CONF_FILE"),
        actionable: take("SPIRA_ACTIONABLE"),
        health_timeout: take("SPIRA_HEALTH_TIMEOUT"),
        notify_age: take("SPIRA_NOTIFY_AGE"),
        id_prefix: take("SPIRA_ID_PREFIX"),
        bd: take("SPIRA_BD"),
        db: take("SPIRA_DB"),
        placeholders,
        systemctl: std::env::var("SPIRA_SYSTEMCTL").unwrap_or_else(|_| "systemctl".to_string()),
        instance: std::env::var("SPIRA_INSTANCE").unwrap_or_else(|_| "prod".to_string()),
        now: read_now(),
    })
}

/// `SPIRA_NOW` from this process's own environment (a test-only clock override `conf.sh`
/// never touches), else the wall clock — read exactly once, at `load()` time.
fn read_now() -> i64 {
    std::env::var("SPIRA_NOW").ok().and_then(|s| s.parse().ok()).unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    })
}

impl Context {
    pub fn watchd_dir(&self) -> PathBuf {
        crate::paths::watchd_dir(&self.run)
    }

    pub fn resolver(&self) -> crate::manifest::MapResolver<'_> {
        crate::manifest::MapResolver(&self.placeholders)
    }

    pub fn health_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.health_timeout.parse().unwrap_or(10))
    }

    /// `SPIRA_NOTIFY_AGE` parsed strictly — `notify` refuses rather than silently treating
    /// junk as "never escalate" (the exact failure the bash's own check exists to end).
    pub fn notify_age(&self) -> Result<i64, String> {
        self.notify_age.parse::<i64>().map_err(|_| {
            format!("watchd: SPIRA_NOTIFY_AGE is '{}' — it must be a whole number of seconds", self.notify_age)
        })
    }
}

/// `SPIRA_HOME` from this process's own environment when set, else found by searching
/// upward from this binary's own directory for an ancestor whose `spira/conf.sh` exists.
///
/// NOT AN ENVIRONMENT LOOKUP EVEN WHEN A CALLER ALREADY SOURCED conf.sh: conf.sh
/// deliberately does not `export` SPIRA_HOME ("for the same reason `SPIRA_ID_PREFIX` is
/// not" — a value a worktree's own conf.sh resolved must never leak into a child process
/// that might be running against a different checkout's idea of itself). So a shell that
/// sourced conf.sh and then runs `watchd` by bare name hands it NO `SPIRA_HOME` at all;
/// this binary has always had to find its own, the same as `gate` and every other
/// Rust-rewritten tool.
///
/// NOT A FIXED PARENT COUNT either (the scar this replaced): a release runs this binary
/// from `<release>/bin/watchd`, one level below `spira/`'s sibling; a testenv or
/// aeon-profile build tree runs it from `<checkout>/target/aeon/watchd`, TWO levels below.
/// Fixing the depth to the release's shape made the conf.sh seam fail closed
/// ("conf.sh could not be sourced") on every other build layout, caught by an aeon-profile
/// testenv run inside a container, not by a unit test whose fixture happened to sit beside
/// the binary either way.
pub fn home_dir() -> PathBuf {
    if let Ok(h) = std::env::var("SPIRA_HOME") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| find_spira_dir(&p, |d| d.join("conf.sh").is_file()))
        .unwrap_or_else(|| PathBuf::from("spira"))
}

/// Walks `exe`'s directory and each ancestor after it, looking for the first whose
/// `spira/` satisfies `exists` (real filesystem in `home_dir`; an in-memory set in tests,
/// so the walk itself is unit tested without touching disk). `None` if no ancestor's
/// `spira/` does — `home_dir` falls back to a bare `"spira"` then.
fn find_spira_dir(exe: &Path, exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let mut dir = exe.parent()?;
    loop {
        let candidate = dir.join("spira");
        if exists(&candidate) {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn has(dirs: &[&str]) -> impl Fn(&Path) -> bool {
        let set: HashSet<PathBuf> = dirs.iter().map(PathBuf::from).collect();
        move |p: &Path| set.contains(p)
    }

    #[test]
    fn a_release_layout_finds_spira_one_level_above_bin() {
        // <release>/bin/watchd; spira/ is a sibling of bin/, one level up.
        let exe = Path::new("/opt/spira/spira-releases/abc123/bin/watchd");
        let exists = has(&["/opt/spira/spira-releases/abc123/spira"]);
        assert_eq!(find_spira_dir(exe, exists), Some(PathBuf::from("/opt/spira/spira-releases/abc123/spira")));
    }

    #[test]
    fn a_testenv_aeon_profile_build_finds_spira_two_levels_above_target_aeon() {
        // <checkout>/target/aeon/watchd; spira/ is beside the checkout root, not beside
        // target/aeon/ — the exact shape that broke the fixed-parent-count version inside
        // a testenv container (this crate's own scar).
        let exe = Path::new("/workspace/target/aeon/watchd");
        let exists = has(&["/workspace/spira"]);
        assert_eq!(find_spira_dir(exe, exists), Some(PathBuf::from("/workspace/spira")));
    }

    #[test]
    fn a_plain_cargo_debug_build_finds_spira_two_levels_above_target_debug() {
        let exe = Path::new("/checkout/target/debug/watchd");
        let exists = has(&["/checkout/spira"]);
        assert_eq!(find_spira_dir(exe, exists), Some(PathBuf::from("/checkout/spira")));
    }

    #[test]
    fn no_ancestor_with_a_spira_dir_is_none() {
        let exe = Path::new("/a/b/c/watchd");
        assert_eq!(find_spira_dir(exe, |_| false), None);
    }

    #[test]
    fn the_nearest_ancestor_wins_when_more_than_one_could_match() {
        // A pathological case (an ancestor further up also happens to have a spira/) —
        // the walk must stop at the first (nearest) match, not the outermost.
        let exe = Path::new("/a/b/c/watchd");
        let exists = has(&["/a/b/spira", "/a/spira"]);
        assert_eq!(find_spira_dir(exe, exists), Some(PathBuf::from("/a/b/spira")));
    }
}
