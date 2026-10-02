// Czar fast pass: deterministic queue-stall detection on a 30-second timer.
// Ported from czar.sh (sp-rpibz) per law-new-subsystems-are-rust (sp-54qsc).
//
// czar-pass --pass
//
// Reads only cheap sources: the landing.log tail since the last pass, the open batch
// record, landstate, and at most 2 gh API calls (forge.sh batch-ci-status) for the
// open batch's run.

use reconciler_engine::core::{last_remedy, record_remedy, step, HysteresisState, RawStatus, Verdict};
use reconciler_engine::io::{append_status, load_state, save_state, StateMap};
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
    czar_log: PathBuf,
    qc_log: PathBuf,
    marker: PathBuf,
    incident_sh: String,
    forge_sh: String,
    queue_dir: PathBuf,
    stall_secs: u64,
    ci_queued_max: u64,
    ci_red_max: u64,
    base_unreadable_grace: u64,
    lock_path: PathBuf,
    reconciler_state: PathBuf,
    reconciler_status_log: PathBuf,
    spira_db: String,
    scope_label: String,
    czar_label: String,
    express_label: String,
    land_unit: String,
    systemctl: String,
    repo_map: Option<PathBuf>,
    now_secs: u64,
    now_iso: String,
}

/// `SPIRA_*` values a bash process `Config::from_env`'s own callers spawn (`. "$SPIRA_HOME/
/// lib.sh"` in `summon_fayth_czar`, inheriting this process's own environment) must never
/// see pre-set — the same per-copy-fact / host-policy keys `cockpit-collect`'s
/// `bootstrap_config` names (wave4-decomposition.md row (b)): a nested conf.sh that sees
/// one of these already set skips deriving it fresh from whatever THAT call was actually
/// pointed at.
const NEVER_EXPORTED: &[&str] = &["SPIRA_HOME", "SPIRA_REPO", "SPIRA_REPO_DERIVED", "SPIRA_REPO_MAP", "SPIRA_FAYTHS", "SPIRA_MAX_AEONS"];

/// Wave 4.8 ("retire conf re-import seams in Rust"): every `env::var(...)` read below used
/// to see only this process's own already-set environment — no spira.toml load at all
/// (wave4-decomposition.md row (b) names czar-pass by file). Merges
/// `spira_config::resolve()`'s in-process answer into THIS process's own environment once,
/// inserting a key only when it is not already set (conf.sh's own `${VAR:=default}` rule)
/// and never one of [`NEVER_EXPORTED`], so every `env::var(...)` read below (and in any
/// child this process spawns) sees a toml override exactly as conf.sh would have resolved
/// it. Best-effort: a missing registry or a containment refusal leaves the environment
/// exactly as it was.
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

fn merge_resolved_env() {
    let env_map: std::collections::BTreeMap<String, String> = env::vars().collect();
    let home = harness_home();
    let repo = spira_config::resolve::derive_home_repo(&home, &env_map);
    if let Ok(resolved) = spira_config::resolve::resolve_for_process(&home, &repo, &env_map) {
        for (k, v) in resolved.values {
            if NEVER_EXPORTED.contains(&k.as_str()) {
                continue;
            }
            if env::var_os(&k).is_none() {
                env::set_var(k, v);
            }
        }
    }
}

