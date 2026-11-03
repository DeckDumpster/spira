//! The one place `watchd` shells into `conf.sh` — the same "seam" pattern `gate` and
//! `queue-watch` already use for `lib.sh` (DESIGN.md "Non-goals": conf.sh/lib.sh stay bash
//! until the config/store-core rewrite group) — but ONLY for `SPIRA_HOME`/`SPIRA_REPO`/
//! `SPIRA_CONF_FILE`, none of which is a registered `spira/conf.d` key. Every other value
//! this module needs (`CONFIG_VARS`) is registered, and comes instead from
//! `spira_config::process::cfg` (per Ryan 2026-10-05: one source of config) — merged into the
//! same `kv` map the seam fills, so everything downstream still just reads `kv`/`Context`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// `SPIRA_HOME`/`SPIRA_REPO`/`SPIRA_CONF_FILE` — the only three names this crate still
/// takes off the `conf.sh` shell seam: none of the three has a `spira/conf.d` entry
/// (`SPIRA_CONF_FILE` names the OLD bash `spira.conf`, not `$SPIRA_TOML`), so none of them
/// is a `spira_config::process::cfg` key.
const VARS: &[&str] = &["SPIRA_HOME", "SPIRA_REPO", "SPIRA_CONF_FILE"];

/// Every other name `watchd` used to take off the same seam, but which IS a registered
/// `spira/conf.d` key — read instead via `spira_config::process::cfg` (per Ryan 2026-10-05:
/// one source of config), the same per-process `$SPIRA_TOML` resolution every other binary
/// now goes through, merged into the same `kv` map `VARS` fills so every downstream `take`
/// below is unchanged.
const CONFIG_VARS: &[&str] = &[
    "SPIRA_RUN",
    "SPIRA_COCKPIT",
    "SPIRA_DB",
    "SPIRA_WORKSPACES",
    "SPIRA_TOWN",
    "SPIRA_WIKI",
    "SPIRA_VIEW",
    "SPIRA_VIEW_SESSION",
    "SPIRA_WATCHERS",
    "SPIRA_WATCHERS_OVERLAY",
    "SPIRA_ACTIONABLE",
    "SPIRA_HEALTH_TIMEOUT",
    "SPIRA_NOTIFY_AGE",
    "SPIRA_ID_PREFIX",
    "SPIRA_CONCIERGE_INBOX",
];

const SCRIPT: &str = r#"set -uo pipefail
HERE="$1"; shift
. "$HERE/conf.sh" >/dev/null || exit 97
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
    pub bdq: String,
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

/// `bash -c '. "$HERE/conf.sh" >/dev/null || exit 97; …' <home> <vars…>`. `home` is the
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
        // law-a-binary-resolves-the-config-it-reads (sp-kgzql): this binary's own release's
        // bin/+spira/ on the CHILD's PATH, never only inherited — a bare shell with no
        // launcher otherwise leaves `command -v spira-config` unresolved inside conf.sh and
        // this seam dies at "exit 97" before it reads a single config value (inbox-triage's
        // own scar, same seam shape).
        .envs(spira_config::release_env::child_path_env_for_process()?)
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
    // ONE SOURCE (per Ryan 2026-10-05): every `CONFIG_VARS` name is registered, so it comes
    // from `spira_config::process::cfg`, never the `conf.sh` seam above — a key that does not
    // resolve is a refusal naming it, not a value this map quietly lacks.
    for name in CONFIG_VARS {
        let v = spira_config::process::cfg(name)?;
        kv.insert((*name).to_string(), v);
    }
    // Placeholders are copied before `take` empties the map: `SPIRA_RUN` is both a placeholder
    // and a field.
    let mut placeholders = HashMap::new();
    for key in crate::manifest::KEYS {
        placeholders.insert(key.to_string(), kv.get(*key).cloned().unwrap_or_default());
    }
    let mut take = |k: &str| kv.remove(k).unwrap_or_default();
    Ok(Context {
        run: take("SPIRA_RUN"),
        watchers: take("SPIRA_WATCHERS"),
        watchers_overlay: take("SPIRA_WATCHERS_OVERLAY"),
        conf_file: take("SPIRA_CONF_FILE"),
        actionable: take("SPIRA_ACTIONABLE"),
        health_timeout: take("SPIRA_HEALTH_TIMEOUT"),
        notify_age: take("SPIRA_NOTIFY_AGE"),
        id_prefix: take("SPIRA_ID_PREFIX"),
        bdq: spira_config::release_env::tool("bdq")?,
        placeholders,
        systemctl: std::env::var("SPIRA_SYSTEMCTL").unwrap_or_else(|_| "systemctl".to_string()),
        // SPIRA_INSTANCE is a registered key too (just not in CONFIG_VARS above, since it
        // is not part of the conf.sh seam's own VARS/CONFIG_VARS split) — the same one
        // door, never the explicit-`home`-parameter `resolve_instance` call this used to
        // make (per Ryan 2026-10-05: one source of config — `cfg()`'s own SPIRA_HOME-env
        // resolution is the one to trust, not a second, caller-supplied `home`).
        instance: spira_config::process::cfg("SPIRA_INSTANCE")?,
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

/// Where this binary's own `conf.sh` lives: `$SPIRA_RELEASE/spira`, the release it runs from.
/// Never `$SPIRA_HOME`, which is a caller's config input a fixture may point elsewhere; never
/// a search upward from the executable. Unset is a refusal naming the key.
pub fn home_dir() -> Result<PathBuf, String> {
    spira_config::release_env::release_root_for_process().map(|r| r.join("spira"))
}
