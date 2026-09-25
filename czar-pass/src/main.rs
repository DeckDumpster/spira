// Czar fast pass: deterministic queue-stall detection on a 30-second timer.
// Ported from czar.sh (sp-rpibz) per law-new-subsystems-are-rust (sp-54qsc).
//
// czar-pass --pass
//
// Reads only cheap sources: the landing.log tail since the last pass, the open batch
// record, landstate, CHECK7's last reason from sentinel.log, and at most 2 gh API
// calls (forge.sh batch-ci-status) for the open batch's run.

use reconciler_engine::core::{last_remedy, record_remedy, step, HysteresisState, RawStatus, Verdict};
use reconciler_engine::io::{append_status, load_state, save_state, StateMap};
use serde_json::Value;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.get(1).map(String::as_str) != Some("--pass") {
        eprintln!("usage: czar-pass --pass");
        return ExitCode::from(2);
    }
    match run_pass() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("czar-pass: {}", e);
            ExitCode::FAILURE
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Configuration
// ──────────────────────────────────────────────────────────────────────────────

struct Config {
    spira_run: PathBuf,
    spira_home: String,
    czar_log: PathBuf,
    qc_log: PathBuf,
    marker: PathBuf,
    incident_sh: String,
    forge_sh: String,
    queue_dir: PathBuf,
    stall_secs: u64,
    ci_queued_max: u64,
    starved_max_s: u64,
    strands_state: PathBuf,
    ci_red_max: u64,
    base_unreadable_grace: u64,
    lock_path: PathBuf,
    reconciler_state: PathBuf,
    reconciler_status_log: PathBuf,
    spira_db: String,
    scope_label: String,
    czar_label: String,
    express_label: String,
    throttle_stamp: PathBuf,
    land_unit: String,
    systemctl: String,
    repo_map: Option<PathBuf>,
    now_secs: u64,
    now_iso: String,
}

