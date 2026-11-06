//! Where `watchd` gets its configuration. `SPIRA_HOME`/`SPIRA_REPO`/`SPIRA_CONF_FILE` are no
//! registered `spira/conf.d` key, so they are derived here exactly as `conf.sh` derived them
//! ([`seam_vars`], in-process: no `bash -c . conf.sh` child); every other value (`CONFIG_VARS`)
//! comes from `spira_config::process::cfg` (per Ryan 2026-10-05: one source of config), merged
//! into the same `kv` map, so everything downstream still just reads `kv`/`Context`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Every other name `watchd` used to take off the same seam, but which IS a registered
/// `spira/conf.d` key — read instead via `spira_config::process::cfg` (per Ryan 2026-10-05:
/// one source of config), the same per-process `$SPIRA_TOML` resolution every other binary
/// now goes through, merged into the same `kv` map [`seam_vars`] fills so every downstream `take`
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

/// The context `conf.sh` used to supply, read in-process. `home` is the
/// directory holding `conf.sh` — `SPIRA_HOME` if already known, else this binary's own
/// directory (mirrors how the bash resolved `BASH_SOURCE[0]`'s directory before sourcing
/// itself).
pub fn load(home: &Path) -> Result<Context, String> {
    // conf.sh no longer runs, but a home without it is still the refusal the seam gave.
    if !home.join("conf.sh").is_file() {
        return Err(format!("watchd: conf.sh could not be sourced from {} — refusing to judge any watcher (exit 1)", home.display()));
    }
    let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let mut kv = seam_vars(home, &env);
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

/// `SPIRA_HOME`, `SPIRA_REPO` and `SPIRA_CONF_FILE` exactly as `conf.sh` left them, computed in-process: `SPIRA_HOME` is the
/// caller's own if set, else `home` resolved (`cd … && pwd -P`); `SPIRA_REPO` the caller's own,
/// else the checkout holding home (`spira_config::resolve::derive_home_repo`); `SPIRA_CONF_FILE`
/// only ever the caller's own (conf.sh never assigns it).
pub fn seam_vars(home: &Path, env: &std::collections::BTreeMap<String, String>) -> HashMap<String, String> {
    let given = |k: &str| env.get(k).filter(|v| !v.is_empty()).cloned();
    let spira_home = given("SPIRA_HOME").map(PathBuf::from).unwrap_or_else(|| home.canonicalize().unwrap_or_else(|_| home.to_path_buf()));
    let repo = spira_config::resolve::derive_home_repo(&spira_home, env);
    HashMap::from([
        ("SPIRA_HOME".to_string(), spira_home.display().to_string()),
        ("SPIRA_REPO".to_string(), repo.display().to_string()),
        ("SPIRA_CONF_FILE".to_string(), env.get("SPIRA_CONF_FILE").cloned().unwrap_or_default()),
    ])
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn tree() -> (testkit::TempDir, PathBuf) {
        let tmp = testkit::TempDir::new("watchd-ctx");
        std::fs::create_dir_all(tmp.path().join("repo/.git")).unwrap();
        std::fs::create_dir_all(tmp.path().join("repo/spira")).unwrap();
        let root = tmp.path().canonicalize().unwrap();
        (tmp, root)
    }

    #[test]
    fn seam_vars_derive_home_and_repo_from_the_conf_dir_when_the_caller_names_neither() {
        let (_tmp, root) = tree();
        let v = seam_vars(&root.join("repo/spira"), &BTreeMap::new());
        assert_eq!(v["SPIRA_HOME"], root.join("repo/spira").display().to_string());
        assert_eq!(v["SPIRA_REPO"], root.join("repo").display().to_string());
        assert_eq!(v["SPIRA_CONF_FILE"], "");
    }

    #[test]
    fn seam_vars_keep_what_the_caller_set() {
        let (_tmp, root) = tree();
        let env: BTreeMap<String, String> =
            [("SPIRA_HOME", "/h"), ("SPIRA_REPO", "/r"), ("SPIRA_CONF_FILE", "/c/spira.conf")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let v = seam_vars(&root.join("repo/spira"), &env);
        assert_eq!((v["SPIRA_HOME"].as_str(), v["SPIRA_REPO"].as_str(), v["SPIRA_CONF_FILE"].as_str()), ("/h", "/r", "/c/spira.conf"));
    }
}
