//! CHECK 6's twelve watchtower detectors, in-process: the same `watchtower` library entry each
//! `watchtower --<check>` ran, with the configuration its `main.rs` resolved, read through the
//! one config door. A key that does not resolve refuses that one check naming the key, never
//! the process. A halted world skips every check but `load-fence`, as the binary does.

use std::path::{Path, PathBuf};
use std::time::Duration;

use watchtower::process::{getenv, getenv_i64, lib_sh_dir, resolved_incident_sh};
use watchtower::{conditions, czar_outcome, deploy_fault, dolt_drop, drift, git, load_fence, lock_holders, pr_stall, probes, release_skew, sccache_wedge, slow_query, throttle};

/// The checks, in the order CHECK 6 ran them (`dispatch.rs`).
pub const CHECKS: [&str; 12] = ["throttle-check", "czar-outcome-check", "pr-stall-check", "lock-holders-check", "release-skew-check", "deploy-fault-check", "sccache-wedge-check", "load-fence-check", "conditions-check", "slow-query-check", "dolt-drop-check", "drift-check"];

fn reg(k: &str) -> Result<String, String> {
    spira_config::process::cfg(k)
}

fn reg_i64(k: &str) -> Result<i64, String> {
    spira_config::process::cfg_parse::<i64>(k)
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn client() -> Result<spira_config::lc_call::LcClient, String> {
    spira_config::lc_call::LcClient::configured()
}

/// One check, by its CHECKS name; `Ok` names what ran.
pub fn run(check: &str) -> Result<String, String> {
    let run = PathBuf::from(reg("SPIRA_RUN")?);
    if check != "load-fence-check" && run.join("world.halted").is_file() {
        return Ok(format!("watchtower: {check} skipped — world is halted"));
    }
    let n = now();
    match check {
        "throttle-check" => throttle_check(n, &run)?,
        "czar-outcome-check" => {
            let cfg = czar_outcome::Cfg { outcome_mins: reg_i64("SPIRA_CZAR_OUTCOME_MINS")?, unclaimed_mins: reg_i64("SPIRA_CZAR_UNCLAIMED_MINS")?, label: reg("SPIRA_CZAR_LABEL")? };
            czar_outcome::run(n, &client()?, &reg("SPIRA_DB")?, &reg("SPIRA_HOME_REPO")?, &resolved_incident_sh(), &cfg);
        }
        "pr-stall-check" => {
            let cfg = pr_stall::Cfg { stall_secs: reg_i64("SPIRA_PR_STALL_MINS")? * 60, gh_bin: reg("SPIRA_GH")?, gh_timeout: Duration::from_secs(getenv_i64("GH_TIMEOUT", 120) as u64) };
            pr_stall::run(n, &lib_sh_dir(), &reg("SPIRA_DB")?, &reg("SPIRA_HOME_REPO")?, &resolved_incident_sh(), &cfg);
        }
        "lock-holders-check" => return lock_holders_check(n, &run),
        "release-skew-check" => {
            let cfg = release_skew::Cfg { systemctl: getenv("SPIRA_SYSTEMCTL").unwrap_or_else(|| "systemctl".into()), max_secs: getenv_i64("SPIRA_RELEASE_SKEW_MAX_SECS", 3600), dwarf_max_secs: getenv_i64("SPIRA_RELEASE_SKEW_DWARF_MAX_SECS", 14400) };
            release_skew::run(n, &run, &reg("SPIRA_DB")?, &reg("SPIRA_HOME_REPO")?, &resolved_incident_sh(), &cfg);
        }
        "deploy-fault-check" => {
            let release_bin = match getenv("SPIRA_RELEASE_BIN") {
                Some(b) => b,
                None => spira_config::release_env::tool("release")?,
            };
            let cfg = deploy_fault::Cfg { release_bin, repo: getenv("SPIRA_REPO").unwrap_or_default(), retry_secs: getenv_i64("SPIRA_DEPLOY_FAULT_RETRY_SECS", 1800), build_timeout_secs: getenv_i64("SPIRA_DEPLOY_FAULT_BUILD_TIMEOUT", 1800).max(1) as u64, releases: reg("SPIRA_RELEASES").ok().filter(|r| !r.is_empty()).map(PathBuf::from) };
            deploy_fault::run(n, &PathBuf::from(reg("SPIRA_QUEUE_DIR")?), &run, &reg("SPIRA_DB")?, &reg("SPIRA_HOME_REPO")?, &resolved_incident_sh(), &cfg);
        }
        "sccache-wedge-check" => {
            let cfg = sccache_wedge::Cfg { sccache: getenv("SPIRA_SCCACHE_BIN").unwrap_or_else(|| "sccache".into()), stale_secs: getenv_i64("SPIRA_SCCACHE_WEDGE_MINS", 10) * 60, cmd_timeout_secs: getenv_i64("SPIRA_SCCACHE_CMD_TIMEOUT", 30).max(1) as u64 };
            sccache_wedge::run(n, Path::new("/proc"), &reg("SPIRA_DB")?, &reg("SPIRA_HOME_REPO")?, &resolved_incident_sh(), &cfg);
        }
        "load-fence-check" => {
            let cfg = load_fence::Cfg {
                comms: getenv("SPIRA_LOAD_FENCE_COMMS").unwrap_or_else(|| load_fence::DEFAULT_COMMS.to_string()).split_whitespace().map(String::from).collect(),
                owner_mark: getenv("SPIRA_LOAD_FENCE_OWNER_MARK").unwrap_or_else(|| load_fence::DEFAULT_OWNER_MARK.to_string()),
            };
            load_fence::run(Path::new("/proc"), &cfg);
        }
        "conditions-check" => conditions_check(n, &run)?,
        "slow-query-check" => {
            let log = getenv("SPIRA_SLOW_QUERY_LOG").map(PathBuf::from).unwrap_or_else(|| run.join("slow-queries.log"));
            slow_query::run(&run, &log, &reg("SPIRA_DB")?, &reg("SPIRA_HOME_REPO")?, &resolved_incident_sh());
        }
        "dolt-drop-check" => {
            let log = getenv("SPIRA_DOLT_LOG").map(PathBuf::from).unwrap_or_else(|| run.join("dolt-beads.log"));
            dolt_drop::run(&log, &getenv("SPIRA_DB").unwrap_or_default(), &getenv("SPIRA_HOME_REPO").unwrap_or_else(|| "spira".into()), &resolved_incident_sh(), getenv_i64("SPIRA_DOLT_DROP_WINDOW_MINS", 5).max(1) as usize, getenv_i64("SPIRA_DOLT_DROP_PER_MIN", 30).max(1) as usize);
        }
        "drift-check" => {
            let home = getenv("SPIRA_HOME").unwrap_or_else(lib_sh_dir);
            drift::run(&home, &drift::checkout_target(getenv("SPIRA_REPO"), getenv("SPIRA_RELEASE")).unwrap_or_default(), &reg("SPIRA_DB")?, &reg("SPIRA_HOME_REPO")?, &resolved_incident_sh(), &run);
        }
        other => return Err(format!("{other} is not one of CHECK 6's watchtower checks")),
    }
    Ok(format!("watchtower: {check} ran"))
}

fn throttle_check(n: i64, run: &Path) -> Result<(), String> {
    let inc = resolved_incident_sh();
    let (db, home_repo) = (reg("SPIRA_DB")?, reg("SPIRA_HOME_REPO")?);
    let cfg = throttle::Cfg { depth_at: reg_i64("SPIRA_QUEUE_THROTTLE_DEPTH_AT")?, release_at: reg_i64("SPIRA_QUEUE_THROTTLE_RELEASE_AT")?, stall_mins: reg_i64("SPIRA_QUEUE_THROTTLE_STALL_MINS")? };
    let stamp = getenv("SPIRA_THROTTLE_STAMP").map(PathBuf::from).unwrap_or_else(|| run.join("queue-throttled"));
    let repo = getenv("SPIRA_TC_REPO").or_else(|| getenv("SPIRA_REPO"));
    let land_ref = getenv("SPIRA_TC_LAND_REF").or_else(|| repo.as_deref().and_then(|r| git::spira_landref(&lib_sh_dir(), r)));
    let override_off = reg("SPIRA_QUEUE_THROTTLE_OVERRIDE")? == "off";
    let ctx = throttle::Ctx { db: &db, home_repo: &home_repo, incident_sh: &inc, stamp: &stamp, override_off, repo: repo.as_deref(), land_ref: land_ref.as_deref(), land_ref_default_for_log: "origin/main" };
    throttle::run(n, &cfg, &ctx);
    Ok(())
}

fn lock_holders_check(n: i64, run: &Path) -> Result<String, String> {
    let stall_mins = reg_i64("SPIRA_QUEUE_THROTTLE_STALL_MINS")?;
    let repo = getenv("SPIRA_TC_REPO").or_else(|| getenv("SPIRA_REPO"));
    let land_ref = getenv("SPIRA_TC_LAND_REF").or_else(|| repo.as_deref().and_then(|r| git::spira_landref(&lib_sh_dir(), r)));
    let Some(depth) = throttle::compute_depth(repo.as_deref(), land_ref.as_deref()) else {
        return Ok("watchtower: lock-holders-check skipped — spira-lc is unreachable, depth unknown".into());
    };
    let since = throttle::minutes_since_last_landed(n);
    if depth == 0 || since.is_some_and(|m| m < stall_mins) {
        return Ok(format!("watchtower: lock-holders-check — no stall (depth={depth} since_land={}m)", throttle::disp(since)));
    }
    let queue_dir = PathBuf::from(reg("SPIRA_QUEUE_DIR")?);
    let (db, home_repo) = (reg("SPIRA_DB")?, reg("SPIRA_HOME_REPO")?);
    let ctx = lock_holders::Ctx { run, queue_dir: &queue_dir, proc_root: Path::new("/proc"), db: &db, home_repo: &home_repo, incident_sh: &resolved_incident_sh() };
    lock_holders::run(n, &format!("depth {depth}, no landing for {}m", throttle::disp(since)), &ctx);
    Ok("watchtower: lock-holders-check ran".into())
}

fn conditions_check(n: i64, run: &Path) -> Result<(), String> {
    let releases = reg("SPIRA_RELEASES")?;
    let cfg = probes::Cfg {
        units: runtime::units::host(),
        releases: (!releases.is_empty()).then_some(releases),
        keep: reg_i64("SPIRA_RELEASES_KEEP")?.max(1) as usize,
        store_slack: reg_i64("SPIRA_RELEASE_STORE_SLACK")?.max(0) as usize,
        unit_glob: reg("SPIRA_RELEASE_CURRENCY_UNITS")?,
        stale_release_secs: reg_i64("SPIRA_RELEASE_STALE_SECS")?,
        failed_runs: reg_i64("SPIRA_FAILED_UNIT_RUNS")?,
        oom_glob: getenv("SPIRA_OOM_UNITS").unwrap_or_else(|| "spira-*".into()),
        oom_hold_secs: getenv_i64("SPIRA_OOM_HOLD_SECS", 21600),
        tmp_path: getenv("SPIRA_TMP_PROBE_PATH").unwrap_or_else(|| "/tmp".into()),
        tmp_floor_mib: getenv_i64("SPIRA_TMPFS_SHED_FREE_MIB", 6144),
        disk_floor_pct: reg_i64("SPIRA_DISK_FLOOR_PCT")?,
        psi_dir: getenv("SPIRA_PSI_DIR").unwrap_or_else(|| "/proc/pressure".into()),
        psi_full_avg60: spira_config::process::cfg_parse::<f64>("SPIRA_PSI_FULL_AVG60")?,
        psi_sustain_secs: reg_i64("SPIRA_PSI_SUSTAIN_SECS")?,
    };
    let (db, home_repo, inc) = (reg("SPIRA_DB")?, reg("SPIRA_HOME_REPO")?, resolved_incident_sh());
    let lc_bin = spira_config::lifecycle_row::lc_bin()?;
    let lc = client()?;
    let ctx = conditions::Ctx { run, db: &db, home_repo: &home_repo, incident_sh: &inc, lc: &lc, lc_bin: &lc_bin };
    conditions::reconcile(n, &ctx, "release-currency", probes::release_currency(&cfg));
    conditions::reconcile(n, &ctx, "failing-units", probes::failing_units(&cfg, run));
    conditions::reconcile(n, &ctx, "oom-restarts", probes::oom_restarts(&cfg, n, run));
    conditions::reconcile(n, &ctx, "pressure", probes::pressure(&cfg));
    conditions::reconcile(n, &ctx, "cold-overlap", probes::cold_overlap(&cfg, run));
    conditions::reconcile(n, &ctx, "release-store", probes::release_store(&cfg));
    let rowless = probes::RowlessCfg { lc: client()?, cap: getenv_i64("SPIRA_ROWLESS_CAP", 20).max(1) as usize, sustain_secs: getenv_i64("SPIRA_ROWLESS_SUSTAIN_SECS", 300), state: run.join("conditions").join("rowless.state"), full_secs: getenv_i64("SPIRA_ROWLESS_FULL_SWEEP_SECS", 3600).max(1) };
    conditions::reconcile(n, &ctx, "rowless-beads", probes::rowless_beads(&rowless, n));
    let drift = probes::ContentDriftCfg { lc: client()?, slack: getenv_i64("SPIRA_CONTENT_DRIFT_SLACK", 10).max(0) as usize, sustain_secs: getenv_i64("SPIRA_CONTENT_DRIFT_SUSTAIN_SECS", 1800), state: run.join("conditions").join("content-drift.state") };
    conditions::reconcile(n, &ctx, "content-drift", probes::content_drift(&drift));
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_twelve_are_check_6s_and_an_unknown_one_is_refused() {
        assert_eq!(super::CHECKS.len(), 12);
        let src = include_str!("dispatch.rs");
        for c in super::CHECKS {
            assert!(src.contains(&format!("\"--{c}\"")), "{c} is one CHECK 6 ran");
        }
    }
}
