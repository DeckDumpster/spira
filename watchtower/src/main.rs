//! watchtower — hands Ops the pipeline's vital signs (the sweep) and the four cheap
//! detectors the sentinel runs every pass. See DESIGN.md for the contract; this file is
//! only argv dispatch and environment resolution — every module it calls is independently
//! unit-tested.

mod cpu_throttle;
mod czar_outcome;
mod disk_mem;
mod disabled_timer;
mod env;
mod failed_units;
mod gate_wait;
mod git;
mod incident;
mod landstate;
mod lapsed;
mod log;
mod pr_stall;
mod seams;
mod sweep;
mod throttle;

use std::path::PathBuf;
use std::time::Duration;

/// `std::env::var`, trimmed to "set and non-empty" — no `spira_config` fallback. Used only
/// inside [`resolved_config`]'s own init, which must not call back into [`getenv`] (that
/// would recurse into `resolved_config()` while it is still being built).
fn raw_env(k: &str) -> Option<String> {
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
fn resolved_config() -> &'static spira_config::resolve::Resolved {
    static RESOLVED: std::sync::OnceLock<spira_config::resolve::Resolved> = std::sync::OnceLock::new();
    RESOLVED.get_or_init(|| {
        let env_map: std::collections::BTreeMap<String, String> = std::env::vars().collect();
        let home = PathBuf::from(raw_env("SPIRA_HOME").unwrap_or_else(lib_sh_dir));
        let repo = spira_config::resolve::derive_home_repo(&home, &env_map);
        spira_config::resolve::resolve_for_process(&home, &repo, &env_map).unwrap_or_default()
    })
}

/// The environment, then `spira_config::resolve()`'s in-process answer — never the
/// reverse, so a test or a fixture's explicit env override still wins exactly as it did
/// before this bead.
fn getenv(k: &str) -> Option<String> {
    raw_env(k).or_else(|| {
        let v = resolved_config().get(k);
        (!v.is_empty()).then(|| v.to_string())
    })
}

