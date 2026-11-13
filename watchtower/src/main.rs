//! watchtower — hands Ops the pipeline's vital signs (the sweep) and the four cheap
//! detectors the sentinel runs every pass. See DESIGN.md for the contract; this file is
//! only argv dispatch and environment resolution — every module it calls is independently
//! unit-tested.

use watchtower::process::{self, getenv, getenv_i64, lib_sh_dir, resolved_incident_sh};
use watchtower::{host_real, conditions, czar_outcome, deploy_fault, drift, git, load_fence, lock_holders, log, pr_stall, probes, release_skew, sccache_wedge, dolt_drop, slow_query, sweep, throttle};

use std::path::PathBuf;
use std::time::Duration;

/// A release tool this pass cannot run without, or exit 2 naming the refusal.
/// The store's client on the declared route; refuses the run when none resolves.
fn required_client() -> spira_store::lc_call::LcClient {
    spira_store::lc_call::LcClient::configured().unwrap_or_else(|e| {
        log::log(&format!("watchtower: {e}"));
        std::process::exit(2)
    })
}

fn required(tool: Result<String, String>) -> String {
    tool.unwrap_or_else(|e| {
        log::log(&format!("watchtower: {e}"));
        std::process::exit(2)
    })
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

/// `$SPIRA_HOME`, falling back to `lib.sh`'s own directory on PATH.
fn spira_home() -> String {
    if let Some(h) = getenv("SPIRA_HOME") {
        return h;
    }
    lib_sh_dir()
}

fn world_halted(run: &std::path::Path) -> bool {
    run.join("world.halted").is_file()
}

/// `process::sweep_cfg`, refusing the process on a key that does not resolve.
fn build_sweep_cfg() -> sweep::Cfg {
    process::sweep_cfg().unwrap_or_else(|e| {
        eprintln!("watchtower: FATAL: {e}");
        std::process::exit(1);
    })
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
                &required_client(),
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
        Some("--release-skew-check") => {
            if world_halted(&run) {
                log::log("watchtower: release-skew-check skipped — world is halted");
                return;
            }
            let cfg = release_skew::Cfg {
                systemctl: getenv("SPIRA_SYSTEMCTL").unwrap_or_else(|| "systemctl".to_string()),
                max_secs: getenv_i64("SPIRA_RELEASE_SKEW_MAX_SECS", 3600),
                dwarf_max_secs: getenv_i64("SPIRA_RELEASE_SKEW_DWARF_MAX_SECS", 14400),
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
                release_bin: getenv("SPIRA_RELEASE_BIN").unwrap_or_else(|| required(spira_config::release_env::tool("release"))),
                repo: getenv("SPIRA_REPO").unwrap_or_default(),
                retry_secs: getenv_i64("SPIRA_DEPLOY_FAULT_RETRY_SECS", 1800),
                build_timeout_secs: getenv_i64("SPIRA_DEPLOY_FAULT_BUILD_TIMEOUT", 1800).max(1) as u64,
                releases: {
                    let r = reg("SPIRA_RELEASES");
                    (!r.is_empty()).then(|| PathBuf::from(r))
                },
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
        Some("--load-fence-check") => {
            let cfg = load_fence::Cfg {
                comms: getenv("SPIRA_LOAD_FENCE_COMMS")
                    .unwrap_or_else(|| load_fence::DEFAULT_COMMS.to_string())
                    .split_whitespace()
                    .map(String::from)
                    .collect(),
                owner_mark: getenv("SPIRA_LOAD_FENCE_OWNER_MARK").unwrap_or_else(|| load_fence::DEFAULT_OWNER_MARK.to_string()),
            };
            load_fence::run(std::path::Path::new("/proc"), &cfg);
        }
        Some("--conditions-check") => {
            if world_halted(&run) {
                log::log("watchtower: conditions-check skipped — world is halted");
                return;
            }
            let cfg = probes::Cfg {
                units: runtime::units::host(getenv("SPIRA_SYSTEMCTL").as_deref()),
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
                oom_glob: getenv("SPIRA_OOM_UNITS").unwrap_or_else(|| "spira-*".to_string()),
                oom_hold_secs: getenv_i64("SPIRA_OOM_HOLD_SECS", 21600),
                tmp_path: getenv("SPIRA_TMP_PROBE_PATH").unwrap_or_else(|| "/tmp".to_string()),
                tmp_floor_mib: getenv_i64("SPIRA_TMPFS_SHED_FREE_MIB", 6144),
                disk_floor_pct: reg_i64("SPIRA_DISK_FLOOR_PCT"),
                disk_fill_hours: reg_i64("SPIRA_DISK_FILL_HOURS"),
                disk_watch_paths: probes::watch_paths(&run, &reg("SPIRA_DISK_WATCH_PATHS"), &reg("SPIRA_DWARF_BUILD_BASE"), &reg("SPIRA_HAND_TARGET_ROOTS")),
                disk_state: run.join("conditions").join("disk-fill.state"),
                psi_dir: getenv("SPIRA_PSI_DIR").unwrap_or_else(|| "/proc/pressure".to_string()),
                psi_full_avg60: reg_parse::<f64>("SPIRA_PSI_FULL_AVG60"),
                psi_sustain_secs: reg_i64("SPIRA_PSI_SUSTAIN_SECS"),
            };
            let db = reg("SPIRA_DB");
            let home_repo = reg("SPIRA_HOME_REPO");
            let inc = resolved_incident_sh();
            let lc_bin = required(spira_store::lifecycle_row::lc_bin());
            let lc = required_client();
            let ctx = conditions::Ctx { run: &run, db: &db, home_repo: &home_repo, incident_sh: &inc, lc: &lc, lc_bin: &lc_bin };
            conditions::reconcile(n, &ctx, "release-currency", probes::release_currency(&cfg));
            conditions::reconcile(n, &ctx, "failing-units", probes::failing_units(&cfg, &run));
            conditions::reconcile(n, &ctx, "oom-restarts", probes::oom_restarts(&cfg, n, &run));
            conditions::reconcile(n, &ctx, "pressure", probes::pressure(&cfg));
            conditions::reconcile(n, &ctx, "disk-room", probes::disk_room(&cfg, n, &probes::read_fs));
            conditions::reconcile(n, &ctx, "cold-overlap", probes::cold_overlap(&cfg, &run));
            conditions::reconcile(n, &ctx, "release-store", probes::release_store(&cfg));
            let rowless_cfg = probes::RowlessCfg {
                lc: required_client(),
                cap: getenv_i64("SPIRA_ROWLESS_CAP", 20).max(1) as usize,
                sustain_secs: getenv_i64("SPIRA_ROWLESS_SUSTAIN_SECS", 300),
                state: run.join("conditions").join("rowless.state"),
                full_secs: getenv_i64("SPIRA_ROWLESS_FULL_SWEEP_SECS", 3600).max(1),
            };
            conditions::reconcile(n, &ctx, "rowless-beads", probes::rowless_beads(&rowless_cfg, n));
            let drift_cfg = probes::ContentDriftCfg {
                lc: required_client(),
                slack: getenv_i64("SPIRA_CONTENT_DRIFT_SLACK", 10).max(0) as usize,
                sustain_secs: getenv_i64("SPIRA_CONTENT_DRIFT_SUSTAIN_SECS", 1800),
                state: run.join("conditions").join("content-drift.state"),
            };
            conditions::reconcile(n, &ctx, "content-drift", probes::content_drift(&drift_cfg));
            match host_real::cfg_of(&run, getenv("SPIRA_TMP_PROBE_PATH"), getenv("SPIRA_PSI_DIR")).and_then(|c| host_real::pass(n as u64, &c, &run)) {
                Ok(m) => log::log(&m),
                Err(e) => log::log(&format!("watchtower: host section refused — {e}")),
            }
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
                &drift::checkout_target(getenv("SPIRA_REPO"), getenv("SPIRA_RELEASE")).unwrap_or_default(),
                &reg("SPIRA_DB"),
                &reg("SPIRA_HOME_REPO"),
                &resolved_incident_sh(),
                &run,
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
        for sub in ["throttle-check", "czar-outcome-check", "pr-stall-check", "release-skew-check"] {
            let msg = format!("watchtower: {sub} skipped \u{2014} world is halted");
            assert!(src.contains(&format!("\"{}\"", msg)), "{sub} must carry the halted-world skip");
        }
    }
}
