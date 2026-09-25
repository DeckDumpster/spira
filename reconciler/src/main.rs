// reconciler: structural-invariant control loop — observe, diff against the declared
// desired state, act to close the gap with a deterministic remedy where one exists, else
// escalate to the Concierge. Shaped like a Kubernetes controller (sp-ocmes, design:
// wiki/projects/spira/designs/reconciler-2026-09-25.md).
//
// reconciler --pass
//
// The pure diff-plus-hysteresis logic (grace periods, unobservable, remedy verification)
// lives in reconciler-engine and is shared with czar-pass; this binary owns observation
// (units, tmux, git, the queue) and the deterministic remedies for the resources czar-pass
// does not already watch: Fleet, Units, Store, Cockpit, Production/Release and two Queue
// checks czar-pass's own detectors do not cover (the base gate itself is base-red, already
// watched there — reconciling it a second time here would file a second incident for the
// same fault).

use reconciler_engine::core::{record_remedy, step, HysteresisState, RawStatus, Verdict};
use reconciler_engine::io::{append_status, load_state, save_state, StateMap};
use std::env;
use std::fs;
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
        eprintln!("usage: reconciler --pass");
        return ExitCode::from(2);
    }
    match run_pass() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("reconciler: {}", e);
            ExitCode::FAILURE
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Configuration — every path and binary is a test seam, mirroring czar-pass's Config.
// ──────────────────────────────────────────────────────────────────────────────

struct Config {
    spira_run: PathBuf,
    spira_home: String,
    log: PathBuf,
    state_path: PathBuf,
    status_log: PathBuf,
    lock_path: PathBuf,
    spira_db: String,
    scope_label: String,
    reconciler_label: String,
    incident_sh: String,
    systemctl: String,
    tmux: String,
    git: String,
    units_manifest_sh: String,
    fleet_status_sh: String,
    queue_certified_list_sh: String,
    cockpit_sh: String,
    queue_sh: String,
    queue_dir: PathBuf,
    repo_map: Option<PathBuf>,
    releases_dir: PathBuf,
    store_unit: String,
    cockpit_sessions: Vec<String>,
    cockpit_mail: String,
    grace_secs: u64,
    preflight_wall_secs: u64,
    now_secs: u64,
    now_iso: String,
}

