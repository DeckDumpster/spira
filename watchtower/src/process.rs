//! The process's config: the toml door for registered keys, the environment then the resolved
//! registry for the env-only ones, and the sweep's `Cfg` built from both. Shared by `main.rs`
//! and by a host (the scheduler's `watch.tower` task) that runs the sweep in-process.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The meminfo file every memory reading shares; `SPIRA_MEMINFO_PATH` points tests at a fixture.
pub fn meminfo_path() -> PathBuf {
    getenv("SPIRA_MEMINFO_PATH").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(format!("{}/meminfo", system_paths::PROC)))
}

use crate::{cpu_throttle, deadline, incident, sweep};

/// `std::env::var`, trimmed to "set and non-empty" — no `spira_config` fallback. Used only
/// inside [`resolved_config`]'s own init, which must not call back into [`getenv`] (that
/// would recurse into `resolved_config()` while it is still being built).
pub fn raw_env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

/// The config for the keys `spira/conf.d` does not declare, resolved once and cached, from the
/// home `spira_config::resolve::locate_home` names (`$SPIRA_HOME`, else `$SPIRA_RELEASE/spira`).
/// A failure is said on stderr and yields an empty resolution, so [`getenv`]'s callers get
/// their own defaults. A registered key reads through [`reg`]/[`reg_i64`] instead.
pub fn resolved_config() -> &'static spira_config::resolve::Resolved {
    static RESOLVED: std::sync::OnceLock<spira_config::resolve::Resolved> = std::sync::OnceLock::new();
    RESOLVED.get_or_init(|| resolve_from(&std::env::vars().collect()))
}

fn resolve_from(env: &BTreeMap<String, String>) -> spira_config::resolve::Resolved {
    match spira_config::resolve::locate_home(env, Path::new("")) {
        Ok(home) => {
            let repo = spira_config::resolve::derive_home_repo(&home, env);
            spira_config::resolve::resolve_or_say("watchtower", &home, &repo, env)
        }
        Err(e) => {
            eprintln!("watchtower: cannot locate the harness home, running on built-in defaults: {e}");
            Default::default()
        }
    }
}

/// The environment, then `spira_config::resolve()`'s in-process answer — never the
/// reverse, so a test or a fixture's explicit env override still wins exactly as it did
/// before this bead. For a key `spira/conf.d` registers, use [`reg`]/[`reg_i64`] instead —
/// this path is for the unregistered keys only (see the migration report).
pub fn getenv(k: &str) -> Option<String> {
    raw_env(k).or_else(|| {
        let v = resolved_config().get(k);
        (!v.is_empty()).then(|| v.to_string())
    })
}

