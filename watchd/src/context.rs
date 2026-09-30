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

/// `SPIRA_HOME` from this process's own environment when set, else `<release-root>/spira` —
/// this binary lives in `<release-root>/bin/watchd`, a SIBLING of `spira/`, never inside it
/// (same fallback `gate`'s `default_home` uses, sp-0tpcs). Getting this one `.parent()` short
/// resolves to `bin/` instead of the release root and every conf.sh-seam call then fails
/// closed with "conf.sh could not be sourced" — caught here, not by a test, because a test
/// that builds its fixture beside the binary would not have noticed either.
pub fn home_dir() -> PathBuf {
    if let Ok(h) = std::env::var("SPIRA_HOME") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    std::env::current_exe().ok().and_then(|p| spira_dir_from_exe(&p)).unwrap_or_else(|| PathBuf::from("spira"))
}

/// `<exe's-parent's-parent>/spira` — pure, so the arithmetic is unit tested without needing
/// to fake `current_exe()`. `None` when the exe path is too shallow to have two ancestors
/// (never true for a real release layout; `home_dir` falls back to a bare `"spira"` then).
fn spira_dir_from_exe(exe: &Path) -> Option<PathBuf> {
    exe.parent().and_then(|d| d.parent()).map(|release_root| release_root.join("spira"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spira_dir_is_a_sibling_of_bin_not_inside_it() {
        // The exe lives at <release-root>/bin/watchd; conf.sh lives at
        // <release-root>/spira/conf.sh — a sibling, never <release-root>/bin/spira.
        let exe = Path::new("/opt/spira/spira-releases/abc123/bin/watchd");
        assert_eq!(spira_dir_from_exe(exe), Some(PathBuf::from("/opt/spira/spira-releases/abc123/spira")));
    }

    #[test]
    fn a_shallow_exe_path_has_no_home_to_derive() {
        assert_eq!(spira_dir_from_exe(Path::new("watchd")), None);
    }
}
