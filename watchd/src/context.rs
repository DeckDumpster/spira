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
        .envs(spira_config::release_env::child_path_env_for_process())
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
    // NON-DESTRUCTIVE: `SPIRA_RUN` and `SPIRA_DB` are both a `WATCHD_KEYS` placeholder AND
    // a named `Context` field below. A `.remove()` here (as the `take` closure does for the
    // fields that are ONLY a placeholder) would empty both before `run`/`db` ever got a
    // chance to read them — caught only by an end-to-end run against the real seam, because
    // every unit test constructs `Context` as a struct literal directly, never through this
    // function.
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
        bd: take("SPIRA_BD"),
        db: take("SPIRA_DB"),
        placeholders,
        systemctl: std::env::var("SPIRA_SYSTEMCTL").unwrap_or_else(|_| "systemctl".to_string()),
        instance: spira_config::resolve::resolve_instance(&std::env::vars().collect(), home)?,
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

/// Where THIS BINARY's own `conf.sh` lives — found by searching upward from the binary's
/// own directory for an ancestor whose `spira/conf.sh` exists.
///
/// DELIBERATELY NEVER `$SPIRA_HOME`, even when a caller has one set. `SPIRA_HOME` is a
/// piece of a CALLER's resolved config — an input conf.sh computes, that a fixture may set
/// to point somewhere else entirely for its own reasons (a test pointed it at a directory
/// holding a three-line stub `conf.sh`, to fake `watch_unit_name` for an unrelated
/// command). The bash's own `watchd.sh` never read `$SPIRA_HOME` to decide what to source
/// either — it sourced conf.sh from `$(dirname "${BASH_SOURCE[0]}")`, i.e. its OWN
/// location, ignoring whatever `$SPIRA_HOME` said. A compiled binary has no
/// `BASH_SOURCE[0]`; searching upward from `current_exe()` is the equivalent question
/// ("where do I, this program, actually live") asked the only way a binary can ask it.
/// Scar: reading `$SPIRA_HOME` first sourced that fixture's stub conf.sh instead of the
/// real one, and every var the real one would have set — `SPIRA_RUN`, `SPIRA_ACTIONABLE`,
/// `SPIRA_NOTIFY_AGE`, the manifest path — came back empty, well before the test's actual
/// assertions were reached.
///
/// NOT A FIXED PARENT COUNT either (a second scar this same walk fixed): a release runs
/// this binary from `<release>/bin/watchd`, one level below `spira/`'s sibling; a testenv
/// or aeon-profile build tree runs it from `<checkout>/target/aeon/watchd`, TWO levels
/// below. Fixing the depth to the release's shape made the conf.sh seam fail closed
/// ("conf.sh could not be sourced") on every other build layout.
pub fn home_dir() -> PathBuf {
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

    /// END TO END, through the real seam (spawns `bash`, parses its NUL-separated output) —
    /// every other test in this crate constructs `Context` as a struct literal directly and
    /// would never have caught this. Scar: `SPIRA_RUN` and `SPIRA_DB` are each BOTH a
    /// `WATCHD_KEYS` placeholder and a named `Context` field; populating `placeholders`
    /// first with the destructive `take` (a `.remove()`) emptied both before `run`/`db`
    /// ever read them, so every daemon row's log path came back relative
    /// ("watchd/<name>.log" instead of "$SPIRA_RUN/watchd/<name>.log") and `health-ids`'
    /// database lookup silently had no database. Found only by running the compiled binary
    /// against a real manifest, not by any unit test, until this one.
    #[test]
    fn load_does_not_let_the_placeholder_map_consume_a_field_the_context_also_needs() {
        let d = testkit::TempDir::new("watchd-context");
        std::fs::write(
            d.join("conf.sh"),
            "SPIRA_RUN=/fixture/run\nSPIRA_DB=/fixture/db\nSPIRA_WATCHERS=/fixture/watchers\n",
        )
        .unwrap();
        let _env = testkit::env(&[("SPIRA_INSTANCE", Some("fixture"))]);
        let ctx = load(&d).expect("the seam to source this trivial conf.sh");
        assert_eq!(ctx.instance, "fixture");
        assert_eq!(ctx.run, "/fixture/run", "SPIRA_RUN is also a WATCHD_KEYS placeholder");
        assert_eq!(ctx.db, "/fixture/db", "SPIRA_DB is also a WATCHD_KEYS placeholder");
        assert_eq!(ctx.watchers, "/fixture/watchers");
        assert_eq!(ctx.placeholders.get("SPIRA_RUN").map(String::as_str), Some("/fixture/run"));
        assert_eq!(ctx.placeholders.get("SPIRA_DB").map(String::as_str), Some("/fixture/db"));
    }
}
