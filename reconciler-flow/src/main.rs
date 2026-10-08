// reconciler-flow — the flow half of the reconciler: backlog trend, stage velocities against
// a trailing baseline (with optional per-stage floors from the desired-state document), stage
// dwell, and round health (flip rate); plus idle capacity, sentinel overrun, rework rate and
// stage dwell regression (design reconciler-time-series-2026-09-27 §3) — over the run/tsd/
// time series. Runs on a 30-minute timer — see systemd/spira-reconciler-flow.timer.
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
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use reconciler_engine::alert::{compose_alert, should_alert};
use reconciler_engine::core::{step, HysteresisState, RawStatus, Verdict};
use reconciler_engine::io::{append_status, load_alerted, load_state, save_alerted, save_state, AlertedSinceMap, StateMap};

use reconciler_flow::core::{
    backlog_trend_raw, dwell_raw, dwell_regression_raw, idle_capacity_raw, round_health_raw,
    rework_raw, sentinel_overrun_raw, velocity_raw, BacklogObserved, DwellObserved,
    DwellRegressionObserved, ReworkObserved, RoundHealthObserved, VelocityObserved,
    DWELL_REGRESSION_STATES,
};
use reconciler_flow::io::{
    append_backlog_sample, backlog_baseline, backlog_count, dwell_metrics,
    dwell_regression_metrics, flow_floors, mail_concierge, rework_metrics, round_health_metrics,
    sentinel_pass_wall_seconds, slots_samples, velocity_metrics, waiting_to_land,
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
    alerted_path: PathBuf,
    status_log: PathBuf,
    tsd_bin: String,
    export_bin: String,
    duckdb_bin: String,
    bd_bin: String,
    spira_db: String,
    scope_label: String,
    desired_dir: PathBuf,
    mail_sh: String,
    window_hours: f64,
    baseline_hours: f64,
    grace_secs: u64,
    unobservable_grace_secs: u64,
    rework_window_hours: f64,
    sentinel_timer_secs: u64,
    now_secs: u64,
    now_iso: String,
}

/// `$SPIRA_HOME`, else the first ancestor of this executable that holds `lib.sh` — same
/// fallback `spira_world::locate_home`/`mail::env::locate_home`/landing-pass's own
/// `harness_home` already use. `resolve_for_process` needs a REAL `home/conf.d` to
/// resolve almost every key (`SPIRA_RUN` included — sp-ivfu3) — an empty `home` makes it
/// refuse outright ("no config registry at conf.d"), exactly what a bare shell with no
/// `$SPIRA_HOME` exported would otherwise hit.
fn harness_home() -> PathBuf {
    if let Ok(h) = env::var("SPIRA_HOME") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    let Ok(exe) = env::current_exe() else { return PathBuf::new() };
    let exe = exe.canonicalize().unwrap_or(exe);
    exe.ancestors()
        .skip(1)
        .take(4)
        .map(|a| a.join("spira"))
        .find(|p| p.join("lib.sh").is_file())
        .unwrap_or_default()
}

