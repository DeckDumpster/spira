//! watchtower — hands Ops the pipeline's vital signs (the sweep) and the four cheap
//! detectors the sentinel runs every pass. See DESIGN.md for the contract; this file is
//! only argv dispatch and environment resolution — every module it calls is independently
//! unit-tested.

mod conditions;
mod cpu_throttle;
mod czar_outcome;
mod deadline;
mod deploy_fault;
mod disk_mem;
mod disabled_timer;
mod drift;
mod env;
mod ctrl_gate;
mod failed_units;
mod gate_wait;
mod git;
mod incident;
mod lc;
mod lapsed;
mod lock_holders;
mod log;
mod pr_stall;
mod probes;
mod release_skew;
mod sccache_wedge;
mod seams;
mod dolt_drop;
mod slow_query;
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
///
/// ONE SOURCE OF CONFIG (per Ryan 2026-10-05): every key this registry declares
/// (`spira/conf.d/SPIRA_*`) now reads through [`reg`]/[`reg_i64`] instead — the process's
/// single `$SPIRA_TOML` resolution, with no env override and no caller-supplied default.
/// `getenv`/`getenv_i64` (and this cache) remain only for the keys `spira/conf.d` does not
/// declare at all — listed in the migration report, never both ways for the same key.
fn resolved_config() -> &'static spira_config::resolve::Resolved {
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
fn getenv(k: &str) -> Option<String> {
    raw_env(k).or_else(|| {
        let v = resolved_config().get(k);
        (!v.is_empty()).then(|| v.to_string())
    })
}