fn getenv_i64(k: &str, default: i64) -> i64 {
    getenv(k).and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn spira_run() -> PathBuf {
    getenv("SPIRA_RUN").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp/spira-run"))
}

/// `$SPIRA_HOME`, falling back to `lib.sh`'s own directory on PATH. Used only for
/// `--disabled-timer-check`'s `world.sh`/`ctrl.sh` seam, which the bash reached the same
/// way (`. "${SPIRA_HOME}/world.sh"` — a literal `$SPIRA_HOME`, never `$0`-relative): a
/// test that overrides `SPIRA_HOME` means that seam too, and production always sets it
/// anyway. The fallback only matters for a suite run in a clean `env -i` that puts
/// `spira/` on PATH without setting `SPIRA_HOME` at all.
fn spira_home() -> String {
    if let Some(h) = getenv("SPIRA_HOME") {
        return h;
    }
    lib_sh_dir()
}

/// Where `lib.sh` itself lives, found on `$PATH` — NEVER `$SPIRA_HOME`. The bash always
/// had lib.sh's functions available by sourcing it relative to `$0`
/// (`. "$(dirname "$0")/lib.sh"`) at the top of the script, regardless of what `SPIRA_HOME`
/// was set to; `SPIRA_HOME` only steers what lib.sh's OWN functions read afterwards (e.g.
/// `bulk_ready_by_fayth`'s chamber lookup). A compiled binary has no `$0` directory to be
/// relative to, so this is the equivalent self-location — used by every seam that needs
/// lib.sh's CODE (`git::spira_landref`, `seams::registry`, `seams::pipeline_probe`).
/// Conflating this with `$SPIRA_HOME` was a real bug (sp-lnmbq): a suite that points
/// `SPIRA_HOME` at a fixture directory holding only a `chamber/` and no `lib.sh` at all
/// (test-watchtower.sh's idle-while-ready section) made every one of those seams source
/// nothing and fail closed, even though the fixture never intended to replace lib.sh
/// itself — only what its ALREADY-real functions read. `SPIRA_HOME` still reaches those
/// functions correctly: it is inherited in the subprocess's own environment, never passed
/// as the sourcing path.
fn lib_sh_dir() -> String {
    incident::which("lib.sh")
        .and_then(|p| std::path::Path::new(&p).parent().map(|d| d.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

fn resolved_incident_sh() -> String {
    incident::resolve(getenv("SPIRA_INCIDENT_SH")).unwrap_or_else(|| "incident.sh".to_string())
}

fn world_halted(run: &std::path::Path) -> bool {
    run.join("world.halted").is_file()
}

fn build_sweep_cfg() -> sweep::Cfg {
    sweep::Cfg {
        spira_run: spira_run(),
        lib_sh_dir: lib_sh_dir(),
        db: getenv("SPIRA_DB").unwrap_or_default(),
        home_repo: getenv("SPIRA_HOME_REPO").unwrap_or_else(|| "spira".to_string()),
        ask_label: getenv("SPIRA_ASK_LABEL").unwrap_or_else(|| "needs-operator".to_string()), // literal-ok: mirrors conf.sh's own derived default (this binary cannot source schema.sh)
        snap_stale_s: getenv_i64("SPIRA_SNAP_STALE_S", 60),
        gate_window_s: getenv_i64("SPIRA_WATCH_GATE_WINDOW", 21600),
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
        journalctl: getenv("SPIRA_JOURNALCTL").unwrap_or_else(|| "journalctl".to_string()),
        prompt_file: getenv("SPIRA_WATCH_PROMPT_FILE").map(PathBuf::from),
        lapsed_dir: getenv("SPIRA_LAPSED_DIR").map(PathBuf::from),
        lapsed_marker: getenv("SPIRA_LAPSED_MARKER").map(PathBuf::from),
        failed_units_state: getenv("SPIRA_FAILED_UNITS_STATE").map(PathBuf::from),
        throttle_stamp: getenv("SPIRA_THROTTLE_STAMP").map(PathBuf::from),
        queue_throttle_override: getenv("SPIRA_QUEUE_THROTTLE_OVERRIDE").unwrap_or_default(),
        incident_sh: resolved_incident_sh(),
        suites_sh: getenv("SPIRA_SUITES_SH"),
        moot_sh: getenv("SPIRA_MOOT_SH"),
        bd: "bd".to_string(),
        branch_guard_sh: getenv("SPIRA_BRANCH_GUARD_SH"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(|s| s.as_str());
    let run = spira_run();
    let n = now();

    match sub {
        None => sweep::run_sweep(n, &build_sweep_cfg()),
        Some("--show") => sweep::run_show(n, &build_sweep_cfg()),
        Some("--throttle-check") => {
            if world_halted(&run) {
                log::log("watchtower: throttle-check skipped — world is halted");
                return;
            }
            let inc = resolved_incident_sh();
            let db = getenv("SPIRA_DB").unwrap_or_default();
            let home_repo = getenv("SPIRA_HOME_REPO").unwrap_or_else(|| "spira".to_string());
            let cfg = throttle::Cfg {
                depth_at: getenv_i64("SPIRA_QUEUE_THROTTLE_DEPTH_AT", 16),
                release_at: getenv_i64("SPIRA_QUEUE_THROTTLE_RELEASE_AT", 8),
                stall_mins: getenv_i64("SPIRA_QUEUE_THROTTLE_STALL_MINS", 50),
            };
            let stamp = getenv("SPIRA_THROTTLE_STAMP").map(PathBuf::from).unwrap_or_else(|| run.join("queue-throttled"));
            let tc_repo = getenv("SPIRA_TC_REPO").or_else(|| getenv("SPIRA_REPO"));
            let mut tc_land_ref = getenv("SPIRA_TC_LAND_REF");
            if tc_land_ref.is_none() {
                if let Some(repo) = &tc_repo {
                    tc_land_ref = git::spira_landref(&lib_sh_dir(), repo);
                }
            }
            let ctx = throttle::Ctx {
                db: &db,
                home_repo: &home_repo,
                incident_sh: &inc,
                stamp: &stamp,
                override_off: getenv("SPIRA_QUEUE_THROTTLE_OVERRIDE").as_deref() == Some("off"),
                repo: tc_repo.as_deref(),
                land_ref: tc_land_ref.as_deref(),
                land_ref_default_for_log: "origin/main",
            };
            throttle::run(n, &run.join("landstate"), &cfg, &ctx);
        }
        Some("--czar-outcome-check") => {
            if world_halted(&run) {
                log::log("watchtower: czar-outcome-check skipped — world is halted");
                return;
            }
            let cfg = czar_outcome::Cfg {
                outcome_mins: getenv_i64("SPIRA_CZAR_OUTCOME_MINS", 30),
                unclaimed_mins: getenv_i64("SPIRA_CZAR_UNCLAIMED_MINS", 10),
                label: getenv("SPIRA_CZAR_LABEL").unwrap_or_else(|| "czar-trigger".to_string()),
            };
            czar_outcome::run(
                n,
                "bd",
                &getenv("SPIRA_DB").unwrap_or_default(),
                &getenv("SPIRA_HOME_REPO").unwrap_or_else(|| "spira".to_string()),
                &resolved_incident_sh(),
                &cfg,
            );
        }
        Some("--pr-stall-check") => {
            if world_halted(&run) {
                log::log("watchtower: pr-stall-check skipped — world is halted");
                return;
            }
            let cfg = pr_stall::Cfg {
                stall_secs: getenv_i64("SPIRA_PR_STALL_MINS", 60) * 60,
                gh_bin: getenv("SPIRA_GH").unwrap_or_else(|| "gh".to_string()),
                gh_timeout: Duration::from_secs(getenv_i64("GH_TIMEOUT", 120) as u64),
            };
            pr_stall::run(
                n,
                &run.join("landstate"),
                &lib_sh_dir(),
                &getenv("SPIRA_DB").unwrap_or_default(),
                &getenv("SPIRA_HOME_REPO").unwrap_or_else(|| "spira".to_string()),
                &resolved_incident_sh(),
                &cfg,
            );
        }
        Some("--disabled-timer-check") => {
            if world_halted(&run) {
                log::log("watchtower: disabled-timer-check skipped — world is halted");
                return;
            }
            let cfg = disabled_timer::Cfg {
                systemctl: getenv("SPIRA_SYSTEMCTL").unwrap_or_else(|| "systemctl".to_string()),
                instance_suffix: getenv("SPIRA_INSTANCE").map(|i| format!("-{i}")).unwrap_or_default(),
            };
            disabled_timer::run(
                &spira_home(),
                &getenv("SPIRA_DB").unwrap_or_default(),
                &getenv("SPIRA_HOME_REPO").unwrap_or_else(|| "spira".to_string()),
                &resolved_incident_sh(),
                &cfg,
            );
        }
        Some(other) => {
            eprintln!("watchtower: unknown argument: {other}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wave 4.8: `getenv`/`getenv_i64` now fall back to `spira_config::resolve()` between
    /// the raw environment and the caller's own hardcoded default. This is the ONLY test
    /// in this binary that calls `getenv`/`resolved_config` — the `OnceLock` inside
    /// `resolved_config` computes once per process and never resets, so a second test with
    /// a different fixture could not observe a different answer. SPIRA_HOME points at a
    /// throwaway fixture with its own `conf.d` (never the real box's), so this never reads
    /// an operator's actual config document or registry.
    #[test]
    fn getenv_falls_back_to_the_registry_then_the_callers_default() {
        let dir = testkit::TempDir::new("watchtower-getenv");
        let home = dir.join("spira");
        std::fs::create_dir_all(home.join("conf.d")).unwrap();
        std::fs::write(
            home.join("conf.d/SPIRA_SNAP_STALE_S"),
            "TYPE=u32\nGROUP=cockpit\nDOC=test\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${SPIRA_SNAP_STALE_S:=77}\"\nSPIRA_CONF_DEFAULT_EOF\n",
        )
        .unwrap();
        std::env::set_var("SPIRA_HOME", &home);
        std::env::set_var("SPIRA_TOML", dir.join("no-such-config.toml"));
        std::env::remove_var("SPIRA_SNAP_STALE_S");

        assert_eq!(getenv_i64("SPIRA_SNAP_STALE_S", 60), 77, "a registry default must reach getenv_i64 without an env override");
        assert_eq!(getenv("SPIRA_NO_SUCH_KEY_AT_ALL_EVER"), None, "an unresolved key still falls through to None");

        std::env::set_var("SPIRA_SNAP_STALE_S", "99");
        assert_eq!(getenv_i64("SPIRA_SNAP_STALE_S", 60), 99, "an explicit env override still wins over the registry");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