impl Config {
    fn from_env() -> Result<Config, String> {
        use spira_config::process::{cfg, cfg_parse};
        // sp-ivfu3: `spira.run`, resolved in-process through `spira_config` — never the
        // literal `/tmp/spira` a bare shell used to get whenever `$SPIRA_RUN` itself was
        // unset (law-a-binary-resolves-the-config-it-reads). REFUSES, named, rather than
        // guessing, when `spira_config` itself cannot resolve.
        let env_map: std::collections::BTreeMap<String, String> = env::vars().collect();
        let spira_run = spira_config::resolve::resolve_run_dir(&env_map, &harness_home())?;
        Ok(Config {
            lock_path: spira_run.join("reconciler-flow.lock"),
            // SPIRA_RECONCILER_FLOW_STATE is not a registered config key (spira/conf.d) —
            // left as a direct env read.
            state_path: env::var("SPIRA_RECONCILER_FLOW_STATE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("reconciler-flow-state.json")),
            // SPIRA_RECONCILER_FLOW_ALERTED is not a registered config key — left as env.
            alerted_path: env::var("SPIRA_RECONCILER_FLOW_ALERTED")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("reconciler-flow-alerted.json")),
            // Under run/tsd/ (design reconciler-time-series-2026-09-27 §2), same move and
            // same env var as reconciler/src/main.rs — one family, two writers.
            // SPIRA_RECONCILER_STATUS_LOG is not a registered config key — left as env.
            status_log: env::var("SPIRA_RECONCILER_STATUS_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("tsd").join("reconciler-status.jsonl")),
            tsd_bin: "tsd-write".to_string(), // by name, on the launcher's PATH (sp-gypjk)
            // Not a registered config key — left as env, like the other binaries below.
            export_bin: env::var("SPIRA_TSD_LIFECYCLE_EXPORT_BIN")
                .unwrap_or_else(|_| "tsd-lifecycle-export".to_string()),
            // SPIRA_DUCKDB_BIN is not a registered config key — left as env.
            duckdb_bin: env::var("SPIRA_DUCKDB_BIN").unwrap_or_else(|_| "duckdb".to_string()),
            bd_bin: cfg("SPIRA_BD")?,
            spira_db: cfg("SPIRA_DB")?,
            scope_label: cfg("SPIRA_SCOPE_LABEL")?,
            desired_dir: PathBuf::from(cfg("SPIRA_DESIRED_DIR")?),
            // SPIRA_MAIL_SH is not a registered config key — left as env.
            mail_sh: env::var("SPIRA_MAIL_SH").unwrap_or_else(|_| "mail".to_string()),
            window_hours: cfg_parse::<f64>("SPIRA_FLOW_WINDOW_HOURS")?,
            baseline_hours: cfg_parse::<f64>("SPIRA_FLOW_BASELINE_HOURS")?,
            grace_secs: cfg_parse::<u64>("SPIRA_FLOW_GRACE_SECS")?,
            // Deliberately its own knob, not derived from grace_secs: a flow gap's 30-minute
            // grace is tuned for real slowdowns, but a blind detector (the query layer
            // itself unreachable) is a different failure and must always cross 1h before it
            // alerts — tuning the gap window faster must never speed this one up too.
            unobservable_grace_secs: cfg_parse::<u64>("SPIRA_FLOW_UNOBSERVABLE_GRACE_SECS")?,
            // The design's own rework window (6h), distinct from the 30-minute flow window
            // the other new invariants share — a ratio over 30 minutes of landings is too
            // thin a sample to mean anything.
            rework_window_hours: cfg_parse::<f64>("SPIRA_FLOW_REWORK_WINDOW_HOURS")?,
            // Must match systemd/spira-sentinel.timer's OnUnitActiveSec — two independent
            // literals of the same fact is exactly how they drift.
            sentinel_timer_secs: cfg_parse::<u64>("SPIRA_FLOW_SENTINEL_PERIOD_SECS")?,
            now_secs: unix_now(),
            now_iso: compute_now_iso(),
            spira_run,
        })
    }
}

/// Epoch seconds; `SPIRA_NOW` overrides, the same clock seam bead, strand, watchd and
/// cockpit-collect honour, so a suite advances past a grace period instead of sleeping.
fn unix_now() -> u64 {
    if let Some(n) = env::var("SPIRA_NOW").ok().and_then(|v| v.parse().ok()) {
        return n;
    }
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn compute_now_iso() -> String {
    spira_config::bounded::bounded("date")
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
    let grace = match raw {
        RawStatus::Unobservable { .. } => cfg.unobservable_grace_secs,
        _ => cfg.grace_secs,
    };
    let (verdict, next) = step(cfg.now_secs, raw, grace, prev);
    append_status(&cfg.status_log, &cfg.now_iso, key, &verdict);
    if next != HysteresisState::default() {
        state.insert(key.to_string(), next);
    }
    verdict
}

/// An invariant read from `bead-stage` is only as fresh as the last export: when the exporter
/// failed, the file is stale and a verdict from it would be a guess.
fn fresh(export: &Result<(), String>, raw: RawStatus) -> RawStatus {
    match export {
        Ok(()) => raw,
        Err(e) => unobservable(format!("bead-stage export failed: {e}")),
    }
}

fn status_word(v: &Verdict) -> &'static str {
    match &v.status {
        RawStatus::Satisfied => "satisfied",
        RawStatus::Gap { .. } => "gap",
        RawStatus::Unobservable { .. } => "unobservable",
    }
}

/// A flow gap has no deterministic remedy: the only action on `is_gap` is the Concierge
/// alert path, deduplicated per gap streak by `reconciler_engine::alert::should_alert` — the
/// same dedup sp-fufyb built for the structural path, wired here instead of resending once
/// per pass for as long as the gap stays open (law-repeating-conditions-escalate-once).
fn maybe_alert(cfg: &Config, alerted: &mut AlertedSinceMap, key: &str, verdict: &Verdict) {
    let (fire, next) = should_alert(verdict, alerted.get(key).copied());
    match next {
        Some(since) => {
            alerted.insert(key.to_string(), since);
        }
        None => {
            alerted.remove(key);
        }
    }
    if !fire {
        return;
    }
    let subject = format!("RECONCILER: flow gap — {key}");
    let evidence = compose_alert(key, cfg.now_secs, verdict, None);
    let body = format!("{evidence}\nThis has no deterministic remedy — it needs judgement, not a retry.\n");
    if let Err(e) = mail_concierge(&cfg.mail_sh, &subject, &body) {
        eprintln!("reconciler-flow: {key}: concierge alert failed: {e}");
    } else {
        log_print(&format!("reconciler-flow: {key} → gap, alerted concierge"));
    }
}

/// Brings `bead-stage` up to date before anything reads it: nothing else schedules the
/// exporter, so a pass that did not run it would evaluate whatever the file last held.
fn run_export(cfg: &Config) -> Result<(), String> {
    let out = spira_config::bounded::bounded(&cfg.export_bin)
        .arg("lifecycle")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", cfg.export_bin))?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!(
        "{} lifecycle: exit {}: {}",
        cfg.export_bin,
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

fn run_pass() -> Result<(), String> {
    let cfg = Config::from_env()?;

    if cfg.spira_run.join("world.halted").exists() {
        log_print("reconciler-flow: skipped — world is halted");
        return Ok(());
    }

    // install.sh creates SPIRA_RUN once, at install time (systemd/install.sh: "the units
    // start things that source only conf.sh, and those fail on a path that does not exist
    // yet"). A bash watcher gets a fresh mkdir on every invocation via lib.sh; this binary
    // sources nothing, so it must not depend on that directory having survived since install.
    std::fs::create_dir_all(&cfg.spira_run)
        .map_err(|e| format!("create {}: {e}", cfg.spira_run.display()))?;

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
    let mut alerted = load_alerted(&cfg.alerted_path);
    let (velocity_floor, dwell_limit) = flow_floors(&cfg.desired_dir);

    let export = run_export(&cfg);
    if let Err(e) = &export {
        eprintln!("reconciler-flow: {e}");
    }

    // ── backlog trend ────────────────────────────────────────────────────────────────────
    let baseline = backlog_baseline(&cfg.duckdb_bin, &cfg.spira_run, cfg.baseline_hours);
    let current = backlog_count(&cfg.bd_bin, &cfg.spira_db, &cfg.scope_label);
    let backlog_raw = match (&current, &baseline) {
        (Ok(cur), Ok(base)) => backlog_trend_raw(&BacklogObserved { current: *cur, baseline: *base }),
        (Err(e), _) => unobservable(e.clone()),
        (_, Err(e)) => unobservable(e.clone()),
    };
    let v = evaluate(&cfg, &mut state, "flow:backlog", backlog_raw);
    maybe_alert(&cfg, &mut alerted, "flow:backlog", &v);
    log_print(&format!("reconciler-flow: flow:backlog → {}", status_word(&v)));
    if let Ok(cur) = current {
        append_backlog_sample(&cfg.tsd_bin, &cfg.spira_run, cur);
    }

    // ── stage velocity ("queue") ─────────────────────────────────────────────────────────
    let waiting = waiting_to_land(&cfg.duckdb_bin, &cfg.spira_run);
    let velocity = velocity_metrics(&cfg.duckdb_bin, &cfg.spira_run, cfg.window_hours, cfg.baseline_hours);
    let velocity_raw_status = match (&waiting, &velocity) {
        (Ok(w), Ok((cur, base))) => velocity_raw(
            &VelocityObserved { current_per_hour: *cur, baseline_per_hour: *base, waiting: *w },
            velocity_floor,
        ),
        (Err(e), _) => unobservable(e.clone()),
        (_, Err(e)) => unobservable(e.clone()),
    };
    let v = evaluate(&cfg, &mut state, "flow:velocity:queue", fresh(&export, velocity_raw_status));
    maybe_alert(&cfg, &mut alerted, "flow:velocity:queue", &v);
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
    let v = evaluate(&cfg, &mut state, "flow:dwell:review", fresh(&export, dwell_raw_status));
    maybe_alert(&cfg, &mut alerted, "flow:dwell:review", &v);
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
    let v = evaluate(&cfg, &mut state, "flow:round-health", fresh(&export, round_health_raw_status));
    maybe_alert(&cfg, &mut alerted, "flow:round-health", &v);
    log_print(&format!("reconciler-flow: flow:round-health → {}", status_word(&v)));

    // ── idle capacity ────────────────────────────────────────────────────────────────────
    let idle_raw_status = match slots_samples(&cfg.duckdb_bin, &cfg.spira_run) {
        Ok((prev, cur)) => idle_capacity_raw(&prev, &cur),
        Err(e) => unobservable(e),
    };
    let v = evaluate(&cfg, &mut state, "flow:idle-capacity", idle_raw_status);
    maybe_alert(&cfg, &mut alerted, "flow:idle-capacity", &v);
    log_print(&format!("reconciler-flow: flow:idle-capacity → {}", status_word(&v)));

    // ── sentinel overrun ─────────────────────────────────────────────────────────────────
    let sentinel_raw_status = match sentinel_pass_wall_seconds(&cfg.duckdb_bin, &cfg.spira_run) {
        Ok(secs) => sentinel_overrun_raw(secs, cfg.sentinel_timer_secs),
        Err(e) => unobservable(e),
    };
    let v = evaluate(&cfg, &mut state, "flow:sentinel-overrun", sentinel_raw_status);
    maybe_alert(&cfg, &mut alerted, "flow:sentinel-overrun", &v);
    log_print(&format!("reconciler-flow: flow:sentinel-overrun → {}", status_word(&v)));

    // ── rework ───────────────────────────────────────────────────────────────────────────
    // Report-only until 24h of bead-stage history exist (design: "the rework alert starts at
    // 1.0 reopens per landed bead over 6h ... report-only for 24h, then tuned") — reusing
    // baseline_hours as that warm-up window, since it is already this pass's own "how much
    // history counts as enough" answer.
    match rework_metrics(&cfg.duckdb_bin, &cfg.spira_run, cfg.rework_window_hours) {
        Ok((reopens, landed, history_hours)) => {
            let raw = fresh(&export, rework_raw(&ReworkObserved { reopens, landed }));
            let v = evaluate(&cfg, &mut state, "flow:rework", raw);
            if history_hours >= cfg.baseline_hours {
                maybe_alert(&cfg, &mut alerted, "flow:rework", &v);
            } else {
                log_print(&format!(
                    "reconciler-flow: flow:rework → report-only, {history_hours:.1}h of {:.0}h warm-up",
                    cfg.baseline_hours
                ));
            }
            log_print(&format!("reconciler-flow: flow:rework → {}", status_word(&v)));
        }
        Err(e) => {
            let v = evaluate(&cfg, &mut state, "flow:rework", unobservable(e));
            maybe_alert(&cfg, &mut alerted, "flow:rework", &v);
            log_print(&format!("reconciler-flow: flow:rework → {}", status_word(&v)));
        }
    }

    // ── stage dwell regression ───────────────────────────────────────────────────────────
    match dwell_regression_metrics(&cfg.duckdb_bin, &cfg.spira_run, cfg.window_hours, cfg.baseline_hours) {
        Ok(rows) => {
            for state_name in DWELL_REGRESSION_STATES {
                let key = format!("flow:dwell-regression:{}", state_name.to_ascii_lowercase());
                let raw = match rows.iter().find(|(s, ..)| s == state_name) {
                    Some((_, cur_p90, cur_n, base_p90, base_n)) => dwell_regression_raw(&DwellRegressionObserved {
                        p90_seconds: *cur_p90,
                        n: *cur_n,
                        baseline_p90_seconds: *base_p90,
                        baseline_n: *base_n,
                    }),
                    None => dwell_regression_raw(&DwellRegressionObserved {
                        p90_seconds: 0.0,
                        n: 0,
                        baseline_p90_seconds: 0.0,
                        baseline_n: 0,
                    }),
                };
                let v = evaluate(&cfg, &mut state, &key, fresh(&export, raw));
                maybe_alert(&cfg, &mut alerted, &key, &v);
                log_print(&format!("reconciler-flow: {key} → {}", status_word(&v)));
            }
        }
        Err(e) => {
            for state_name in DWELL_REGRESSION_STATES {
                let key = format!("flow:dwell-regression:{}", state_name.to_ascii_lowercase());
                let v = evaluate(&cfg, &mut state, &key, unobservable(e.clone()));
                maybe_alert(&cfg, &mut alerted, &key, &v);
                log_print(&format!("reconciler-flow: {key} → {}", status_word(&v)));
            }
        }
    }

    let _ = save_state(&cfg.state_path, &state);
    let _ = save_alerted(&cfg.alerted_path, &alerted);
    let elapsed = unix_now().saturating_sub(cfg.now_secs);
    log_print(&format!("reconciler-flow: pass complete ({elapsed}s)"));

    drop(lock_file);
    Ok(())
}