impl Config {
    fn from_env() -> Config {
        let spira_run_str =
            env::var("SPIRA_RUN").unwrap_or_else(|_| "/tmp/spira".to_string());
        let spira_run = PathBuf::from(&spira_run_str);
        let spira_home = env::var("SPIRA_HOME").unwrap_or_default();
        let now = unix_now();
        let iso = compute_now_iso();
        Config {
            czar_log: env::var("SPIRA_CZAR_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("czar.log")),
            qc_log: env::var("SPIRA_QUEUE_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("landing.log")),
            marker: env::var("SPIRA_CZAR_PASS_MARKER")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("czar-pass.swept")),
            incident_sh: env::var("SPIRA_INCIDENT_SH")
                .unwrap_or_else(|_| format!("{}/incident.sh", spira_home)),
            forge_sh: env::var("SPIRA_FORGE")
                .unwrap_or_else(|_| format!("{}/forge.sh", spira_home)),
            queue_dir: env::var("SPIRA_QUEUE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("queue")),
            stall_secs: env::var("SPIRA_LOOP_STALL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3000),
            ci_queued_max: env::var("SPIRA_CI_QUEUED_MAX_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(600),
            starved_max_s: env::var("SPIRA_STARVED_MAX_MINS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .map(|v| v * 60)
                .unwrap_or(1200),
            strands_state: env::var("SPIRA_STRANDS_STATE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("strands.json")),
            ci_red_max: env::var("SPIRA_CI_RED_MAX_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(600),
            base_unreadable_grace: env::var("SPIRA_BASE_CI_UNREADABLE_GRACE_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(120),
            lock_path: spira_run.join("czar-pass.lock"),
            reconciler_state: env::var("SPIRA_RECONCILER_STATE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("reconciler-state.json")),
            reconciler_status_log: env::var("SPIRA_RECONCILER_STATUS_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("reconciler-status.jsonl")),
            spira_db: env::var("SPIRA_DB").unwrap_or_default(),
            scope_label: env::var("SPIRA_SCOPE_LABEL").unwrap_or_default(),
            czar_label: env::var("SPIRA_CZAR_LABEL")
                .unwrap_or_else(|_| "czar-trigger".to_string()),
            express_label: env::var("SPIRA_EXPRESS_LABEL")
                .unwrap_or_else(|_| "express".to_string()),
            throttle_stamp: env::var("SPIRA_THROTTLE_STAMP")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("queue-throttled")),
            land_unit: env::var("SPIRA_LAND_UNIT")
                .unwrap_or_else(|_| "spira-landing".to_string()),
            systemctl: env::var("SPIRA_SYSTEMCTL")
                .unwrap_or_else(|_| "systemctl".to_string()),
            repo_map: env::var("SPIRA_REPO_MAP").ok().map(PathBuf::from),
            now_secs: now,
            now_iso: iso,
            spira_run,
            spira_home,
        }
    }

    fn stage(&self, class: &str) -> Stage {
        let key = format!(
            "SPIRA_CZAR_STAGE_{}",
            class.to_uppercase().replace('-', "_")
        );
        if env::var(&key).as_deref() == Ok("act") {
            Stage::Act
        } else {
            Stage::Shadow
        }
    }
}

#[derive(Clone, PartialEq)]
enum Stage {
    Shadow,
    Act,
}

// ──────────────────────────────────────────────────────────────────────────────
// Utilities
// ──────────────────────────────────────────────────────────────────────────────

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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

fn append_czar_log(path: &Path, content: &str) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(content.as_bytes());
    }
}

fn telem(cfg: &Config, class: &str, verdict: &Verdict, remedy: &str, tier: &str) {
    let detected = if verdict.is_gap { "yes" } else { "no" };
    let lat = verdict.since.map(|s| cfg.now_secs.saturating_sub(s)).unwrap_or(0);
    let line = format!(
        "{} CLASS={} DETECTED={} STATUS={} REMEDY={} TIER={} LATENCY={}s\n",
        cfg.now_iso, class, detected, status_word(verdict), remedy, tier, lat
    );
    append_czar_log(&cfg.czar_log, &line);
}

// ──────────────────────────────────────────────────────────────────────────────
// The reconciler invariant engine seam: every detector's hysteresis and remedy-
// verification runs through here instead of its own hand-rolled first-seen marker file
// (sp-pu7v6). `evaluate` does exactly three things: advance `key`'s HysteresisState by one
// pass, record the raw reading to the time series (every pass, not only when it changes —
// UNOBSERVABLE IS NEVER SATISFIED, so a forge call that failed is never indistinguishable
// from "nothing going on"), and return the Verdict a detector decides its action from.
// ──────────────────────────────────────────────────────────────────────────────

fn evaluate(cfg: &Config, state: &mut StateMap, key: &str, raw: RawStatus, grace_secs: u64) -> Verdict {
    let prev = state.remove(key).unwrap_or_default();
    let (verdict, next) = step(cfg.now_secs, raw, grace_secs, prev);
    append_status(&cfg.reconciler_status_log, &cfg.now_iso, key, &verdict);
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

// ──────────────────────────────────────────────────────────────────────────────
// Shadow / act helpers
// ──────────────────────────────────────────────────────────────────────────────

fn czar_would_log(cfg: &Config, class: &str, kind: &str, desc: &str) {
    append_czar_log(
        &cfg.czar_log,
        &format!(
            "{} CZAR-WOULD: {} {} — {}\n",
            cfg.now_iso, class, kind, desc
        ),
    );
}

fn infer(cfg: &Config, class: &str, ref_: &str, subj: &str, body: &str) {
    match cfg.stage(class) {
        Stage::Shadow => czar_would_log(cfg, class, "inference", subj),
        Stage::Act => {
            let labels = if cfg.scope_label.is_empty() {
                cfg.czar_label.clone()
            } else {
                format!("{},{}", cfg.scope_label, cfg.czar_label)
            };
            let mut child = Command::new("bash")
                .arg(&cfg.incident_sh)
                .arg("file")
                .arg(subj)
                .arg("-")
                .env("SPIRA_DB", &cfg.spira_db)
                .env("SPIRA_INCIDENT_LABELS", labels)
                .env("SPIRA_INCIDENT_TYPE", "task")
                .env("SPIRA_INCIDENT_PRIORITY", "1")
                .env("SPIRA_INCIDENT_ACTOR", "czar-pass")
                .env("SPIRA_SIN_EXEMPT", "1")
                .env("SPIRA_INCIDENT_REPO", "spira")
                .env("SPIRA_INCIDENT_REF", format!("incident:queue-{}", ref_))
                .env("SPIRA_INCIDENT_CAUSE", class)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            if let Ok(ref mut child) = child {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(body.as_bytes());
                }
                let _ = child.wait();
            }
            log_print(&format!(
                "czar pass: {} → inference (filed, summoning czar)",
                class
            ));
            summon_fayth_czar(cfg);
        }
    }
}

// A red base blocks every branch of its own repository, not one batch's members, so it is
// filed at P0 + express against the REPOSITORY THAT IS RED rather than at czar-pass's usual
// P1 against "spira" — a red main.py needs a builder in the failing repo, at the front of
// the queue, not a routine queue-health ticket. Priority does not imply express — the label
// is added explicitly, since incident.sh's `bd create` does not route through bead.sh.
fn infer_urgent(cfg: &Config, class: &str, ref_: &str, subj: &str, body: &str, repo: &str) {
    match cfg.stage(class) {
        Stage::Shadow => czar_would_log(cfg, class, "inference", subj),
        Stage::Act => {
            let mut labels = format!("plan,{}", cfg.express_label);
            if !cfg.scope_label.is_empty() {
                labels = format!("{},{}", cfg.scope_label, labels);
            }
            let mut child = Command::new("bash")
                .arg(&cfg.incident_sh)
                .arg("file")
                .arg(subj)
                .arg("-")
                .env("SPIRA_DB", &cfg.spira_db)
                .env("SPIRA_INCIDENT_LABELS", labels)
                .env("SPIRA_INCIDENT_TYPE", "bug")
                .env("SPIRA_INCIDENT_PRIORITY", "0")
                .env("SPIRA_INCIDENT_ACTOR", "czar-pass")
                .env("SPIRA_SIN_EXEMPT", "1")
                .env("SPIRA_INCIDENT_REPO", repo)
                .env("SPIRA_INCIDENT_REF", format!("incident:base-red:{}", ref_))
                .env("SPIRA_INCIDENT_CAUSE", class)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            if let Ok(ref mut child) = child {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(body.as_bytes());
                }
                let _ = child.wait();
            }
            log_print(&format!(
                "czar pass: {} → inference (filed, summoning czar)",
                class
            ));
            summon_fayth_czar(cfg);
        }
    }
}

fn det_action(cfg: &Config, class: &str, desc: &str, action: impl FnOnce()) {
    match cfg.stage(class) {
        Stage::Shadow => czar_would_log(cfg, class, "det", desc),
        Stage::Act => {
            log_print(&format!("czar pass: {} → det: {}", class, desc));
            action();
        }
    }
}

fn summon_fayth_czar(cfg: &Config) {
    if cfg.spira_run.join("world.halted").exists() {
        return;
    }
    let script = format!(". \"{}/lib.sh\" && summon_fayth czar", cfg.spira_home);
    let _ = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .stderr(Stdio::null())
        .status();
}

// ──────────────────────────────────────────────────────────────────────────────
// Repo map lookup
// ──────────────────────────────────────────────────────────────────────────────

fn repo_root(name: &str, repo_map: &Option<PathBuf>) -> Option<PathBuf> {
    let map_path = repo_map.as_ref()?;
    let content = fs::read_to_string(map_path).ok()?;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with('#') || t.is_empty() {
            continue;
        }
        let parts: Vec<&str> = t.splitn(6, '|').collect();
        if parts.len() >= 2 {
            let n = parts[0].trim();
            let p = parts[1].trim();
            if n == name && !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
    }
    None
}

// The base ref a repository's branches are cut from and judged against (repo-map column
// 4), reduced to a bare branch name: `gh run list --branch` wants "main", not "origin/main".
// Empty in repo-map means "let spira_landref resolve it" (doctor.sh's own leave-empty
// convention) — czar-pass has no such resolver, so an empty or absent base skips base-red
// for that repo rather than guessing.
fn repo_base(name: &str, repo_map: &Option<PathBuf>) -> Option<String> {
    let map_path = repo_map.as_ref()?;
    let content = fs::read_to_string(map_path).ok()?;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with('#') || t.is_empty() {
            continue;
        }
        let parts: Vec<&str> = t.splitn(6, '|').collect();
        if parts.len() >= 4 && parts[0].trim() == name {
            let base = parts[3].trim();
            if base.is_empty() {
                return None;
            }
            return Some(base.rsplit('/').next().unwrap_or(base).to_string());
        }
    }
    None
}

// ──────────────────────────────────────────────────────────────────────────────
// Queue / forge helpers
// ──────────────────────────────────────────────────────────────────────────────

// Find all queue/<repo>/open files; returns (repo_name, open_path) pairs.
fn find_open_files(queue_dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(queue_dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let open = dir.join("open");
        if open.is_file() {
            let name = dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            out.push((name, open));
        }
    }
    out
}

fn read_branch(open_path: &Path) -> Option<String> {
    let content = fs::read_to_string(open_path).ok()?;
    for line in content.lines() {
        if let Some(branch) = line.strip_prefix("branch=") {
            return Some(branch.to_string());
        }
    }
    None
}

fn file_mtime(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

// A failed forge call and "nothing going on" must stay distinguishable (sp-pu7v6): a
// spawn failure, a non-zero exit or invalid UTF-8 all report Err, so a caller can report
// its invariant unobservable instead of silently reading the failure as "satisfied".
fn run_forge(forge_sh: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("bash");
    cmd.arg(forge_sh);
    for a in args {
        cmd.arg(a);
    }
    let output = cmd
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("forge.sh spawn failed: {}", e))?;
    if !output.status.success() {
        return Err(format!("forge.sh exited {}", output.status));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("forge.sh: non-utf8 output: {}", e))
}

fn parse_field(output: &str, key: &str) -> Option<String> {
    let prefix = format!("{}: ", key);
    output
        .lines()
        .find(|l| l.starts_with(&prefix))
        .map(|l| l[prefix.len()..].trim().to_string())
}

fn parse_iso_to_epoch(ts: &str) -> Option<u64> {
    Command::new("date")
        .args(["-u", "-d", ts, "+%s"])
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
}

// ──────────────────────────────────────────────────────────────────────────────
// Main pass
// ──────────────────────────────────────────────────────────────────────────────

fn run_pass() -> Result<(), String> {
    let cfg = Config::from_env();

    // World halted
    if cfg.spira_run.join("world.halted").exists() {
        log_print("czar pass: skipped — world is halted");
        return Ok(());
    }

    // Flock — non-blocking: if another pass is running, skip.
    let lock_file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&cfg.lock_path)
        .map_err(|e| format!("open lock {}: {}", cfg.lock_path.display(), e))?;
    if unsafe { flock(lock_file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
        log_print("czar pass: already running — skip");
        return Ok(());
    }

    // New log entries since last pass marker.
    let prev = fs::read_to_string(&cfg.marker)
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let new_lines = read_new_lines(&cfg.qc_log, &prev);

    // CHECK7 reason from sentinel.log — used by the starved detector.
    let check7 = read_check7(&cfg.spira_run);

    // Every invariant's hysteresis lives in one persisted state map, loaded once per pass
    // and saved once per pass (sp-pu7v6) — replacing a flat first-seen marker file per class.
    let mut state = load_state(&cfg.reconciler_state);

    // ── DETECTOR: deadlock ────────────────────────────────────────────────────
    let (dl_v, dl_rem, dl_tier) = detect_deadlock(&cfg, &mut state, &new_lines);

    // ── DETECTOR: attribution-failed ──────────────────────────────────────────
    let (af_v, af_rem, af_tier) = detect_attribution_failed(&cfg, &mut state, &new_lines);

    // ── DETECTOR: sort-failed ─────────────────────────────────────────────────
    let (sf_v, sf_rem, sf_tier) = detect_sort_failed(&cfg, &mut state, &new_lines);

    // ── DETECTOR: loop-stalled ────────────────────────────────────────────────
    let (ls_v, ls_rem, ls_tier) = detect_loop_stalled(&cfg, &mut state);

    // ── DETECTORS: ci-stalled + ci-red (shared forge calls) ──────────────────
    let (cis_v, cis_rem, cis_tier, cir_v, cir_rem, cir_tier) = detect_ci(&cfg, &mut state);

    // ── DETECTOR: base-red (the base ref's own gate run, not a batch's) ──────
    let (br_v, br_rem, br_tier) = detect_base_red(&cfg, &mut state);

    // ── DETECTOR: starved ─────────────────────────────────────────────────────
    let (sv_v, sv_rem, sv_tier) = detect_starved(&cfg, &mut state, &check7);

    // Telemetry — one line per class per pass.
    telem(&cfg, "deadlock",           &dl_v,  dl_rem,  dl_tier);
    telem(&cfg, "attribution-failed", &af_v,  af_rem,  af_tier);
    telem(&cfg, "sort-failed",        &sf_v,  sf_rem,  sf_tier);
    telem(&cfg, "loop-stalled",       &ls_v,  ls_rem,  ls_tier);
    telem(&cfg, "ci-stalled",         &cis_v, cis_rem, cis_tier);
    telem(&cfg, "ci-red",             &cir_v, cir_rem, cir_tier);
    telem(&cfg, "base-red",           &br_v,  br_rem,  br_tier);
    telem(&cfg, "starved",            &sv_v,  sv_rem,  sv_tier);

    let _ = save_state(&cfg.reconciler_state, &state);

    // Marker
    let _ = fs::write(&cfg.marker, format!("{}\n", cfg.now_iso));

    let elapsed = unix_now().saturating_sub(cfg.now_secs);
    log_print(&format!("czar pass: complete ({}s)", elapsed));

    drop(lock_file);
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Log reading
// ──────────────────────────────────────────────────────────────────────────────

fn read_new_lines(qc_log: &Path, prev: &str) -> String {
    if !qc_log.exists() {
        return String::new();
    }
    let content = fs::read_to_string(qc_log).unwrap_or_default();
    if prev.is_empty() {
        let lines: Vec<&str> = content.lines().collect();
        let start = lines.len().saturating_sub(200);
        lines[start..].join("\n")
    } else {
        content
            .lines()
            .filter(|l| {
                l.split_whitespace()
                    .next()
                    .map(|ts| ts > prev)
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn read_check7(spira_run: &Path) -> String {
    let log = spira_run.join("sentinel.log");
    if !log.exists() {
        return String::new();
    }
    let content = fs::read_to_string(&log).unwrap_or_default();
    let lines: Vec<&str> = content.lines().filter(|l| l.contains("CHECK7")).collect();
    let start = lines.len().saturating_sub(20);
    lines[start..].join("\n")
}

// ──────────────────────────────────────────────────────────────────────────────
// Individual detectors — return (detected_str, remedy_str, tier_str)
// ──────────────────────────────────────────────────────────────────────────────

fn detect_deadlock(cfg: &Config, state: &mut StateMap, new_lines: &str) -> (Verdict, &'static str, &'static str) {
    let trigger = "no suites identified; leaving batch open";
    let raw = if new_lines.contains(trigger) {
        RawStatus::Gap {
            desired: "batch closed or requeued".into(),
            observed: trigger.into(),
            since_hint: None,
        }
    } else {
        RawStatus::Satisfied
    };
    let verdict = evaluate(cfg, state, "deadlock", raw, 0);
    if !verdict.is_gap {
        return (verdict, "none", "det");
    }

    let ctx: String = new_lines
        .lines()
        .filter(|l| l.contains(trigger))
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");

    // A deterministic remedy is attempted once per streak; if it is still a gap on the
    // next pass, remedy_failed says so and this pass escalates instead of retrying blind.
    let mut run_id = String::new();
    let mut repo_path = String::new();
    if !verdict.remedy_failed && cfg.queue_dir.is_dir() {
        'outer: for (repo_name, open_path) in find_open_files(&cfg.queue_dir) {
            if let Some(branch) = read_branch(&open_path) {
                if let Some(rp) = repo_root(&repo_name, &cfg.repo_map) {
                    let rp_str = rp.to_string_lossy().to_string();
                    let out = run_forge(&cfg.forge_sh, &["run-id", &rp_str, &branch]);
                    let rid = out.map(|s| s.trim().to_string()).unwrap_or_default();
                    if !rid.is_empty() {
                        run_id = rid;
                        repo_path = rp_str;
                        break 'outer;
                    }
                }
            }
        }
    }

    if !run_id.is_empty() {
        let desc = format!("workflow-rerun {}", run_id);
        let forge = cfg.forge_sh.clone();
        let rid = run_id.clone();
        let rp = repo_path.clone();
        det_action(cfg, "deadlock", &desc, move || {
            let _ = Command::new("bash").arg(&forge).args(["workflow-rerun", &rp, &rid]).status();
        });
        if let Some(st) = state.get_mut("deadlock") {
            record_remedy(st, cfg.now_secs, &desc);
        }
        (verdict, "det-rerun", "det")
    } else {
        let tried = last_remedy(state.get("deadlock").unwrap_or(&HysteresisState::default()))
            .map(|d| format!("\nLast remedy tried: {}\n", d))
            .unwrap_or_default();
        let body = format!(
            "verdict left the batch PR open after a red with no attributable suite.\n\
             The queue cannot advance until the batch is closed or requeued by hand.\n\
             {}\nRecent log lines:\n{}\n",
            tried, ctx
        );
        infer(
            cfg,
            "deadlock",
            "deadlock-batch-open",
            "QUEUE: batch open — no suites identified (DEADLOCK)",
            &body,
        );
        (verdict, "inference", "inf")
    }
}

fn detect_attribution_failed(cfg: &Config, state: &mut StateMap, new_lines: &str) -> (Verdict, &'static str, &'static str) {
    // grep -qE 'ejected 0, requeued [1-9]'
    let detected = new_lines.lines().any(|l| {
        if let Some(pos) = l.find("ejected 0, requeued ") {
            let rest = &l[pos + "ejected 0, requeued ".len()..];
            rest.chars().next().map(|c| c.is_ascii_digit() && c != '0').unwrap_or(false)
        } else {
            false
        }
    });
    let raw = if detected {
        RawStatus::Gap {
            desired: "attribution ejects the offender".into(),
            observed: "ejected 0, requeued the whole batch".into(),
            since_hint: None,
        }
    } else {
        RawStatus::Satisfied
    };
    let verdict = evaluate(cfg, state, "attribution-failed", raw, 0);
    if !verdict.is_gap {
        return (verdict, "none", "det");
    }
    let ctx: String = new_lines
        .lines()
        .filter(|l| l.contains("ejected 0, requeued"))
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    let body = format!(
        "Attribution found no branch to isolate the offender.\n\
         The entire batch was requeued with the failing member still in it; this loops.\n\n\
         Recent log lines:\n{}\n",
        ctx
    );
    infer(
        cfg,
        "attribution-failed",
        "attribution-failed-requeue",
        "QUEUE: attribution ejected 0, requeued whole batch",
        &body,
    );
    (verdict, "inference", "inf")
}

fn detect_sort_failed(cfg: &Config, state: &mut StateMap, new_lines: &str) -> (Verdict, &'static str, &'static str) {
    let raw = if new_lines.contains("queue_sort_rows: ranking failed") {
        RawStatus::Gap {
            desired: "queue sorted by priority".into(),
            observed: "queue_sort_rows: ranking failed".into(),
            since_hint: None,
        }
    } else {
        RawStatus::Satisfied
    };
    let verdict = evaluate(cfg, state, "sort-failed", raw, 0);
    if !verdict.is_gap {
        return (verdict, "none", "det");
    }
    let ctx: String = new_lines
        .lines()
        .filter(|l| l.contains("queue_sort_rows: ranking failed"))
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    let body = format!(
        "The queue sorter failed and fell back to unranked order.\n\
         Priority ordering is suspended until the sorter recovers.\n\n\
         Recent log lines:\n{}\n",
        ctx
    );
    infer(
        cfg,
        "sort-failed",
        "sort-failed-ranking",
        "QUEUE: queue_sort_rows ranking failed",
        &body,
    );
    (verdict, "inference", "inf")
}

fn detect_loop_stalled(cfg: &Config, state: &mut StateMap) -> (Verdict, &'static str, &'static str) {
    let last_ts: Option<String> = if cfg.qc_log.exists() {
        fs::read_to_string(&cfg.qc_log)
            .ok()
            .and_then(|c| {
                c.lines()
                    .filter(|l| l.contains("landing: pass complete"))
                    .last()
                    .and_then(|l| l.get(..20))
                    .map(|s| s.to_string())
            })
    } else {
        None
    };

    // No log yet, or no pass-complete line yet, is a legitimate fresh-install absence —
    // not a failure to observe. An unparseable timestamp IS a failure to observe: the old
    // code silently read that as "satisfied", which is exactly the bug this engine closes
    // (law-a-control-that-cannot-check-must-refuse).
    let raw = match &last_ts {
        None => RawStatus::Satisfied,
        Some(ts) => match parse_iso_to_epoch(ts) {
            None => RawStatus::Unobservable { reason: format!("landing.log timestamp unparseable: {:?}", ts) },
            Some(ts_epoch) => {
                let age = cfg.now_secs.saturating_sub(ts_epoch);
                if age > cfg.stall_secs {
                    RawStatus::Gap {
                        desired: format!("a landing pass within {}s", cfg.stall_secs),
                        observed: format!("last pass {}s ago ({})", age, ts),
                        since_hint: Some(ts_epoch),
                    }
                } else {
                    RawStatus::Satisfied
                }
            }
        },
    };

    let verdict = evaluate(cfg, state, "loop-stalled", raw, 0);
    if !verdict.is_gap {
        return (verdict, "none", "det");
    }
    if matches!(verdict.status, RawStatus::Unobservable { .. }) {
        return (verdict, "none", "det");
    }

    let age = verdict.since.map(|s| cfg.now_secs.saturating_sub(s)).unwrap_or(0);
    let unit_service = format!("{}.service", cfg.land_unit);
    let is_failed = Command::new(&cfg.systemctl)
        .args(["--user", "is-failed", &unit_service])
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim() == "failed")
        .unwrap_or(false);

    if is_failed && !verdict.remedy_failed {
        let sc = cfg.systemctl.clone();
        let svc = unit_service.clone();
        let desc = format!("reset-failed + start {}", cfg.land_unit);
        det_action(cfg, "loop-stalled", &desc, move || {
            let _ = Command::new(&sc).args(["--user", "reset-failed", &svc]).status();
            let _ = Command::new(&sc).args(["--user", "start", &svc]).status();
        });
        if let Some(st) = state.get_mut("loop-stalled") {
            record_remedy(st, cfg.now_secs, &desc);
        }
        (verdict, "det-restart", "det")
    } else {
        let body = format!(
            "No landing: pass complete in the last {}s (threshold: {}s).\n\nSee: {}\n",
            age,
            cfg.stall_secs,
            cfg.qc_log.display()
        );
        infer(
            cfg,
            "loop-stalled",
            "loop-stalled",
            &format!("QUEUE: landing loop stalled — no pass for {}s", age),
            &body,
        );
        (verdict, "inference", "inf")
    }
}

/// The worst reading across a set of per-repo readings for one invariant class: a real gap
/// outranks "could not tell", which outranks satisfied. Used only to roll per-repo verdicts
/// up into the one CLASS= line czar.log has always printed per invariant; each repo's own
/// verdict — and its own hysteresis — is still tracked and recorded separately.
fn worst_of(statuses: Vec<RawStatus>) -> RawStatus {
    if let Some(gap) = statuses.iter().find(|s| matches!(s, RawStatus::Gap { .. })) {
        return gap.clone();
    }
    if let Some(unobs) = statuses.into_iter().find(|s| matches!(s, RawStatus::Unobservable { .. })) {
        return unobs;
    }
    RawStatus::Satisfied
}

fn detect_ci(cfg: &Config, state: &mut StateMap) -> (Verdict, &'static str, &'static str, Verdict, &'static str, &'static str) {
    let mut cis_remedy: &'static str = "none";
    let mut cis_tier: &'static str = "det";
    let mut cir_remedy: &'static str = "none";
    let mut cir_tier: &'static str = "det";
    let mut cis_statuses: Vec<RawStatus> = Vec::new();
    let mut cir_statuses: Vec<RawStatus> = Vec::new();

    if cfg.queue_dir.is_dir() {
        for (repo_name, open_path) in find_open_files(&cfg.queue_dir) {
            let branch = match read_branch(&open_path) {
                Some(b) => b,
                None => continue,
            };
            let repo_path = match repo_root(&repo_name, &cfg.repo_map) {
                Some(p) => p,
                None => continue,
            };
            let repo_path_str = repo_path.to_string_lossy().to_string();
            let repo_safe = repo_name.replace(',', "-");
            let cis_key = format!("ci-stalled:{}", repo_safe);
            let cir_key = format!("ci-red:{}", repo_safe);

            let ci_out = match run_forge(&cfg.forge_sh, &["batch-ci-status", &repo_path_str, &branch]) {
                Ok(out) => out,
                Err(e) => {
                    // A failed forge call is not "no CI activity": both invariants for this
                    // repo are unobservable this pass, never silently read as satisfied.
                    let reason = format!("{}: batch-ci-status: {}", repo_name, e);
                    let v1 = evaluate(cfg, state, &cis_key, RawStatus::Unobservable { reason: reason.clone() }, 0);
                    cis_statuses.push(v1.status);
                    let v2 = evaluate(cfg, state, &cir_key, RawStatus::Unobservable { reason }, 0);
                    cir_statuses.push(v2.status);
                    continue;
                }
            };
            let run_id = parse_field(&ci_out, "run-id").unwrap_or_default();
            let queued_since = parse_field(&ci_out, "queued-since").and_then(|s| s.parse::<u64>().ok());

            // ci-stalled: job queued with no runner > threshold
            let cis_raw = match queued_since {
                Some(qs) => {
                    let stalled_s = cfg.now_secs.saturating_sub(qs);
                    if stalled_s > cfg.ci_queued_max {
                        RawStatus::Gap {
                            desired: format!("CI job picked up within {}s", cfg.ci_queued_max),
                            observed: format!("{}: queued {}s", repo_name, stalled_s),
                            since_hint: Some(qs),
                        }
                    } else {
                        RawStatus::Satisfied
                    }
                }
                None => RawStatus::Satisfied,
            };
            let cis_v = evaluate(cfg, state, &cis_key, cis_raw, 0);
            cis_statuses.push(cis_v.status.clone());
            if cis_v.is_gap {
                let stalled_s = cis_v.since.map(|s| cfg.now_secs.saturating_sub(s)).unwrap_or(0);
                if !run_id.is_empty() && !cis_v.remedy_failed {
                    let desc = format!("workflow-rerun {} ({}, queued {}s)", run_id, repo_name, stalled_s);
                    let forge = cfg.forge_sh.clone();
                    let rp = repo_path_str.clone();
                    let rid = run_id.clone();
                    det_action(cfg, "ci-stalled", &desc, move || {
                        let _ = Command::new("bash").arg(&forge).args(["workflow-rerun", &rp, &rid]).status();
                    });
                    if let Some(st) = state.get_mut(&cis_key) {
                        record_remedy(st, cfg.now_secs, &desc);
                    }
                    cis_remedy = "det-rerun";
                } else {
                    let body = format!(
                        "A CI job for the open batch in {} has been queued for {}s \
                         (threshold: {}s).\nBranch: {}\n\n\
                         Cancel the stuck run and re-dispatch the whole workflow.\n\
                         Never rerun --failed: that strands the run on the torn-down VM label.\n",
                        repo_name, stalled_s, cfg.ci_queued_max, branch
                    );
                    infer(
                        cfg,
                        "ci-stalled",
                        &format!("ci-stalled-{}", repo_safe),
                        &format!("QUEUE: CI job queued with no runner for {}s ({})", stalled_s, repo_name),
                        &body,
                    );
                    cis_remedy = "inference";
                    cis_tier = "inf";
                }
            }

            // ci-red: run completed failure, verdict not acting.
            // Measures from run-completed-at (CI completion), not from batch open mtime.
            // If run-completed-at is absent, that reading is Satisfied: a batch's own gate
            // hasn't reported a conclusion, distinct from base-red's "no run at all" case
            // below, which is a repo's base ref, not a batch's PR, and unobservable there.
            let conclusion = parse_field(&ci_out, "run-conclusion");
            let completed_secs = parse_field(&ci_out, "run-completed-at").and_then(|s| s.trim().parse::<u64>().ok());
            let cir_raw = match (conclusion.as_deref(), completed_secs) {
                (Some("failure"), Some(completed_secs)) => {
                    let red_age = cfg.now_secs.saturating_sub(completed_secs);
                    if red_age > cfg.ci_red_max {
                        RawStatus::Gap {
                            desired: "verdict acts on a red run".into(),
                            observed: format!("{}: red for {}s", repo_name, red_age),
                            since_hint: Some(completed_secs),
                        }
                    } else {
                        RawStatus::Satisfied
                    }
                }
                _ => RawStatus::Satisfied,
            };
            let cir_v = evaluate(cfg, state, &cir_key, cir_raw, 0);
            cir_statuses.push(cir_v.status.clone());
            if cir_v.is_gap {
                let red_age = cir_v.since.map(|s| cfg.now_secs.saturating_sub(s)).unwrap_or(0);
                let body = format!(
                    "CI run completed red for the open batch in {}.\n\
                     Red for {}s (threshold {}s); verdict has not acted.\n\
                     Branch: {}\nInvestigate the red and act.\n",
                    repo_name, red_age, cfg.ci_red_max, branch
                );
                infer(
                    cfg,
                    "ci-red",
                    &format!("ci-red-{}", repo_safe),
                    &format!("QUEUE: CI run completed red, verdict not acting for {}s ({})", red_age, repo_name),
                    &body,
                );
                cir_remedy = "inference";
                cir_tier = "inf";
            }
        }
    }

    let cis_agg = evaluate(cfg, state, "ci-stalled", worst_of(cis_statuses), 0);
    let cir_agg = evaluate(cfg, state, "ci-red", worst_of(cir_statuses), 0);
    (cis_agg, cis_remedy, cis_tier, cir_agg, cir_remedy, cir_tier)
}

// The base ref's OWN gate run, as opposed to ci-red above (a batch's PR run). A batch PR
// runs diff-selected suites; the push that lands it on the base ref runs the whole corpus,
// so a batch can go green and land red with nothing else watching for that. Fires at once
// on the first red pass — no age threshold — because the entire point is to beat "cut by
// hand two hours later" (sp-eve0i), and infer_urgent files at P0 + express so it is not
// merely first in a P0 queue that is itself hours deep.
//
// UNREADABLE IS NOT GREEN (law-a-control-that-cannot-check-must-refuse): when the forge
// seam returns no run at all for the base branch, that is filed too, distinctly, once it
// has persisted past base_unreadable_grace — long enough that it is not just GitHub not
// yet having created the run row for a push that landed seconds ago.
fn detect_base_red(cfg: &Config, state: &mut StateMap) -> (Verdict, &'static str, &'static str) {
    let mut remedy: &'static str = "none";
    let mut tier: &'static str = "det";
    let mut statuses: Vec<RawStatus> = Vec::new();

    if cfg.queue_dir.is_dir() {
        for (repo_name, _open_path) in find_open_files(&cfg.queue_dir) {
            let repo_path = match repo_root(&repo_name, &cfg.repo_map) {
                Some(p) => p,
                None => continue,
            };
            let base_branch = match repo_base(&repo_name, &cfg.repo_map) {
                Some(b) => b,
                None => continue,
            };
            let repo_path_str = repo_path.to_string_lossy().to_string();
            let repo_safe = repo_name.replace(',', "-");
            let key = format!("base-red:{}", repo_safe);

            let ci_out = run_forge(&cfg.forge_sh, &["batch-ci-status", &repo_path_str, &base_branch]);
            let run_id = ci_out.as_ref().ok().and_then(|out| parse_field(out, "run-id"));

            if run_id.is_none() {
                // No run at all for the base branch — the same grace period the old code
                // gave a fresh marker file applies here as the engine's own grace_secs, so
                // an ordinary push whose run GitHub has not created yet stays quiet.
                let reason = match &ci_out {
                    Err(e) => format!("{}'s base ({}): {}", repo_name, base_branch, e),
                    Ok(_) => format!("{}'s base ({}) CI status returned no run information", repo_name, base_branch),
                };
                let v = evaluate(cfg, state, &key, RawStatus::Unobservable { reason }, cfg.base_unreadable_grace);
                statuses.push(v.status.clone());
                if v.is_gap {
                    let age = v.since.map(|s| cfg.now_secs.saturating_sub(s)).unwrap_or(0);
                    let body = format!(
                        "{}'s base ({}) CI status could not be read for {}s — the forge \
                         seam returned no run information for that branch.\n\
                         A base whose status cannot be read is treated as failed, not \
                         green: nothing here can tell you it is safe to land on.\n",
                        repo_name, base_branch, age
                    );
                    infer_urgent(
                        cfg,
                        "base-red",
                        &format!("{}:unreadable", repo_name),
                        &format!("QUEUE: {}'s base CI status could not be read", repo_name),
                        &body,
                        &repo_name,
                    );
                    remedy = "inference";
                    tier = "inf";
                }
                continue;
            }
            let ci_out = ci_out.expect("run_id.is_some() implies ci_out was Ok");

            let conclusion = parse_field(&ci_out, "run-conclusion");
            let raw = if conclusion.as_deref() == Some("failure") {
                RawStatus::Gap {
                    desired: format!("{}'s base gate is green", repo_name),
                    observed: format!("{}'s base ({}) gate run completed red", repo_name, base_branch),
                    since_hint: None,
                }
            } else {
                RawStatus::Satisfied
            };
            let v = evaluate(cfg, state, &key, raw, 0);
            statuses.push(v.status.clone());
            if v.is_gap {
                let sha = parse_field(&ci_out, "head-sha").unwrap_or_else(|| "unknown".to_string());
                let run_url = parse_field(&ci_out, "run-url").unwrap_or_else(|| "(no run url)".to_string());
                let mut suites: Vec<String> = ci_out
                    .lines()
                    .filter_map(|l| l.strip_prefix("red-suite: ").map(|s| s.to_string()))
                    .collect();
                suites.sort();
                suites.dedup();
                let suite_key = if suites.is_empty() { "unknown".to_string() } else { suites.join(",") };
                let suite_list = if suites.is_empty() { "(suite names unavailable)".to_string() } else { suites.join(", ") };

                let body = format!(
                    "{}'s own base ({}) gate run completed red.\n\n\
                     failing suites   {}\n\
                     base sha         {}\n\
                     run url          {}\n\n\
                     Nothing else stops the next batch landing on this red base. Post-hoc \
                     attribution is acceptable — naming who caused it is not required —\
                     but this bead does not close until the base is green again.\n",
                    repo_name, base_branch, suite_list, sha, run_url
                );
                infer_urgent(
                    cfg,
                    "base-red",
                    &format!("{}:{}", repo_name, suite_key),
                    &format!("QUEUE: {}'s base is red — {}", repo_name, suite_list),
                    &body,
                    &repo_name,
                );
                remedy = "inference";
                tier = "inf";
            }
        }
    }

    let agg = evaluate(cfg, state, "base-red", worst_of(statuses), 0);
    (agg, remedy, tier)
}

fn detect_starved(cfg: &Config, state: &mut StateMap, check7: &str) -> (Verdict, &'static str, &'static str) {
    if !cfg.strands_state.exists() {
        let v = evaluate(cfg, state, "starved", RawStatus::Satisfied, 0);
        return (v, "none", "det");
    }

    // A missing file is a legitimate "nothing tracked yet" absence; a file that exists but
    // cannot be read or parsed is a failure to observe, not evidence every partition is
    // fine (law-a-control-that-cannot-check-must-refuse) — the old code read both the same
    // way, silently, as "no".
    let content = match fs::read_to_string(&cfg.strands_state) {
        Ok(c) => c,
        Err(e) => {
            let v = evaluate(cfg, state, "starved", RawStatus::Unobservable { reason: format!("strands.json: {}", e) }, 0);
            return (v, "none", "det");
        }
    };
    let parsed: Result<Value, _> = serde_json::from_str(&content);
    let obj = match parsed.as_ref().ok().and_then(|v| v.as_object()) {
        Some(o) => o,
        None => {
            let v = evaluate(
                cfg,
                state,
                "starved",
                RawStatus::Unobservable { reason: "strands.json: not a JSON object".into() },
                0,
            );
            return (v, "none", "det");
        }
    };

    let mut remedy: &'static str = "none";
    let mut tier: &'static str = "det";
    let mut statuses: Vec<RawStatus> = Vec::new();

    for (key, val) in obj {
        let parts: Vec<&str> = key.splitn(3, ':').collect();
        if parts.len() != 3 || parts[1] != "starved" {
            continue;
        }
        let first_val = match val.get("first").and_then(|v| v.as_f64()) {
            Some(f) => f as u64,
            None => continue,
        };
        let part = parts[0];
        let sv_safe = part.replace(',', "-").replace(' ', "-");
        let raw = RawStatus::Gap {
            desired: "ready work has a serving aeon".into(),
            observed: format!("partition [{}] starved", part),
            since_hint: Some(first_val),
        };
        let v = evaluate(cfg, state, &format!("starved:{}", sv_safe), raw, cfg.starved_max_s);
        statuses.push(v.status.clone());
        if !v.is_gap {
            continue;
        }
        let age = v.since.map(|s| cfg.now_secs.saturating_sub(s)).unwrap_or(0);

        match cfg.stage("starved") {
            Stage::Shadow => {
                append_czar_log(
                    &cfg.czar_log,
                    &format!(
                        "{} CZAR-WOULD: starved inference — partition [{}] starved {}m\n",
                        cfg.now_iso,
                        part,
                        age / 60
                    ),
                );
                remedy = "shadow-inference";
                tier = "inf";
            }
            Stage::Act => {
                let tc_stamp = &cfg.throttle_stamp;
                let throttle_active = tc_stamp.exists()
                    && check7.contains("throttle active");
                let deliberate = check7.contains("SPIRA_MAX_AEONS=0")
                    || check7.contains("world.halted")
                    || check7.contains("suspended");

                if throttle_active {
                    let wt = format!("{}/watchtower.sh", cfg.spira_home);
                    let _ = Command::new("bash").arg(&wt).arg("--throttle-check").status();
                    log_print(&format!(
                        "czar pass: starved → recomputed throttle depth (partition: {})",
                        part
                    ));
                    remedy = "det-throttle-recompute";
                } else if deliberate {
                    log_print(&format!(
                        "czar pass: starved → deliberate state (partition: {})",
                        part
                    ));
                    remedy = "deliberate-state";
                } else {
                    let check7_tail: String = check7
                        .lines()
                        .rev()
                        .take(5)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join("\n");
                    let body = format!(
                        "Ready work in partition [{}] has had no serving aeons for {}m \
                         (threshold: {}m).\n\nCHECK7 reason (sentinel.log):\n{}\n",
                        part,
                        age / 60,
                        cfg.starved_max_s / 60,
                        check7_tail
                    );
                    infer(
                        cfg,
                        "starved",
                        &format!("starved-{}", sv_safe),
                        &format!(
                            "QUEUE: partition [{}] starved for {}m",
                            part,
                            age / 60
                        ),
                        &body,
                    );
                    remedy = "inference";
                    tier = "inf";
                }
            }
        }
    }

    let agg = evaluate(cfg, state, "starved", worst_of(statuses), 0);
    (agg, remedy, tier)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    // A private scratch directory per test, so parallel `cargo test` threads never collide
    // on the same path (the real callers always get SPIRA_RUN handed to them by the caller,
    // never a shared ambient default).
    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "czar-pass-test-{}-{}-{}",
            name,
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parse_field_reads_the_named_key() {
        let out = "run-id: 42\nrun-conclusion: failure\nhead-sha: abc123\n";
        assert_eq!(parse_field(out, "run-id"), Some("42".to_string()));
        assert_eq!(parse_field(out, "run-conclusion"), Some("failure".to_string()));
    }

    #[test]
    fn parse_field_is_anchored_to_the_prefix_not_a_substring() {
        // "run-id" must not match inside "run-id-extra: x" — the prefix carries ": ".
        let out = "run-id-extra: x\n";
        assert_eq!(parse_field(out, "run-id"), None);
    }

    #[test]
    fn parse_field_missing_key_is_none() {
        assert_eq!(parse_field("run-id: 42\n", "queued-since"), None);
    }

    #[test]
    fn parse_iso_to_epoch_reads_a_known_instant() {
        // 2021-01-01T00:00:00Z is a fixed, well-known epoch value.
        assert_eq!(parse_iso_to_epoch("2021-01-01T00:00:00Z"), Some(1_609_459_200));
    }

    #[test]
    fn parse_iso_to_epoch_rejects_garbage() {
        assert_eq!(parse_iso_to_epoch("not-a-timestamp"), None);
    }

    #[test]
    fn fs_path_replaces_class_separators_that_would_escape_the_directory() {
        let dir = PathBuf::from("/tmp/spira-run");
        let p = fs_path(&dir, "queue/red,flaky");
        assert_eq!(p, PathBuf::from("/tmp/spira-run/czar-pass-first.queue-red-flaky"));
    }

    #[test]
    fn fs_record_is_first_write_wins() {
        let dir = scratch_dir("fs-record-first-write-wins");
        fs_record(&dir, "deadlock", 100);
        fs_record(&dir, "deadlock", 200); // must not overwrite the first timestamp
        assert_eq!(fs_get(&dir, "deadlock"), Some(100));
    }

    #[test]
    fn fs_clear_removes_the_marker() {
        let dir = scratch_dir("fs-clear");
        fs_record(&dir, "deadlock", 100);
        fs_clear(&dir, "deadlock");
        assert_eq!(fs_get(&dir, "deadlock"), None);
    }

    #[test]
    fn latency_secs_is_zero_with_no_recorded_first_sighting() {
        let dir = scratch_dir("latency-no-record");
        assert_eq!(latency_secs(&dir, "deadlock", 500), 0);
    }

    #[test]
    fn latency_secs_measures_from_the_first_sighting() {
        let dir = scratch_dir("latency-measures");
        fs_record(&dir, "deadlock", 100);
        assert_eq!(latency_secs(&dir, "deadlock", 350), 250);
    }

    #[test]
    fn repo_root_finds_the_named_repo() {
        let dir = scratch_dir("repo-root");
        let map = dir.join("repo-map");
        fs::write(&map, "spira | /srv/checkouts/spira | queue | origin/main | | \n").unwrap();
        assert_eq!(
            repo_root("spira", &Some(map)),
            Some(PathBuf::from("/srv/checkouts/spira"))
        );
    }

    #[test]
    fn repo_root_with_no_map_configured_is_none() {
        assert_eq!(repo_root("spira", &None), None);
    }

    #[test]
    fn repo_base_reduces_a_full_ref_to_a_bare_branch_name() {
        let dir = scratch_dir("repo-base");
        let map = dir.join("repo-map");
        fs::write(&map, "spira | /srv/checkouts/spira | queue | origin/main | | \n").unwrap();
        assert_eq!(repo_base("spira", &Some(map)), Some("main".to_string()));
    }

    #[test]
    fn repo_base_empty_column_means_let_the_caller_resolve_it() {
        let dir = scratch_dir("repo-base-empty");
        let map = dir.join("repo-map");
        fs::write(&map, "spira | /srv/checkouts/spira | queue | | | \n").unwrap();
        assert_eq!(repo_base("spira", &Some(map)), None);
    }

    #[test]
    fn find_open_files_lists_only_queue_dirs_that_have_an_open_marker() {
        let dir = scratch_dir("find-open-files");
        fs::create_dir_all(dir.join("spira")).unwrap();
        fs::write(dir.join("spira").join("open"), "branch=spira/queue/x\n").unwrap();
        fs::create_dir_all(dir.join("other-repo")).unwrap(); // no open marker: not returned

        let mut found = find_open_files(&dir);
        found.sort();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "spira");
    }

    #[test]
    fn read_branch_reads_the_branch_line() {
        let dir = scratch_dir("read-branch");
        let open = dir.join("open");
        fs::write(&open, "opened_at=100\nbranch=spira/queue/abc123\n").unwrap();
        assert_eq!(read_branch(&open), Some("spira/queue/abc123".to_string()));
    }

    #[test]
    fn read_branch_missing_line_is_none() {
        let dir = scratch_dir("read-branch-missing");
        let open = dir.join("open");
        fs::write(&open, "opened_at=100\n").unwrap();
        assert_eq!(read_branch(&open), None);
    }

    #[test]
    fn file_mtime_is_none_for_a_missing_file() {
        let dir = scratch_dir("file-mtime-missing");
        assert_eq!(file_mtime(&dir.join("does-not-exist")), None);
    }

    #[test]
    fn file_mtime_is_recent_for_a_freshly_written_file() {
        let dir = scratch_dir("file-mtime-fresh");
        let f = dir.join("f");
        fs::write(&f, "x").unwrap();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let mtime = file_mtime(&f).expect("freshly written file must have an mtime");
        assert!(mtime <= now && now - mtime < 30, "mtime {} vs now {}", mtime, now);
    }

    #[test]
    fn read_new_lines_with_no_marker_returns_the_tail() {
        let dir = scratch_dir("read-new-lines-no-marker");
        let log = dir.join("landing.log");
        fs::write(&log, "2026-01-01T00:00:00Z spira: verdict\n").unwrap();
        assert!(read_new_lines(&log, "").contains("verdict"));
    }

    #[test]
    fn read_new_lines_excludes_lines_at_or_before_the_marker() {
        // The two-pass sequence run_pass() actually performs: pass 1 sees the line with
        // no marker, then writes cfg.now_iso as the marker; pass 2 must not see the same
        // line again, so a marker at or after the line's own timestamp excludes it.
        let dir = scratch_dir("read-new-lines-marker");
        let log = dir.join("landing.log");
        let line_ts = "2026-01-01T00:00:00Z";
        fs::write(&log, format!("{} spira: verdict\n", line_ts)).unwrap();

        assert!(read_new_lines(&log, "").contains("verdict"));

        let marker_after_first_pass = "2026-01-01T00:00:01Z";
        let second = read_new_lines(&log, marker_after_first_pass);
        assert!(
            !second.contains("verdict"),
            "same line must not re-fire once the marker has advanced past it: got {:?}",
            second
        );
    }

    #[test]
    fn read_new_lines_includes_lines_strictly_after_the_marker() {
        let dir = scratch_dir("read-new-lines-after-marker");
        let log = dir.join("landing.log");
        fs::write(
            &log,
            "2026-01-01T00:00:00Z spira: old\n2026-01-01T00:00:02Z spira: new\n",
        )
        .unwrap();
        let out = read_new_lines(&log, "2026-01-01T00:00:01Z");
        assert!(!out.contains("old"), "line before the marker must be excluded: {:?}", out);
        assert!(out.contains("new"), "line after the marker must be included: {:?}", out);
    }
}
