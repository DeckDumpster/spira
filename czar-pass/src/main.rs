// Czar fast pass: deterministic queue-stall detection on a 30-second timer.
// Ported from czar.sh (sp-rpibz) per law-new-subsystems-are-rust (sp-54qsc).
//
// czar-pass --pass
//
// Reads only cheap sources: the landing.log tail since the last pass, the open batch
// record, landstate, CHECK7's last reason from sentinel.log, and at most 2 gh API
// calls (forge.sh batch-ci-status) for the open batch's run.

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
    lock_path: PathBuf,
    spira_db: String,
    scope_label: String,
    czar_label: String,
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
            lock_path: spira_run.join("czar-pass.lock"),
            spira_db: env::var("SPIRA_DB").unwrap_or_default(),
            scope_label: env::var("SPIRA_SCOPE_LABEL").unwrap_or_default(),
            czar_label: env::var("SPIRA_CZAR_LABEL")
                .unwrap_or_else(|_| "czar-trigger".to_string()),
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

// First-seen state files: czar-pass-first.<sanitized-class>
fn fs_path(spira_run: &Path, class: &str) -> PathBuf {
    let safe = class.replace(['/', ','], "-");
    spira_run.join(format!("czar-pass-first.{}", safe))
}

fn fs_get(spira_run: &Path, class: &str) -> Option<u64> {
    fs::read_to_string(fs_path(spira_run, class))
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

fn fs_record(spira_run: &Path, class: &str, now: u64) {
    let p = fs_path(spira_run, class);
    if !p.exists() {
        let _ = fs::write(&p, format!("{}\n", now));
    }
}

fn fs_clear(spira_run: &Path, class: &str) {
    let _ = fs::remove_file(fs_path(spira_run, class));
}

fn latency_secs(spira_run: &Path, class: &str, now: u64) -> u64 {
    fs_get(spira_run, class)
        .map(|first| now.saturating_sub(first))
        .unwrap_or(0)
}

fn telem(cfg: &Config, class: &str, detected: &str, remedy: &str, tier: &str) {
    let lat = latency_secs(&cfg.spira_run, class, cfg.now_secs);
    let line = format!(
        "{} CLASS={} DETECTED={} REMEDY={} TIER={} LATENCY={}s\n",
        cfg.now_iso, class, detected, remedy, tier, lat
    );
    append_czar_log(&cfg.czar_log, &line);
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

fn run_forge(forge_sh: &str, args: &[&str]) -> String {
    let mut cmd = Command::new("bash");
    cmd.arg(forge_sh);
    for a in args {
        cmd.arg(a);
    }
    cmd.stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default()
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

    // ── DETECTOR: deadlock ────────────────────────────────────────────────────
    let (dl_det, dl_rem, dl_tier) =
        detect_deadlock(&cfg, &new_lines);

    // ── DETECTOR: attribution-failed ──────────────────────────────────────────
    let (af_det, af_rem, af_tier) =
        detect_attribution_failed(&cfg, &new_lines);

    // ── DETECTOR: sort-failed ─────────────────────────────────────────────────
    let (sf_det, sf_rem, sf_tier) =
        detect_sort_failed(&cfg, &new_lines);

    // ── DETECTOR: loop-stalled ────────────────────────────────────────────────
    let (ls_det, ls_rem, ls_tier) =
        detect_loop_stalled(&cfg);

    // ── DETECTORS: ci-stalled + ci-red (shared forge calls) ──────────────────
    let (cis_det, cis_rem, cis_tier, cir_det, cir_rem, cir_tier) =
        detect_ci(&cfg);

    // ── DETECTOR: starved ─────────────────────────────────────────────────────
    let (sv_det, sv_rem, sv_tier) =
        detect_starved(&cfg, &check7);

    // Telemetry — one line per class per pass.
    telem(&cfg, "deadlock",           dl_det,  dl_rem,  dl_tier);
    telem(&cfg, "attribution-failed", af_det,  af_rem,  af_tier);
    telem(&cfg, "sort-failed",        sf_det,  sf_rem,  sf_tier);
    telem(&cfg, "loop-stalled",       ls_det,  ls_rem,  ls_tier);
    telem(&cfg, "ci-stalled",         cis_det, cis_rem, cis_tier);
    telem(&cfg, "ci-red",             cir_det, cir_rem, cir_tier);
    telem(&cfg, "starved",            sv_det,  sv_rem,  sv_tier);

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

fn detect_deadlock<'a>(cfg: &Config, new_lines: &str) -> (&'a str, &'a str, &'a str) {
    let trigger = "no suites identified; leaving batch open";
    if new_lines.contains(trigger) {
        fs_record(&cfg.spira_run, "deadlock", cfg.now_secs);
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
        let first = fs_get(&cfg.spira_run, "deadlock").unwrap_or(cfg.now_secs);
        let age = cfg.now_secs.saturating_sub(first);

        // Attempt a deterministic remedy (workflow-rerun) if age < 90s.
        let mut run_id = String::new();
        let mut repo_path = String::new();
        if age < 90 && cfg.queue_dir.is_dir() {
            'outer: for (repo_name, open_path) in find_open_files(&cfg.queue_dir) {
                if let Some(branch) = read_branch(&open_path) {
                    if let Some(rp) = repo_root(&repo_name, &cfg.repo_map) {
                        let rp_str = rp.to_string_lossy().to_string();
                        let out = run_forge(
                            &cfg.forge_sh,
                            &["run-id", &rp_str, &branch],
                        );
                        let rid = out.trim().to_string();
                        if !rid.is_empty() {
                            run_id = rid;
                            repo_path = rp_str;
                            break 'outer;
                        }
                    }
                }
            }
        }

        if !run_id.is_empty() && age < 90 {
            let forge = cfg.forge_sh.clone();
            let rid = run_id.clone();
            let rp = repo_path.clone();
            det_action(cfg, "deadlock", &format!("workflow-rerun {}", run_id), move || {
                let _ = Command::new("bash")
                    .arg(&forge)
                    .args(["workflow-rerun", &rp, &rid])
                    .status();
            });
            ("yes", "det-rerun", "det")
        } else {
            let body = format!(
                "verdict left the batch PR open after a red with no attributable suite.\n\
                 The queue cannot advance until the batch is closed or requeued by hand.\n\n\
                 Recent log lines:\n{}\n",
                ctx
            );
            infer(
                cfg,
                "deadlock",
                "deadlock-batch-open",
                "QUEUE: batch open — no suites identified (DEADLOCK)",
                &body,
            );
            ("yes", "inference", "inf")
        }
    } else {
        fs_clear(&cfg.spira_run, "deadlock");
        ("no", "none", "det")
    }
}

fn detect_attribution_failed<'a>(cfg: &Config, new_lines: &str) -> (&'a str, &'a str, &'a str) {
    // grep -qE 'ejected 0, requeued [1-9]'
    let detected = new_lines.lines().any(|l| {
        if let Some(pos) = l.find("ejected 0, requeued ") {
            let rest = &l[pos + "ejected 0, requeued ".len()..];
            rest.chars().next().map(|c| c.is_ascii_digit() && c != '0').unwrap_or(false)
        } else {
            false
        }
    });
    if detected {
        fs_record(&cfg.spira_run, "attribution-failed", cfg.now_secs);
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
        ("yes", "inference", "inf")
    } else {
        fs_clear(&cfg.spira_run, "attribution-failed");
        ("no", "none", "det")
    }
}