impl Config {
    fn from_env() -> Config {
        let spira_run_str = env::var("SPIRA_RUN").unwrap_or_else(|_| "/tmp/spira".to_string());
        let spira_run = PathBuf::from(&spira_run_str);
        let spira_home = env::var("SPIRA_HOME").unwrap_or_default();
        let sessions = env::var("COCKPIT_SESSIONS")
            .unwrap_or_else(|_| "brain hunk chat".to_string())
            .split_whitespace()
            .map(String::from)
            .collect();
        Config {
            log: env::var("SPIRA_RECONCILER_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("reconciler.log")),
            state_path: env::var("SPIRA_RECONCILER_STATE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("reconciler-state.json")),
            status_log: env::var("SPIRA_RECONCILER_STATUS_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("reconciler-status.jsonl")),
            lock_path: spira_run.join("reconciler.lock"),
            spira_db: env::var("SPIRA_DB").unwrap_or_default(),
            scope_label: env::var("SPIRA_SCOPE_LABEL").unwrap_or_default(),
            reconciler_label: env::var("SPIRA_RECONCILER_LABEL")
                .unwrap_or_else(|_| "reconciler-gap".to_string()),
            incident_sh: env::var("SPIRA_INCIDENT_SH")
                .unwrap_or_else(|_| format!("{}/incident.sh", spira_home)),
            systemctl: env::var("SPIRA_SYSTEMCTL").unwrap_or_else(|_| "systemctl".to_string()),
            tmux: env::var("SPIRA_TMUX").unwrap_or_else(|_| "tmux".to_string()),
            git: env::var("SPIRA_GIT").unwrap_or_else(|_| "git".to_string()),
            units_manifest_sh: env::var("SPIRA_UNITS_MANIFEST_SH")
                .unwrap_or_else(|_| format!("{}/units-manifest.sh", spira_home)),
            fleet_status_sh: env::var("SPIRA_FLEET_STATUS_SH")
                .unwrap_or_else(|_| format!("{}/fleet-status.sh", spira_home)),
            queue_certified_list_sh: env::var("SPIRA_QUEUE_CERTIFIED_LIST_SH")
                .unwrap_or_else(|_| format!("{}/queue-certified-list.sh", spira_home)),
            cockpit_sh: env::var("SPIRA_COCKPIT_SH")
                .unwrap_or_else(|_| format!("{}/cockpit.sh", spira_home)),
            queue_sh: env::var("SPIRA_QUEUE_SH")
                .unwrap_or_else(|_| format!("{}/queue.sh", spira_home)),
            queue_dir: env::var("SPIRA_QUEUE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("queue")),
            repo_map: env::var("SPIRA_REPO_MAP").ok().map(PathBuf::from),
            releases_dir: env::var("SPIRA_RELEASES").map(PathBuf::from).unwrap_or_default(),
            store_unit: env::var("SPIRA_STORE_UNIT")
                .unwrap_or_else(|_| "dolt-beads.service".to_string()),
            cockpit_sessions: sessions,
            cockpit_mail: env::var("COCKPIT_MAIL").unwrap_or_default(),
            grace_secs: env::var("SPIRA_RECONCILER_GRACE_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300),
            preflight_wall_secs: env::var("SPIRA_PREFLIGHT_WALL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(240),
            now_secs: unix_now(),
            now_iso: compute_now_iso(),
            spira_run,
            spira_home,
        }
    }
}

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

fn log_print(cfg: &Config, msg: &str) {
    let line = format!("{} spira: {}\n", cfg.now_iso, msg);
    print!("{}", line);
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&cfg.log) {
        let _ = f.write_all(line.as_bytes());
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// One thing to reconcile: a key, this pass's raw reading, and what to do about a gap.
// ──────────────────────────────────────────────────────────────────────────────

enum Remedy {
    /// No deterministic fix — a gap here always escalates to the Concierge.
    Escalate,
    Command { program: String, args: Vec<String> },
}

struct Check {
    key: String,
    raw: RawStatus,
    remedy: Remedy,
}

fn run_cmd(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default()
}

fn run_cmd_ok(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ──────────────────────────────────────────────────────────────────────────────
// Repo map — copied from czar-pass's own reader (sp-54qsc); the two binaries share the
// file's format but not a crate, so this stays a small, literal copy rather than a new
// cross-crate dependency for six lines of parsing.
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

fn queue_repo_names(queue_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(queue_dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        if entry.path().is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                out.push(name.to_string());
            }
        }
    }
    out.sort();
    out
}

// ──────────────────────────────────────────────────────────────────────────────
// Units: the ENABLE manifest is the desired set; systemctl is the observation.
// ──────────────────────────────────────────────────────────────────────────────

// `is-enabled`/`is-active` exit non-zero for the very states this asks about (a disabled
// unit, an inactive unit) — exit status alone cannot distinguish "disabled" from "systemctl
// could not tell you". systemctl's own contract is the discriminator: both verbs print one
// of a small set of known state words on stdout even on a non-zero exit; anything else
// (empty output, an error message, a missing binary) is genuinely unreadable.
fn systemctl_state(cfg: &Config, verb: &str, unit: &str) -> Option<String> {
    let out = Command::new(&cfg.systemctl)
        .args(["--user", verb, unit])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let word = String::from_utf8(out.stdout).ok()?.trim().to_string();
    const KNOWN: &[&str] = &[
        "enabled", "disabled", "static", "masked", "linked", "alias", "generated", "indirect", "enabled-runtime",
        "active", "inactive", "failed", "activating", "deactivating", "reloading",
    ];
    if KNOWN.contains(&word.as_str()) {
        Some(word)
    } else {
        None
    }
}

fn observe_units(cfg: &Config) -> Vec<Check> {
    let out = run_cmd("bash", &[&cfg.units_manifest_sh]);
    let mut checks = Vec::new();
    for unit in out.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let enabled = systemctl_state(cfg, "is-enabled", unit);
        let active = systemctl_state(cfg, "is-active", unit);
        let (enabled, active) = match (enabled, active) {
            (Some(e), Some(a)) => (e, a),
            _ => {
                checks.push(Check {
                    key: format!("units:{}", unit),
                    raw: RawStatus::Unobservable {
                        reason: format!("systemctl could not read {}", unit),
                    },
                    remedy: Remedy::Escalate,
                });
                continue;
            }
        };
        let is_enabled = enabled == "enabled";
        let is_active = active == "active";

        if unit == cfg.store_unit {
            // Never restart the store (sp-ocmes evidence, 2026-09-25): a gap here always
            // escalates, whatever it is — including "just not enabled", since even `enable`
            // with no restart risks nothing but is still a decision left to a person for the
            // one unit an automatic fix cannot safely guess wrong on.
            let raw = if is_active {
                RawStatus::Satisfied
            } else {
                RawStatus::Gap { desired: "active".into(), observed: active.clone(), since_hint: None }
            };
            checks.push(Check { key: format!("store:{}", unit), raw, remedy: Remedy::Escalate });
            continue;
        }

        if unit.ends_with(".timer") {
            let raw = if is_enabled && is_active {
                RawStatus::Satisfied
            } else {
                RawStatus::Gap {
                    desired: "enabled,active".into(),
                    observed: format!("{},{}", enabled, active),
                    since_hint: None,
                }
            };
            checks.push(Check {
                key: format!("units-timer:{}", unit),
                raw,
                remedy: Remedy::Command {
                    program: cfg.systemctl.clone(),
                    args: vec!["--user".into(), "enable".into(), "--now".into(), unit.to_string()],
                },
            });
            continue;
        }

        // A daemon row: needs to be active. Enabled-but-not-active is restarted; active-but-
        // not-enabled is enabled WITHOUT --now, so a unit already running is never bounced
        // just to satisfy its enablement bit.
        let raw = if is_active {
            RawStatus::Satisfied
        } else {
            RawStatus::Gap { desired: "active".into(), observed: active.clone(), since_hint: None }
        };
        let remedy = if !is_active {
            Remedy::Command {
                program: cfg.systemctl.clone(),
                args: vec!["--user".into(), "restart".into(), unit.to_string()],
            }
        } else if !is_enabled {
            Remedy::Command {
                program: cfg.systemctl.clone(),
                args: vec!["--user".into(), "enable".into(), unit.to_string()],
            }
        } else {
            Remedy::Escalate
        };
        checks.push(Check { key: format!("units-daemon:{}", unit), raw, remedy });
    }
    checks
}

// ──────────────────────────────────────────────────────────────────────────────
// Fleet: ready work per builder partition vs live builders under it.
// ──────────────────────────────────────────────────────────────────────────────

fn observe_fleet(cfg: &Config) -> Vec<Check> {
    let out = run_cmd("bash", &[&cfg.fleet_status_sh]);
    let mut max_live: Option<i64> = None;
    let mut live_lanes: i64 = 0;
    let mut rows: Vec<(String, i64, i64)> = Vec::new();

    for line in out.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() != 3 {
            continue;
        }
        if parts[0] == "TOTAL" {
            max_live = parts[1].trim().parse().ok();
            live_lanes = parts[2].trim().parse().unwrap_or(0);
            continue;
        }
        let ready: i64 = parts[1].trim().parse().unwrap_or(0);
        let live: i64 = parts[2].trim().parse().unwrap_or(0);
        rows.push((parts[0].to_string(), ready, live));
    }

    let mut checks = Vec::new();
    let max_live = match max_live {
        Some(m) => m,
        None => {
            // SPIRA_MAX_LIVE_AEONS is unset on some installs by design (conf.sh's own
            // default is empty) — there is no ceiling to diff against, so this is not a
            // gap, it is unobservable.
            for (labels, _, _) in rows {
                checks.push(Check {
                    key: format!("fleet:{}", labels),
                    raw: RawStatus::Unobservable { reason: "SPIRA_MAX_LIVE_AEONS is unset".into() },
                    remedy: Remedy::Escalate,
                });
            }
            return checks;
        }
    };

    for (labels, ready, live) in rows {
        let headroom = (max_live - live_lanes).max(0);
        let desired_live = ready.min(headroom);
        let raw = if live >= desired_live {
            RawStatus::Satisfied
        } else {
            RawStatus::Gap {
                desired: desired_live.to_string(),
                observed: live.to_string(),
                since_hint: None,
            }
        };
        checks.push(Check { key: format!("fleet:{}", labels), raw, remedy: Remedy::Escalate });
    }
    checks
}

// ──────────────────────────────────────────────────────────────────────────────
// Cockpit: the operator's tmux server has its sessions and both dashboards, unless the
// absence of every @cockpit pane says `layout.sh down` was run on purpose.
// ──────────────────────────────────────────────────────────────────────────────

fn observe_cockpit(cfg: &Config) -> Check {
    // `layout.sh down` records this (sp-ocmes): the operator dismissed the dashboards on
    // purpose, and a deliberate state is not a fault (law-a-deliberate-state-is-not-a-fault).
    // `layout.sh up` clears it the moment it rebuilds anything, so this is stale for no
    // longer than the next time the operator — or this very remedy — asks for the cockpit.
    if cfg.spira_run.join("cockpit.down").exists() {
        return Check { key: "cockpit".into(), raw: RawStatus::Satisfied, remedy: Remedy::Escalate };
    }

    if !run_cmd_ok(&cfg.tmux, &["list-sessions"]) {
        return Check {
            key: "cockpit".into(),
            raw: RawStatus::Gap { desired: "tmux server up".into(), observed: "no server".into(), since_hint: None },
            remedy: cockpit_remedy(cfg),
        };
    }

    let dash_session = match cfg.cockpit_sessions.first() {
        Some(s) => s.clone(),
        None => "brain".to_string(),
    };
    let mut sessions = cfg.cockpit_sessions.clone();
    sessions.push("cockpit".to_string());
    for s in &sessions {
        if !run_cmd_ok(&cfg.tmux, &["has-session", "-t", &format!("={}", s)]) {
            return Check {
                key: "cockpit".into(),
                raw: RawStatus::Gap {
                    desired: "sessions present".into(),
                    observed: format!("session {} missing", s),
                    since_hint: None,
                },
                remedy: cockpit_remedy(cfg),
            };
        }
    }

    let tags_out = run_cmd(&cfg.tmux, &["list-panes", "-t", &format!("{}:0", dash_session), "-F", "#{@cockpit}"]);
    let tags: Vec<&str> = tags_out.lines().map(str::trim).filter(|l| !l.is_empty()).collect();

    let has_health = tags.contains(&"health");
    let needs_mail = !cfg.cockpit_mail.is_empty();
    let has_mail = tags.contains(&"mail");

    if !has_health || (needs_mail && !has_mail) {
        return Check {
            key: "cockpit".into(),
            raw: RawStatus::Gap {
                desired: "health + mail dashboards".into(),
                observed: tags.join(","),
                since_hint: None,
            },
            remedy: cockpit_remedy(cfg),
        };
    }

    Check { key: "cockpit".into(), raw: RawStatus::Satisfied, remedy: Remedy::Escalate }
}

fn cockpit_remedy(cfg: &Config) -> Remedy {
    Remedy::Command { program: "bash".into(), args: vec![cfg.cockpit_sh.clone(), "--no-attach".into()] }
}

// ──────────────────────────────────────────────────────────────────────────────
// Production/Release: the checkout SPIRA_HOME names is the activated release, or main.
// Read-only — the production checkout is never written by this program.
// ──────────────────────────────────────────────────────────────────────────────

fn on_main_branch(cfg: &Config) -> Option<bool> {
    let out = Command::new(&cfg.git)
        .args(["-C", &cfg.spira_home, "rev-parse", "--abbrev-ref", "HEAD"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok().map(|s| s.trim() == "main")
}

fn observe_release(cfg: &Config) -> Check {
    let home_resolved = fs::canonicalize(&cfg.spira_home).ok();
    let releases_current = if cfg.releases_dir.as_os_str().is_empty() {
        None
    } else {
        fs::canonicalize(cfg.releases_dir.join("current")).ok()
    };

    let at_release = match (&home_resolved, &releases_current) {
        (Some(h), Some(c)) => h.starts_with(c) || h == c,
        _ => false,
    };
    let on_main = on_main_branch(cfg);

    if at_release || on_main == Some(true) {
        return Check { key: "release".into(), raw: RawStatus::Satisfied, remedy: Remedy::Escalate };
    }
    if home_resolved.is_none() || on_main.is_none() {
        return Check {
            key: "release".into(),
            raw: RawStatus::Unobservable { reason: "checkout or branch unreadable".into() },
            remedy: Remedy::Escalate,
        };
    }
    Check {
        key: "release".into(),
        raw: RawStatus::Gap {
            desired: "activated release or main".into(),
            observed: format!("{:?}", home_resolved.unwrap()),
            since_hint: None,
        },
        remedy: Remedy::Escalate,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Queue: two structural checks czar-pass's own detectors do not already cover. The base
// gate itself is base-red, already watched by czar-pass — reconciling it here too would
// file a second incident for the same fault, so it is deliberately not repeated.
// ──────────────────────────────────────────────────────────────────────────────

fn branch_mergeable(cfg: &Config, repo: &Path, base: &str, branch: &str) -> Option<bool> {
    let repo_str = repo.to_string_lossy().to_string();
    let mb_out = Command::new(&cfg.git)
        .args(["-C", &repo_str, "merge-base", base, branch])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !mb_out.status.success() {
        return None;
    }
    let merge_base = String::from_utf8(mb_out.stdout).ok()?.trim().to_string();
    if merge_base.is_empty() {
        return None;
    }
    let mt_out = Command::new(&cfg.git)
        .args(["-C", &repo_str, "merge-tree", &merge_base, base, branch])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&mt_out.stdout);
    Some(!text.contains("<<<<<<<"))
}

fn observe_queue_mergeable(cfg: &Config) -> Vec<Check> {
    let mut checks = Vec::new();
    for repo_name in queue_repo_names(&cfg.queue_dir) {
        let repo_path = match repo_root(&repo_name, &cfg.repo_map) {
            Some(p) => p,
            None => continue,
        };
        let out = run_cmd("bash", &[&cfg.queue_certified_list_sh, &repo_name]);
        for line in out.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() != 3 {
                continue;
            }
            let (id, branch, base) = (parts[0], parts[1], parts[2]);
            let key = format!("queue-mergeable:{}:{}", repo_name, id);
            let raw = match branch_mergeable(cfg, &repo_path, base, branch) {
                None => RawStatus::Unobservable { reason: format!("cannot read {} in {}", branch, repo_name) },
                Some(true) => RawStatus::Satisfied,
                Some(false) => RawStatus::Gap {
                    desired: format!("{} merges onto {}", branch, base),
                    observed: "conflict".into(),
                    since_hint: None,
                },
            };
            checks.push(Check {
                key,
                raw,
                remedy: Remedy::Command {
                    program: "bash".into(),
                    args: vec![
                        cfg.queue_sh.clone(),
                        "eject".into(),
                        id.to_string(),
                        "--reason".into(),
                        "reconciler: certified branch no longer merges onto its base — needs rebase".into(),
                        repo_name.clone(),
                    ],
                },
            });
        }
    }
    checks
}

fn observe_queue_lock_age(cfg: &Config) -> Vec<Check> {
    let mut checks = Vec::new();
    for repo_name in queue_repo_names(&cfg.queue_dir) {
        let lock_path = cfg.queue_dir.join(&repo_name).join("lock");
        if !lock_path.exists() {
            continue;
        }
        let file = match fs::OpenOptions::new().read(true).write(true).open(&lock_path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let held = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0;
        if !held {
            // We took the lock ourselves — nobody else holds it. Release at once.
            unsafe { flock(file.as_raw_fd(), 8 /* LOCK_UN */) };
            continue;
        }
        let mtime = fs::metadata(&lock_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        let raw = match mtime {
            None => RawStatus::Unobservable { reason: "lock file mtime unreadable".into() },
            Some(mt) => {
                let age = cfg.now_secs.saturating_sub(mt);
                if age > cfg.preflight_wall_secs {
                    RawStatus::Gap {
                        desired: format!("< {}s", cfg.preflight_wall_secs),
                        observed: format!("{}s", age),
                        since_hint: Some(mt),
                    }
                } else {
                    RawStatus::Satisfied
                }
            }
        };
        checks.push(Check {
            key: format!("queue-lock-age:{}", repo_name),
            raw,
            remedy: Remedy::Escalate,
        });
    }
    checks
}

// ──────────────────────────────────────────────────────────────────────────────
// evaluate: run one Check through the pure engine, act on the verdict.
// ──────────────────────────────────────────────────────────────────────────────

fn evaluate(cfg: &Config, state: &mut StateMap, check: Check) {
    let prev = state.remove(&check.key).unwrap_or_default();
    let (verdict, mut next) = step(cfg.now_secs, check.raw, cfg.grace_secs, prev);
    append_status(&cfg.status_log, &cfg.now_iso, &check.key, &verdict);

    if verdict.is_gap {
        if verdict.remedy_failed {
            escalate(cfg, &check.key, &verdict);
        } else {
            match &check.remedy {
                Remedy::Escalate => escalate(cfg, &check.key, &verdict),
                Remedy::Command { program, args } => {
                    let desc = format!("{} {}", program, args.join(" "));
                    log_print(cfg, &format!("reconciler: {} → remedy: {}", check.key, desc));
                    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
                    let _ = run_cmd_ok(program, &arg_refs);
                    record_remedy(&mut next, cfg.now_secs, &desc);
                }
            }
        }
    }

    if next != HysteresisState::default() {
        state.insert(check.key, next);
    }
}

fn describe(verdict: &Verdict) -> String {
    match &verdict.status {
        RawStatus::Satisfied => "satisfied".to_string(),
        RawStatus::Gap { desired, observed, .. } => format!("desired={} observed={}", desired, observed),
        RawStatus::Unobservable { reason } => format!("unobservable: {}", reason),
    }
}

fn escalate(cfg: &Config, key: &str, verdict: &Verdict) {
    let subj = format!("RECONCILER: {} — {}", key, describe(verdict));
    let age = verdict.since.map(|s| cfg.now_secs.saturating_sub(s)).unwrap_or(0);
    let body = format!(
        "The reconciler found a structural gap that has outlasted its grace period.\n\n\
         key      {}\n\
         age      {}s\n\
         status   {}\n\
         remedy attempted and did not close it: {}\n",
        key, age, describe(verdict), verdict.remedy_failed
    );
    log_print(cfg, &format!("reconciler: {} → escalate", key));

    let mut labels = cfg.reconciler_label.clone();
    if !cfg.scope_label.is_empty() {
        labels = format!("{},{}", cfg.scope_label, labels);
    }
    let child = Command::new("bash")
        .arg(&cfg.incident_sh)
        .arg("file")
        .arg(&subj)
        .arg("-")
        .env("SPIRA_DB", &cfg.spira_db)
        .env("SPIRA_INCIDENT_LABELS", labels)
        .env("SPIRA_INCIDENT_TYPE", "task")
        .env("SPIRA_INCIDENT_PRIORITY", "1")
        .env("SPIRA_INCIDENT_ACTOR", "reconciler")
        .env("SPIRA_SIN_EXEMPT", "1")
        .env("SPIRA_INCIDENT_REPO", "spira")
        .env("SPIRA_INCIDENT_REF", format!("incident:reconciler:{}", key))
        .env("SPIRA_INCIDENT_CAUSE", key)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(body.as_bytes());
        }
        let _ = child.wait();
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Main pass
// ──────────────────────────────────────────────────────────────────────────────

fn run_pass() -> Result<(), String> {
    let cfg = Config::from_env();

    if cfg.spira_run.join("world.halted").exists() {
        log_print(&cfg, "reconciler: skipped — world is halted");
        return Ok(());
    }

    let lock_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&cfg.lock_path)
        .map_err(|e| format!("open lock {}: {}", cfg.lock_path.display(), e))?;
    if unsafe { flock(lock_file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
        log_print(&cfg, "reconciler: already running — skip");
        return Ok(());
    }

    let mut state = load_state(&cfg.state_path);

    let mut checks = Vec::new();
    checks.extend(observe_units(&cfg));
    checks.extend(observe_fleet(&cfg));
    checks.push(observe_cockpit(&cfg));
    checks.push(observe_release(&cfg));
    checks.extend(observe_queue_mergeable(&cfg));
    checks.extend(observe_queue_lock_age(&cfg));

    let n = checks.len();
    for check in checks {
        evaluate(&cfg, &mut state, check);
    }

    let _ = save_state(&cfg.state_path, &state);

    let elapsed = unix_now().saturating_sub(cfg.now_secs);
    log_print(&cfg, &format!("reconciler: complete ({} checks, {}s)", n, elapsed));

    drop(lock_file);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "reconciler-test-{}-{}-{}",
            name,
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn repo_root_finds_the_named_repo() {
        let dir = scratch_dir("repo-root");
        let map = dir.join("repo-map");
        fs::write(&map, "spira | /srv/checkouts/spira | queue | origin/main | | \n").unwrap();
        assert_eq!(repo_root("spira", &Some(map)), Some(PathBuf::from("/srv/checkouts/spira")));
    }

    #[test]
    fn repo_root_with_no_map_configured_is_none() {
        assert_eq!(repo_root("spira", &None), None);
    }

    #[test]
    fn queue_repo_names_lists_only_directories() {
        let dir = scratch_dir("queue-repo-names");
        fs::create_dir_all(dir.join("spira")).unwrap();
        fs::write(dir.join("not-a-dir"), "x").unwrap();
        assert_eq!(queue_repo_names(&dir), vec!["spira".to_string()]);
    }

    #[test]
    fn queue_repo_names_missing_dir_is_empty() {
        let dir = scratch_dir("queue-repo-names-missing").join("does-not-exist");
        assert!(queue_repo_names(&dir).is_empty());
    }

    #[test]
    fn describe_reports_the_raw_reading_not_a_smoothed_word() {
        let (verdict, _) = step(
            100,
            RawStatus::Gap { desired: "d".into(), observed: "o".into(), since_hint: None },
            0,
            HysteresisState::default(),
        );
        assert_eq!(describe(&verdict), "desired=d observed=o");
    }

    #[test]
    fn evaluate_inside_grace_raises_nothing() {
        let dir = scratch_dir("evaluate-inside-grace");
        let marker = dir.join("remedy-ran");
        let cfg = Config { grace_secs: 300, now_secs: 100, ..test_config() };
        let mut state = StateMap::new();
        evaluate(&cfg, &mut state, Check {
            key: "k".into(),
            raw: RawStatus::Gap { desired: "d".into(), observed: "o".into(), since_hint: None },
            remedy: Remedy::Command { program: "touch".into(), args: vec![marker.to_string_lossy().to_string()] },
        });
        assert!(!marker.exists(), "a gap inside its grace period must not run a remedy");
        let (_, expect_next) = step(
            100,
            RawStatus::Gap { desired: "d".into(), observed: "o".into(), since_hint: None },
            300,
            HysteresisState::default(),
        );
        assert_eq!(state.get("k"), Some(&expect_next), "the streak's `since` must still be recorded");
    }

    #[test]
    fn evaluate_runs_a_command_remedy_once_the_gap_outlasts_grace() {
        let dir = scratch_dir("evaluate-runs-remedy");
        let marker = dir.join("remedy-ran");
        let cfg = Config { grace_secs: 0, now_secs: 100, ..test_config() };
        let mut state = StateMap::new();
        evaluate(&cfg, &mut state, Check {
            key: "k".into(),
            raw: RawStatus::Gap { desired: "d".into(), observed: "o".into(), since_hint: None },
            remedy: Remedy::Command { program: "touch".into(), args: vec![marker.to_string_lossy().to_string()] },
        });
        assert!(marker.exists(), "a gap past grace with a command remedy must run it");
    }

    #[test]
    fn evaluate_remedy_failure_escalates_instead_of_retrying_blind() {
        let dir = scratch_dir("evaluate-remedy-failure");
        let marker = dir.join("remedy-ran");
        let escalated = dir.join("escalated");
        let stub_incident = dir.join("stub-incident.sh");
        fs::write(&stub_incident, format!("#!/usr/bin/env bash\ncat >/dev/null\ntouch {}\n", escalated.display())).unwrap();
        Command::new("chmod").args(["+x", stub_incident.to_str().unwrap()]).status().unwrap();

        let cfg1 = Config { grace_secs: 0, now_secs: 100, incident_sh: stub_incident.to_string_lossy().to_string(), ..test_config() };
        let mut state = StateMap::new();
        let raw = || RawStatus::Gap { desired: "d".into(), observed: "o".into(), since_hint: None };

        // Pass 1: past grace, command remedy attempted and recorded.
        evaluate(&cfg1, &mut state, Check {
            key: "k".into(),
            raw: raw(),
            remedy: Remedy::Command { program: "touch".into(), args: vec![marker.to_string_lossy().to_string()] },
        });
        assert!(marker.exists());
        fs::remove_file(&marker).unwrap();

        // Pass 2: still a gap — the engine reports remedy_failed, so evaluate must escalate
        // rather than run the command a second time.
        let cfg2 = Config { now_secs: 200, ..test_config_from(&cfg1) };
        evaluate(&cfg2, &mut state, Check {
            key: "k".into(),
            raw: raw(),
            remedy: Remedy::Command { program: "touch".into(), args: vec![marker.to_string_lossy().to_string()] },
        });
        assert!(!marker.exists(), "a remedy that did not close its gap must not be retried blind");
        assert!(escalated.exists(), "a remedy that did not close its gap must escalate on the next pass");
    }

    #[test]
    fn evaluate_unobservable_inside_grace_raises_nothing() {
        let dir = scratch_dir("evaluate-unobservable-grace");
        let escalated = dir.join("escalated");
        let stub_incident = dir.join("stub-incident.sh");
        fs::write(&stub_incident, format!("#!/usr/bin/env bash\ncat >/dev/null\ntouch {}\n", escalated.display())).unwrap();
        Command::new("chmod").args(["+x", stub_incident.to_str().unwrap()]).status().unwrap();

        let cfg = Config { grace_secs: 300, now_secs: 100, incident_sh: stub_incident.to_string_lossy().to_string(), ..test_config() };
        let mut state = StateMap::new();
        evaluate(&cfg, &mut state, Check {
            key: "k".into(),
            raw: RawStatus::Unobservable { reason: "cannot read it".into() },
            remedy: Remedy::Escalate,
        });
        assert!(!escalated.exists(), "an unobservable reading inside its grace period must not escalate");
    }

    #[test]
    fn evaluate_unobservable_past_grace_escalates_but_status_is_never_satisfied() {
        let dir = scratch_dir("evaluate-unobservable-escalate");
        let escalated = dir.join("escalated");
        let stub_incident = dir.join("stub-incident.sh");
        fs::write(&stub_incident, format!("#!/usr/bin/env bash\ncat >/dev/null\ntouch {}\n", escalated.display())).unwrap();
        Command::new("chmod").args(["+x", stub_incident.to_str().unwrap()]).status().unwrap();

        let cfg = Config { grace_secs: 0, now_secs: 100, incident_sh: stub_incident.to_string_lossy().to_string(), ..test_config() };
        let mut state = StateMap::new();
        evaluate(&cfg, &mut state, Check {
            key: "k".into(),
            raw: RawStatus::Unobservable { reason: "cannot read it".into() },
            remedy: Remedy::Escalate,
        });
        assert!(escalated.exists(), "unobservable sustained past grace must still escalate (law-a-control-that-cannot-check-must-refuse)");
    }

    fn test_config() -> Config {
        Config {
            spira_run: PathBuf::from("/tmp"),
            spira_home: String::new(),
            log: PathBuf::from("/dev/null"),
            state_path: PathBuf::from("/dev/null"),
            status_log: PathBuf::from("/dev/null"),
            lock_path: PathBuf::from("/dev/null"),
            spira_db: String::new(),
            scope_label: String::new(),
            reconciler_label: "reconciler-gap".into(),
            incident_sh: "/bin/true".into(),
            systemctl: "systemctl".into(),
            tmux: "tmux".into(),
            git: "git".into(),
            units_manifest_sh: String::new(),
            fleet_status_sh: String::new(),
            queue_certified_list_sh: String::new(),
            cockpit_sh: String::new(),
            queue_sh: String::new(),
            queue_dir: PathBuf::from("/dev/null"),
            repo_map: None,
            releases_dir: PathBuf::new(),
            store_unit: "dolt-beads.service".into(),
            cockpit_sessions: vec!["brain".into(), "hunk".into(), "chat".into()],
            cockpit_mail: String::new(),
            grace_secs: 300,
            preflight_wall_secs: 240,
            now_secs: 0,
            now_iso: "2026-09-25T00:00:00Z".into(),
        }
    }

    fn test_config_from(base: &Config) -> Config {
        Config {
            spira_run: base.spira_run.clone(),
            spira_home: base.spira_home.clone(),
            log: base.log.clone(),
            state_path: base.state_path.clone(),
            status_log: base.status_log.clone(),
            lock_path: base.lock_path.clone(),
            spira_db: base.spira_db.clone(),
            scope_label: base.scope_label.clone(),
            reconciler_label: base.reconciler_label.clone(),
            incident_sh: base.incident_sh.clone(),
            systemctl: base.systemctl.clone(),
            tmux: base.tmux.clone(),
            git: base.git.clone(),
            units_manifest_sh: base.units_manifest_sh.clone(),
            fleet_status_sh: base.fleet_status_sh.clone(),
            queue_certified_list_sh: base.queue_certified_list_sh.clone(),
            cockpit_sh: base.cockpit_sh.clone(),
            queue_sh: base.queue_sh.clone(),
            queue_dir: base.queue_dir.clone(),
            repo_map: base.repo_map.clone(),
            releases_dir: base.releases_dir.clone(),
            store_unit: base.store_unit.clone(),
            cockpit_sessions: base.cockpit_sessions.clone(),
            cockpit_mail: base.cockpit_mail.clone(),
            grace_secs: base.grace_secs,
            preflight_wall_secs: base.preflight_wall_secs,
            now_secs: base.now_secs,
            now_iso: base.now_iso.clone(),
        }
    }
}