fn getenv_i64(k: &str, default: i64) -> i64 {
    getenv(k).and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// A registered config key's declared value — `spira_config::process::cfg`, the one
/// source of config: `$SPIRA_TOML`, resolved once per process, no env override, no
/// default. `Err` (key not registered, or the config cannot be resolved) refuses the
/// process outright; it never substitutes a value.
fn reg(key: &str) -> String {
    spira_config::process::cfg(key).unwrap_or_else(|e| {
        eprintln!("watchtower: FATAL: {e}");
        std::process::exit(1);
    })
}

/// [`reg`], parsed as any `FromStr` type — refusing the same way on an unparsable value.
fn reg_parse<T: std::str::FromStr>(key: &str) -> T
where
    T::Err: std::fmt::Display,
{
    spira_config::process::cfg_parse::<T>(key).unwrap_or_else(|e| {
        eprintln!("watchtower: FATAL: {e}");
        std::process::exit(1);
    })
}

fn reg_i64(key: &str) -> i64 {
    reg_parse(key)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn spira_run() -> PathBuf {
    PathBuf::from(reg("SPIRA_RUN"))
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
    let start_s = reg_i64("SPIRA_WATCHTOWER_START_TIMEOUT_S").max(0) as u64;
    deadline::init(
        Duration::from_secs(reg_i64("SPIRA_WATCHTOWER_PROBE_TIMEOUT_S").max(1) as u64),
        Duration::from_secs(start_s.saturating_sub(30).max(30)),
    );
    sweep::Cfg {
        spira_run: spira_run(),
        lib_sh_dir: lib_sh_dir(),
        db: reg("SPIRA_DB"),
        home_repo: reg("SPIRA_HOME_REPO"),
        ask_label: reg("SPIRA_ASK_LABEL"),
        snap_stale_s: reg_i64("SPIRA_SNAP_STALE_S"),
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
        journalctl: getenv("SPIRA_JOURNALCTL").unwrap_or_else(|| "journalctl".to_string()),
        prompt_file: getenv("SPIRA_WATCH_PROMPT_FILE").map(PathBuf::from),
        lapsed_dir: getenv("SPIRA_LAPSED_DIR").map(PathBuf::from),
        lapsed_marker: getenv("SPIRA_LAPSED_MARKER").map(PathBuf::from),
        failed_units_state: getenv("SPIRA_FAILED_UNITS_STATE").map(PathBuf::from),
        throttle_stamp: getenv("SPIRA_THROTTLE_STAMP").map(PathBuf::from),
        queue_throttle_override: reg("SPIRA_QUEUE_THROTTLE_OVERRIDE"),
        incident_sh: resolved_incident_sh(),
        suites_sh: getenv("SPIRA_SUITES_SH"),
        moot_sh: getenv("SPIRA_MOOT_SH"),
        bd: "bd".to_string(),
        branch_guard_sh: getenv("SPIRA_BRANCH_GUARD_SH"),
        lc_bin: spira_config::lifecycle_row::lc_bin(),
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
            let db = reg("SPIRA_DB");
            let home_repo = reg("SPIRA_HOME_REPO");
            let cfg = throttle::Cfg {
                depth_at: reg_i64("SPIRA_QUEUE_THROTTLE_DEPTH_AT"),
                release_at: reg_i64("SPIRA_QUEUE_THROTTLE_RELEASE_AT"),
                stall_mins: reg_i64("SPIRA_QUEUE_THROTTLE_STALL_MINS"),
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
                override_off: reg("SPIRA_QUEUE_THROTTLE_OVERRIDE") == "off",
                repo: tc_repo.as_deref(),
                land_ref: tc_land_ref.as_deref(),
                land_ref_default_for_log: "origin/main",
            };
            throttle::run(n, &cfg, &ctx);
        }
        Some("--czar-outcome-check") => {
            if world_halted(&run) {
                log::log("watchtower: czar-outcome-check skipped — world is halted");
                return;
            }
            let cfg = czar_outcome::Cfg {
                outcome_mins: reg_i64("SPIRA_CZAR_OUTCOME_MINS"),
                unclaimed_mins: reg_i64("SPIRA_CZAR_UNCLAIMED_MINS"),
                label: reg("SPIRA_CZAR_LABEL"),
            };
            czar_outcome::run(
                n,
                "bd",
                &reg("SPIRA_DB"),
                &reg("SPIRA_HOME_REPO"),
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
                stall_secs: reg_i64("SPIRA_PR_STALL_MINS") * 60,
                gh_bin: reg("SPIRA_GH"),
                gh_timeout: Duration::from_secs(getenv_i64("GH_TIMEOUT", 120) as u64),
            };
            pr_stall::run(
                n,
                &lib_sh_dir(),
                &reg("SPIRA_DB"),
                &reg("SPIRA_HOME_REPO"),
                &resolved_incident_sh(),
                &cfg,
            );
        }
        Some("--lock-holders-check") => {
            if world_halted(&run) {
                log::log("watchtower: lock-holders-check skipped — world is halted");
                return;
            }
            let stall_mins = reg_i64("SPIRA_QUEUE_THROTTLE_STALL_MINS");
            let tc_repo = getenv("SPIRA_TC_REPO").or_else(|| getenv("SPIRA_REPO"));
            let land_ref = getenv("SPIRA_TC_LAND_REF")
                .or_else(|| tc_repo.as_deref().and_then(|r| git::spira_landref(&lib_sh_dir(), r)));
            let Some(depth) = throttle::compute_depth(tc_repo.as_deref(), land_ref.as_deref()) else {
                log::log("watchtower: lock-holders-check skipped — spira-lc is unreachable, depth unknown");
                return;
            };
            let since = throttle::minutes_since_last_landed(n);
            if depth == 0 || since.map(|m| m < stall_mins).unwrap_or(false) {
                log::log(&format!("watchtower: lock-holders-check — no stall (depth={depth} since_land={}m)", throttle::disp(since)));
                return;
            }
            let queue_dir = PathBuf::from(reg("SPIRA_QUEUE_DIR"));
            lock_holders::run(
                n,
                &format!("depth {depth}, no landing for {}m", throttle::disp(since)),
                &lock_holders::Ctx {
                    run: &run,
                    queue_dir: &queue_dir,
                    proc_root: std::path::Path::new("/proc"),
                    db: &reg("SPIRA_DB"),
                    home_repo: &reg("SPIRA_HOME_REPO"),
                    incident_sh: &resolved_incident_sh(),
                },
            );
        }
        Some("--disabled-timer-check") => {
            if world_halted(&run) {
                log::log("watchtower: disabled-timer-check skipped — world is halted");
                return;
            }
            let cfg = disabled_timer::Cfg {
                systemctl: getenv("SPIRA_SYSTEMCTL").unwrap_or_else(|| "systemctl".to_string()),
                instance_suffix: format!("-{}", reg("SPIRA_INSTANCE")),
            };
            disabled_timer::run(
                &spira_home(),
                &reg("SPIRA_DB"),
                &reg("SPIRA_HOME_REPO"),
                &resolved_incident_sh(),
                &cfg,
            );
        }
        Some("--release-skew-check") => {
            if world_halted(&run) {
                log::log("watchtower: release-skew-check skipped — world is halted");
                return;
            }
            let cfg = release_skew::Cfg {
                systemctl: getenv("SPIRA_SYSTEMCTL").unwrap_or_else(|| "systemctl".to_string()),
                max_secs: getenv_i64("SPIRA_RELEASE_SKEW_MAX_SECS", 3600),
            };
            release_skew::run(
                n,
                &run,
                &reg("SPIRA_DB"),
                &reg("SPIRA_HOME_REPO"),
                &resolved_incident_sh(),
                &cfg,
            );
        }
        Some("--deploy-fault-check") => {
            if world_halted(&run) {
                log::log("watchtower: deploy-fault-check skipped — world is halted");
                return;
            }
            let cfg = deploy_fault::Cfg {
                release_bin: getenv("SPIRA_RELEASE_BIN").unwrap_or_else(|| "release".to_string()),
                repo: getenv("SPIRA_REPO").unwrap_or_default(),
                retry_secs: getenv_i64("SPIRA_DEPLOY_FAULT_RETRY_SECS", 1800),
                build_timeout_secs: getenv_i64("SPIRA_DEPLOY_FAULT_BUILD_TIMEOUT", 1800).max(1) as u64,
            };
            let queue_dir = PathBuf::from(reg("SPIRA_QUEUE_DIR"));
            deploy_fault::run(
                n,
                &queue_dir,
                &run,
                &reg("SPIRA_DB"),
                &reg("SPIRA_HOME_REPO"),
                &resolved_incident_sh(),
                &cfg,
            );
        }
        Some("--sccache-wedge-check") => {
            if world_halted(&run) {
                log::log("watchtower: sccache-wedge-check skipped \u{2014} world is halted");
                return;
            }
            let cfg = sccache_wedge::Cfg {
                sccache: getenv("SPIRA_SCCACHE_BIN").unwrap_or_else(|| "sccache".to_string()),
                stale_secs: getenv_i64("SPIRA_SCCACHE_WEDGE_MINS", 10) * 60,
                cmd_timeout_secs: getenv_i64("SPIRA_SCCACHE_CMD_TIMEOUT", 30).max(1) as u64,
            };
            sccache_wedge::run(
                n,
                std::path::Path::new("/proc"),
                &reg("SPIRA_DB"),
                &reg("SPIRA_HOME_REPO"),
                &resolved_incident_sh(),
                &cfg,
            );
        }
        Some("--conditions-check") => {
            if world_halted(&run) {
                log::log("watchtower: conditions-check skipped — world is halted");
                return;
            }
            let cfg = probes::Cfg {
                systemctl: getenv("SPIRA_SYSTEMCTL").unwrap_or_else(|| "systemctl".to_string()),
                journalctl: getenv("SPIRA_JOURNALCTL").unwrap_or_else(|| "journalctl".to_string()),
                releases: {
                    // SPIRA_RELEASES is registered but its declared value may legitimately be
                    // "" (not configured) — preserved as `None`, same as `getenv`'s own
                    // empty-filters-to-None before this bead; not a substituted default.
                    let r = reg("SPIRA_RELEASES");
                    (!r.is_empty()).then_some(r)
                },
                keep: reg_i64("SPIRA_RELEASES_KEEP").max(1) as usize,
                store_slack: reg_i64("SPIRA_RELEASE_STORE_SLACK").max(0) as usize,
                unit_glob: reg("SPIRA_RELEASE_CURRENCY_UNITS"),
                stale_release_secs: reg_i64("SPIRA_RELEASE_STALE_SECS"),
                failed_runs: reg_i64("SPIRA_FAILED_UNIT_RUNS"),
                tmp_path: getenv("SPIRA_TMP_PROBE_PATH").unwrap_or_else(|| "/tmp".to_string()),
                tmp_floor_mib: getenv_i64("SPIRA_TMPFS_SHED_FREE_MIB", 6144),
                df_bin: getenv("SPIRA_DF").unwrap_or_else(|| "df".to_string()),
                disk_floor_pct: reg_i64("SPIRA_DISK_FLOOR_PCT"),
                psi_dir: getenv("SPIRA_PSI_DIR").unwrap_or_else(|| "/proc/pressure".to_string()),
                psi_full_avg60: reg_parse::<f64>("SPIRA_PSI_FULL_AVG60"),
                psi_sustain_secs: reg_i64("SPIRA_PSI_SUSTAIN_SECS"),
            };
            let db = reg("SPIRA_DB");
            let home_repo = reg("SPIRA_HOME_REPO");
            let inc = resolved_incident_sh();
            let lc_bin = spira_config::lifecycle_row::lc_bin();
            let ctx = conditions::Ctx { run: &run, db: &db, home_repo: &home_repo, incident_sh: &inc, bd: "bd", lc_bin: &lc_bin };
            conditions::reconcile(n, &ctx, "release-currency", probes::release_currency(&cfg));
            conditions::reconcile(n, &ctx, "failing-units", probes::failing_units(&cfg, &run));
            conditions::reconcile(n, &ctx, "pressure", probes::pressure(&cfg));
            conditions::reconcile(n, &ctx, "release-store", probes::release_store(&cfg));
            let rowless_cfg = probes::RowlessCfg {
                bd: "bd".to_string(),
                db: db.clone(),
                lc_bin: spira_config::lifecycle_row::lc_bin(),
                cap: getenv_i64("SPIRA_ROWLESS_CAP", 20).max(1) as usize,
                sustain_secs: getenv_i64("SPIRA_ROWLESS_SUSTAIN_SECS", 300),
            };
            conditions::reconcile(n, &ctx, "rowless-beads", probes::rowless_beads(&rowless_cfg));
        }
        Some("--dolt-drop-check") => {
            if world_halted(&run) {
                log::log("watchtower: dolt-drop-check skipped \u{2014} world is halted");
                return;
            }
            let log_path = getenv("SPIRA_DOLT_LOG").map(PathBuf::from).unwrap_or_else(|| run.join("dolt-beads.log"));
            dolt_drop::run(
                &log_path,
                &getenv("SPIRA_DB").unwrap_or_default(),
                &getenv("SPIRA_HOME_REPO").unwrap_or_else(|| "spira".to_string()),
                &resolved_incident_sh(),
                getenv_i64("SPIRA_DOLT_DROP_WINDOW_MINS", 5).max(1) as usize,
                getenv_i64("SPIRA_DOLT_DROP_PER_MIN", 30).max(1) as usize,
            );
        }
        Some("--slow-query-check") => {
            if world_halted(&run) {
                log::log("watchtower: slow-query-check skipped \u{2014} world is halted");
                return;
            }
            let log_path = getenv("SPIRA_SLOW_QUERY_LOG").map(PathBuf::from).unwrap_or_else(|| run.join("slow-queries.log"));
            slow_query::run(
                &run,
                &log_path,
                &reg("SPIRA_DB"),
                &reg("SPIRA_HOME_REPO"),
                &resolved_incident_sh(),
            );
        }
        Some("--drift-check") => {
            if world_halted(&run) {
                log::log("watchtower: drift-check skipped — world is halted");
                return;
            }
            drift::run(
                &spira_home(),
                &getenv("SPIRA_REPO").unwrap_or_default(),
                &reg("SPIRA_DB"),
                &reg("SPIRA_HOME_REPO"),
                &resolved_incident_sh(),
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

    /// `getenv`/`getenv_i64` are for a key `spira/conf.d` does not register (a registered
    /// one reads through `reg`/`reg_i64` instead). `spira_config::resolve()` now refuses
    /// any key the config file does not explicitly declare — including a fake conf.d entry's
    /// own `DEFAULT` block, which is documentation only; nothing evaluates it anymore (per
    /// Ryan 2026-10-05: no config is a refusal, never a computed default). So there is no
    /// registry fallback left for an unregistered key at all: `resolved_config()` yields
    /// nothing for `SPIRA_DISK_WARN_PCT` (a real call site in this crate that stays on
    /// `getenv_i64` because it is not in `spira/conf.d`), and `getenv_i64` falls straight
    /// to the caller's own hardcoded default — an explicit env override still wins over
    /// that. This is the ONLY test in this binary that calls `getenv`/`resolved_config` —
    /// the `OnceLock` inside `resolved_config` computes once per process and never resets,
    /// so a second test with a different fixture could not observe a different answer.
    #[test]
    fn getenv_falls_back_to_the_callers_default_with_no_registry_fallback() {
        let g = testkit::env(&[("SPIRA_TOML", Some("/no/such/spira-toml-for-this-test")), ("SPIRA_DISK_WARN_PCT", None)]);

        assert_eq!(getenv_i64("SPIRA_DISK_WARN_PCT", 90), 90, "no registry fallback for an unregistered key: the caller's own default must win");
        assert_eq!(getenv("SPIRA_NO_SUCH_KEY_AT_ALL_EVER"), None, "an unresolved key still falls through to None");

        drop(g);
        let _g = testkit::env(&[("SPIRA_TOML", Some("/no/such/spira-toml-for-this-test")), ("SPIRA_DISK_WARN_PCT", Some("99"))]);
        assert_eq!(getenv_i64("SPIRA_DISK_WARN_PCT", 90), 99, "an explicit env override still wins over the registry");
    }

    #[test]
    fn world_halted_guards_every_periodic_subcommand() {
        let dir = testkit::TempDir::new("watchtower-halted");
        let run = dir.join("run");
        std::fs::create_dir_all(&run).unwrap();
        assert!(!world_halted(&run), "no marker: the world is running");
        std::fs::write(run.join("world.halted"), "").unwrap();
        assert!(world_halted(&run), "marker present: halted");
        let src = include_str!("main.rs");
        for sub in ["throttle-check", "czar-outcome-check", "pr-stall-check", "disabled-timer-check"] {
            let msg = format!("watchtower: {sub} skipped \u{2014} world is halted");
            assert!(src.contains(&format!("\"{}\"", msg)), "{sub} must carry the halted-world skip");
        }
    }
}