fn detect_sort_failed<'a>(cfg: &Config, new_lines: &str) -> (&'a str, &'a str, &'a str) {
    if new_lines.contains("queue_sort_rows: ranking failed") {
        fs_record(&cfg.spira_run, "sort-failed", cfg.now_secs);
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
        ("yes", "inference", "inf")
    } else {
        fs_clear(&cfg.spira_run, "sort-failed");
        ("no", "none", "det")
    }
}

fn detect_loop_stalled<'a>(cfg: &Config) -> (&'a str, &'a str, &'a str) {
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

    if let Some(ts) = last_ts {
        if let Some(ts_epoch) = parse_iso_to_epoch(&ts) {
            let age = cfg.now_secs.saturating_sub(ts_epoch);
            if age > cfg.stall_secs {
                fs_record(&cfg.spira_run, "loop-stalled", cfg.now_secs);
                let unit_service = format!("{}.service", cfg.land_unit);
                let is_failed = Command::new(&cfg.systemctl)
                    .args(["--user", "is-failed", &unit_service])
                    .stderr(Stdio::null())
                    .output()
                    .ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| s.trim() == "failed")
                    .unwrap_or(false);

                return if is_failed {
                    let sc = cfg.systemctl.clone();
                    let svc = unit_service.clone();
                    det_action(
                        cfg,
                        "loop-stalled",
                        &format!("reset-failed + start {}", cfg.land_unit),
                        move || {
                            let _ = Command::new(&sc)
                                .args(["--user", "reset-failed", &svc])
                                .status();
                            let _ = Command::new(&sc)
                                .args(["--user", "start", &svc])
                                .status();
                        },
                    );
                    ("yes", "det-restart", "det")
                } else {
                    let body = format!(
                        "No landing: pass complete in the last {}s (threshold: {}s).\n\
                         Last pass: {}\n\nSee: {}\n",
                        age,
                        cfg.stall_secs,
                        ts,
                        cfg.qc_log.display()
                    );
                    infer(
                        cfg,
                        "loop-stalled",
                        "loop-stalled",
                        &format!(
                            "QUEUE: landing loop stalled — no pass for {}s",
                            age
                        ),
                        &body,
                    );
                    ("yes", "inference", "inf")
                };
            }
        }
    }

    fs_clear(&cfg.spira_run, "loop-stalled");
    ("no", "none", "det")
}

