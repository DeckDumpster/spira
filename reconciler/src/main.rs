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
// does not already watch: Fleet, Units, Store, Cockpit, Production/Release, Disk, and two
// Queue checks czar-pass's own detectors do not cover (the base gate itself is base-red,
// already watched there — reconciling it a second time here would file a second incident
// for the same fault).

use reconciler_engine::core::{record_remedy, step, HysteresisState, RawStatus, Verdict};
use reconciler_engine::effect::{
    append_event, compose_no_effect, is_shadowed, kind_of, load_pending, measure, save_pending, Outcome,
    PendingEffect, PendingMap, RemedyEvent,
};
use reconciler_engine::io::{append_status, load_state, save_state, StateMap};
use spira_desired_state::compose::Composite;
use spira_desired_state::resource::{parse_resource, CockpitSpec, DiskSpec, FleetSpec, KindSpec, ReleaseSpec, UnitsSpec};
use spira_desired_state::store::FsStore;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

// ──────────────────────────────────────────────────────────────────────────────
// Desired state (sp-8c3ib): the typed Composite this host last materialised via `spira
// apply`/`spira compose` (spira-desired-state) is the primary source for Fleet, Units,
// Cockpit and Release. A kind absent from the Composite — including "no Composite at all",
// the state of every install that has not yet adopted it — falls back to the bash/env
// default that kind always had, per resource rather than for the whole pass.
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Default, Clone)]
struct DesiredState {
    fleet: Option<FleetSpec>,
    units: Option<UnitsSpec>,
    cockpit: Option<CockpitSpec>,
    release: Option<ReleaseSpec>,
    disk: Option<DiskSpec>,
}

impl DesiredState {
    fn load(cfg_log: impl Fn(&str), dir: &Path) -> DesiredState {
        let composite = match FsStore::new(dir).read_current() {
            Ok(Some(c)) => c,
            Ok(None) => return DesiredState::default(),
            Err(e) => {
                cfg_log(&format!("reconciler: desired-state at {} unreadable: {}", dir.display(), e));
                return DesiredState::default();
            }
        };
        DesiredState::from_composite(&composite)
    }

    fn from_composite(composite: &Composite) -> DesiredState {
        let mut ds = DesiredState::default();
        for resource in composite.resources.values() {
            let parsed = match parse_resource(&resource.raw) {
                Ok(p) => p,
                Err(_) => continue,
            };
            match parsed.spec {
                KindSpec::Fleet(f) => ds.fleet = Some(f),
                KindSpec::Units(u) => ds.units = Some(u),
                KindSpec::Cockpit(c) => ds.cockpit = Some(c),
                KindSpec::Release(r) => ds.release = Some(r),
                KindSpec::Disk(d) => ds.disk = Some(d),
                _ => {}
            }
        }
        ds
    }
}

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
    remedy_log: PathBuf,
    effect_state: PathBuf,
    mail_sh: String,
    release: String,
    shadow_kinds: Vec<String>,
    effect_passes: u32,
    lock_path: PathBuf,
    spira_db: String,
    scope_label: String,
    reconciler_label: String,
    incident_sh: String,
    systemctl: String,
    ctrl_path: PathBuf,
    instance: String,
    reminders_path: PathBuf,
    tmux: String,
    git: String,
    units_manifest_sh: String,
    fleet_status_sh: String,
    queue_certified_list_sh: String,
    cockpit_sh: String,
    queue_bin: String,
    lc_bin: String,
    bd_bin: String,
    queue_dir: PathBuf,
    repo_map: Option<PathBuf>,
    releases_dir: PathBuf,
    store_unit: String,
    cockpit_sessions: Vec<String>,
    cockpit_mail: String,
    disk_usage_sh: String,
    disk_remedy_sh: String,
    disk_floor_pct: u32,
    grace_secs: u64,
    preflight_wall_secs: u64,
    now_secs: u64,
    now_iso: String,
    desired_dir: PathBuf,
    desired: DesiredState,
}

/// `$SPIRA_HOME`, else the first ancestor of this executable that holds `lib.sh` — same
/// fallback `spira_world::locate_home`/`mail::env::locate_home`/landing-pass's own
/// `harness_home` already use, and the same one `reconciler-flow`'s own copy of this
/// function uses. `resolve_run_dir` needs a REAL `home/conf.d` to resolve `SPIRA_RUN`
/// (sp-ivfu3) — an empty `home` makes it refuse outright ("no config registry at
/// conf.d"), exactly what a bare shell with no `$SPIRA_HOME` exported would otherwise hit.
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

