// reconciler-flow — the flow half of the reconciler: backlog trend, stage velocities against
// a trailing baseline (with optional per-stage floors from the desired-state document), stage
// dwell, and round health (flip rate), over the run/tsd/ time series. Runs on a 30-minute
// timer (Ryan's default window) — see systemd/spira-reconciler-flow.timer.
//
// reconciler-flow --pass
//
// A flow gap has no deterministic remedy (per the design): every gap this pass confirms goes
// straight to the Concierge alert path (reconciler_flow::io::mail_concierge), not to
// incident.sh. Hysteresis (grace period, unobservable-is-never-satisfied) is the same engine
// czar-pass's structural invariants use (reconciler-engine, sp-pu7v6) — this binary owns its
// own state file so a slow 30-minute pass never contends with czar-pass's 30-second one.

use std::env;
use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};

use reconciler_engine::core::{step, HysteresisState, RawStatus, Verdict};
use reconciler_engine::io::{append_status, load_state, save_state, StateMap};

use reconciler_flow::core::{
    backlog_trend_raw, dwell_raw, round_health_raw, velocity_raw, BacklogObserved, DwellObserved,
    RoundHealthObserved, VelocityObserved,
};
use reconciler_flow::io::{
    append_backlog_sample, backlog_baseline, backlog_count, certified_waiting, dwell_metrics,
    flow_floors, mail_concierge, round_health_metrics, velocity_metrics,
};

extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}
const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.get(1).map(String::as_str) != Some("--pass") {
        eprintln!("usage: reconciler-flow --pass");
        return ExitCode::from(2);
    }
    match run_pass() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("reconciler-flow: {e}");
            ExitCode::FAILURE
        }
    }
}

struct Config {
    spira_run: PathBuf,
    lock_path: PathBuf,
    state_path: PathBuf,
    status_log: PathBuf,
    tsd_bin: String,
    duckdb_bin: String,
    bd_bin: String,
    spira_db: String,
    scope_label: String,
    landstate_dir: PathBuf,
    desired_dir: PathBuf,
    mail_sh: String,
    window_hours: f64,
    baseline_hours: f64,
    grace_secs: u64,
    now_secs: u64,
    now_iso: String,
}