fn detect_ci<'a>(cfg: &Config) -> (&'a str, &'a str, &'a str, &'a str, &'a str, &'a str) {
    let mut cis_detected = false;
    let mut cis_remedy: &'a str = "none";
    let mut cis_tier: &'a str = "det";
    let mut cir_detected = false;
    let mut cir_remedy: &'a str = "none";
    let mut cir_tier: &'a str = "det";

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

            let ci_out = run_forge(
                &cfg.forge_sh,
                &["batch-ci-status", &repo_path_str, &branch],
            );
            let run_id = parse_field(&ci_out, "run-id").unwrap_or_default();
            let queued_since = parse_field(&ci_out, "queued-since");

            // ci-stalled: job queued with no runner > threshold
            if let Some(qs_str) = queued_since {
                if let Ok(qs) = qs_str.parse::<u64>() {
                    let stalled_s = cfg.now_secs.saturating_sub(qs);
                    if stalled_s > cfg.ci_queued_max {
                        cis_detected = true;
                        fs_record(&cfg.spira_run, "ci-stalled", cfg.now_secs);
                        if !run_id.is_empty() {
                            let forge = cfg.forge_sh.clone();
                            let rp = repo_path_str.clone();
                            let rid = run_id.clone();
                            det_action(
                                cfg,
                                "ci-stalled",
                                &format!(
                                    "workflow-rerun {} ({}, queued {}s)",
                                    run_id, repo_name, stalled_s
                                ),
                                move || {
                                    let _ = Command::new("bash")
                                        .arg(&forge)
                                        .args(["workflow-rerun", &rp, &rid])
                                        .status();
                                },
                            );
                            cis_remedy = "det-rerun";
                        } else {
                            let repo_safe = repo_name.replace(',', "-");
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
                                &format!(
                                    "QUEUE: CI job queued with no runner for {}s ({})",
                                    stalled_s, repo_name
                                ),
                                &body,
                            );
                            cis_remedy = "inference";
                            cis_tier = "inf";
                        }
                        continue;
                    }
                }
            }

            // ci-red: run completed failure, verdict not acting.
            // Measures from run-completed-at (CI completion), not from batch open mtime.
            // If run-completed-at is absent, skip: safe default.
            let conclusion = parse_field(&ci_out, "run-conclusion");
            if conclusion.as_deref() == Some("failure") {
                if let Some(completed_str) = parse_field(&ci_out, "run-completed-at") {
                    if let Ok(completed_secs) = completed_str.trim().parse::<u64>() {
                        let red_age = cfg.now_secs.saturating_sub(completed_secs);
                        if red_age > cfg.ci_red_max {
                            cir_detected = true;
                            fs_record(&cfg.spira_run, "ci-red", cfg.now_secs);
                            let repo_safe = repo_name.replace(',', "-");
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
                                &format!(
                                    "QUEUE: CI run completed red, verdict not acting for {}s ({})",
                                    red_age, repo_name
                                ),
                                &body,
                            );
                            cir_remedy = "inference";
                            cir_tier = "inf";
                        }
                    }
                }
            }
        }
    }

    if !cis_detected {
        fs_clear(&cfg.spira_run, "ci-stalled");
    }
    if !cir_detected {
        fs_clear(&cfg.spira_run, "ci-red");
    }

    (
        if cis_detected { "yes" } else { "no" },
        cis_remedy,
        cis_tier,
        if cir_detected { "yes" } else { "no" },
        cir_remedy,
        cir_tier,
    )
}

fn detect_starved<'a>(cfg: &Config, check7: &str) -> (&'a str, &'a str, &'a str) {
    if !cfg.strands_state.exists() {
        return ("no", "none", "det");
    }

    let content = match fs::read_to_string(&cfg.strands_state) {
        Ok(c) => c,
        Err(_) => return ("no", "none", "det"),
    };
    let json: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return ("no", "none", "det"),
    };
    let obj = match json.as_object() {
        Some(o) => o,
        None => return ("no", "none", "det"),
    };

    let mut detected = false;
    let mut remedy: &'a str = "none";
    let mut tier: &'a str = "det";

    for (key, val) in obj {
        let parts: Vec<&str> = key.splitn(3, ':').collect();
        if parts.len() != 3 || parts[1] != "starved" {
            continue;
        }
        let first_val = match val.get("first").and_then(|v| v.as_f64()) {
            Some(f) => f as u64,
            None => continue,
        };
        let age = cfg.now_secs.saturating_sub(first_val);
        if age <= cfg.starved_max_s {
            continue;
        }

        detected = true;
        let part = parts[0];
        let sv_safe = part.replace(',', "-").replace(' ', "-");
        fs_record(&cfg.spira_run, "starved", cfg.now_secs);

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

    if !detected {
        fs_clear(&cfg.spira_run, "starved");
    }

    (if detected { "yes" } else { "no" }, remedy, tier)
}