/// The release the running harness copy is: the directory name above its `spira/` dir, or
/// `unknown` when the home cannot be located.
fn release_name(home: &Path) -> String {
    home.parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

impl Config {
    fn from_env() -> Result<Config, String> {
        use spira_config::process::{cfg, cfg_parse};

        // SPIRA_RUN is a PROCEDURAL registry key (spira/conf.d/SPIRA_RUN carries no
        // generated default — spira/conf.sh's spira_conf_defaults() still sets it inline),
        // so it is resolved through `resolve_run_dir` rather than a bare `cfg("SPIRA_RUN")`
        // — same as reconciler-flow's own copy of this read. A `spira.toml` that fails to
        // resolve, or resolves SPIRA_RUN empty, is a named refusal (sp-ivfu3), never the
        // literal `/tmp/spira` a bare shell used to get.
        let env_map: std::collections::BTreeMap<String, String> = env::vars().collect();
        let spira_run = spira_config::resolve::resolve_run_dir(&env_map, &harness_home())?;
        // SPIRA_HOME is not a registered config key (spira/conf.d) — a per-copy fact, read
        // from the raw environment same as always.
        let spira_home = env::var("SPIRA_HOME").unwrap_or_default();
        let sessions = cfg("COCKPIT_SESSIONS")?.split_whitespace().map(String::from).collect();
        Ok(Config {
            // SPIRA_RECONCILER_LOG/_STATE/_STATUS_LOG are not registered config keys —
            // per-invocation paths under spira_run, left as direct env reads.
            log: env::var("SPIRA_RECONCILER_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("reconciler.log")),
            state_path: env::var("SPIRA_RECONCILER_STATE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("reconciler-state.json")),
            // Under run/tsd/ (design reconciler-time-series-2026-09-27 §2): the reconciler's
            // per-resource status is one of the families that live there, moved from
            // $SPIRA_RUN directly.
            status_log: env::var("SPIRA_RECONCILER_STATUS_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("tsd").join("reconciler-status.jsonl")),
            remedy_log: env::var("SPIRA_RECONCILER_REMEDY_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("tsd").join("reconciler-remedy.jsonl")),
            effect_state: env::var("SPIRA_RECONCILER_EFFECT_STATE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("reconciler-effects.json")),
            // SPIRA_MAIL_SH / SPIRA_RECONCILER_SHADOW_KINDS / SPIRA_RECONCILER_EFFECT_PASSES
            // are not registered config keys — left as direct env reads.
            mail_sh: env::var("SPIRA_MAIL_SH").unwrap_or_else(|_| "mail".to_string()),
            release: release_name(&harness_home()),
            shadow_kinds: env::var("SPIRA_RECONCILER_SHADOW_KINDS")
                .unwrap_or_default()
                .split_whitespace()
                .map(String::from)
                .collect(),
            effect_passes: env::var("SPIRA_RECONCILER_EFFECT_PASSES")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|n| *n > 0)
                .unwrap_or(2),
            lock_path: spira_run.join("reconciler.lock"),
            spira_db: cfg("SPIRA_DB")?,
            scope_label: cfg("SPIRA_SCOPE_LABEL")?,
            reconciler_label: cfg("SPIRA_RECONCILER_LABEL")?,
            // SPIRA_INCIDENT_SH / SPIRA_SYSTEMCTL / SPIRA_TMUX / SPIRA_GIT /
            // SPIRA_UNITS_MANIFEST_SH / SPIRA_FLEET_STATUS_SH / SPIRA_QUEUE_CERTIFIED_LIST_SH
            // / SPIRA_COCKPIT_SH are not registered config keys — script/binary names,
            // overridable only as a test seam, left as direct env reads.
            incident_sh: env::var("SPIRA_INCIDENT_SH")
                .unwrap_or_else(|_| "incident.sh".to_string()),
            systemctl: env::var("SPIRA_SYSTEMCTL").unwrap_or_else(|_| "systemctl".to_string()),
            ctrl_path: PathBuf::from(cfg("SPIRA_CTRL")?),
            instance: cfg("SPIRA_INSTANCE")?,
            reminders_path: spira_run.join("reconciler-reminders.json"),
            tmux: env::var("SPIRA_TMUX").unwrap_or_else(|_| "tmux".to_string()),
            git: env::var("SPIRA_GIT").unwrap_or_else(|_| "git".to_string()),
            units_manifest_sh: env::var("SPIRA_UNITS_MANIFEST_SH")
                .unwrap_or_else(|_| "units-manifest.sh".to_string()),
            fleet_status_sh: env::var("SPIRA_FLEET_STATUS_SH")
                .unwrap_or_else(|_| "fleet-status.sh".to_string()),
            queue_certified_list_sh: env::var("SPIRA_QUEUE_CERTIFIED_LIST_SH")
                .unwrap_or_else(|_| "queue-certified-list.sh".to_string()),
            cockpit_sh: env::var("SPIRA_COCKPIT_SH")
                .unwrap_or_else(|_| "cockpit.sh".to_string()),
            // The queue binary, by name on the launcher's PATH (sp-gypjk).
            queue_bin: "queue".into(),
            lc_bin: spira_config::lifecycle_row::lc_bin(),
            bd_bin: cfg("SPIRA_BD")?,
            queue_dir: PathBuf::from(cfg("SPIRA_QUEUE_DIR")?),
            repo_map: Some(PathBuf::from(cfg("SPIRA_REPO_MAP")?)),
            releases_dir: PathBuf::from(cfg("SPIRA_RELEASES")?),
            // SPIRA_STORE_UNIT is not a registered config key — left as a direct env read.
            store_unit: env::var("SPIRA_STORE_UNIT")
                .unwrap_or_else(|_| "dolt-beads.service".to_string()),
            cockpit_sessions: sessions,
            cockpit_mail: cfg("COCKPIT_MAIL")?,
            // SPIRA_DISK_USAGE_SH / SPIRA_DISK_REMEDY_SH are not registered config keys —
            // left as direct env reads.
            disk_usage_sh: env::var("SPIRA_DISK_USAGE_SH")
                .unwrap_or_else(|_| "disk-usage.sh".to_string()),
            disk_remedy_sh: env::var("SPIRA_DISK_REMEDY_SH")
                .unwrap_or_else(|_| "disk-remedy.sh".to_string()),
            disk_floor_pct: cfg_parse::<u32>("SPIRA_DISK_FLOOR_PCT")?,
            grace_secs: cfg_parse::<u64>("SPIRA_RECONCILER_GRACE_SECS")?,
            preflight_wall_secs: cfg_parse::<u64>("SPIRA_PREFLIGHT_WALL_SECS")?,
            now_secs: unix_now(),
            now_iso: compute_now_iso(),
            desired_dir: spira_desired_state::store::default_dir(),
            desired: DesiredState::default(),
            spira_run,
            spira_home,
        })
    }
}

impl Config {
    fn with_desired_state(mut self) -> Config {
        self.desired = DesiredState::load(|msg| eprintln!("{}", msg), &self.desired_dir);
        self
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
    spira_config::bounded::bounded(program)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default()
}

fn run_cmd_ok(program: &str, args: &[&str]) -> bool {
    spira_config::bounded::bounded(program)
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
    let out = spira_config::bounded::bounded(&cfg.systemctl)
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

/// A unit's desired (enabled, active) — from the Composite's UnitsSpec when it names the
/// unit, else the legacy assumption every name in the manifest carries: it should be both
/// (sp-8c3ib: units-manifest.sh's ENABLE array only ever lists what should be enabled, never
/// what should not be, so a unit the Composite has not caught up to yet keeps the old default
/// rather than being treated as having no desired state at all).
fn desired_unit_state(cfg: &Config, unit: &str) -> (bool, bool) {
    match &cfg.desired.units {
        Some(spec) => match spec.units.iter().find(|u| u.name == unit) {
            Some(u) => (u.enabled, u.active),
            None => (true, true),
        },
        None => (true, true),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Declared overrides: a spira-ctrl suspension is desired state. The reconciler reads it
// every pass, never remedies what it covers, and reminds once when it falls due — it never
// lifts one.
// ──────────────────────────────────────────────────────────────────────────────

fn load_ctrl(cfg: &Config) -> spira_ctrl::CtrlData {
    spira_ctrl::read(&cfg.ctrl_path).unwrap_or_else(|e| {
        log_print(cfg, &format!("reconciler: control file unreadable, no override honoured: {}", e));
        spira_ctrl::CtrlData::new()
    })
}

/// The suspension covering `unit`, as (subject, reason). A subject names a unit by its full
/// name, its name without the extension, or that without the instance suffix.
fn suspension_of(ctrl: &spira_ctrl::CtrlData, instance: &str, unit: &str) -> Option<(String, String)> {
    let subjects = [
        unit.to_string(),
        unit.rsplit_once('.').map(|(b, _)| b.to_string()).unwrap_or_else(|| unit.to_string()),
        spira_ctrl::subject_of_masked_unit(unit, instance),
    ];
    subjects.into_iter().find_map(|s| {
        spira_ctrl::reason(ctrl, &s).map(|r| (s, r.to_string()))
    })
}

#[derive(Debug, PartialEq, Eq)]
enum Due {
    NotDue,
    Until(String),
    OwnerLanded(String),
}

/// A suspension is due when its `until` date has come, or its owner bead has landed.
fn reminder_due(owner: &str, until: Option<&str>, today: &str, owner_landed: bool) -> Due {
    if let Some(u) = until.filter(|u| *u <= today) {
        return Due::Until(u.to_string());
    }
    if owner_landed {
        return Due::OwnerLanded(owner.to_string());
    }
    Due::NotDue
}

fn owner_landed(cfg: &Config, owner: &str) -> bool {
    if owner.is_empty() || cfg.lc_bin.is_empty() {
        return false;
    }
    matches!(spira_config::lc_state::row_with(&cfg.lc_bin, owner), Ok(Some(r)) if r.state == "LANDED")
}

/// Mails the Concierge once per (subject, owner, until) declaration when it falls due. The
/// sent set is persisted; a failed send is not recorded, so it is retried next pass.
fn remind_due_suspensions(cfg: &Config, ctrl: &spira_ctrl::CtrlData) {
    let today = &cfg.now_iso[..cfg.now_iso.len().min(10)];
    let mut sent: std::collections::BTreeSet<String> = fs::read_to_string(&cfg.reminders_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let live: std::collections::BTreeSet<String> = ctrl
        .keys()
        .filter_map(|s| spira_ctrl::declared(ctrl, s).map(|(o, u)| format!("{}|{}|{}", s, o, u.unwrap_or(""))))
        .collect();
    let before = sent.clone();
    sent.retain(|k| live.contains(k));
    for subject in ctrl.keys() {
        let Some((owner, until)) = spira_ctrl::declared(ctrl, subject) else { continue };
        let marker = format!("{}|{}|{}", subject, owner, until.unwrap_or(""));
        if sent.contains(&marker) {
            continue;
        }
        let landed = owner_landed(cfg, owner);
        let why = match reminder_due(owner, until, today, landed) {
            Due::NotDue => continue,
            Due::Until(u) => format!("its until date {} has come", u),
            Due::OwnerLanded(o) => format!("its owner bead {} has landed", o),
        };
        let reason = spira_ctrl::reason(ctrl, subject).unwrap_or("");
        let subj = format!("reconciler: suspension of {} is due for review", subject);
        let body = format!(
            "The suspension of {subject} is due: {why}.\n\nreason  {reason}\nowner   {owner}\n\n\
             The reconciler has not lifted it and will not. Lift it with `ctrl resume {subject}`, or \
             extend it by suspending again with a new owner or --until.\n"
        );
        match mail_concierge(&cfg.mail_sh, &subj, &body) {
            Ok(()) => {
                log_print(cfg, &format!("reconciler: reminded — {}: {}", subject, why));
                sent.insert(marker);
            }
            Err(e) => log_print(cfg, &format!("reconciler: reminder for {} not sent: {}", subject, e)),
        }
    }
    if sent != before {
        if let Ok(json) = serde_json::to_string_pretty(&sent) {
            let _ = fs::write(&cfg.reminders_path, json);
        }
    }
}

fn unit_key(cfg: &Config, unit: &str) -> String {
    if unit == cfg.store_unit {
        format!("store:{}", unit)
    } else if unit.ends_with(".timer") {
        format!("units-timer:{}", unit)
    } else {
        format!("units-daemon:{}", unit)
    }
}

fn observe_units(cfg: &Config) -> Vec<Check> {
    // The Composite's UnitsSpec.units is the declared unit set (sp-8c3ib); an install that
    // has not materialised one yet falls back to units-manifest.sh's own walk of
    // systemd/units.sh's ENABLE array, exactly as before.
    let manifest_names;
    let unit_names: Vec<&str> = match &cfg.desired.units {
        Some(spec) if !spec.units.is_empty() => spec.units.iter().map(|u| u.name.as_str()).collect(),
        _ => {
            manifest_names = run_cmd("bash", &[&cfg.units_manifest_sh]);
            manifest_names.lines().map(str::trim).filter(|l| !l.is_empty()).collect()
        }
    };
    let ctrl = load_ctrl(cfg);
    let mut checks = Vec::new();
    for unit in unit_names {
        if let Some((subject, reason)) = suspension_of(&ctrl, &cfg.instance, unit) {
            checks.push(Check {
                key: unit_key(cfg, unit),
                raw: RawStatus::Deliberate { reason: format!("suspended ({}): {}", subject, reason) },
                remedy: Remedy::Escalate,
            });
            continue;
        }
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

        let (want_enabled, want_active) = desired_unit_state(cfg, unit);

        if unit.ends_with(".timer") {
            let raw = if is_enabled == want_enabled && is_active == want_active {
                RawStatus::Satisfied
            } else {
                RawStatus::Gap {
                    desired: format!("enabled={},active={}", want_enabled, want_active),
                    observed: format!("{},{}", enabled, active),
                    since_hint: None,
                }
            };
            // No deterministic remedy exists here for anything but "should be enabled and
            // active" — the only shape units-manifest.sh (or the Composite's own
            // examples/default.toml) has ever declared.
            let remedy = if want_enabled && want_active {
                Remedy::Command {
                    program: cfg.systemctl.clone(),
                    args: vec!["--user".into(), "enable".into(), "--now".into(), unit.to_string()],
                }
            } else {
                Remedy::Escalate
            };
            checks.push(Check { key: format!("units-timer:{}", unit), raw, remedy });
            continue;
        }

        // A daemon row: sp-ocmes evidence, 2026-09-25: several *.service rows were found
        // disabled, not merely stopped. An inactive-but-wanted-active daemon is restarted; an
        // active-but-not-enabled one wanted enabled is enabled WITHOUT --now, so a unit
        // already running is never bounced just to satisfy its enablement bit. Wanting a
        // daemon down is not a shape this bead's remedy list covers, so it always escalates.
        let raw = if is_active == want_active && is_enabled == want_enabled {
            RawStatus::Satisfied
        } else {
            RawStatus::Gap {
                desired: format!("enabled={},active={}", want_enabled, want_active),
                observed: format!("{},{}", enabled, active),
                since_hint: None,
            }
        };
        let remedy = if want_active && !is_active {
            Remedy::Command {
                program: cfg.systemctl.clone(),
                args: vec!["--user".into(), "restart".into(), unit.to_string()],
            }
        } else if want_enabled && !is_enabled {
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
    let mut pool: Option<i64> = None;
    let mut rows: Vec<(String, i64, i64, bool)> = Vec::new();

    for line in out.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.is_empty() {
            continue;
        }
        if parts[0] == "TOTAL" {
            max_live = parts.get(1).and_then(|s| s.trim().parse().ok());
            live_lanes = parts.get(2).and_then(|s| s.trim().parse().ok()).unwrap_or(0);
            pool = parts.get(3).and_then(|s| s.trim().parse().ok());
            continue;
        }
        if parts.len() < 3 {
            continue;
        }
        let ready: i64 = parts[1].trim().parse().unwrap_or(0);
        let live: i64 = parts[2].trim().parse().unwrap_or(0);
        // A 4th column names whether this partition is drawn from SPIRA_MAX_AEONS (the
        // task pool) or is a lane's own (ops/qa/groomer, which draw outside it). Its
        // absence — every fixture and install before this bead — means "task", the only
        // shape fleet-status.sh ever emitted.
        let is_task = parts.get(3).map(|s| s.trim() == "1").unwrap_or(true);
        rows.push((parts[0].to_string(), ready, live, is_task));
    }

    // The Composite's FleetSpec.ceiling is the declared desired state (sp-8c3ib); an install
    // that has not materialised one yet falls back to fleet-status.sh's own reading of
    // SPIRA_MAX_LIVE_AEONS, exactly as before.
    let max_live = cfg.desired.fleet.as_ref().map(|f| f.ceiling as i64).or(max_live);

    let mut checks = Vec::new();
    let max_live = match max_live {
        Some(m) => m,
        None => {
            // Neither the Composite nor SPIRA_MAX_LIVE_AEONS declares a ceiling — there is
            // no ceiling to diff against, so this is not a gap, it is unobservable. Every
            // partition shares the one missing ceiling, so this is one Check, not one per
            // partition (sp-uqmg0): fanning N incident.sh filings out of a single pass can
            // outrun the oneshot's own TimeoutStartSec.
            if !rows.is_empty() {
                checks.push(Check {
                    key: "fleet".into(),
                    raw: RawStatus::Unobservable { reason: "no Fleet ceiling declared".into() },
                    remedy: Remedy::Escalate,
                });
            }
            return checks;
        }
    };

    for (labels, ready, live, is_task) in rows {
        let headroom = (max_live - live_lanes).max(0);
        let desired_live = ready.min(headroom);
        // SPIRA_MAX_AEONS=0 is an operator pausing the task pool on purpose (aeons.sh
        // pool 0) — a deliberate state, not a fault. A lane partition draws outside the
        // pool, so its own gap still means something.
        let deliberately_paused = is_task && pool == Some(0);
        let raw = if live >= desired_live || deliberately_paused {
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

    // The Composite's CockpitSpec.dashboards is the declared set (sp-8c3ib); an install that
    // has not materialised one yet falls back to health always, plus mail when COCKPIT_MAIL
    // names an aggregator, exactly as before.
    let want_dashboards: Vec<String> = match &cfg.desired.cockpit {
        Some(spec) => spec.dashboards.clone(),
        None => {
            let mut d = vec!["health".to_string()];
            if !cfg.cockpit_mail.is_empty() {
                d.push("mail".to_string());
            }
            d
        }
    };
    let missing = want_dashboards.iter().any(|d| !tags.contains(&d.as_str()));

    if missing {
        return Check {
            key: "cockpit".into(),
            raw: RawStatus::Gap {
                desired: want_dashboards.join(","),
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
    let out = spira_config::bounded::bounded(&cfg.git)
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
    let on_main = on_main_branch(cfg);

    // The Composite's ReleaseSpec names the declared target explicitly (sp-8c3ib); an install
    // that has not materialised one yet falls back to "latest" against $SPIRA_RELEASES —
    // exactly the target the old on_main-or-releases_dir/current logic always meant.
    let (target, allow_override) = match &cfg.desired.release {
        Some(spec) => (spec.release.clone(), spec.allow_override),
        None => ("latest".to_string(), false),
    };

    let target_dir = if target == "main" || cfg.releases_dir.as_os_str().is_empty() {
        None
    } else if target == "latest" {
        Some(cfg.releases_dir.join("current"))
    } else {
        Some(cfg.releases_dir.join(&target))
    };
    let target_resolved = target_dir.and_then(|p| fs::canonicalize(p).ok());

    let at_target = match (&home_resolved, &target_resolved) {
        (Some(h), Some(t)) => h.starts_with(t) || h == t,
        _ => false,
    };
    // A checkout on main always satisfies this invariant, whatever the declared target — the
    // same escape hatch the pre-desired-state logic always had.
    let at_release = at_target || on_main == Some(true);

    if at_release {
        return Check { key: "release".into(), raw: RawStatus::Satisfied, remedy: Remedy::Escalate };
    }
    if home_resolved.is_none() || on_main.is_none() {
        return Check {
            key: "release".into(),
            raw: RawStatus::Unobservable { reason: "checkout or branch unreadable".into() },
            remedy: Remedy::Escalate,
        };
    }
    if allow_override {
        // The Composite explicitly permits a local override away from its declared release
        // — a deliberate state, not a fault (law-a-deliberate-state-is-not-a-fault).
        return Check { key: "release".into(), raw: RawStatus::Satisfied, remedy: Remedy::Escalate };
    }
    Check {
        key: "release".into(),
        raw: RawStatus::Gap {
            desired: format!("release {}", target),
            observed: format!("{:?}", home_resolved.unwrap()),
            since_hint: None,
        },
        remedy: Remedy::Escalate,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Disk: a free-space floor on every path configured, observed via disk-usage.sh (df, plus
// podman's own idea of its storage root — never a guessed path). sp-lkfto.3: the root
// filesystem held 2.6 GB free of 62 GB mid-round with every suite running 2x slower, and
// nothing noticed until it hit 100% and crashed the store. The remedy (disk-remedy.sh)
// reaps closed-bead worktrees and prunes dangling podman images/volumes; if that still
// does not clear the floor, the streak escalates to the Concierge exactly like any other
// remedy that did not hold.
// ──────────────────────────────────────────────────────────────────────────────

fn observe_disk(cfg: &Config) -> Vec<Check> {
    let (floor_pct, declared_paths): (u32, &[String]) = match &cfg.desired.disk {
        Some(d) => (d.floor_pct, d.paths.as_slice()),
        None => (cfg.disk_floor_pct, &[]),
    };
    let mut args: Vec<&str> = vec![&cfg.disk_usage_sh];
    args.extend(declared_paths.iter().map(String::as_str));
    let out = run_cmd("bash", &args);

    let mut checks = Vec::new();
    for line in out.lines() {
        let parts: Vec<&str> = line.splitn(2, '\t').collect();
        if parts.len() != 2 {
            continue;
        }
        let path = parts[0].to_string();
        let raw = match parts[1].trim() {
            "ERR" => RawStatus::Unobservable { reason: format!("could not read free space for {}", path) },
            v => match v.parse::<i64>() {
                Ok(free_pct) if free_pct >= floor_pct as i64 => RawStatus::Satisfied,
                Ok(free_pct) => RawStatus::Gap {
                    desired: format!(">= {}% free", floor_pct),
                    observed: format!("{}% free", free_pct),
                    since_hint: None,
                },
                Err(_) => continue,
            },
        };
        let remedy = match raw {
            RawStatus::Gap { .. } => {
                Remedy::Command { program: "bash".into(), args: vec![cfg.disk_remedy_sh.clone()] }
            }
            _ => Remedy::Escalate,
        };
        checks.push(Check { key: format!("disk:{}", path), raw, remedy });
    }
    checks
}

// ──────────────────────────────────────────────────────────────────────────────
// Queue: two structural checks czar-pass's own detectors do not already cover. The base
// gate itself is base-red, already watched by czar-pass — reconciling it here too would
// file a second incident for the same fault, so it is deliberately not repeated.
// ──────────────────────────────────────────────────────────────────────────────

fn branch_mergeable(cfg: &Config, repo: &Path, base: &str, branch: &str) -> Option<bool> {
    let repo_str = repo.to_string_lossy().to_string();
    let mb_out = spira_config::bounded::bounded(&cfg.git)
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
    let mt_out = spira_config::bounded::bounded(&cfg.git)
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
                    program: cfg.queue_bin.clone(),
                    args: vec![
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

fn orphan_reading(out: &str) -> RawStatus {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(out.trim()) else {
        return RawStatus::Unobservable { reason: "spira-lc requeue-orphans gave no readable answer".into() };
    };
    let Some(orphans) = v.get("orphans").and_then(|o| o.as_array()) else {
        return RawStatus::Unobservable { reason: "spira-lc requeue-orphans named no orphans field".into() };
    };
    if orphans.is_empty() {
        return RawStatus::Satisfied;
    }
    let ids: Vec<&str> = orphans.iter().filter_map(|o| o.as_str()).collect();
    RawStatus::Gap { desired: "no bead IN_DELIVERY without an open batch naming it".into(), observed: ids.join(" "), since_hint: None }
}

fn observe_lc_orphans(cfg: &Config) -> Check {
    let actor = "reconciler";
    let raw = orphan_reading(&run_cmd(&cfg.lc_bin, &["requeue-orphans", "--actor", actor]));
    Check {
        key: "lc-orphan-in-delivery".into(),
        raw,
        remedy: Remedy::Command { program: cfg.lc_bin.clone(), args: vec!["requeue-orphans".into(), "--actor".into(), actor.into(), "--apply".into()] },
    }
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
// Junk rows: a READY lifecycle row whose id is no bead in the store (prose and fixture
// keys). Unreadable store or machine is unobservable, never an empty store — an empty
// reading must not drop every row.
// ──────────────────────────────────────────────────────────────────────────────

const JUNK_ROW_PREFIX: &str = "junk-row:";

fn junk_row_ids(rows: &[spira_config::lc_state::Row], store_ids: &HashSet<String>) -> Vec<String> {
    rows.iter()
        .filter(|r| r.state == "READY" && !store_ids.contains(&r.bead_id))
        .map(|r| r.bead_id.clone())
        .collect()
}

fn store_bead_ids(cfg: &Config) -> Result<HashSet<String>, String> {
    if cfg.bd_bin.is_empty() {
        return Err("SPIRA_BD is not set".into());
    }
    let out = spira_config::bounded::bounded(&cfg.bd_bin)
        .args(["-C", &cfg.spira_db, "list", "--all", "--brief", "--json", "--limit", "0"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("{}: {}", cfg.bd_bin, e))?;
    if !out.status.success() {
        return Err(format!("{} list exited {}", cfg.bd_bin, out.status.code().unwrap_or(-1)));
    }
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("{} list: unparsable json: {}", cfg.bd_bin, e))?;
    let ids: HashSet<String> = rows
        .iter()
        .filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(String::from))
        .collect();
    if ids.is_empty() {
        return Err("the store lists no beads".into());
    }
    Ok(ids)
}

fn observe_junk_rows(cfg: &Config) -> Vec<Check> {
    let unobservable = |reason: String| {
        vec![Check {
            key: "junk-row".into(),
            raw: RawStatus::Unobservable { reason },
            remedy: Remedy::Escalate,
        }]
    };
    let store_ids = match store_bead_ids(cfg) {
        Ok(ids) => ids,
        Err(e) => return unobservable(e),
    };
    let rows = match spira_config::lc_state::list_with(&cfg.lc_bin) {
        Ok(rows) => rows,
        Err(e) => return unobservable(e),
    };
    junk_row_ids(&rows, &store_ids)
        .into_iter()
        .map(|id| Check {
            key: format!("{}{}", JUNK_ROW_PREFIX, id),
            raw: RawStatus::Gap {
                desired: "a bead in the store".into(),
                observed: "lifecycle row with no bead".into(),
                since_hint: None,
            },
            remedy: Remedy::Command {
                program: cfg.lc_bin.clone(),
                args: vec![
                    "drop".into(),
                    id,
                    "reconciler: READY lifecycle row whose id is no bead in the store".into(),
                    "reconciler".into(),
                ],
            },
        })
        .collect()
}

// ──────────────────────────────────────────────────────────────────────────────
// evaluate: run one Check through the pure engine, act on the verdict.
// ──────────────────────────────────────────────────────────────────────────────

/// The invariant metric every remedy here should lower: 0 when satisfied, 1 while it is
/// not (a gap or an unreadable input).
fn gap_metric(raw: &RawStatus) -> f64 {
    if matches!(raw, RawStatus::Satisfied) { 0.0 } else { 1.0 }
}

fn write_event(cfg: &Config, event: &str, key: &str, action: &str, before: f64, after: Option<f64>) {
    append_event(
        &cfg.remedy_log,
        &RemedyEvent {
            ts: &cfg.now_iso,
            event,
            kind: kind_of(key),
            key,
            action,
            metric: "gap",
            before,
            after,
            release: &cfg.release,
        },
    );
}

fn judge_pending(cfg: &Config, pending: &mut PendingMap, key: &str, after: f64) {
    let Some(prior) = pending.remove(key) else { return };
    match measure(prior.clone(), after, cfg.effect_passes) {
        Outcome::Pending(p) => {
            pending.insert(key.to_string(), p);
        }
        Outcome::Effect { before, after } => {
            write_event(cfg, "effect", key, &prior.action, before, Some(after));
        }
        Outcome::NoEffect { before, after } => {
            write_event(cfg, "no-effect", key, &prior.action, before, Some(after));
            log_print(cfg, &format!("reconciler: {} → remedy had no effect", key));
            let subject = format!("RECONCILER: remedy for {} had no effect", key);
            let body = compose_no_effect(key, &prior, after, cfg.now_secs, cfg.effect_passes);
            if let Err(e) = mail_concierge(&cfg.mail_sh, &subject, &body) {
                log_print(cfg, &format!("reconciler: {} → no-effect mail failed: {}", key, e));
            }
        }
    }
}

fn mail_concierge(mail_sh: &str, subject: &str, body: &str) -> Result<(), String> {
    let mut child = spira_config::bounded::bounded(mail_sh)
        .args(["send", "concierge", "--from", "Reconciler <reconciler@spira>", "--subject", subject, "--kind", "note"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{mail_sh}: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(body.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| format!("{mail_sh}: {e}"))?;
    if !out.status.success() {
        return Err(format!("{mail_sh} send concierge: exit {}", out.status.code().unwrap_or(-1)));
    }
    Ok(())
}

fn evaluate(cfg: &Config, state: &mut StateMap, pending: &mut PendingMap, check: Check) {
    let prev = state.remove(&check.key).unwrap_or_default();
    let metric_now = gap_metric(&check.raw);
    let (verdict, mut next) = step(cfg.now_secs, check.raw, cfg.grace_secs, prev);
    append_status(&cfg.status_log, &cfg.now_iso, &check.key, &verdict);
    judge_pending(cfg, pending, &check.key, metric_now);

    if verdict.is_gap {
        if verdict.remedy_failed {
            escalate(cfg, &check.key, &verdict);
        } else {
            match &check.remedy {
                Remedy::Escalate => escalate(cfg, &check.key, &verdict),
                Remedy::Command { program, args } => {
                    let desc = format!("{} {}", program, args.join(" "));
                    if is_shadowed(&cfg.shadow_kinds, kind_of(&check.key)) {
                        log_print(cfg, &format!("reconciler: {} → remedy held (shadow): {}", check.key, desc));
                        write_event(cfg, "shadow", &check.key, &desc, metric_now, None);
                        escalate(cfg, &check.key, &verdict);
                    } else {
                        log_print(cfg, &format!("reconciler: {} → remedy: {}", check.key, desc));
                        write_event(cfg, "remedy", &check.key, &desc, metric_now, None);
                        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
                        let _ = run_cmd_ok(program, &arg_refs);
                        record_remedy(&mut next, cfg.now_secs, &desc);
                        pending.insert(
                            check.key.clone(),
                            PendingEffect {
                                action: desc,
                                metric: "gap".into(),
                                before: metric_now,
                                acted_at: cfg.now_secs,
                                passes_seen: 0,
                            },
                        );
                    }
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
        RawStatus::Deliberate { reason } => format!("deliberate: {}", reason),
        RawStatus::Gap { desired, observed, .. } => format!("desired={} observed={}", desired, observed),
        RawStatus::Unobservable { reason } => format!("unobservable: {}", reason),
    }
}

// FILES A BEAD, NOT reconciler_engine::alert'S LIVE WAKE — a decision, not an oversight.
// Structural gaps reach here only after a remedy already failed or none exists, which makes
// them trackable Ops work, not a judgement call; incident.sh's own dedup (one bead per ref,
// a recurrence count bumped every pass, an operator page only past its threshold) already
// absorbs a persisting gap without repeat noise, the same job should_alert's per-streak dedup
// does for a medium — live mail — that cannot absorb a repeat itself. Two dedup mechanisms
// because there are two destinations with different absorption, not one duplicated by mistake.
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
    let child = spira_config::bounded::bounded("bash")
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
    let cfg = Config::from_env()?.with_desired_state();

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
    let mut pending = load_pending(&cfg.effect_state);

    let mut checks = Vec::new();
    checks.extend(observe_units(&cfg));
    checks.extend(observe_fleet(&cfg));
    checks.push(observe_cockpit(&cfg));
    checks.push(observe_release(&cfg));
    checks.extend(observe_disk(&cfg));
    checks.extend(observe_queue_mergeable(&cfg));
    checks.extend(observe_queue_lock_age(&cfg));
    checks.push(observe_lc_orphans(&cfg));
    let junk = observe_junk_rows(&cfg);
    state.retain(|k, _| !k.starts_with(JUNK_ROW_PREFIX) || junk.iter().any(|c| &c.key == k));
    checks.extend(junk);

    remind_due_suspensions(&cfg, &load_ctrl(&cfg));

    let n = checks.len();
    for check in checks {
        evaluate(&cfg, &mut state, &mut pending, check);
    }

    let _ = save_state(&cfg.state_path, &state);
    let _ = save_pending(&cfg.effect_state, &pending);

    let elapsed = unix_now().saturating_sub(cfg.now_secs);
    log_print(&cfg, &format!("reconciler: complete ({} checks, {}s)", n, elapsed));

    drop(lock_file);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("reconciler-test-{}-{}", name, SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()))
    }

    #[test]
    fn orphan_reading_is_a_gap_only_when_an_orphan_is_named() {
        let gap = orphan_reading(r#"{"orphans":["sp-a","sp-b"],"requeued":[],"failed":[]}"#);
        assert!(matches!(&gap, RawStatus::Gap { observed, .. } if observed == "sp-a sp-b"), "{gap:?}");
        assert_eq!(orphan_reading(r#"{"orphans":[],"requeued":[],"failed":[]}"#), RawStatus::Satisfied);
        assert!(matches!(orphan_reading(""), RawStatus::Unobservable { .. }));
        assert!(matches!(orphan_reading("{}"), RawStatus::Unobservable { .. }));
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

    fn suspension_fixture(tag: &str, owner: &str, until: Option<&str>) -> (testkit::TempDir, Config) {
        let dir = scratch_dir(tag);
        let bin = |name: &str, body: &str| {
            testkit::write_exe(dir.join(name), body);
            dir.join(name).to_string_lossy().to_string()
        };
        let calls = dir.join("systemctl-calls");
        let systemctl = bin(
            "systemctl",
            &format!(
                "#!/bin/sh\necho \"$*\" >> {}\ncase \"$2\" in is-enabled) echo disabled; exit 1;; is-active) echo inactive; exit 3;; esac\n",
                calls.display()
            ),
        );
        let manifest = bin("manifest", "#!/bin/sh\necho spira-round-template-prod.timer\n");
        let lc = bin("lc", "#!/bin/sh\necho '{\"bead\":{\"bead_id\":\"sp-xn3nou\",\"state\":\"LANDED\"}}'\n");
        let mail = bin("mail", &format!("#!/bin/sh\ncat >> {}\necho --- >> {}\n", dir.join("mail-body").display(), dir.join("mail-body").display()));
        let ctrl_path = dir.join("control");
        let mut data = spira_ctrl::CtrlData::new();
        spira_ctrl::suspend(&mut data, "spira-round-template", "fails every run", owner, "2026-10-07", "operator");
        if let Some(u) = until {
            spira_ctrl::set_until(&mut data, "spira-round-template", u);
        }
        spira_ctrl::write_atomic(&ctrl_path, &data).unwrap();
        let cfg = Config {
            systemctl,
            units_manifest_sh: manifest,
            lc_bin: lc,
            mail_sh: mail,
            ctrl_path,
            reminders_path: dir.join("reminders.json"),
            status_log: dir.join("status.jsonl"),
            now_iso: "2026-10-08T00:00:00Z".into(),
            grace_secs: 0,
            ..test_config()
        };
        (dir, cfg)
    }

    #[test]
    fn a_suspended_timer_survives_a_remedy_pass_and_reads_deliberate() {
        let (dir, cfg) = suspension_fixture("suspended-unit", "sp-never", None);
        let mut state = StateMap::new();
        let checks = observe_units(&cfg);
        assert_eq!(checks.len(), 1);
        for c in checks {
            evaluate(&cfg, &mut state, &mut Default::default(), c);
        }
        let calls = fs::read_to_string(dir.join("systemctl-calls")).unwrap_or_default();
        assert!(!calls.contains("enable") && !calls.contains("restart"), "a remedy ran against a suspended unit: {calls}");
        let status = fs::read_to_string(&cfg.status_log).unwrap();
        assert!(status.contains("\"status\":\"deliberate\""), "{status}");
        assert!(status.contains("fails every run"), "{status}");
    }

    #[test]
    fn positive_control_an_unsuspended_timer_is_remedied() {
        let (dir, cfg) = suspension_fixture("unsuspended-unit", "sp-never", None);
        fs::remove_file(&cfg.ctrl_path).unwrap();
        let mut state = StateMap::new();
        for c in observe_units(&cfg) {
            evaluate(&cfg, &mut state, &mut Default::default(), c);
        }
        let calls = fs::read_to_string(dir.join("systemctl-calls")).unwrap();
        assert!(calls.contains("enable --now spira-round-template-prod.timer"), "{calls}");
    }

    #[test]
    fn a_reminder_is_sent_once_when_the_owner_bead_lands_and_the_suspension_stays() {
        let (dir, cfg) = suspension_fixture("reminder-owner", "sp-xn3nou", None);
        let ctrl = load_ctrl(&cfg);
        remind_due_suspensions(&cfg, &ctrl);
        remind_due_suspensions(&cfg, &ctrl);
        let mail = fs::read_to_string(dir.join("mail-body")).unwrap();
        assert_eq!(mail.matches("---").count(), 1, "reminded more than once: {mail}");
        assert!(mail.contains("sp-xn3nou") && mail.contains("has landed"), "{mail}");
        assert!(spira_ctrl::is_suspended(&load_ctrl(&cfg), "spira-round-template"), "the reconciler lifted a suspension");
    }

    #[test]
    fn a_reminder_is_sent_when_until_has_come_and_not_before() {
        let (dir, cfg) = suspension_fixture("reminder-until", "sp-never", Some("2026-10-09"));
        let cfg_lc = Config { lc_bin: "/bin/false".into(), ..test_config_from(&cfg) };
        remind_due_suspensions(&cfg_lc, &load_ctrl(&cfg_lc));
        assert!(!dir.join("mail-body").exists(), "reminded before until");
        let later = Config { now_iso: "2026-10-09T00:00:00Z".into(), ..test_config_from(&cfg_lc) };
        remind_due_suspensions(&later, &load_ctrl(&later));
        assert!(fs::read_to_string(dir.join("mail-body")).unwrap().contains("2026-10-09"));
    }

    #[test]
    fn a_failed_send_is_retried_not_recorded() {
        let (dir, cfg) = suspension_fixture("reminder-retry", "sp-xn3nou", None);
        let broken = Config { mail_sh: "/bin/false".into(), ..test_config_from(&cfg) };
        remind_due_suspensions(&broken, &load_ctrl(&broken));
        remind_due_suspensions(&cfg, &load_ctrl(&cfg));
        assert!(dir.join("mail-body").exists());
    }

    #[test]
    fn reminder_due_reads_until_and_owner() {
        assert_eq!(reminder_due("sp-1", None, "2026-10-07", false), Due::NotDue);
        assert_eq!(reminder_due("sp-1", Some("2026-10-07"), "2026-10-07", false), Due::Until("2026-10-07".into()));
        assert_eq!(reminder_due("sp-1", Some("2026-10-08"), "2026-10-07", true), Due::OwnerLanded("sp-1".into()));
    }

    #[test]
    fn suspension_of_matches_unit_base_and_instance_stripped_subject() {
        let mut d = spira_ctrl::CtrlData::new();
        spira_ctrl::suspend(&mut d, "spira-groom", "r", "sp-1", "w", "b");
        assert!(suspension_of(&d, "prod", "spira-groom-prod.timer").is_some());
        assert!(suspension_of(&d, "prod", "spira-groom.timer").is_some());
        assert!(suspension_of(&d, "prod", "spira-other-prod.timer").is_none());
    }

    #[test]
    fn evaluate_inside_grace_raises_nothing() {
        let dir = scratch_dir("evaluate-inside-grace");
        let marker = dir.join("remedy-ran");
        let cfg = Config { grace_secs: 300, now_secs: 100, ..test_config() };
        let mut state = StateMap::new();
        evaluate(&cfg, &mut state, &mut PendingMap::new(), Check {
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
        evaluate(&cfg, &mut state, &mut PendingMap::new(), Check {
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
        testkit::write_exe(&stub_incident, &format!("#!/usr/bin/env bash\ncat >/dev/null\ntouch {}\n", escalated.display()));

        let cfg1 = Config { grace_secs: 0, now_secs: 100, incident_sh: stub_incident.to_string_lossy().to_string(), ..test_config() };
        let mut state = StateMap::new();
        let raw = || RawStatus::Gap { desired: "d".into(), observed: "o".into(), since_hint: None };

        // Pass 1: past grace, command remedy attempted and recorded.
        evaluate(&cfg1, &mut state, &mut PendingMap::new(), Check {
            key: "k".into(),
            raw: raw(),
            remedy: Remedy::Command { program: "touch".into(), args: vec![marker.to_string_lossy().to_string()] },
        });
        assert!(marker.exists());
        fs::remove_file(&marker).unwrap();

        // Pass 2: still a gap — the engine reports remedy_failed, so evaluate must escalate
        // rather than run the command a second time.
        let cfg2 = Config { now_secs: 200, ..test_config_from(&cfg1) };
        evaluate(&cfg2, &mut state, &mut PendingMap::new(), Check {
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
        testkit::write_exe(&stub_incident, &format!("#!/usr/bin/env bash\ncat >/dev/null\ntouch {}\n", escalated.display()));

        let cfg = Config { grace_secs: 300, now_secs: 100, incident_sh: stub_incident.to_string_lossy().to_string(), ..test_config() };
        let mut state = StateMap::new();
        evaluate(&cfg, &mut state, &mut PendingMap::new(), Check {
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
        testkit::write_exe(&stub_incident, &format!("#!/usr/bin/env bash\ncat >/dev/null\ntouch {}\n", escalated.display()));

        let cfg = Config { grace_secs: 0, now_secs: 100, incident_sh: stub_incident.to_string_lossy().to_string(), ..test_config() };
        let mut state = StateMap::new();
        evaluate(&cfg, &mut state, &mut PendingMap::new(), Check {
            key: "k".into(),
            raw: RawStatus::Unobservable { reason: "cannot read it".into() },
            remedy: Remedy::Escalate,
        });
        assert!(escalated.exists(), "unobservable sustained past grace must still escalate (law-a-control-that-cannot-check-must-refuse)");
    }

    fn gap_raw() -> RawStatus {
        RawStatus::Gap { desired: "d".into(), observed: "o".into(), since_hint: None }
    }

    fn touch_remedy(path: &Path) -> Remedy {
        Remedy::Command { program: "touch".into(), args: vec![path.to_string_lossy().to_string()] }
    }

    fn effect_cfg(dir: &Path, mail: &Path) -> Config {
        Config {
            grace_secs: 0,
            remedy_log: dir.join("remedy.jsonl"),
            mail_sh: mail.to_string_lossy().to_string(),
            incident_sh: "/bin/true".into(),
            release: "r-fixture".into(),
            effect_passes: 2,
            ..test_config()
        }
    }

    fn stub_mail(dir: &Path) -> PathBuf {
        let mail = dir.join("stub-mail.sh");
        testkit::write_exe(&mail, &format!("#!/usr/bin/env bash\ncat >> {}\n", dir.join("mailed").display()));
        mail
    }

    #[test]
    fn a_remedy_writes_one_event_and_an_effective_one_reads_effect() {
        let dir = scratch_dir("effect-remedy-event");
        let mail = stub_mail(&dir);
        let mut state = StateMap::new();
        let mut pending = PendingMap::new();

        let cfg1 = Config { now_secs: 100, ..effect_cfg(&dir, &mail) };
        evaluate(&cfg1, &mut state, &mut pending, Check { key: "disk:/v".into(), raw: gap_raw(), remedy: touch_remedy(&dir.join("ran")) });
        let log = fs::read_to_string(dir.join("remedy.jsonl")).unwrap();
        assert_eq!(log.lines().count(), 1, "one remedy writes exactly one event");
        for field in ["\"event\":\"remedy\"", "\"kind\":\"disk\"", "\"key\":\"disk:/v\"", "\"action\":\"touch ", "\"before\":1.0", "\"release\":\"r-fixture\"", "\"ts\":"] {
            assert!(log.contains(field), "event lacks {field}: {log}");
        }

        let cfg2 = Config { now_secs: 200, ..test_config_from(&cfg1) };
        evaluate(&cfg2, &mut state, &mut pending, Check { key: "disk:/v".into(), raw: RawStatus::Satisfied, remedy: touch_remedy(&dir.join("ran")) });
        let log = fs::read_to_string(dir.join("remedy.jsonl")).unwrap();
        assert!(log.contains("\"event\":\"effect\"") && log.contains("\"after\":0.0"), "{log}");
        assert!(!log.contains("no-effect"));
        assert!(!dir.join("mailed").exists(), "an effective remedy mails nothing");
        assert!(pending.is_empty());
    }

    #[test]
    fn a_planted_no_op_remedy_is_flagged_no_effect_and_mailed_with_both_readings() {
        let dir = scratch_dir("effect-no-op");
        let mail = stub_mail(&dir);
        let mut state = StateMap::new();
        let mut pending = PendingMap::new();
        let noop = || Remedy::Command { program: "true".into(), args: vec![] };

        let cfg1 = Config { now_secs: 100, ..effect_cfg(&dir, &mail) };
        evaluate(&cfg1, &mut state, &mut pending, Check { key: "disk:/v".into(), raw: gap_raw(), remedy: noop() });
        let cfg2 = Config { now_secs: 200, ..test_config_from(&cfg1) };
        evaluate(&cfg2, &mut state, &mut pending, Check { key: "disk:/v".into(), raw: gap_raw(), remedy: noop() });
        assert!(!dir.join("mailed").exists(), "one pass of no movement is still inside the pass budget");
        let cfg3 = Config { now_secs: 300, ..test_config_from(&cfg1) };
        evaluate(&cfg3, &mut state, &mut pending, Check { key: "disk:/v".into(), raw: gap_raw(), remedy: noop() });

        let log = fs::read_to_string(dir.join("remedy.jsonl")).unwrap();
        assert!(log.contains("\"event\":\"no-effect\"") && log.contains("\"before\":1.0") && log.contains("\"after\":1.0"), "{log}");
        let mailed = fs::read_to_string(dir.join("mailed")).unwrap();
        assert!(mailed.contains("before: 1") && mailed.contains("after: 1") && mailed.contains("disk:/v"), "{mailed}");
    }

    #[test]
    fn a_shadowed_kind_records_the_remedy_it_would_have_run_and_does_not_run_it() {
        let dir = scratch_dir("effect-shadow");
        let mail = stub_mail(&dir);
        let ran = dir.join("ran");
        let cfg = Config { now_secs: 100, shadow_kinds: vec!["disk".into()], ..effect_cfg(&dir, &mail) };
        let mut state = StateMap::new();
        let mut pending = PendingMap::new();
        evaluate(&cfg, &mut state, &mut pending, Check { key: "disk:/v".into(), raw: gap_raw(), remedy: touch_remedy(&ran) });
        assert!(!ran.exists(), "the kill switch holds the remedy back");
        assert!(fs::read_to_string(dir.join("remedy.jsonl")).unwrap().contains("\"event\":\"shadow\""));
        assert!(pending.is_empty(), "a remedy that did not act has no effect to measure");

        let other = Config { shadow_kinds: vec!["units-timer".into()], ..test_config_from(&cfg) };
        evaluate(&other, &mut state, &mut pending, Check { key: "disk:/w".into(), raw: gap_raw(), remedy: touch_remedy(&ran) });
        assert!(ran.exists(), "a kind the switch does not name still acts");
    }

    #[test]
    fn junk_rows_are_ready_rows_without_a_bead() {
        let row = |id: &str, state: &str| spira_config::lc_state::Row {
            bead_id: id.into(),
            state: state.into(),
            ..Default::default()
        };
        let store: HashSet<String> = ["sp-real".to_string()].into();
        let rows = vec![row("sp-real", "READY"), row("prose key", "READY"), row("gone", "LANDED")];
        assert_eq!(junk_row_ids(&rows, &store), vec!["prose key".to_string()]);
    }

    fn test_config() -> Config {
        Config {
            spira_run: PathBuf::from("/tmp"),
            spira_home: String::new(),
            log: PathBuf::from("/dev/null"),
            state_path: PathBuf::from("/dev/null"),
            status_log: PathBuf::from("/dev/null"),
            remedy_log: PathBuf::from("/dev/null"),
            effect_state: PathBuf::from("/dev/null"),
            mail_sh: "/bin/true".into(),
            release: "r-test".into(),
            shadow_kinds: Vec::new(),
            effect_passes: 2,
            lock_path: PathBuf::from("/dev/null"),
            spira_db: String::new(),
            scope_label: String::new(),
            reconciler_label: "reconciler-gap".into(),
            incident_sh: "/bin/true".into(),
            systemctl: "systemctl".into(),
            ctrl_path: PathBuf::from("/dev/null"),
            instance: "prod".into(),
            reminders_path: PathBuf::from("/dev/null"),
            tmux: "tmux".into(),
            git: "git".into(),
            units_manifest_sh: String::new(),
            fleet_status_sh: String::new(),
            queue_certified_list_sh: String::new(),
            cockpit_sh: String::new(),
            queue_bin: String::new(),
            lc_bin: String::new(),
            bd_bin: String::new(),
            queue_dir: PathBuf::from("/dev/null"),
            repo_map: None,
            releases_dir: PathBuf::new(),
            store_unit: "dolt-beads.service".into(),
            cockpit_sessions: vec!["brain".into(), "hunk".into(), "chat".into()],
            cockpit_mail: String::new(),
            disk_usage_sh: String::new(),
            disk_remedy_sh: String::new(),
            disk_floor_pct: 15,
            grace_secs: 300,
            preflight_wall_secs: 240,
            now_secs: 0,
            now_iso: "2026-09-25T00:00:00Z".into(),
            desired_dir: PathBuf::from("/dev/null"),
            desired: DesiredState::default(),
        }
    }

    fn test_config_from(base: &Config) -> Config {
        Config {
            spira_run: base.spira_run.clone(),
            spira_home: base.spira_home.clone(),
            log: base.log.clone(),
            state_path: base.state_path.clone(),
            status_log: base.status_log.clone(),
            remedy_log: base.remedy_log.clone(),
            effect_state: base.effect_state.clone(),
            mail_sh: base.mail_sh.clone(),
            release: base.release.clone(),
            shadow_kinds: base.shadow_kinds.clone(),
            effect_passes: base.effect_passes,
            lock_path: base.lock_path.clone(),
            spira_db: base.spira_db.clone(),
            scope_label: base.scope_label.clone(),
            reconciler_label: base.reconciler_label.clone(),
            incident_sh: base.incident_sh.clone(),
            systemctl: base.systemctl.clone(),
            ctrl_path: base.ctrl_path.clone(),
            instance: base.instance.clone(),
            reminders_path: base.reminders_path.clone(),
            tmux: base.tmux.clone(),
            git: base.git.clone(),
            units_manifest_sh: base.units_manifest_sh.clone(),
            fleet_status_sh: base.fleet_status_sh.clone(),
            queue_certified_list_sh: base.queue_certified_list_sh.clone(),
            cockpit_sh: base.cockpit_sh.clone(),
            queue_bin: base.queue_bin.clone(),
            lc_bin: base.lc_bin.clone(),
            bd_bin: base.bd_bin.clone(),
            queue_dir: base.queue_dir.clone(),
            repo_map: base.repo_map.clone(),
            releases_dir: base.releases_dir.clone(),
            store_unit: base.store_unit.clone(),
            cockpit_sessions: base.cockpit_sessions.clone(),
            cockpit_mail: base.cockpit_mail.clone(),
            disk_usage_sh: base.disk_usage_sh.clone(),
            disk_remedy_sh: base.disk_remedy_sh.clone(),
            disk_floor_pct: base.disk_floor_pct,
            grace_secs: base.grace_secs,
            preflight_wall_secs: base.preflight_wall_secs,
            now_secs: base.now_secs,
            now_iso: base.now_iso.clone(),
            desired_dir: base.desired_dir.clone(),
            desired: base.desired.clone(),
        }
    }
}
