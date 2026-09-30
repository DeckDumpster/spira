//! watchtower — hands Ops the pipeline's vital signs (the sweep) and the four cheap
//! detectors the sentinel runs every pass. See DESIGN.md for the contract; this file is
//! only argv dispatch and environment resolution — every module it calls is independently
//! unit-tested.

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

fn getenv(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
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

/// `$SPIRA_HOME`, falling back to `lib.sh`'s own directory on PATH. The bash always had
/// lib.sh's functions available by sourcing it relative to `$0` (`. "$(dirname "$0")/
/// lib.sh"`) regardless of whether `SPIRA_HOME` was set; a compiled binary has no `$0`
/// directory to be relative TO, so this is the equivalent self-location for the seams
/// that need it (`git::spira_landref`, `seams::repo_root`, `seams::timer_priority_and_
/// suspended`, `seams::pipeline_probe`) — every production caller sets `SPIRA_HOME`
/// anyway, but a test harness that only puts `spira/` on PATH (the common shape for a
/// suite run in a clean `env -i`) must still resolve it.
fn spira_home() -> String {
    if let Some(h) = getenv("SPIRA_HOME") {
        return h;
    }
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
        spira_home: spira_home(),
        db: getenv("SPIRA_DB").unwrap_or_default(),
        home_repo: getenv("SPIRA_HOME_REPO").unwrap_or_else(|| "spira".to_string()),
        ask_label: getenv("SPIRA_ASK_LABEL").unwrap_or_else(|| "needs-operator".to_string()),
        snap_stale_s: getenv_i64("SPIRA_SNAP_STALE_S", 60),
        gate_window_s: getenv_i64("SPIRA_WATCH_GATE_WINDOW", 21600),
        gate_log: getenv("SPIRA_GATE_LOG").map(PathBuf::from),
        yield_window_s: getenv_i64("SPIRA_YIELD_WINDOW", 86400),
        yield_sh: getenv("SPIRA_YIELD_SH"),
        disk_warn_pct: getenv_i64("SPIRA_DISK_WARN_PCT", 90),
        mem_warn_mb: getenv_i64("SPIRA_MEM_WARN_MB", 1500),
        meminfo_path: getenv("SPIRA_MEMINFO_PATH").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/proc/meminfo")),
        failed_units_warn_mins: getenv_i64("SPIRA_FAILED_UNITS_WARN_MINS", 15),
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
                    tc_land_ref = git::spira_landref(&spira_home(), repo);
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
                &spira_home(),
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