impl Config {
    fn from_env() -> Config {
        let spira_run = env::var("SPIRA_RUN").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/tmp/spira"));
        let spira_home = env::var("SPIRA_HOME").unwrap_or_default();
        Config {
            lock_path: spira_run.join("reconciler-flow.lock"),
            state_path: env::var("SPIRA_RECONCILER_FLOW_STATE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("reconciler-flow-state.json")),
            status_log: env::var("SPIRA_RECONCILER_STATUS_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("reconciler-status.jsonl")),
            tsd_bin: env::var("SPIRA_TSD_BIN").unwrap_or_default(),
            duckdb_bin: env::var("SPIRA_DUCKDB_BIN").unwrap_or_else(|_| "duckdb".to_string()),
            bd_bin: env::var("SPIRA_BD").unwrap_or_else(|_| "bd".to_string()),
            spira_db: env::var("SPIRA_DB").unwrap_or_default(),
            scope_label: env::var("SPIRA_SCOPE_LABEL").unwrap_or_default(),
            landstate_dir: spira_run.join("landstate"),
            desired_dir: env::var("SPIRA_DESIRED_DIR").map(PathBuf::from).unwrap_or_else(|_| {
                let config_home = env::var("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from(env::var("HOME").unwrap_or_default()).join(".config"));
                config_home.join("spira").join("desired")
            }),
            mail_sh: env::var("SPIRA_MAIL_SH").unwrap_or_else(|_| format!("{spira_home}/mail.sh")),
            window_hours: env::var("SPIRA_FLOW_WINDOW_HOURS").ok().and_then(|v| v.parse().ok()).unwrap_or(0.5),
            baseline_hours: env::var("SPIRA_FLOW_BASELINE_HOURS").ok().and_then(|v| v.parse().ok()).unwrap_or(24.0),
            grace_secs: env::var("SPIRA_FLOW_GRACE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(1800),
            now_secs: unix_now(),
            now_iso: compute_now_iso(),
            spira_run,
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn compute_now_iso() -> String {
    Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

fn log_print(msg: &str) {
    println!("{} spira: {}", compute_now_iso(), msg);
}

fn unobservable(reason: String) -> RawStatus {
    RawStatus::Unobservable { reason }
}

fn evaluate(cfg: &Config, state: &mut StateMap, key: &str, raw: RawStatus) -> Verdict {
    let prev = state.remove(key).unwrap_or_default();
    let (verdict, next) = step(cfg.now_secs, raw, cfg.grace_secs, prev);
    append_status(&cfg.status_log, &cfg.now_iso, key, &verdict);
    if next != HysteresisState::default() {
        state.insert(key.to_string(), next);
    }
    verdict
}

fn status_word(v: &Verdict) -> &'static str {
    match &v.status {
        RawStatus::Satisfied => "satisfied",
        RawStatus::Gap { .. } => "gap",
        RawStatus::Unobservable { .. } => "unobservable",
    }
}

/// A flow gap has no deterministic remedy: the only action on `is_gap` is the Concierge
/// alert path. Deduplication across passes is sp-fufyb's mandate, not reimplemented here —
/// this sends once per pass the gap is confirmed.
fn maybe_alert(cfg: &Config, key: &str, verdict: &Verdict) {
    if !verdict.is_gap {
        return;
    }
    let (desired, observed) = match &verdict.status {
        RawStatus::Gap { desired, observed, .. } => (desired.clone(), observed.clone()),
        _ => return,
    };
    let age_m = verdict.since.map(|s| cfg.now_secs.saturating_sub(s) / 60).unwrap_or(0);
    let subject = format!("RECONCILER: flow gap — {key}");
    let body = format!(
        "Flow invariant {key} has been a gap for {age_m}m.\n\n\
         desired:  {desired}\n\
         observed: {observed}\n\n\
         This has no deterministic remedy — it needs judgement, not a retry.\n",
    );
    if let Err(e) = mail_concierge(&cfg.mail_sh, &subject, &body) {
        eprintln!("reconciler-flow: {key}: concierge alert failed: {e}");
    } else {
        log_print(&format!("reconciler-flow: {key} → gap, alerted concierge"));
    }
}

fn run_pass() -> Result<(), String> {
    let cfg = Config::from_env();

    if cfg.spira_run.join("world.halted").exists() {
        log_print("reconciler-flow: skipped — world is halted");
        return Ok(());
    }

    let lock_file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&cfg.lock_path)
        .map_err(|e| format!("open lock {}: {e}", cfg.lock_path.display()))?;
    if unsafe { flock(lock_file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
        log_print("reconciler-flow: already running — skip");
        return Ok(());
    }

    let mut state = load_state(&cfg.state_path);
    let (velocity_floor, dwell_limit) = flow_floors(&cfg.desired_dir);

    // ── backlog trend ────────────────────────────────────────────────────────────────────
    let baseline = backlog_baseline(&cfg.duckdb_bin, &cfg.spira_run, cfg.baseline_hours);
    let current = backlog_count(&cfg.bd_bin, &cfg.spira_db, &cfg.scope_label);
    let backlog_raw = match (&current, &baseline) {
        (Ok(cur), Ok(base)) => backlog_trend_raw(&BacklogObserved { current: *cur, baseline: *base }),
        (Err(e), _) => unobservable(e.clone()),
        (_, Err(e)) => unobservable(e.clone()),
    };
    let v = evaluate(&cfg, &mut state, "flow:backlog", backlog_raw);
    maybe_alert(&cfg, "flow:backlog", &v);
    log_print(&format!("reconciler-flow: flow:backlog → {}", status_word(&v)));
    if let Ok(cur) = current {
        append_backlog_sample(&cfg.tsd_bin, &cfg.spira_run, cur);
    }

    // ── stage velocity ("queue") ─────────────────────────────────────────────────────────
    let waiting = certified_waiting(&cfg.landstate_dir);
    let velocity = velocity_metrics(&cfg.duckdb_bin, &cfg.spira_run, cfg.window_hours, cfg.baseline_hours);
    let velocity_raw_status = match (&waiting, &velocity) {
        (Ok(w), Ok((cur, base))) => velocity_raw(
            &VelocityObserved { current_per_hour: *cur, baseline_per_hour: *base, waiting: *w },
            velocity_floor,
        ),
        (Err(e), _) => unobservable(e.clone()),
        (_, Err(e)) => unobservable(e.clone()),
    };
    let v = evaluate(&cfg, &mut state, "flow:velocity:queue", velocity_raw_status);
    maybe_alert(&cfg, "flow:velocity:queue", &v);
    log_print(&format!("reconciler-flow: flow:velocity:queue → {}", status_word(&v)));

    // ── stage dwell ("review") ───────────────────────────────────────────────────────────
    let dwell = dwell_metrics(&cfg.duckdb_bin, &cfg.spira_run, cfg.window_hours, cfg.baseline_hours);
    let dwell_raw_status = match dwell {
        Ok((cur_p95, cur_n, base_p95, base_n)) => dwell_raw(
            &DwellObserved { p95_seconds: cur_p95, n: cur_n },
            dwell_limit,
            if base_n > 0 { Some(base_p95) } else { None },
        ),
        Err(e) => unobservable(e),
    };
    let v = evaluate(&cfg, &mut state, "flow:dwell:review", dwell_raw_status);
    maybe_alert(&cfg, "flow:dwell:review", &v);
    log_print(&format!("reconciler-flow: flow:dwell:review → {}", status_word(&v)));

    // ── round health (flip rate) ─────────────────────────────────────────────────────────
    let round_health = round_health_metrics(&cfg.duckdb_bin, &cfg.spira_run, cfg.window_hours, cfg.baseline_hours);
    let round_health_raw_status = match round_health {
        Ok((cur_flips, cur_transitions, base_flips, base_transitions)) => {
            let baseline_flip_rate = if base_transitions > 0 { base_flips as f64 / base_transitions as f64 } else { 0.0 };
            round_health_raw(&RoundHealthObserved { flips: cur_flips, transitions: cur_transitions, baseline_flip_rate })
        }
        Err(e) => unobservable(e),
    };
    let v = evaluate(&cfg, &mut state, "flow:round-health", round_health_raw_status);
    maybe_alert(&cfg, "flow:round-health", &v);
    log_print(&format!("reconciler-flow: flow:round-health → {}", status_word(&v)));

    let _ = save_state(&cfg.state_path, &state);
    let elapsed = unix_now().saturating_sub(cfg.now_secs);
    log_print(&format!("reconciler-flow: pass complete ({elapsed}s)"));

    drop(lock_file);
    Ok(())
}
