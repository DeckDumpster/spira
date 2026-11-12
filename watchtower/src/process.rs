//! The process's config: the toml door for registered keys, the environment then the resolved
//! registry for the env-only ones, and the sweep's `Cfg` built from both. Shared by `main.rs`
//! and by a host (the scheduler's `watch.tower` task) that runs the sweep in-process.

use std::path::PathBuf;
use std::time::Duration;

use crate::{cpu_throttle, deadline, incident, sweep};

/// `std::env::var`, trimmed to "set and non-empty" — no `spira_config` fallback. Used only
/// inside [`resolved_config`]'s own init, which must not call back into [`getenv`] (that
/// would recurse into `resolved_config()` while it is still being built).
pub fn raw_env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

/// Wave 4.8 ("retire conf re-import seams in Rust"): this crate used to read every
/// `SPIRA_*` key straight out of its own process environment, with no snapshot and no
/// the config document load at all (wave4-decomposition.md row (b) names watchtower by file) —
/// so an operator's toml value for, say, `SPIRA_QUEUE_THROTTLE_DEPTH_AT` was silently
/// ignored; only an explicit env override (set by the launching unit) ever took effect.
/// Resolved once, lazily, and cached: `spira_config::resolve_for_process`, using
/// [`spira_home`]'s own derivation for `SPIRA_HOME` and `spira_config::resolve::
/// derive_home_repo` for `SPIRA_REPO`. A resolution failure (unreadable registry,
/// containment refusal) yields an empty [`spira_config::resolve::Resolved`] — [`getenv`]'s
/// own `unwrap_or(default)` callers see exactly the behaviour this crate had before this
/// bead, never a panic.
///
/// ONE SOURCE OF CONFIG (per Ryan 2026-10-05): every key this registry declares
/// (`spira/conf.d/SPIRA_*`) now reads through [`reg`]/[`reg_i64`] instead — the process's
/// single `$SPIRA_TOML` resolution, with no env override and no caller-supplied default.
/// `getenv`/`getenv_i64` (and this cache) remain only for the keys `spira/conf.d` does not
/// declare at all — listed in the migration report, never both ways for the same key.
pub fn resolved_config() -> &'static spira_config::resolve::Resolved {
    static RESOLVED: std::sync::OnceLock<spira_config::resolve::Resolved> = std::sync::OnceLock::new();
    RESOLVED.get_or_init(|| {
        let env_map: std::collections::BTreeMap<String, String> = std::env::vars().collect();
        let home = PathBuf::from(raw_env("SPIRA_HOME").unwrap_or_else(lib_sh_dir));
        let repo = spira_config::resolve::derive_home_repo(&home, &env_map);
        spira_config::resolve::resolve_or_say("watchtower", &home, &repo, &env_map)
    })
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