impl Config {
    fn from_env() -> Config {
        merge_resolved_env();
        // sp-ivfu3: `merge_resolved_env` above already set `SPIRA_RUN` in this process's
        // own environment when `spira_config` could resolve it at all — the only way this
        // read still comes up empty is a `spira.toml` that failed to parse, and that is a
        // named refusal now, never the literal `/tmp/spira` a bare shell used to get.
        let spira_run_str = env::var("SPIRA_RUN").unwrap_or_default();
        if spira_run_str.is_empty() {
            eprintln!("czar-pass: FATAL: cannot resolve spira.run (SPIRA_RUN is unset and spira_config could not resolve it)");
            std::process::exit(1);
        }
        let spira_run = PathBuf::from(&spira_run_str);
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
            // By name on the launcher's PATH (sp-gypjk); SPIRA_INCIDENT_SH / SPIRA_FORGE
            // remain the seams that name another script. forge.sh is retired (sp-t4y60) —
            // sp-yv4b3 found this default still naming the deleted script.
            incident_sh: env::var("SPIRA_INCIDENT_SH").unwrap_or_else(|_| "incident.sh".into()),
            forge_sh: env::var("SPIRA_FORGE").unwrap_or_else(|_| "forge".into()),
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
            land_unit: env::var("SPIRA_LAND_UNIT")
                .unwrap_or_else(|_| "spira-landing".to_string()),
            systemctl: env::var("SPIRA_SYSTEMCTL")
                .unwrap_or_else(|_| "systemctl".to_string()),
            repo_map: env::var("SPIRA_REPO_MAP").ok().map(PathBuf::from),
            now_secs: now,
            now_iso: iso,
            spira_run,
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
            let mut child = script(&cfg.incident_sh)
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
            let mut child = script(&cfg.incident_sh)
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

/// `sentinel --summon czar` directly — no lib.sh sourcing at all (wave 4.27, family G,
/// sp-gzmd2: `summon_fayth` moved in-process into the sentinel crate). The own
/// `world.halted` pre-check this used to need is gone too: `sentinel --summon` runs the
/// SAME `world_gate` sentinel's own pass does, which also catches a live DRAIN this
/// hand-rolled check never did.
fn summon_fayth_czar(_cfg: &Config) {
    let _ = Command::new("sentinel")
        .arg("--summon")
        .arg("czar")
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
/// A harness script: a bare name (sp-gypjk: the release's `spira/`, on the launcher's PATH)
/// is exec'd by name; a path (a test seam's mock) goes through `bash`, as before.
fn script(s: &str) -> Command {
    if s.contains('/') {
        let mut c = Command::new("bash");
        c.arg(s);
        c
    } else {
        Command::new(s)
    }
}

fn run_forge(forge_sh: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = script(forge_sh);
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

    // Telemetry — one line per class per pass.
    telem(&cfg, "deadlock",           &dl_v,  dl_rem,  dl_tier);
    telem(&cfg, "attribution-failed", &af_v,  af_rem,  af_tier);
    telem(&cfg, "sort-failed",        &sf_v,  sf_rem,  sf_tier);
    telem(&cfg, "loop-stalled",       &ls_v,  ls_rem,  ls_tier);
    telem(&cfg, "ci-stalled",         &cis_v, cis_rem, cis_tier);
    telem(&cfg, "ci-red",             &cir_v, cir_rem, cir_tier);
    telem(&cfg, "base-red",           &br_v,  br_rem,  br_tier);

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

/// Rolls per-repo verdicts up into the one CLASS= line czar.log has always printed per
/// invariant. Each repo's own verdict — its own since-when, its own grace period, its own
/// remedy history — is tracked and recorded separately; this never re-derives is_gap from a
/// fresh grace check (that already bypassed a per-repo grace period once — sp-pu7v6 — by
/// re-running the worst raw reading through the engine at grace_secs=0 regardless of what
/// grace the per-repo verdict itself was actually judged against). A gap already past its
/// own grace outranks one still inside it, which outranks "could not tell", which outranks
/// satisfied.
fn rollup(verdicts: &[Verdict]) -> Verdict {
    if let Some(v) = verdicts.iter().find(|v| v.is_gap) {
        return v.clone();
    }
    if let Some(v) = verdicts.iter().find(|v| !matches!(v.status, RawStatus::Satisfied)) {
        return v.clone();
    }
    Verdict {
        status: RawStatus::Satisfied,
        since: None,
        is_gap: false,
        just_closed: verdicts.iter().any(|v| v.just_closed),
        remedy_failed: false,
    }
}

fn detect_ci(cfg: &Config, state: &mut StateMap) -> (Verdict, &'static str, &'static str, Verdict, &'static str, &'static str) {
    let mut cis_remedy: &'static str = "none";
    let mut cis_tier: &'static str = "det";
    let mut cir_remedy: &'static str = "none";
    let mut cir_tier: &'static str = "det";
    let mut cis_verdicts: Vec<Verdict> = Vec::new();
    let mut cir_verdicts: Vec<Verdict> = Vec::new();

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
                    cis_verdicts.push(v1);
                    let v2 = evaluate(cfg, state, &cir_key, RawStatus::Unobservable { reason }, 0);
                    cir_verdicts.push(v2);
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
            cis_verdicts.push(cis_v.clone());
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
            cir_verdicts.push(cir_v.clone());
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

    (rollup(&cis_verdicts), cis_remedy, cis_tier, rollup(&cir_verdicts), cir_remedy, cir_tier)
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
    let mut verdicts: Vec<Verdict> = Vec::new();

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
                verdicts.push(v.clone());
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
            verdicts.push(v.clone());
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

    (rollup(&verdicts), remedy, tier)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    // Serialises the two tests below that touch the real process environment (SPIRA_FORGE,
    // PATH) — same pattern as release's `ENV_LOCK`/`PathGuard` (release/src/tests.rs).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        forge: Option<std::ffi::OsString>,
        path: Option<std::ffi::OsString>,
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.forge {
                Some(f) => env::set_var("SPIRA_FORGE", f),
                None => env::remove_var("SPIRA_FORGE"),
            }
            match &self.path {
                Some(p) => env::set_var("PATH", p),
                None => env::remove_var("PATH"),
            }
        }
    }

    // Wave 4.8: merge_resolved_env() must reach a registry key `Config::from_env` never
    // hardcoded a default for (SPIRA_CZAR_LABEL's conf.d default, "czar-trigger", is the
    // same literal already in czar_label's own unwrap_or_else below — this proves the
    // MERGE path, not merely that the two defaults happen to agree), and must never leak
    // a NEVER_EXPORTED key into this process's own environment.
    #[test]
    fn merge_resolved_env_reaches_a_registry_default_and_never_exports_the_forbidden_set() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved_home = env::var_os("SPIRA_HOME");
        let saved_label = env::var_os("SPIRA_CZAR_LABEL");
        let saved_max_aeons = env::var_os("SPIRA_MAX_AEONS");
        env::remove_var("SPIRA_CZAR_LABEL");
        env::remove_var("SPIRA_MAX_AEONS");
        let dir = testkit::TempDir::new("czar-pass-merge-env");
        let home = dir.join("spira");
        fs::create_dir_all(home.join("conf.d")).unwrap();
        fs::write(
            home.join("conf.d/SPIRA_CZAR_LABEL"),
            "TYPE=string\nGROUP=czar\nDOC=test\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${SPIRA_CZAR_LABEL:=czar-trigger}\"\nSPIRA_CONF_DEFAULT_EOF\n",
        )
        .unwrap();
        env::set_var("SPIRA_HOME", &home);

        merge_resolved_env();

        let got_label = env::var("SPIRA_CZAR_LABEL").ok();
        let got_max_aeons = env::var_os("SPIRA_MAX_AEONS");

        match saved_home {
            Some(v) => env::set_var("SPIRA_HOME", v),
            None => env::remove_var("SPIRA_HOME"),
        }
        match saved_label {
            Some(v) => env::set_var("SPIRA_CZAR_LABEL", v),
            None => env::remove_var("SPIRA_CZAR_LABEL"),
        }
        match saved_max_aeons {
            Some(v) => env::set_var("SPIRA_MAX_AEONS", v),
            None => env::remove_var("SPIRA_MAX_AEONS"),
        }

        assert_eq!(got_label, Some("czar-trigger".to_string()), "a registry default must reach the real environment");
        assert_eq!(got_max_aeons, None, "SPIRA_MAX_AEONS must never leak into this process's own environment");
        let _ = fs::remove_dir_all(&dir);
    }

    // REGRESSION (sp-yv4b3): production queue-watch went blind — "forge check-status 459:
    // No such file or directory (os error 2)" — because this default named the retired
    // `forge.sh` instead of the release's `forge` binary. Fails on the pre-fix default.
    #[test]
    fn config_from_env_defaults_forge_sh_to_the_bare_release_binary() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = EnvGuard { forge: env::var_os("SPIRA_FORGE"), path: env::var_os("PATH") };
        env::remove_var("SPIRA_FORGE");
        let cfg = Config::from_env();
        assert_eq!(cfg.forge_sh, "forge", "default must name the bare release binary, not forge.sh");
    }

