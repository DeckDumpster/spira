//! Shared setup for the `units-install` and `unit-ensure` binaries: resolving host values and
//! the manifest from the environment a caller (`deploy.sh`, `landing-pass`, a human) already
//! set, exactly as `systemd/install.sh` and `unit-ensure.sh` both did by sourcing the same
//! `conf.sh`/`units.sh`. Kept out of the library's own tested modules (which take everything
//! as plain values) so the environment-reading glue is the only thing duplicated nowhere.

use crate::manifest::{self, Manifest};
use crate::values::HostValues;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn env_var(k: &str) -> String {
    env::var(k).unwrap_or_default()
}

pub fn nonempty_env(k: &str) -> Option<String> {
    env::var(k).ok().filter(|v| !v.is_empty())
}

pub fn which(prog: &str) -> Option<String> {
    let path = env::var("PATH").ok()?;
    for dir in path.split(':') {
        let p = Path::new(dir).join(prog);
        if p.is_file() {
            return Some(p.to_string_lossy().to_string());
        }
    }
    None
}

/// Resolve host values from the environment (conf.sh's own precedence: explicit environment
/// wins). Derives nothing beyond what conf.sh itself derives with a plain, no-side-effect
/// default for a render-relevant key: `SPIRA_HOME = SPIRA_REPO/spira` (conf.sh: no-colon
/// derivation), `SPIRA_COCKPIT = dirname(SPIRA_HOME)/cockpit` (conf.sh line ~937),
/// `SPIRA_SNAP_STALE_S = 60` (line ~951), `SPIRA_TESTDB_PORT = 3308` (line ~1015) — the three
/// `: "${VAR:=default}"` conf.sh lines that matter to rendering. A caller that already
/// sourced conf.sh (a human, `deploy.sh`) exports the real value first, so this default is
/// reached only when nothing did — never a silent override of an explicit setting.
pub fn host_from_env(instance: &str) -> HostValues {
    let repo = nonempty_env("SPIRA_REPO").unwrap_or_default();
    let home = nonempty_env("SPIRA_HOME").unwrap_or_else(|| format!("{repo}/spira"));
    let cockpit = nonempty_env("SPIRA_COCKPIT").unwrap_or_else(|| {
        let parent = Path::new(&home).parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        format!("{parent}/cockpit")
    });
    let dolt = nonempty_env("DOLT").or_else(|| which("dolt")).unwrap_or_default();
    HostValues {
        home,
        repo,
        run: env_var("SPIRA_RUN"),
        db: env_var("SPIRA_DB"),
        cockpit,
        dolt_data: env_var("SPIRA_DOLT_DATA"),
        testdb_data: env_var("SPIRA_TESTDB_DATA"),
        dolt,
        prod: env_var("SPIRA_PROD"),
        instance: instance.to_string(),
        testdb_port: nonempty_env("SPIRA_TESTDB_PORT").unwrap_or_else(|| "3308".to_string()),
        snap_stale_s: nonempty_env("SPIRA_SNAP_STALE_S").unwrap_or_else(|| "60".to_string()),
        path_tail: crate::orchestrate::path_tail().unwrap_or_default(),
    }
}

/// `watchd.sh units` — the watcher manifest. Still bash (not this bead's scope); called by
/// bare name exactly as units.sh did.
///
/// `systemd/units.sh` reached `watchd.sh` after `conf.sh` was already sourced, so
/// `SPIRA_WATCHERS` had conf.sh's own default (`$SPIRA_HOME/watchers`, conf.sh line ~877)
/// applied for free; this binary does not source conf.sh, and a batch-container or minimal
/// test env can invoke it with `SPIRA_WATCHERS` unset, which `watchd.sh` itself does not
/// default — same gap class as `bootstrap::host_from_env`'s render defaults, closed the same
/// way: apply the one default `watchd.sh` actually needs here, in this process's own
/// environment for the child, not the caller's.
pub fn watch_names() -> Result<Vec<String>, String> {
    let watchers = nonempty_env("SPIRA_WATCHERS").or_else(|| nonempty_env("SPIRA_HOME").map(|h| format!("{h}/watchers")));
    let mut cmd = Command::new("watchd.sh");
    cmd.arg("units");
    if let Some(w) = watchers {
        cmd.env("SPIRA_WATCHERS", w);
    }
    let out = cmd.output().map_err(|e| format!("cannot run watchd.sh: {e}"))?;
    if !out.status.success() {
        return Err("the watcher manifest is malformed".into());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.split_whitespace().map(|u| u.strip_prefix("spira-watch@").unwrap_or(u).strip_suffix(".service").unwrap_or(u).to_string()).collect())
}

/// `ctrl suspended`'s TSV (`subject<TAB>reason` per line, sp-6onps) — the control plane
/// moved from a bash library (`CTRL_LIB=1 . ctrl.sh`) to a compiled binary, which cannot be
/// sourced, so install.sh now reads every suspension once this way instead of parsing
/// `ctrl.sh list --json`'s render. A missing/failing `ctrl` means nothing is suspended,
/// matching the bash fallback.
pub fn suspended_set() -> std::collections::BTreeSet<String> {
    let out = Command::new("ctrl").arg("suspended").output();
    let Ok(out) = out else { return Default::default() };
    if !out.status.success() {
        return Default::default();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().filter_map(|l| l.split('\t').next()).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

pub fn unit_dir() -> PathBuf {
    if let Some(d) = nonempty_env("SPIRA_UNIT_DIR") {
        return PathBuf::from(d);
    }
    crate::orchestrate::default_unit_dir(&nonempty_env).unwrap_or_else(|| PathBuf::from(".config/systemd/user"))
}

pub fn templates_dir() -> PathBuf {
    let d = nonempty_env("SPIRA_HOME").map(|h| PathBuf::from(h).join("../systemd")).or_else(|| nonempty_env("SPIRA_REPO").map(|r| PathBuf::from(r).join("systemd"))).unwrap_or_else(|| PathBuf::from("systemd"));
    d.canonicalize().unwrap_or(d)
}

/// Build this box's manifest from the environment, printing units.sh's own informational
/// notes to stderr.
pub fn manifest_from_env(instance: &str) -> Result<Manifest, String> {
    let inotify_present = which("inotifywait").is_some();
    let m = manifest::build(&manifest::Inputs {
        instance: instance.to_string(),
        dolt_data_set: nonempty_env("SPIRA_DOLT_DATA").is_some(),
        testdb_data_set: nonempty_env("SPIRA_TESTDB_DATA").is_some(),
        broker_enable: env_var("SPIRA_BROKER_ENABLE") == "1",
        inotify_present,
        watch_names: watch_names(),
    })?;
    for note in &m.notes {
        eprintln!("note: {note}");
    }
    Ok(m)
}

pub fn world_halted() -> bool {
    nonempty_env("SPIRA_RUN").map(|r| Path::new(&r).join("world.halted").exists()).unwrap_or(false)
}