/// Where `lib.sh` itself lives, found on `$PATH` — NEVER `$SPIRA_HOME`. The bash always
/// had lib.sh's functions available by sourcing it relative to `$0`
/// (`. "$(dirname "$0")/lib.sh"`) at the top of the script, regardless of what `SPIRA_HOME`
/// was set to; `SPIRA_HOME` only steers what lib.sh's OWN functions read afterwards (e.g.
/// `bulk_ready_by_rune`'s runes lookup). A compiled binary has no `$0` directory to be
/// relative to, so this is the equivalent self-location — used by every seam that needs
/// lib.sh's CODE (`git::spira_landref`, `seams::registry`, `seams::pipeline_probe`).
/// Conflating this with `$SPIRA_HOME` was a real bug (sp-lnmbq): a suite that points
/// `SPIRA_HOME` at a fixture directory holding only a `runes/` and no `lib.sh` at all
/// (test-watchtower.sh's idle-while-ready section) made every one of those seams source
/// nothing and fail closed, even though the fixture never intended to replace lib.sh
/// itself — only what its ALREADY-real functions read. `SPIRA_HOME` still reaches those
/// functions correctly: it is inherited in the subprocess's own environment, never passed
/// as the sourcing path.
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
    Ok(sweep::Cfg {
        spira_run: PathBuf::from(reg("SPIRA_RUN")?),
        lib_sh_dir: lib_sh_dir(),
        db: reg("SPIRA_DB")?,
        home_repo: reg("SPIRA_HOME_REPO")?,
        ask_label: reg("SPIRA_ASK_LABEL")?,
        snap_stale_s: reg_i64("SPIRA_SNAP_STALE_S")?,
        gate_window_s: getenv_i64("SPIRA_WATCH_GATE_WINDOW", 21600),
        gate_silence_window_s: getenv_i64("SPIRA_WATCH_GATE_SILENCE_WINDOW", 3600),
        gate_p90_limit_s: getenv_i64("SPIRA_WATCH_GATE_P90_LIMIT", 300),
        gate_log: getenv("SPIRA_GATE_LOG").map(PathBuf::from),
        yield_window_s: getenv_i64("SPIRA_YIELD_WINDOW", 86400),
        yield_sh: getenv("SPIRA_YIELD_SH"),
        disk_warn_pct: getenv_i64("SPIRA_DISK_WARN_PCT", 90),
        mem_warn_mb: getenv_i64("SPIRA_MEM_WARN_MB", 1500),
        meminfo_path: getenv("SPIRA_MEMINFO_PATH").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/proc/meminfo")),
        failed_units_warn_mins: getenv_i64("SPIRA_FAILED_UNITS_WARN_MINS", 15),
        cpu_throttle_units: getenv("SPIRA_CPU_THROTTLE_UNITS").unwrap_or_else(|| cpu_throttle::DEFAULT_UNITS.to_string()).split_whitespace().map(String::from).collect(),
        cpu_throttle_warn_pct: getenv_i64("SPIRA_CPU_THROTTLE_WARN_PCT", 5),
        cpu_throttle_min_periods: getenv_i64("SPIRA_CPU_THROTTLE_MIN_PERIODS", 100),
        cgroup_root: getenv("SPIRA_CGROUP_ROOT").unwrap_or_else(|| "/sys/fs/cgroup".to_string()),
        unsent_warn_h: getenv_i64("SPIRA_UNSENT_WARN_H", 24),
        closed_stranded_warn_h: getenv_i64("SPIRA_CLOSED_STRANDED_WARN_H", 48),
        drain_warn_mins: getenv_i64("SPIRA_DRAIN_WARN_MINS", 15),
        idle_while_ready_n: getenv_i64("SPIRA_IDLE_WHILE_READY_N", 5).max(0) as usize,
        systemctl: getenv("SPIRA_SYSTEMCTL").unwrap_or_else(|| "systemctl".to_string()),
        units: runtime::units::host(getenv("SPIRA_SYSTEMCTL").as_deref()),
        prompt_file: getenv("SPIRA_WATCH_PROMPT_FILE").map(PathBuf::from),
        lapsed_dir: getenv("SPIRA_LAPSED_DIR").map(PathBuf::from),
        lapsed_marker: getenv("SPIRA_LAPSED_MARKER").map(PathBuf::from),
        failed_units_state: getenv("SPIRA_FAILED_UNITS_STATE").map(PathBuf::from),
        throttle_stamp: getenv("SPIRA_THROTTLE_STAMP").map(PathBuf::from),
        queue_throttle_override: reg("SPIRA_QUEUE_THROTTLE_OVERRIDE")?,
        incident_sh: resolved_incident_sh(),
        suites_sh: getenv("SPIRA_SUITES_SH"),
        cockpit_db: spira_config::process::cfg("COCKPIT_DB").ok().filter(|d| !d.is_empty()),
        moot_timeout_s: getenv_i64("SPIRA_MOOT_TIMEOUT", 30).max(1) as u64,
        resolve_bin: match getenv("SPIRA_RESOLVE_SH") {
            Some(r) => r,
            None => spira_config::release_env::tool("resolve")?,
        },
        lc: spira_store::lc_call::LcClient::configured()?,
        branch_guard_sh: getenv("SPIRA_BRANCH_GUARD_SH"),
        lc_bin: spira_store::lifecycle_row::lc_bin()?,
    })
}