    // End-to-end: with SPIRA_FORGE unset and a PATH holding ONLY a stub named `forge`
    // (never `forge.sh`, exactly what forge.sh's deletion left production with), the
    // resolved default must actually reach it through `run_forge`/`script`.
    #[test]
    fn default_forge_sh_actually_reaches_a_bare_forge_stub_on_path() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = EnvGuard { forge: env::var_os("SPIRA_FORGE"), path: env::var_os("PATH") };
        env::remove_var("SPIRA_FORGE");

        let dir = scratch_dir("default-forge-stub");
        testkit::write_exe(
            dir.join("forge"),
            "#!/bin/sh\ncase \"$1\" in batch-ci-status) printf 'run-id: 1\\nrun-conclusion: success\\n' ;; *) exit 1 ;; esac\n",
        );
        // PREPEND (never replace): other tests run concurrently in this binary and need the
        // real PATH to keep resolving; the stub only needs to win the lookup for the bare
        // name `forge` itself.
        let real_path = env::var_os("PATH").unwrap_or_default();
        let mut new_path = dir.path().as_os_str().to_os_string();
        new_path.push(":");
        new_path.push(&real_path);
        env::set_var("PATH", &new_path);

        let cfg = Config::from_env();
        assert_eq!(cfg.forge_sh, "forge");
        let out = run_forge(&cfg.forge_sh, &["batch-ci-status", "/tmp/r", "branch"]).expect("run_forge should reach the stub");
        assert!(out.contains("run-conclusion"), "unexpected output: {out:?}");
    }

    // A private scratch directory per test, so parallel `cargo test` threads never collide
    // on the same path (the real callers always get SPIRA_RUN handed to them by the caller,
    // never a shared ambient default).
    fn scratch_dir(name: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("czar-pass-test-{}-{}", name, SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()))
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

    // ────────────────────────────────────────────────────────────────────────────────
    // Detector fire/silent boundaries (UC-ops-detection-remediation-23).
    //
    // Every case runs in Shadow stage (the default with no SPIRA_CZAR_STAGE_* set):
    // det_action/infer only append a CZAR-WOULD line there, so a "fire" case never spawns
    // a real forge.sh workflow-rerun or files a real incident — exactly what a unit test
    // wants from a detector whose whole job is to trigger a side effect.
    // ────────────────────────────────────────────────────────────────────────────────

    const KNOWN_EPOCH: u64 = 1_609_459_200; // 2021-01-01T00:00:00Z, for parse_iso_to_epoch

    fn test_config(dir: &Path, now: u64) -> Config {
        Config {
            spira_run: dir.to_path_buf(),
            czar_log: dir.join("czar.log"),
            qc_log: dir.join("landing.log"),
            marker: dir.join("czar-pass.swept"),
            incident_sh: dir.join("incident.sh").to_string_lossy().to_string(),
            forge_sh: dir.join("forge.sh").to_string_lossy().to_string(),
            queue_dir: dir.join("queue"),
            stall_secs: 3000,
            ci_queued_max: 600,
            ci_red_max: 600,
            base_unreadable_grace: 120,
            lock_path: dir.join("czar-pass.lock"),
            reconciler_state: dir.join("reconciler-state.json"),
            reconciler_status_log: dir.join("reconciler-status.jsonl"),
            spira_db: String::new(),
            scope_label: String::new(),
            czar_label: "czar-trigger".to_string(),
            express_label: "express".to_string(),
            land_unit: "spira-landing".to_string(),
            // A path that cannot exist, so `systemctl is-failed` always fails to spawn
            // (is_failed = false) instead of depending on the test host's real systemd.
            systemctl: dir.join("no-such-systemctl").to_string_lossy().to_string(),
            repo_map: None,
            now_secs: now,
            now_iso: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn write_repo_open(dir: &Path, repo: &str, branch: &str) {
        let repo_dir = dir.join("queue").join(repo);
        fs::create_dir_all(&repo_dir).unwrap();
        fs::write(repo_dir.join("open"), format!("branch={}\n", branch)).unwrap();
    }

    fn write_repo_map(dir: &Path, repo: &str, repo_path: &str, base: &str) -> PathBuf {
        let map = dir.join("repo-map");
        fs::write(&map, format!("{} | {} | queue | {} | | \n", repo, repo_path, base)).unwrap();
        map
    }

    #[test]
    fn detect_deadlock_silent_without_the_trigger_line() {
        let dir = scratch_dir("deadlock-silent");
        let cfg = test_config(&dir, 1000);
        let mut state = StateMap::new();
        let (v, remedy, tier) = detect_deadlock(&cfg, &mut state, "landing: pass complete\n");
        assert!(!v.is_gap);
        assert_eq!(remedy, "none");
        assert_eq!(tier, "det");
    }

    #[test]
    fn detect_deadlock_fires_on_the_trigger_line() {
        let dir = scratch_dir("deadlock-fire");
        let cfg = test_config(&dir, 1000);
        let mut state = StateMap::new();
        let (v, remedy, tier) =
            detect_deadlock(&cfg, &mut state, "no suites identified; leaving batch open\n");
        assert!(v.is_gap);
        assert_eq!(remedy, "inference");
        assert_eq!(tier, "inf");
    }

    #[test]
    fn detect_attribution_failed_silent_with_no_matching_line() {
        let dir = scratch_dir("attribution-silent-absent");
        let cfg = test_config(&dir, 1000);
        let mut state = StateMap::new();
        let (v, _remedy, _tier) =
            detect_attribution_failed(&cfg, &mut state, "landing: verdict PASS\n");
        assert!(!v.is_gap);
    }

    #[test]
    fn detect_attribution_failed_silent_when_requeued_is_zero() {
        let dir = scratch_dir("attribution-silent-zero");
        let cfg = test_config(&dir, 1000);
        let mut state = StateMap::new();
        let (v, _remedy, _tier) =
            detect_attribution_failed(&cfg, &mut state, "ejected 0, requeued 0\n");
        assert!(!v.is_gap, "requeued 0 is a no-op, not an attribution failure");
    }

    #[test]
    fn detect_attribution_failed_fires_when_requeued_is_nonzero() {
        let dir = scratch_dir("attribution-fire");
        let cfg = test_config(&dir, 1000);
        let mut state = StateMap::new();
        let (v, remedy, tier) =
            detect_attribution_failed(&cfg, &mut state, "ejected 0, requeued 3\n");
        assert!(v.is_gap);
        assert_eq!(remedy, "inference");
        assert_eq!(tier, "inf");
    }

    #[test]
    fn detect_sort_failed_silent_without_the_trigger() {
        let dir = scratch_dir("sort-silent");
        let cfg = test_config(&dir, 1000);
        let mut state = StateMap::new();
        let (v, _remedy, _tier) = detect_sort_failed(&cfg, &mut state, "landing: verdict PASS\n");
        assert!(!v.is_gap);
    }

    #[test]
    fn detect_sort_failed_fires_on_the_ranking_failure_line() {
        let dir = scratch_dir("sort-fire");
        let cfg = test_config(&dir, 1000);
        let mut state = StateMap::new();
        let (v, remedy, tier) =
            detect_sort_failed(&cfg, &mut state, "queue_sort_rows: ranking failed\n");
        assert!(v.is_gap);
        assert_eq!(remedy, "inference");
        assert_eq!(tier, "inf");
    }

    #[test]
    fn detect_loop_stalled_silent_with_no_landing_log() {
        let dir = scratch_dir("loop-silent-no-log");
        let cfg = test_config(&dir, KNOWN_EPOCH + 100);
        let mut state = StateMap::new();
        let (v, remedy, tier) = detect_loop_stalled(&cfg, &mut state);
        assert!(!v.is_gap);
        assert_eq!(remedy, "none");
        assert_eq!(tier, "det");
    }

    #[test]
    fn detect_loop_stalled_silent_within_the_stall_window() {
        let dir = scratch_dir("loop-silent-recent");
        let cfg = test_config(&dir, KNOWN_EPOCH + 100); // age 100s < stall_secs 3000
        fs::write(&cfg.qc_log, "2021-01-01T00:00:00Z spira: landing: pass complete\n").unwrap();
        let mut state = StateMap::new();
        let (v, _remedy, _tier) = detect_loop_stalled(&cfg, &mut state);
        assert!(!v.is_gap);
    }

    #[test]
    fn detect_loop_stalled_fires_past_the_stall_window() {
        let dir = scratch_dir("loop-fire");
        let cfg = test_config(&dir, KNOWN_EPOCH + 5000); // age 5000s > stall_secs 3000
        fs::write(&cfg.qc_log, "2021-01-01T00:00:00Z spira: landing: pass complete\n").unwrap();
        let mut state = StateMap::new();
        let (v, remedy, tier) = detect_loop_stalled(&cfg, &mut state);
        assert!(v.is_gap);
        // The stub systemctl path cannot exist, so is_failed is always false and the
        // detector falls through to the inference ladder, not the det-restart one.
        assert_eq!(remedy, "inference");
        assert_eq!(tier, "inf");
    }

    #[test]
    fn detect_loop_stalled_is_unobservable_and_takes_no_action_on_an_unparseable_timestamp() {
        let dir = scratch_dir("loop-unobservable");
        let cfg = test_config(&dir, KNOWN_EPOCH + 100);
        fs::write(&cfg.qc_log, "GARBAGE-NOT-AN-ISO-TS landing: pass complete\n").unwrap();
        let mut state = StateMap::new();
        let (v, remedy, tier) = detect_loop_stalled(&cfg, &mut state);
        assert!(matches!(v.status, RawStatus::Unobservable { .. }));
        assert!(v.is_gap, "grace_secs is 0, so even an unobservable reading is a gap at once");
        assert_eq!(remedy, "none", "an unreadable timestamp must never trigger a restart or a filing");
        assert_eq!(tier, "det");
    }

    #[test]
    fn detect_ci_silent_with_no_queue_dir() {
        let dir = scratch_dir("ci-no-queue");
        let cfg = test_config(&dir, 2_000_000_000);
        let mut state = StateMap::new();
        let (cis_v, cis_remedy, _t1, cir_v, cir_remedy, _t2) = detect_ci(&cfg, &mut state);
        assert!(!cis_v.is_gap);
        assert!(!cir_v.is_gap);
        assert_eq!(cis_remedy, "none");
        assert_eq!(cir_remedy, "none");
    }

    #[test]
    fn detect_ci_stalled_fires_past_the_queued_threshold() {
        let dir = scratch_dir("ci-stalled-fire");
        let now = 2_000_000_000u64;
        let mut cfg = test_config(&dir, now);
        write_repo_open(&dir, "spira", "spira/queue/abc123");
        cfg.repo_map = Some(write_repo_map(&dir, "spira", "/tmp/spira-checkout", "origin/main"));
        let queued_since = now - cfg.ci_queued_max - 1; // just past threshold, no run-id
        fs::write(&cfg.forge_sh, format!("printf 'queued-since: {}\\n'\n", queued_since)).unwrap();
        let mut state = StateMap::new();
        let (cis_v, cis_remedy, cis_tier, cir_v, _r2, _t2) = detect_ci(&cfg, &mut state);
        assert!(cis_v.is_gap);
        assert_eq!(cis_remedy, "inference");
        assert_eq!(cis_tier, "inf");
        assert!(!cir_v.is_gap, "no run-conclusion field at all is satisfied for ci-red");
    }

    #[test]
    fn detect_ci_stalled_silent_within_the_queued_threshold() {
        let dir = scratch_dir("ci-stalled-silent");
        let now = 2_000_000_000u64;
        let mut cfg = test_config(&dir, now);
        write_repo_open(&dir, "spira", "spira/queue/abc123");
        cfg.repo_map = Some(write_repo_map(&dir, "spira", "/tmp/spira-checkout", "origin/main"));
        let queued_since = now - 10; // well inside the threshold
        fs::write(&cfg.forge_sh, format!("printf 'queued-since: {}\\n'\n", queued_since)).unwrap();
        let mut state = StateMap::new();
        let (cis_v, cis_remedy, _t1, _cir_v, _r2, _t2) = detect_ci(&cfg, &mut state);
        assert!(!cis_v.is_gap);
        assert_eq!(cis_remedy, "none");
    }

    #[test]
    fn detect_ci_red_fires_past_the_red_age_threshold() {
        let dir = scratch_dir("ci-red-fire");
        let now = 2_000_000_000u64;
        let mut cfg = test_config(&dir, now);
        write_repo_open(&dir, "spira", "spira/queue/abc123");
        cfg.repo_map = Some(write_repo_map(&dir, "spira", "/tmp/spira-checkout", "origin/main"));
        let completed = now - cfg.ci_red_max - 1;
        fs::write(
            &cfg.forge_sh,
            format!("printf 'run-conclusion: failure\\nrun-completed-at: {}\\n'\n", completed),
        )
        .unwrap();
        let mut state = StateMap::new();
        let (_cis_v, _r1, _t1, cir_v, cir_remedy, cir_tier) = detect_ci(&cfg, &mut state);
        assert!(cir_v.is_gap);
        assert_eq!(cir_remedy, "inference");
        assert_eq!(cir_tier, "inf");
    }

    #[test]
    fn detect_ci_red_silent_when_recently_completed() {
        let dir = scratch_dir("ci-red-silent");
        let now = 2_000_000_000u64;
        let mut cfg = test_config(&dir, now);
        write_repo_open(&dir, "spira", "spira/queue/abc123");
        cfg.repo_map = Some(write_repo_map(&dir, "spira", "/tmp/spira-checkout", "origin/main"));
        let completed = now - 5; // well inside ci_red_max
        fs::write(
            &cfg.forge_sh,
            format!("printf 'run-conclusion: failure\\nrun-completed-at: {}\\n'\n", completed),
        )
        .unwrap();
        let mut state = StateMap::new();
        let (_cis_v, _r1, _t1, cir_v, cir_remedy, _t2) = detect_ci(&cfg, &mut state);
        assert!(!cir_v.is_gap);
        assert_eq!(cir_remedy, "none");
    }

    #[test]
    fn detect_ci_is_unobservable_and_takes_no_action_when_forge_fails() {
        let dir = scratch_dir("ci-unobservable");
        let mut cfg = test_config(&dir, 2_000_000_000);
        write_repo_open(&dir, "spira", "spira/queue/abc123");
        cfg.repo_map = Some(write_repo_map(&dir, "spira", "/tmp/spira-checkout", "origin/main"));
        fs::write(&cfg.forge_sh, "exit 1\n").unwrap();
        let mut state = StateMap::new();
        let (cis_v, cis_remedy, _t1, cir_v, cir_remedy, _t2) = detect_ci(&cfg, &mut state);
        assert!(matches!(cis_v.status, RawStatus::Unobservable { .. }));
        assert!(matches!(cir_v.status, RawStatus::Unobservable { .. }));
        assert_eq!(cis_remedy, "none", "a failed forge call must never be read as clear");
        assert_eq!(cir_remedy, "none");
    }

    #[test]
    fn detect_base_red_silent_when_the_base_gate_is_green() {
        let dir = scratch_dir("base-red-silent");
        let mut cfg = test_config(&dir, 2_000_000_000);
        write_repo_open(&dir, "spira", "spira/queue/abc123");
        cfg.repo_map = Some(write_repo_map(&dir, "spira", "/tmp/spira-checkout", "origin/main"));
        fs::write(&cfg.forge_sh, "printf 'run-id: 1\\nrun-conclusion: success\\n'\n").unwrap();
        let mut state = StateMap::new();
        let (v, remedy, _tier) = detect_base_red(&cfg, &mut state);
        assert!(!v.is_gap);
        assert_eq!(remedy, "none");
    }

    #[test]
    fn detect_base_red_fires_at_once_on_a_red_base() {
        let dir = scratch_dir("base-red-fire");
        let mut cfg = test_config(&dir, 2_000_000_000);
        write_repo_open(&dir, "spira", "spira/queue/abc123");
        cfg.repo_map = Some(write_repo_map(&dir, "spira", "/tmp/spira-checkout", "origin/main"));
        fs::write(
            &cfg.forge_sh,
            "printf 'run-id: 1\\nrun-conclusion: failure\\nhead-sha: deadbeef\\nrun-url: https://example/run/1\\n'\n",
        )
        .unwrap();
        let mut state = StateMap::new();
        let (v, remedy, tier) = detect_base_red(&cfg, &mut state);
        assert!(v.is_gap, "a red base fires on the very first pass — there is no age threshold");
        assert_eq!(remedy, "inference");
        assert_eq!(tier, "inf");
    }

    #[test]
    fn detect_base_red_unreadable_is_grace_suppressed_then_fires() {
        let dir = scratch_dir("base-red-unreadable");
        let now = 2_000_000_000u64;
        let mut cfg = test_config(&dir, now);
        write_repo_open(&dir, "spira", "spira/queue/abc123");
        cfg.repo_map = Some(write_repo_map(&dir, "spira", "/tmp/spira-checkout", "origin/main"));
        fs::write(&cfg.forge_sh, "exit 0\n").unwrap(); // succeeds but names no run at all
        let mut state = StateMap::new();

        let (v1, remedy1, _t1) = detect_base_red(&cfg, &mut state);
        assert!(matches!(v1.status, RawStatus::Unobservable { .. }));
        assert!(!v1.is_gap, "inside the grace window an unreadable base stays quiet");
        assert_eq!(remedy1, "none");

        cfg.now_secs = now + cfg.base_unreadable_grace + 1;
        let (v2, remedy2, tier2) = detect_base_red(&cfg, &mut state);
        assert!(v2.is_gap, "past the grace window an unreadable base is failed, not green");
        assert_eq!(remedy2, "inference");
        assert_eq!(tier2, "inf");
    }
}