pub fn getenv_i64(k: &str, default: i64) -> i64 {
    getenv(k).and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Where `lib.sh` itself lives, found on `$PATH`, never `$SPIRA_HOME`; empty when it is not
/// on `$PATH`.
pub fn lib_sh_dir() -> String {
    incident::which("lib.sh")
        .and_then(|p| std::path::Path::new(&p).parent().map(|d| d.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

pub fn resolved_incident_sh() -> String {
    incident::resolve(getenv("SPIRA_INCIDENT_SH")).unwrap_or_else(|| "incident.sh".to_string())
}


fn reg(key: &str) -> Result<String, String> {
    spira_config::process::cfg(key)
}

fn reg_i64(key: &str) -> Result<i64, String> {
    spira_config::process::cfg_parse::<i64>(key)
}

/// The sweep's `Cfg`: every registered key through the toml door (an unresolved one refuses,
/// naming it), every env-only key with the bash's own default. Arms `deadline` from the two
/// timeout keys, as every sweep has.
pub fn sweep_cfg() -> Result<sweep::Cfg, String> {
    let start_s = reg_i64("SPIRA_WATCHTOWER_START_TIMEOUT_S")?.max(0) as u64;
    deadline::init(
        Duration::from_secs(reg_i64("SPIRA_WATCHTOWER_PROBE_TIMEOUT_S")?.max(1) as u64),
        Duration::from_secs(start_s.saturating_sub(30).max(30)),
    );
    let run = PathBuf::from(reg("SPIRA_RUN")?);
    let yields = strand::yield_report::Sources::resolve(&run, getenv);
    Ok(sweep::Cfg {
        spira_run: run,
        cockpit_view_dir: getenv("XDG_RUNTIME_DIR").map(|d| spira_config::cockpit_view_dir(std::path::Path::new(&d))),
        lib_sh_dir: lib_sh_dir(),
        db: reg("SPIRA_DB")?,
        home_repo: reg("SPIRA_HOME_REPO")?,
        ask_label: reg("SPIRA_ASK_LABEL")?,
        snap_stale_s: reg_i64("SPIRA_SNAP_STALE_S")?,
        gate_window_s: getenv_i64("SPIRA_WATCH_GATE_WINDOW", 21600),
        gate_silence_window_s: getenv_i64("SPIRA_WATCH_GATE_SILENCE_WINDOW", 3600),
        gate_deadline_s: reg_i64("SPIRA_GATE_DEADLINE")?,
        gate_log: getenv("SPIRA_GATE_LOG").map(PathBuf::from),
        noverdict_window_s: reg_i64("SPIRA_NOVERDICT_WINDOW")?,
        noverdict_max: reg_i64("SPIRA_NOVERDICT_MAX")?,
        yield_window_s: yields.window_s as i64,
        yield_dir: yields.dir,
        disk_warn_pct: getenv_i64("SPIRA_DISK_WARN_PCT", 90),
        mem_warn_mb: getenv_i64("SPIRA_MEM_WARN_MB", 1500),
        meminfo_path: meminfo_path(),
        failed_units_warn_mins: getenv_i64("SPIRA_FAILED_UNITS_WARN_MINS", 15),
        cpu_throttle_units: cpu_throttle::units_of(&reg("SPIRA_CPU_THROTTLE_UNITS")?),
        cpu_throttle_warn_pct: getenv_i64("SPIRA_CPU_THROTTLE_WARN_PCT", 5),
        cpu_throttle_min_periods: getenv_i64("SPIRA_CPU_THROTTLE_MIN_PERIODS", 100),
        cgroup_root: getenv("SPIRA_CGROUP_ROOT").unwrap_or_else(|| system_paths::SYS_FS_CGROUP.to_string()),
        unsent_warn_h: getenv_i64("SPIRA_UNSENT_WARN_H", 24),
        closed_stranded_warn_h: getenv_i64("SPIRA_CLOSED_STRANDED_WARN_H", 48),
        drain_warn_mins: getenv_i64("SPIRA_DRAIN_WARN_MINS", 15),
        idle_while_ready_n: getenv_i64("SPIRA_IDLE_WHILE_READY_N", 5).max(0) as usize,
        units: runtime::units::host(getenv("SPIRA_SYSTEMCTL").as_deref()),
        prompt_file: getenv("SPIRA_WATCH_PROMPT_FILE").map(PathBuf::from),
        lapsed_dir: getenv("SPIRA_LAPSED_DIR").map(PathBuf::from),
        lapsed_marker: getenv("SPIRA_LAPSED_MARKER").map(PathBuf::from),
        failed_units_state: getenv("SPIRA_FAILED_UNITS_STATE").map(PathBuf::from),
        throttle_stamp: getenv("SPIRA_THROTTLE_STAMP").map(PathBuf::from),
        queue_throttle_override: reg("SPIRA_QUEUE_THROTTLE_OVERRIDE")?,
        incident_sh: resolved_incident_sh(),
        cockpit_db: spira_config::process::cfg("COCKPIT_DB").ok().filter(|d| !d.is_empty()),
        moot_timeout_s: getenv_i64("SPIRA_MOOT_TIMEOUT", 30).max(1) as u64,
        resolve_bin: match getenv("SPIRA_RESOLVE_SH") {
            Some(r) => r,
            None => spira_config::release_env::tool("resolve")?,
        },
        lc: spira_store::lc_call::LcClient::configured()?,
        lc_bin: spira_store::lifecycle_row::lc_bin()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config whose resolution shows which home it was handed: an empty `runes_dir` is `<home>/runes`.
    fn home_revealing_toml(dir: &std::path::Path) -> PathBuf {
        spira_config::process::fixture_toml(dir, &[("SPIRA_RUNES_DIR", "")])
    }

    fn handed(pairs: &[(&str, &std::path::Path)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.display().to_string())).collect()
    }

    #[test_id(tcocjqi)]
    #[test]
    fn with_no_spira_home_the_config_resolves_from_the_releases_spira_dir() {
        let d = testkit::TempDir::new("wt-resolve-release");
        let release = d.path().join("release");
        std::fs::create_dir(&release).unwrap();
        std::fs::rename(spira_config::process::fixture_registry_home(d.path()), release.join("spira")).unwrap();
        let decoy = d.path().join("decoy");
        std::fs::create_dir(&decoy).unwrap();
        std::fs::write(decoy.join("lib.sh"), "").unwrap();
        let toml = home_revealing_toml(d.path());

        let resolved = resolve_from(&handed(&[("SPIRA_RELEASE", &release), ("SPIRA_TOML", &toml), ("PATH", &decoy)]));

        assert_eq!(resolved.get("SPIRA_RUNES_DIR"), release.join("spira/runes").to_str().unwrap());
    }

    #[test_id(tzp6ovf)]
    #[test]
    fn with_no_named_home_nothing_is_resolved_even_from_a_good_config() {
        let d = testkit::TempDir::new("wt-resolve-nohome");
        let toml = home_revealing_toml(d.path());

        let resolved = resolve_from(&handed(&[("SPIRA_TOML", &toml)]));

        assert!(resolved.values.is_empty() && resolved.warnings.is_empty(), "{:?}", resolved.values.get("SPIRA_RUNES_DIR"));
    }
}
