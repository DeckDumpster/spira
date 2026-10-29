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

use reconciler_engine::alert::{compose_alert, should_alert};
use reconciler_engine::core::{last_remedy, record_remedy, step, HysteresisState, RawStatus, Verdict};
use reconciler_engine::io::{append_status, load_alerted, load_state, save_alerted, save_state, AlertedSinceMap, StateMap};
use reconciler_engine::{clock, mail, paths};
use reconciler_engine::effect::{
    append_event, compose_no_effect, is_shadowed, kind_of, load_pending, measure, save_pending, Outcome,
    PendingEffect, PendingMap, RemedyEvent,
};
use reconciler_engine::holds::{self, Outcome as HoldOutcome};
use spira_desired_state::compose::Composite;
use spira_desired_state::resource::{parse_resource, CockpitSpec, DiskSpec, FleetSpec, KindSpec, ReleaseSpec, UnitsSpec};
use spira_desired_state::store::FsStore;
mod hold_sweep;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::UNIX_EPOCH;

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
    let sink = spira_otel::metrics::MetricsSink::for_component(spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref(), "spira-reconciler");
    match spira_otel::metrics::instrument_result(&sink, "reconciler", "pass", run_pass) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("reconciler: {}", e);
            ExitCode::FAILURE
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Configuration — every program is a field of one injected Seams struct; every path is
// derived from a registered key.
// ──────────────────────────────────────────────────────────────────────────────

/// Every program the pass runs, by name on the launcher's PATH, and the one unit it will
/// never remedy. A test injects its own; nothing here is read from the environment.
#[derive(Clone)]
struct Seams {
    systemctl: String,
    tmux: String,
    git: String,
    forge: String,
    mail: String,
    units_install: String,
    spira_config: String,
    spira_claim: String,
    strand: String,
    queue_helpers: String,
    queue: String,
    spira_lc: String,
    bead: String,
    target_reap: String,
    podman: String,
    df: String,
    rebuild: String,
    layout: String,
    date: String,
    store_unit: String,
    lint: String,
    guard: String,
}

impl Seams {
    fn production() -> Seams {
        Seams::resolving(spira_config::release_env::tool)
    }

    fn resolving(name: impl Fn(&str) -> String) -> Seams {
        Seams {
            systemctl: name("systemctl"),
            tmux: name("tmux"),
            git: name("git"),
            forge: name("forge"),
            mail: name("mail"),
            units_install: name("units-install"),
            spira_config: name("spira-config"),
            spira_claim: name("spira-claim"),
            strand: name("strand"),
            queue_helpers: name("queue-helpers"),
            queue: name("queue"),
            spira_lc: name("spira-lc"),
            bead: name("bead"),
            target_reap: name("target-reap"),
            podman: name("podman"),
            df: name("df"),
            rebuild: name("rebuild"),
            layout: name("layout"),
            date: name("date"),
            store_unit: name("dolt-beads.service"),
            lint: name("spira-lint"),
            guard: name("lifecycle-guard"),
        }
    }
}

#[derive(Clone)]
struct Config {
    seams: Seams,
    spira_run: PathBuf,
    spira_home: String,
    log: PathBuf,
    state_path: PathBuf,
    alerted_path: PathBuf,
    store: std::sync::Arc<tsd::store::Pusher>,
    effect_state: PathBuf,
    mail_root: PathBuf,
    release: String,
    shadow_kinds: Vec<String>,
    effect_passes: u32,
    lock_path: PathBuf,
    spira_db: String,
    bd_bin: String,
    queue_dir: PathBuf,
    repo_map: Option<PathBuf>,
    releases_dir: PathBuf,
    cockpit_sessions: Vec<String>,
    cockpit_mail: String,
    dolt_data: String,
    max_live_dwarves: String,
    max_dwarves: String,
    disk_floor_pct: u32,
    grace_secs: u64,
    activation_grace_secs: u64,
    fence_deadline_secs: u64,
    preflight_wall_secs: u64,
    now_secs: u64,
    now_iso: String,
    desired_dir: PathBuf,
    desired: DesiredState,
    ctrl_path: PathBuf,
    instance: String,
    reminders_path: PathBuf,
}

const MAIN_REF: &str = "local/main";

/// The release the running harness copy is: the directory name above its `spira/` dir, or
/// `unknown` when the home cannot be located.
fn release_name(home: &Path) -> String {
    home.parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

impl Config {
    fn from_process(seams: Seams) -> Result<Config, String> {
        use spira_config::process::{cfg, cfg_parse};

        // SPIRA_RUN is a PROCEDURAL registry key, so it is resolved through the run-dir
        // resolver rather than a bare `cfg("SPIRA_RUN")`. A `spira.toml` that fails to
        // resolve is a named refusal, never a literal `/tmp/spira`.
        let spira_run = spira_config::resolve::run_dir_for_process()?;
        let spira_home = spira_config::resolve::locate_home_for_process()?.to_string_lossy().into_owned();
        let sessions = cfg("COCKPIT_SESSIONS")?.split_whitespace().map(String::from).collect();
        Ok(Config {
            log: spira_run.join("reconciler.log"),
            state_path: paths::state(&spira_run),
            alerted_path: paths::alerted(&spira_run),
            store: std::sync::Arc::new(tsd::store::Pusher::new(cfg("SPIRA_METRICS_ENDPOINT").ok().filter(|e| !e.trim().is_empty()))),
            effect_state: paths::effect_state(&spira_run),
            mail_root: PathBuf::from(cfg("SPIRA_MAIL")?),
            release: release_name(Path::new(&spira_home)),
            shadow_kinds: cfg("SPIRA_RECONCILER_SHADOW_KINDS")?.split_whitespace().map(String::from).collect(),
            effect_passes: match cfg_parse::<u32>("SPIRA_RECONCILER_EFFECT_PASSES")? {
                0 => 2,
                n => n,
            },
            lock_path: spira_run.join("reconciler.lock"),
            spira_db: cfg("SPIRA_DB")?,
            bd_bin: cfg("SPIRA_BD")?,
            queue_dir: PathBuf::from(cfg("SPIRA_QUEUE_DIR")?),
            repo_map: Some(PathBuf::from(cfg("SPIRA_REPO_MAP")?)),
            releases_dir: PathBuf::from(cfg("SPIRA_RELEASES")?),
            cockpit_sessions: sessions,
            cockpit_mail: cfg("COCKPIT_MAIL")?,
            dolt_data: cfg("SPIRA_DOLT_DATA")?,
            max_live_dwarves: cfg("SPIRA_MAX_LIVE_DWARVES")?,
            max_dwarves: cfg("SPIRA_MAX_DWARVES")?,
            disk_floor_pct: cfg_parse::<u32>("SPIRA_DISK_FLOOR_PCT")?,
            grace_secs: cfg_parse::<u64>("SPIRA_RECONCILER_GRACE_SECS")?,
            activation_grace_secs: cfg_parse::<u64>("SPIRA_MAIN_HEALTH_ACTIVATION_GRACE_SECS")?,
            fence_deadline_secs: cfg_parse::<u64>("SPIRA_MAIN_HEALTH_FENCE_DEADLINE_SECS")?,
            preflight_wall_secs: cfg_parse::<u64>("SPIRA_PREFLIGHT_WALL_SECS")?,
            now_secs: clock::now_secs(&seams.date),
            now_iso: clock::now_iso(&seams.date),
            desired_dir: spira_desired_state::store::default_dir()?,
            desired: DesiredState::default(),
            ctrl_path: PathBuf::from(cfg("SPIRA_CTRL")?),
            instance: cfg("SPIRA_INSTANCE")?,
            reminders_path: paths::reminders(&spira_run),
            seams,
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

fn log_print(cfg: &Config, msg: &str) {
    let line = format!("{} spira: {}\n", clock::now_iso(&cfg.seams.date), msg);
    {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(line.as_bytes());
        let _ = out.flush();
    }
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
    /// Every step runs, in order, whether or not an earlier one succeeded.
    Sequence(Vec<(String, Vec<String>)>),
}

impl Remedy {
    fn steps(&self) -> Vec<(&str, Vec<&str>)> {
        match self {
            Remedy::Escalate => Vec::new(),
            Remedy::Command { program, args } => vec![(program.as_str(), args.iter().map(String::as_str).collect())],
            Remedy::Sequence(steps) => steps
                .iter()
                .map(|(p, a)| (p.as_str(), a.iter().map(String::as_str).collect()))
                .collect(),
        }
    }

    fn describe(&self) -> String {
        self.steps().iter().map(|(p, a)| format!("{} {}", p, a.join(" "))).collect::<Vec<_>>().join(" ; ")
    }
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
fn systemctl_state(cfg: &Config, verb: &str, unit: &str) -> Result<String, String> {
    let out = spira_config::bounded::bounded(&cfg.seams.systemctl)
        .args(["--user", verb, unit])
        .output()
        .map_err(|e| format!("{} {} {}: {}", cfg.seams.systemctl, verb, unit, e))?;
    let word = String::from_utf8_lossy(&out.stdout).trim().to_string();
    const KNOWN: &[&str] = &[
        "enabled", "disabled", "static", "masked", "linked", "alias", "generated", "indirect", "enabled-runtime",
        "active", "inactive", "failed", "activating", "deactivating", "reloading",
    ];
    if KNOWN.contains(&word.as_str()) {
        return Ok(word);
    }
    let how = match out.status.code() {
        Some(124) => format!("timed out after {}s", spira_config::bounded::CALL_TIMEOUT_SECS),
        Some(c) => format!("exit status {}", c),
        None => "killed by a signal".to_string(),
    };
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(format!("systemctl {} {}: {}; stdout {:?}; stderr {:?}", verb, unit, how, word, stderr))
}

/// A unit's desired (enabled, active) — from the Composite's UnitsSpec when it names the
/// unit, else the legacy assumption every name in the manifest carries: it should be both
/// (the ENABLE set only ever lists what should be enabled, never what should not be, so a
/// unit the Composite has not caught up to yet keeps the old default rather than being
/// treated as having no desired state at all).
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
    subjects.into_iter().find_map(|s| spira_ctrl::reason(ctrl, &s).map(|r| (s, r.to_string())))
}

#[derive(Debug, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
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
    if owner.is_empty() {
        return false;
    }
    matches!(spira_config::lc_state::row_with(&cfg.seams.spira_lc, owner), Ok(Some(r)) if r.state == "LANDED")
}

fn reminder_body(subject: &str, why: &str, reason: &str, owner: &str) -> String {
    let owned = if owner.is_empty() {
        String::new()
    } else {
        format!(" Its owner is bead {owner}, the work expected to make the suspension unnecessary.")
    };
    format!(
        "## Note\n\nThe suspension of {subject} is due for review because {why}. It was recorded with this reason: {reason}.{owned}\n\n\
         The reconciler has not lifted the suspension and will not. Lift it with `ctrl resume {subject}`, or \
         extend it by suspending again with a new owner or --until.\n"
    )
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
    let failed_path = cfg.reminders_path.with_extension("failed.json");
    let mut failed: std::collections::BTreeMap<String, String> = fs::read_to_string(&failed_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let failed_before = failed.clone();
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
        let body = reminder_body(subject, &why, reason, owner);
        match mail::note_concierge(&cfg.seams.mail, &subj, &body) {
            Ok(()) => {
                log_print(cfg, &format!("reconciler: reminded — {}: {}", subject, why));
                sent.insert(marker);
            }
            Err(e) => {
                if failed.get(&marker) != Some(&e) {
                    log_print(cfg, &format!("reconciler: reminder for {} not sent: {}", subject, e));
                }
                failed.insert(marker, e);
            }
        }
    }
    failed.retain(|k, _| live.contains(k) && !sent.contains(k));
    if failed != failed_before {
        if let Ok(json) = serde_json::to_string_pretty(&failed) {
            let _ = fs::write(&failed_path, json);
        }
    }
    if sent != before {
        if let Ok(json) = serde_json::to_string_pretty(&sent) {
            let _ = fs::write(&cfg.reminders_path, json);
        }
    }
}

fn unit_key(cfg: &Config, unit: &str) -> String {
    if unit == cfg.seams.store_unit {
        format!("store:{}", unit)
    } else if unit.ends_with(".timer") {
        format!("units-timer:{}", unit)
    } else {
        format!("units-daemon:{}", unit)
    }
}

fn observe_units(cfg: &Config) -> Vec<Check> {
    // The Composite's UnitsSpec.units is the declared unit set; an install that has not
    // materialised one yet falls back to the installer's own ENABLE set.
    let manifest_names;
    let unit_names: Vec<&str> = match &cfg.desired.units {
        Some(spec) if !spec.units.is_empty() => spec.units.iter().map(|u| u.name.as_str()).collect(),
        _ => {
            manifest_names = run_cmd(&cfg.seams.units_install, &["--list-enable"]);
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
            (Ok(e), Ok(a)) => (e, a),
            (e, a) => {
                let reasons: Vec<String> = [e.err(), a.err()].into_iter().flatten().collect();
                checks.push(Check {
                    key: format!("units:{}", unit),
                    raw: RawStatus::Unobservable { reason: reasons.join(" | ") },
                    remedy: Remedy::Escalate,
                });
                continue;
            }
        };
        let is_enabled = enabled == "enabled";
        let is_active = active == "active";

        if unit == cfg.seams.store_unit {
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
            // active" — the only shape the ENABLE set (or the Composite's own
            // examples/default.toml) has ever declared.
            let remedy = if want_enabled && want_active {
                Remedy::Command {
                    program: cfg.seams.systemctl.clone(),
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
                program: cfg.seams.systemctl.clone(),
                args: vec!["--user".into(), "restart".into(), unit.to_string()],
            }
        } else if want_enabled && !is_enabled {
            Remedy::Command {
                program: cfg.seams.systemctl.clone(),
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

struct FleetReadings {
    /// `(labels, ready, live, is_task)` per builder partition.
    rows: Vec<(String, i64, i64, bool)>,
    live_lanes: i64,
    max_live: Option<i64>,
    pool: Option<i64>,
}

fn count_of(out: &str) -> i64 {
    out.trim().parse().unwrap_or(0)
}

/// Observed fleet occupancy from the same tools the sentinel decides calls with: the runes
/// supplies the partitions, `spira-claim` the claimable work under each, `strand` the dwarves live.
fn fleet_readings(cfg: &Config) -> FleetReadings {
    let sc = &cfg.seams.spira_config;
    let words = |out: String| out.split_whitespace().map(String::from).collect::<Vec<_>>();
    let task_runes = words(run_cmd(sc, &["rune", "task"]));
    let live_of = |rune: &str| count_of(&run_cmd(&cfg.seams.strand, &["dwarf-count", rune]));

    let live_lanes = words(run_cmd(sc, &["rune", "lane"])).iter().filter(|f| !task_runes.contains(f)).map(|f| live_of(f)).sum();

    let mut rows = Vec::new();
    for line in run_cmd(sc, &["rune", "partitions"]).lines() {
        let (labels, exclude) = line.split_once('\t').unwrap_or((line, ""));
        if labels.is_empty() {
            continue;
        }
        let ready = count_of(&run_cmd(&cfg.seams.spira_claim, &["ready-count", labels, exclude]));
        let runes = run_cmd(sc, &["rune", "for-labels", labels]);
        let runes: Vec<&str> = runes.split_whitespace().collect();
        let live = runes.iter().map(|f| live_of(f)).sum();
        let is_task = runes.iter().any(|f| task_runes.iter().any(|t| t == f));
        rows.push((labels.to_string(), ready, live, is_task));
    }

    FleetReadings {
        rows,
        live_lanes,
        max_live: cfg.max_live_dwarves.trim().parse().ok(),
        pool: cfg.max_dwarves.trim().parse().ok(),
    }
}

fn observe_fleet(cfg: &Config) -> Vec<Check> {
    let FleetReadings { rows, live_lanes, max_live, pool } = fleet_readings(cfg);

    // The Composite's FleetSpec.ceiling is the declared desired state; an install that has
    // not materialised one yet falls back to SPIRA_MAX_LIVE_DWARVES.
    let max_live = cfg.desired.fleet.as_ref().map(|f| f.ceiling as i64).or(max_live);

    let mut checks = Vec::new();
    let max_live = match max_live {
        Some(m) => m,
        None => {
            // Neither the Composite nor SPIRA_MAX_LIVE_DWARVES declares a ceiling — there is
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
        // SPIRA_MAX_DWARVES=0 is an operator pausing the task pool on purpose (dwarves.sh
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
// absence of every @cockpit pane says `layout down` was run on purpose.
// ──────────────────────────────────────────────────────────────────────────────

fn observe_cockpit(cfg: &Config) -> Check {
    // `layout down` records this: the operator dismissed the dashboards on purpose, and a
    // deliberate state is not a fault (law-a-deliberate-state-is-not-a-fault). `layout up`
    // clears it the moment it rebuilds anything, so this is stale for no longer than the next
    // time the operator — or this very remedy — asks for the cockpit.
    if cfg.spira_run.join("cockpit.down").exists() {
        return Check { key: "cockpit".into(), raw: RawStatus::Satisfied, remedy: Remedy::Escalate };
    }

    if !run_cmd_ok(&cfg.seams.tmux, &["list-sessions"]) {
        return Check {
            key: "cockpit".into(),
            raw: RawStatus::Gap { desired: "tmux server up".into(), observed: "no server".into(), since_hint: None },
            remedy: rebuild_remedy(cfg),
        };
    }

    let dash_session = match cfg.cockpit_sessions.first() {
        Some(s) => s.clone(),
        None => "brain".to_string(),
    };
    let mut sessions = cfg.cockpit_sessions.clone();
    sessions.push("cockpit".to_string());
    for s in &sessions {
        if !run_cmd_ok(&cfg.seams.tmux, &["has-session", "-t", &format!("={}", s)]) {
            return Check {
                key: "cockpit".into(),
                raw: RawStatus::Gap {
                    desired: "sessions present".into(),
                    observed: format!("session {} missing", s),
                    since_hint: None,
                },
                remedy: rebuild_remedy(cfg),
            };
        }
    }

    let tags_out = run_cmd(&cfg.seams.tmux, &["list-panes", "-t", &format!("{}:0", dash_session), "-F", "#{@cockpit}"]);
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
            remedy: ensure_remedy(cfg),
        };
    }

    Check { key: "cockpit".into(), raw: RawStatus::Satisfied, remedy: Remedy::Escalate }
}

/// No server, or a session gone: only `rebuild` can make a cockpit from nothing.
fn rebuild_remedy(cfg: &Config) -> Remedy {
    Remedy::Command { program: cfg.seams.rebuild.clone(), args: Vec::new() }
}

/// Sessions up and a dashboard missing: `layout ensure` heals in place and respawns nothing
/// that is fine.
fn ensure_remedy(cfg: &Config) -> Remedy {
    Remedy::Command { program: cfg.seams.layout.clone(), args: vec!["ensure".into()] }
}

// ──────────────────────────────────────────────────────────────────────────────
// Production/Release: the checkout SPIRA_HOME names is the activated release, or main.
// Read-only — the production checkout is never written by this program.
// ──────────────────────────────────────────────────────────────────────────────

fn on_main_branch(cfg: &Config) -> Option<bool> {
    let out = spira_config::bounded::bounded(&cfg.seams.git)
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
// Disk: a free-space floor on every path configured, observed with df (plus podman's own
// idea of its storage root — never a guessed path). The root filesystem once held 2.6 GB free
// of 62 GB mid-round with every suite running 2x slower, and nothing noticed until it hit 100%
// and crashed the store. The remedy reaps the build output of landed worktrees (target-reap)
// and prunes dangling podman images and unattached volumes; if that still does not clear the
// floor, the streak escalates to the Concierge exactly like any other remedy that did not hold.
// ──────────────────────────────────────────────────────────────────────────────

/// Free percentage from `df -kP <path>` output: the last row's total and available columns.
fn parse_df_free_pct(out: &str) -> Option<i64> {
    let row = out.lines().rfind(|l| !l.trim().is_empty())?;
    let cols: Vec<&str> = row.split_whitespace().collect();
    let total: i64 = cols.get(1)?.parse().ok()?;
    let avail: i64 = cols.get(3)?.parse().ok()?;
    (total > 0).then(|| avail * 100 / total)
}

fn free_pct(cfg: &Config, path: &str) -> Option<i64> {
    parse_df_free_pct(&run_cmd(&cfg.seams.df, &["-kP", path]))
}

/// Without a Composite DiskSpec: the root filesystem, the store's data directory, and
/// podman's storage root. A default-set path that cannot be read (podman not installed, the
/// store in server mode elsewhere) is simply omitted — there is no declared expectation it exist.
fn default_disk_paths(cfg: &Config) -> Vec<String> {
    let mut paths = vec!["/".to_string()];
    if !cfg.dolt_data.is_empty() {
        paths.push(cfg.dolt_data.clone());
    }
    let graph_root = run_cmd(&cfg.seams.podman, &["info", "--format", "{{.Store.GraphRoot}}"]);
    if !graph_root.trim().is_empty() {
        paths.push(graph_root.trim().to_string());
    }
    paths
}

fn disk_remedy(cfg: &Config) -> Remedy {
    let podman = |args: &[&str]| (cfg.seams.podman.clone(), args.iter().map(|a| a.to_string()).collect());
    Remedy::Sequence(vec![
        (cfg.seams.target_reap.clone(), Vec::new()),
        podman(&["image", "prune", "-f"]),
        podman(&["volume", "prune", "-f"]),
    ])
}

fn observe_disk(cfg: &Config) -> Vec<Check> {
    let (floor_pct, declared_paths): (u32, &[String]) = match &cfg.desired.disk {
        Some(d) => (d.floor_pct, d.paths.as_slice()),
        None => (cfg.disk_floor_pct, &[]),
    };
    let explicit = !declared_paths.is_empty();
    let paths: Vec<String> = if explicit { declared_paths.to_vec() } else { default_disk_paths(cfg) };

    let mut checks = Vec::new();
    for path in paths {
        let raw = match free_pct(cfg, &path) {
            // A path the Composite names that cannot be read still gets a row (unobservable,
            // never silently dropped) so a typo cannot vanish from the check set
            // (law-a-control-that-cannot-check-must-refuse).
            None if explicit => RawStatus::Unobservable { reason: format!("could not read free space for {}", path) },
            None => continue,
            Some(free) if free >= floor_pct as i64 => RawStatus::Satisfied,
            Some(free) => RawStatus::Gap {
                desired: format!(">= {}% free", floor_pct),
                observed: format!("{}% free", free),
                since_hint: None,
            },
        };
        let remedy = match raw {
            RawStatus::Gap { .. } => disk_remedy(cfg),
            _ => Remedy::Escalate,
        };
        checks.push(Check { key: format!("disk:{}", path), raw, remedy });
    }
    checks
}

// ──────────────────────────────────────────────────────────────────────────────
// ──────────────────────────────────────────────────────────────────────────────
// Queue: two structural checks czar-pass's own detectors do not already cover. The base
// gate itself is base-red, already watched by czar-pass — reconciling it here too would
// file a second incident for the same fault, so it is deliberately not repeated.
// ──────────────────────────────────────────────────────────────────────────────

fn branch_mergeable(cfg: &Config, repo: &Path, base: &str, branch: &str) -> Option<bool> {
    let repo_str = repo.to_string_lossy().to_string();
    let mb_out = spira_config::bounded::bounded(&cfg.seams.git)
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
    let mt_out = spira_config::bounded::bounded(&cfg.seams.git)
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
        let base = run_cmd(&cfg.seams.spira_config, &["repo", "landref", &repo_name]);
        let base = base.trim();
        if base.is_empty() {
            continue;
        }
        let out = run_cmd(&cfg.seams.queue_helpers, &["certified-list", &repo_path.to_string_lossy()]);
        for line in out.lines() {
            let Some(id) = line.split_whitespace().next() else { continue };
            let branch = format!("spira/{id}");
            let branch = branch.as_str();
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
                    program: cfg.seams.queue.clone(),
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

const STRANDED_RUN_PREFIX: &str = "queue-stranded-run:";

fn stranded_runs(out: &str) -> Vec<(String, String)> {
    let mut runs: Vec<(String, String)> = Vec::new();
    for line in out.lines().filter(|l| l.starts_with("STRANDED ")) {
        let field = |name: &str| line.split_whitespace().find_map(|w| w.strip_prefix(name)).map(str::to_string);
        let Some(run) = field("run=") else { continue };
        if runs.iter().any(|(r, _)| *r == run) {
            continue;
        }
        runs.push((run, field("label=").unwrap_or_default()));
    }
    runs
}

fn observe_queue_stranded_runs(cfg: &Config) -> Vec<Check> {
    let mut checks = Vec::new();
    for repo_name in queue_repo_names(&cfg.queue_dir) {
        let Some(repo_path) = repo_root(&repo_name, &cfg.repo_map) else { continue };
        let repo_str = repo_path.to_string_lossy().to_string();
        let out = run_cmd(&cfg.seams.forge, &["stranded-runners", &repo_str]);
        for (run, label) in stranded_runs(&out) {
            checks.push(Check {
                key: format!("{STRANDED_RUN_PREFIX}{repo_name}:{run}"),
                raw: RawStatus::Gap {
                    desired: "every queued job's runner label is carried by an online runner".into(),
                    observed: format!("run {run} queued on {label}, runner gone"),
                    since_hint: None,
                },
                remedy: Remedy::Command { program: cfg.seams.forge.clone(), args: vec!["workflow-rerun".into(), repo_str.clone(), run] },
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

const LANDED_BATCH_PREFIX: &str = "batch-landed:";

struct GreenBatch {
    id: String,
    repo: String,
    version: String,
    head: String,
}

fn green_batches(out: &str) -> Option<Vec<GreenBatch>> {
    let v: serde_json::Value = serde_json::from_str(out.trim()).ok()?;
    let text = |b: &serde_json::Value, k: &str| match b.get(k) {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(n @ serde_json::Value::Number(_)) => Some(n.to_string()),
        _ => None,
    };
    Some(
        v.as_array()?
            .iter()
            .filter(|b| b.get("state").and_then(|s| s.as_str()) == Some("GREEN"))
            .filter_map(|b| {
                Some(GreenBatch { id: text(b, "batch_id")?, repo: text(b, "repo")?, version: text(b, "version")?, head: text(b, "head")? })
            })
            .filter(|g| !g.head.is_empty())
            .collect(),
    )
}

fn head_on_ref(cfg: &Config, repo: &Path, head: &str, landref: &str) -> Option<bool> {
    let status = spira_config::bounded::bounded(&cfg.seams.git)
        .args(["-C", &repo.to_string_lossy(), "merge-base", "--is-ancestor", head, landref])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    match status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

fn observe_landed_batches(cfg: &Config) -> Vec<Check> {
    let out = run_cmd(&cfg.seams.spira_lc, &["list", "--batches", "--window", "1"]);
    let Some(greens) = green_batches(&out) else {
        return vec![Check {
            key: "batch-landed".into(),
            raw: RawStatus::Unobservable { reason: "spira-lc list --batches gave no readable answer".into() },
            remedy: Remedy::Escalate,
        }];
    };
    let mut checks = Vec::new();
    for g in greens {
        let Some(repo_path) = repo_root(&g.repo, &cfg.repo_map) else { continue };
        let landref = run_cmd(&cfg.seams.spira_config, &["repo", "landref", &g.repo]);
        let landref = landref.trim();
        if landref.is_empty() {
            continue;
        }
        let raw = match head_on_ref(cfg, &repo_path, &g.head, landref) {
            None => RawStatus::Unobservable { reason: format!("cannot compare {} with {} in {}", g.head, landref, g.repo) },
            Some(false) => RawStatus::Satisfied,
            Some(true) => RawStatus::Gap {
                desired: format!("batch {} LANDED", g.id),
                observed: format!("GREEN with head {} already on {}", g.head, landref),
                since_hint: None,
            },
        };
        checks.push(Check {
            key: format!("{LANDED_BATCH_PREFIX}{}:{}", g.repo, g.id),
            raw,
            remedy: Remedy::Command {
                program: cfg.seams.spira_lc.clone(),
                args: vec![
                    "land".into(),
                    g.id.clone(),
                    "--expect".into(),
                    "GREEN".into(),
                    "--version".into(),
                    g.version.clone(),
                    "--actor".into(),
                    "reconciler".into(),
                    "--sha".into(),
                    g.head.clone(),
                ],
            },
        });
    }
    checks
}

fn observe_lc_orphans(cfg: &Config) -> Check {
    let actor = "reconciler";
    let raw = orphan_reading(&run_cmd(&cfg.seams.spira_lc, &["requeue-orphans", "--actor", actor]));
    Check {
        key: "lc-orphan-in-delivery".into(),
        raw,
        remedy: Remedy::Command { program: cfg.seams.spira_lc.clone(), args: vec!["requeue-orphans".into(), "--actor".into(), actor.into(), "--apply".into()] },
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
        let beat_path = cfg.queue_dir.join(&repo_name).join("lock.beat");
        let mtime = [&lock_path, &beat_path]
            .iter()
            .filter_map(|p| fs::metadata(p).ok())
            .filter_map(|m| m.modified().ok())
            .filter_map(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .max();
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
    let rows = match spira_config::lc_state::list_with(&cfg.seams.spira_lc) {
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
                program: cfg.seams.spira_lc.clone(),
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
// Epic edges: a blocks edge onto an open epic can never clear, because an epic closes only
// when its children do. The remedy rewrites it as parent-child.
// ──────────────────────────────────────────────────────────────────────────────

const EPIC_EDGE_PREFIX: &str = "epic-blocks:";

fn epic_edge_pairs(out: &str) -> Vec<(String, String)> {
    out.lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            Some((w.next()?.to_string(), w.next()?.to_string()))
        })
        .collect()
}

fn observe_epic_edges(cfg: &Config) -> Vec<Check> {
    let out = match spira_config::bounded::bounded(&cfg.seams.bead)
        .args(["dep", "epic-edges"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        Ok(o) => {
            return vec![Check {
                key: "epic-blocks-edge".into(),
                raw: RawStatus::Unobservable { reason: format!("{} dep epic-edges exited {}", cfg.seams.bead, o.status.code().unwrap_or(-1)) },
                remedy: Remedy::Escalate,
            }]
        }
        Err(e) => {
            return vec![Check {
                key: "epic-blocks-edge".into(),
                raw: RawStatus::Unobservable { reason: format!("{}: {}", cfg.seams.bead, e) },
                remedy: Remedy::Escalate,
            }]
        }
    };
    epic_edge_pairs(&out)
        .into_iter()
        .map(|(child, epic)| Check {
            key: format!("{}{}:{}", EPIC_EDGE_PREFIX, child, epic),
            raw: RawStatus::Gap {
                desired: "a parent-child edge onto an epic".into(),
                observed: format!("{} blocked by open epic {}", child, epic),
                since_hint: None,
            },
            remedy: Remedy::Command {
                program: cfg.seams.bead.clone(),
                args: vec!["dep".into(), "convert".into(), child, epic],
            },
        })
        .collect()
}

// ──────────────────────────────────────────────────────────────────────────────
// Main health: the landing ref must pass the fences every branch is judged by. A base
// that is red turns every bead's gate red, so a red fence escalates at once.
// ──────────────────────────────────────────────────────────────────────────────

struct Fence {
    class: &'static str,
    program: String,
    args: Vec<String>,
}

fn base_fences(cfg: &Config, tree: &str, sha: &str) -> Vec<Fence> {
    vec![
        Fence { class: "lint", program: cfg.seams.lint.clone(), args: vec!["--root".into(), tree.into(), "--base".into(), sha.into()] },
        Fence { class: "lifecycle-guard", program: cfg.seams.guard.clone(), args: vec!["--gate".into(), tree.into()] },
    ]
}

fn active_release(cfg: &Config) -> String {
    if cfg.releases_dir.as_os_str().is_empty() {
        return cfg.release.clone();
    }
    fs::canonicalize(cfg.releases_dir.join("current"))
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| cfg.release.clone())
}

fn lint_awaiting_activation(cfg: &Config, repo: &str, sha: &str) -> Option<RawStatus> {
    let git = &cfg.seams.git;
    let release = run_cmd(git, &["-C", repo, "rev-parse", "--verify", "--quiet", &format!("{}^{{commit}}", active_release(cfg))]);
    let release = release.trim();
    if release.is_empty() || run_cmd_ok(git, &["-C", repo, "merge-base", "--is-ancestor", sha, release]) {
        return None;
    }
    let oldest = run_cmd(git, &["-C", repo, "log", "--reverse", "--format=%ct", &format!("{}..{}", release, sha)]);
    let landed: u64 = oldest.lines().next()?.trim().parse().ok()?;
    let lag = cfg.now_secs.saturating_sub(landed);
    if lag <= cfg.activation_grace_secs {
        return Some(RawStatus::Deliberate {
            reason: format!("pending activation: {} is {}, the active release is {}", MAIN_REF, sha, release),
        });
    }
    Some(RawStatus::Gap {
        desired: format!("the active release contains {}", MAIN_REF),
        observed: format!(
            "lint not judged: {} is {} but the active release is {}; activation has lagged {}s (grace {}s)",
            MAIN_REF, sha, release, lag, cfg.activation_grace_secs
        ),
        since_hint: None,
    })
}

fn observe_main_health(cfg: &Config) -> Vec<Check> {
    let Some(repo) = repo_root("spira", &cfg.repo_map) else { return Vec::new() };
    let repo_str = repo.to_string_lossy().to_string();
    let git = &cfg.seams.git;
    let unobservable = |reason: String| {
        vec![Check { key: "main-health".into(), raw: RawStatus::Unobservable { reason }, remedy: Remedy::Escalate }]
    };
    let sha = run_cmd(git, &["-C", &repo_str, "rev-parse", "--verify", "--quiet", &format!("{}^{{commit}}", MAIN_REF)]);
    let sha = sha.trim().to_string();
    if sha.is_empty() {
        return unobservable(format!("{} does not resolve in {}", MAIN_REF, repo_str));
    }
    let cache_path = paths::main_health_cache(&cfg.spira_run);
    let mut cache: serde_json::Map<String, serde_json::Value> = fs::read_to_string(&cache_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let cached = |cache: &serde_json::Map<String, serde_json::Value>, class: &str| -> Option<RawStatus> {
        let e = cache.get(class)?;
        if e.get("sha")?.as_str()? != sha {
            return None;
        }
        match e.get("observed").and_then(|o| o.as_str()) {
            None => Some(RawStatus::Satisfied),
            Some(observed) => Some(RawStatus::Gap {
                desired: format!("{} passes on {}", class, MAIN_REF),
                observed: observed.to_string(),
                since_hint: None,
            }),
        }
    };
    let lint_pending = lint_awaiting_activation(cfg, &repo_str, &sha);
    let pending_check = lint_pending.map(|raw| Check { key: "main-health:lint".into(), raw, remedy: Remedy::Escalate });
    let lint_is_pending = pending_check.is_some();
    let judged = move |f: &Fence| !(lint_is_pending && f.class == "lint");
    let fences: Vec<Fence> = base_fences(cfg, "", &sha).into_iter().filter(judged).collect();
    if fences.iter().all(|f| cached(&cache, f.class).is_some()) {
        let mut checks: Vec<Check> = pending_check.into_iter().collect();
        checks.extend(fences.iter().map(|f| Check {
            key: format!("main-health:{}", f.class),
            raw: cached(&cache, f.class).expect("checked above"),
            remedy: Remedy::Escalate,
        }));
        return checks;
    }
    let tree = paths::main_health_tree(&cfg.spira_run);
    let tree_str = tree.to_string_lossy().to_string();
    let _ = run_cmd_ok(git, &["-C", &repo_str, "worktree", "remove", "--force", &tree_str]);
    let _ = fs::remove_dir_all(&tree);
    if !run_cmd_ok(git, &["-C", &repo_str, "worktree", "add", "--detach", &tree_str, &sha]) {
        return unobservable(format!("cannot check out {} into {}", sha, tree_str));
    }
    let mut checks: Vec<Check> = pending_check.into_iter().collect();
    for fence in base_fences(cfg, &tree_str, &sha).into_iter().filter(judged) {
        if let Some(raw) = cached(&cache, fence.class) {
            checks.push(Check { key: format!("main-health:{}", fence.class), raw, remedy: Remedy::Escalate });
            continue;
        }
        let started = std::time::Instant::now();
        // batch-job: a fence over a whole checkout runs to SPIRA_MAIN_HEALTH_FENCE_DEADLINE_SECS (a hang guard), not the 5 s call deadline
        let out = spira_config::bounded::bounded_for(&fence.program, &cfg.fence_deadline_secs.to_string())
            .args(&fence.args)
            .current_dir(&tree)
            .stdin(Stdio::null())
            .output();
        let elapsed = started.elapsed().as_secs_f64();
        let log = paths::main_health_log(&cfg.spira_run, fence.class);
        let raw = match &out {
            Err(e) => RawStatus::Unobservable { reason: format!("{}: {}", fence.program, e) },
            Ok(o) if o.status.success() => RawStatus::Satisfied,
            Ok(o) => {
                let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
                let _ = fs::write(&log, text);
                match o.status.code() {
                    Some(1) => RawStatus::Gap {
                        desired: format!("{} passes on {}", fence.class, MAIN_REF),
                        observed: format!("{} red at {} (output in {})", fence.class, sha, log.display()),
                        since_hint: None,
                    },
                    Some(124) => RawStatus::Unobservable {
                        reason: format!(
                            "{} timed out on {}: killed at the {}s deadline (SPIRA_MAIN_HEALTH_FENCE_DEADLINE_SECS) after {:.1}s; output in {}",
                            fence.class, sha, cfg.fence_deadline_secs, elapsed, log.display()
                        ),
                    },
                    code => RawStatus::Unobservable {
                        reason: format!(
                            "{} {} on {} after {:.1}s; output in {}",
                            fence.class,
                            code.map_or("killed by a signal".to_string(), |c| format!("exit {}", c)),
                            sha,
                            elapsed,
                            log.display()
                        ),
                    },
                }
            }
        };
        match &raw {
            RawStatus::Satisfied => {
                cache.insert(fence.class.to_string(), serde_json::json!({ "sha": sha }));
            }
            RawStatus::Gap { observed, .. } => {
                cache.insert(fence.class.to_string(), serde_json::json!({ "sha": sha, "observed": observed }));
            }
            _ => {
                cache.remove(fence.class);
            }
        }
        checks.push(Check { key: format!("main-health:{}", fence.class), raw, remedy: Remedy::Escalate });
    }
    let _ = fs::write(&cache_path, serde_json::Value::Object(cache).to_string());
    let _ = run_cmd_ok(git, &["-C", &repo_str, "worktree", "remove", "--force", &tree_str]);
    let _ = fs::remove_dir_all(&tree);
    checks
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
        &cfg.store,
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
            if let Err(e) = mail::note_concierge(&cfg.seams.mail, &subject, &body) {
                log_print(cfg, &format!("reconciler: {} → no-effect mail failed: {}", key, e));
            }
        }
    }
}

fn evaluate(cfg: &Config, state: &mut StateMap, pending: &mut PendingMap, alerted: &mut AlertedSinceMap, check: Check) {
    let prev = state.remove(&check.key).unwrap_or_default();
    let prev_remedy = last_remedy(&prev).map(str::to_string);
    let metric_now = gap_metric(&check.raw);
    let first_unobservable_unit_read =
        kind_of(&check.key) == "units" && matches!(check.raw, RawStatus::Unobservable { .. }) && prev == HysteresisState::default();
    let (mut verdict, mut next) = step(cfg.now_secs, check.raw, cfg.grace_secs, prev);
    if first_unobservable_unit_read {
        verdict.is_gap = false;
    }
    append_status(&cfg.store, &cfg.now_iso, &check.key, &verdict);
    judge_pending(cfg, pending, &check.key, metric_now);

    if verdict.is_gap {
        if verdict.remedy_failed {
            escalate(cfg, alerted, &check.key, &verdict, prev_remedy.as_deref());
        } else {
            match &check.remedy {
                Remedy::Escalate => escalate(cfg, alerted, &check.key, &verdict, None),
                remedy => {
                    let desc = remedy.describe();
                    if is_shadowed(&cfg.shadow_kinds, kind_of(&check.key)) {
                        log_print(cfg, &format!("reconciler: {} → remedy held (shadow): {}", check.key, desc));
                        write_event(cfg, "shadow", &check.key, &desc, metric_now, None);
                        escalate(cfg, alerted, &check.key, &verdict, None);
                    } else {
                        log_print(cfg, &format!("reconciler: {} → remedy: {}", check.key, desc));
                        write_event(cfg, "remedy", &check.key, &desc, metric_now, None);
                        for (program, args) in remedy.steps() {
                            let _ = run_cmd_ok(program, &args);
                        }
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
    } else {
        alerted.remove(&check.key);
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

// A structural gap reaches here only after a remedy already failed or none exists: judgement,
// not a bead. It goes to the Concierge inbox as one note per gap streak — `should_alert` keys
// the streak by when it began, so a gap that persists is not re-sent every pass.
fn escalate(cfg: &Config, alerted: &mut AlertedSinceMap, key: &str, verdict: &Verdict, last_remedy: Option<&str>) {
    let (fire, next) = should_alert(verdict, alerted.get(key).copied());
    match next {
        Some(since) => alerted.insert(key.to_string(), since),
        None => alerted.remove(key),
    };
    if !fire {
        return;
    }
    let subject = format!("RECONCILER: {} — {}", key, describe(verdict));
    let evidence = compose_alert(key, cfg.now_secs, verdict, last_remedy);
    log_print(cfg, &format!("reconciler: {} → escalate", key));
    let body = format!("## Alert\n{subject}\n\nThe reconciler found a structural gap that has outlasted its grace period.\n\n{evidence}");
    if let Err(e) = mail::send(&cfg.seams.mail, "concierge", mail::FROM, &subject, "alert", None, None, &body) {
        log_print(cfg, &format!("reconciler: {} → escalation mail failed: {}", key, e));
        alerted.remove(key);
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Hold sweep: every held bead visited, moved one step for its kind (reconciler_engine::holds).
// Throttled to its own cadence — it costs a store read per held bead — and its remedies are
// telemetered and measured like any other, under the kind `holds`.
// ──────────────────────────────────────────────────────────────────────────────

const HOLD_SWEEP_INTERVAL_SECS: u64 = 600;

fn run_hold_sweep(cfg: &Config, pending: &mut PendingMap) {
    let state_path = cfg.spira_run.join("reconciler-holds.json");
    let mut state = holds::load_state(&state_path);
    if cfg.now_secs.saturating_sub(state.last_pass) < HOLD_SWEEP_INTERVAL_SECS {
        return;
    }
    let mut live = hold_sweep::Live {
        lc_bin: cfg.seams.spira_lc.clone(),
        mail_sh: cfg.seams.mail.clone(),
        mail_root: cfg.mail_root.clone(),
        actor: "reconciler".to_string(),
        now: cfg.now_secs,
    };
    let shadow = is_shadowed(&cfg.shadow_kinds, "holds");
    let breaker = spira_config::process::cfg_parse::<u32>("SPIRA_NO_PROGRESS_BREAKER_CLAIMS")
        .and_then(|claims| Ok((claims, spira_config::process::cfg_parse::<u32>("SPIRA_NO_PROGRESS_BREAKER_WINDOW_MINUTES")?)))
        .and_then(|(claims, minutes)| holds::Breaker::new(claims, minutes));
    let breaker = match breaker {
        Ok(b) => Some(b),
        Err(e) => {
            log_print(cfg, &format!("reconciler: holds: no-progress breaker unconfigured, waits not lifted: {e}"));
            None
        }
    };
    match holds::sweep(cfg.now_secs, &mut live, &mut state, shadow, breaker) {
        Err(e) => {
            log_print(cfg, &format!("reconciler: holds: cannot tell, nothing acted on: {e}"));
            state.last_pass = cfg.now_secs;
        }
        Ok(report) => {
            let judged: Vec<String> = pending.keys().filter(|k| k.starts_with("holds:")).cloned().collect();
            for key in judged {
                judge_pending(cfg, pending, &key, if report.wanted.contains(&key) { 1.0 } else { 0.0 });
            }
            for note in &report.skipped {
                log_print(cfg, &format!("reconciler: holds: skipped {note}"));
            }
            for t in &report.taken {
                let desc = format!("{} {}", t.action.name(), t.bead);
                match &t.outcome {
                    HoldOutcome::Done => {
                        log_print(cfg, &format!("reconciler: {} → remedy: {desc}", t.key));
                        write_event(cfg, "remedy", &t.key, &desc, 1.0, None);
                        pending.insert(
                            t.key.clone(),
                            PendingEffect { action: desc, metric: "gap".into(), before: 1.0, acted_at: cfg.now_secs, passes_seen: 0 },
                        );
                    }
                    HoldOutcome::Shadowed => {
                        log_print(cfg, &format!("reconciler: {} → remedy held (shadow): {desc}", t.key));
                        write_event(cfg, "shadow", &t.key, &desc, 1.0, None);
                    }
                    HoldOutcome::Failed(e) => log_print(cfg, &format!("reconciler: {} → {desc} failed: {e}", t.key)),
                }
            }
            log_print(cfg, &format!("reconciler: holds: visited {}, acted {}", report.visited, report.taken.len()));
        }
    }
    let _ = holds::save_state(&state_path, &state);
}

// ──────────────────────────────────────────────────────────────────────────────
// Main pass
// ──────────────────────────────────────────────────────────────────────────────

fn observed(cfg: &Config, name: &str, observe: impl FnOnce() -> Vec<Check>) -> Vec<Check> {
    log_print(cfg, &format!("reconciler: invariant {name}: observing"));
    let started = clock::now_secs(&cfg.seams.date);
    let checks = observe();
    let took = clock::now_secs(&cfg.seams.date).saturating_sub(started);
    log_print(cfg, &format!("reconciler: invariant {name}: {} check(s), {took}s", checks.len()));
    checks
}

fn run_pass() -> Result<(), String> {
    let cfg = Config::from_process(Seams::production())?.with_desired_state();

    if cfg.spira_run.join("world.halted").exists() {
        log_print(&cfg, "reconciler: skipped — world is halted");
        stamp_pass(&cfg);
        return Ok(());
    }

    let lock_file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&cfg.lock_path)
        .map_err(|e| format!("open lock {}: {}", cfg.lock_path.display(), e))?;
    if unsafe { flock(lock_file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
        log_print(&cfg, "reconciler: already running — skip");
        return Ok(());
    }

    let mut state = load_state(&cfg.state_path);
    let mut pending = load_pending(&cfg.effect_state);
    let mut alerted = load_alerted(&cfg.alerted_path);

    type Job<'a> = (&'static str, Box<dyn FnOnce() -> Vec<Check> + Send + 'a>);
    let jobs: Vec<Job> = vec![
        ("units", Box::new(|| observe_units(&cfg))),
        ("fleet", Box::new(|| observe_fleet(&cfg))),
        ("cockpit", Box::new(|| vec![observe_cockpit(&cfg)])),
        ("release", Box::new(|| vec![observe_release(&cfg)])),
        ("disk", Box::new(|| observe_disk(&cfg))),
        ("queue-mergeable", Box::new(|| observe_queue_mergeable(&cfg))),
        ("queue-lock-age", Box::new(|| observe_queue_lock_age(&cfg))),
        ("queue-stranded-runs", Box::new(|| observe_queue_stranded_runs(&cfg))),
        ("lc-orphans", Box::new(|| vec![observe_lc_orphans(&cfg)])),
        ("batch-landed", Box::new(|| observe_landed_batches(&cfg))),
        ("junk-row", Box::new(|| observe_junk_rows(&cfg))),
        ("epic-edges", Box::new(|| observe_epic_edges(&cfg))),
        ("main-health", Box::new(|| observe_main_health(&cfg))),
    ];
    let mut by_name: std::collections::BTreeMap<&'static str, Vec<Check>> = std::thread::scope(|scope| {
        let handles: Vec<_> = jobs
            .into_iter()
            .map(|(name, job)| {
                let cfg = &cfg;
                (name, scope.spawn(move || observed(cfg, name, job)))
            })
            .collect();
        handles
            .into_iter()
            .map(|(name, h)| match h.join() {
                Ok(checks) => (name, checks),
                Err(_) => {
                    log_print(&cfg, &format!("reconciler: invariant {name}: panicked"));
                    (
                        name,
                        vec![Check {
                            key: format!("invariant:{name}"),
                            raw: RawStatus::Unobservable { reason: format!("the {name} observer panicked") },
                            remedy: Remedy::Escalate,
                        }],
                    )
                }
            })
            .collect()
    });
    for (name, prefix) in [
        ("queue-stranded-runs", STRANDED_RUN_PREFIX),
        ("batch-landed", LANDED_BATCH_PREFIX),
        ("junk-row", JUNK_ROW_PREFIX),
        ("epic-edges", EPIC_EDGE_PREFIX),
    ] {
        let seen = &by_name[name];
        state.retain(|k, _| !k.starts_with(prefix) || seen.iter().any(|c| &c.key == k));
    }
    let mut checks = Vec::new();
    for name in [
        "units", "fleet", "cockpit", "release", "disk", "queue-mergeable", "queue-lock-age",
        "queue-stranded-runs", "lc-orphans", "batch-landed", "junk-row", "epic-edges", "main-health",
    ] {
        checks.extend(by_name.remove(name).unwrap_or_default());
    }

    remind_due_suspensions(&cfg, &load_ctrl(&cfg));

    let n = checks.len();
    for check in checks {
        evaluate(&cfg, &mut state, &mut pending, &mut alerted, check);
    }

    run_hold_sweep(&cfg, &mut pending);

    let _ = save_state(&cfg.state_path, &state);
    let _ = save_pending(&cfg.effect_state, &pending);
    let _ = save_alerted(&cfg.alerted_path, &alerted);

    let elapsed = clock::now_secs(&cfg.seams.date).saturating_sub(cfg.now_secs);
    log_print(&cfg, &format!("reconciler: complete ({} checks, {}s)", n, elapsed));

    stamp_pass(&cfg);
    drop(lock_file);
    Ok(())
}

fn stamp_pass(cfg: &Config) {
    let _ = fs::write(paths::pass_stamp(&cfg.spira_run), format!("{}\n", clock::now_secs(&cfg.seams.date)));
}

#[cfg(test)]
mod tests {
    #[test]
    fn seams_resolve_release_tools_when_path_lacks_the_release_bin() {
        let dir = testkit::TempDir::new("seams-release");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        for tool in ["spira-lc", "queue", "rebuild"] {
            let p = dir.join("bin").join(tool);
            testkit::write_exe(&p, "");
        }
        let release = dir.path().display().to_string();
        let seams = Seams::resolving(|n| spira_config::release_env::resolve_tool(n, Some(&release), None, Some("/nonexistent")));
        assert_eq!(seams.spira_lc, format!("{release}/bin/spira-lc"));
        assert_eq!(seams.queue, format!("{release}/bin/queue"));
        assert_eq!(seams.rebuild, format!("{release}/bin/rebuild"));
        assert_eq!(seams.git, "git");
    }

    use super::*;

    fn fixture_store() -> std::sync::Arc<tsd::store::Pusher> {
        let s = Box::leak(Box::new(spira_otel::fixture::FakeStore::start()));
        std::sync::Arc::new(tsd::store::Pusher::new(Some(s.addr().to_string())))
    }

    fn stored(cfg: &Config, family: &str) -> String {
        let rows = tsd::store::Reader::new(cfg.store.endpoint()).unwrap().rows_or_empty(family, 0).unwrap();
        rows.iter().map(|r| format!("{r}\n")).collect()
    }
    use std::time::SystemTime;

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

    #[test]
    fn evaluate_inside_grace_raises_nothing() {
        let dir = scratch_dir("evaluate-inside-grace");
        let marker = dir.join("remedy-ran");
        let cfg = Config { grace_secs: 300, now_secs: 100, ..test_config() };
        let mut state = StateMap::new();
        let mut alerted = AlertedSinceMap::new();
        evaluate(&cfg, &mut state, &mut PendingMap::new(), &mut alerted, Check {
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
        let mut alerted = AlertedSinceMap::new();
        evaluate(&cfg, &mut state, &mut PendingMap::new(), &mut alerted, Check {
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
        let stub_incident = dir.join("stub-mail");
        testkit::write_exe(&stub_incident, &format!("#!/usr/bin/env bash\ncat >/dev/null\ntouch {}\n", escalated.display()));

        let cfg1 = Config { grace_secs: 0, now_secs: 100, seams: stub_mail_seams(&stub_incident), ..test_config() };
        let mut state = StateMap::new();
        let mut alerted = AlertedSinceMap::new();
        let raw = || RawStatus::Gap { desired: "d".into(), observed: "o".into(), since_hint: None };

        // Pass 1: past grace, command remedy attempted and recorded.
        evaluate(&cfg1, &mut state, &mut PendingMap::new(), &mut alerted, Check {
            key: "k".into(),
            raw: raw(),
            remedy: Remedy::Command { program: "touch".into(), args: vec![marker.to_string_lossy().to_string()] },
        });
        assert!(marker.exists());
        fs::remove_file(&marker).unwrap();

        // Pass 2: still a gap — the engine reports remedy_failed, so evaluate must escalate
        // rather than run the command a second time.
        let cfg2 = Config { now_secs: 200, ..cfg1.clone() };
        evaluate(&cfg2, &mut state, &mut PendingMap::new(), &mut alerted, Check {
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
        let stub_incident = dir.join("stub-mail");
        testkit::write_exe(&stub_incident, &format!("#!/usr/bin/env bash\ncat >/dev/null\ntouch {}\n", escalated.display()));

        let cfg = Config { grace_secs: 300, now_secs: 100, seams: stub_mail_seams(&stub_incident), ..test_config() };
        let mut state = StateMap::new();
        let mut alerted = AlertedSinceMap::new();
        evaluate(&cfg, &mut state, &mut PendingMap::new(), &mut alerted, Check {
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
        let stub_incident = dir.join("stub-mail");
        testkit::write_exe(&stub_incident, &format!("#!/usr/bin/env bash\ncat >/dev/null\ntouch {}\n", escalated.display()));

        let cfg = Config { grace_secs: 0, now_secs: 100, seams: stub_mail_seams(&stub_incident), ..test_config() };
        let mut state = StateMap::new();
        let mut alerted = AlertedSinceMap::new();
        evaluate(&cfg, &mut state, &mut PendingMap::new(), &mut alerted, Check {
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

    fn effect_cfg(_dir: &Path, mail: &Path) -> Config {
        Config {
            grace_secs: 0,
            store: fixture_store(),
            seams: stub_mail_seams(mail),
            release: "r-fixture".into(),
            effect_passes: 2,
            ..test_config()
        }
    }

    fn stub_mail(dir: &Path) -> PathBuf {
        let mail = dir.join("stub-mail.sh");
        testkit::write_exe(&mail, &format!("#!/usr/bin/env bash\nbody=$(cat)\ncase \"$*\" in *\"had no effect\"*) printf '%s' \"$body\" >> {} ;; esac\n", dir.join("mailed").display()));
        mail
    }

    #[test]
    fn a_remedy_writes_one_event_and_an_effective_one_reads_effect() {
        let dir = scratch_dir("effect-remedy-event");
        let mail = stub_mail(&dir);
        let mut state = StateMap::new();
        let mut pending = PendingMap::new();
        let mut alerted = AlertedSinceMap::new();

        let cfg1 = Config { now_secs: 100, ..effect_cfg(&dir, &mail) };
        evaluate(&cfg1, &mut state, &mut pending, &mut alerted, Check { key: "disk:/v".into(), raw: gap_raw(), remedy: touch_remedy(&dir.join("ran")) });
        let log = stored(&cfg1, "reconciler-remedy");
        assert_eq!(log.lines().count(), 1, "one remedy writes exactly one event");
        for field in ["\"event\":\"remedy\"", "\"kind\":\"disk\"", "\"key\":\"disk:/v\"", "\"action\":\"touch ", "\"before\":1", "\"release\":\"r-fixture\"", "\"ts\":"] {
            assert!(log.contains(field), "event lacks {field}: {log}");
        }

        let cfg2 = Config { now_secs: 200, ..cfg1.clone() };
        evaluate(&cfg2, &mut state, &mut pending, &mut alerted, Check { key: "disk:/v".into(), raw: RawStatus::Satisfied, remedy: touch_remedy(&dir.join("ran")) });
        let log = stored(&cfg2, "reconciler-remedy");
        assert!(log.contains("\"event\":\"effect\"") && log.contains("\"after\":0"), "{log}");
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
        let mut alerted = AlertedSinceMap::new();
        let noop = || Remedy::Command { program: "true".into(), args: vec![] };

        let cfg1 = Config { now_secs: 100, ..effect_cfg(&dir, &mail) };
        evaluate(&cfg1, &mut state, &mut pending, &mut alerted, Check { key: "disk:/v".into(), raw: gap_raw(), remedy: noop() });
        let cfg2 = Config { now_secs: 200, ..cfg1.clone() };
        evaluate(&cfg2, &mut state, &mut pending, &mut alerted, Check { key: "disk:/v".into(), raw: gap_raw(), remedy: noop() });
        assert!(!dir.join("mailed").exists(), "one pass of no movement is still inside the pass budget");
        let cfg3 = Config { now_secs: 300, ..cfg1.clone() };
        evaluate(&cfg3, &mut state, &mut pending, &mut alerted, Check { key: "disk:/v".into(), raw: gap_raw(), remedy: noop() });

        let log = stored(&cfg3, "reconciler-remedy");
        assert!(log.contains("\"event\":\"no-effect\"") && log.contains("\"before\":1") && log.contains("\"after\":1"), "{log}");
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
        let mut alerted = AlertedSinceMap::new();
        evaluate(&cfg, &mut state, &mut pending, &mut alerted, Check { key: "disk:/v".into(), raw: gap_raw(), remedy: touch_remedy(&ran) });
        assert!(!ran.exists(), "the kill switch holds the remedy back");
        assert!(stored(&cfg, "reconciler-remedy").contains("\"event\":\"shadow\""));
        assert!(pending.is_empty(), "a remedy that did not act has no effect to measure");

        let other = Config { shadow_kinds: vec!["units-timer".into()], ..cfg.clone() };
        evaluate(&other, &mut state, &mut pending, &mut alerted, Check { key: "disk:/w".into(), raw: gap_raw(), remedy: touch_remedy(&ran) });
        assert!(ran.exists(), "a kind the switch does not name still acts");
    }

    #[test]
    fn stranded_runs_reads_one_entry_per_run_and_ignores_other_lines() {
        let out = "STRANDED run=77 job=\"a\" label=ci-r-77-1 queued=700s runner=none vmid=?\n\
                   STRANDED run=77 job=\"b\" label=ci-r-77-1 queued=700s runner=none vmid=?\n\
                   STRANDED run=78 job=\"a\" label=ci-r-78-2 queued=900s runner=x vmid=3\n\
                   noise run=99\n";
        assert_eq!(stranded_runs(out), vec![("77".to_string(), "ci-r-77-1".to_string()), ("78".to_string(), "ci-r-78-2".to_string())]);
        assert!(stranded_runs("").is_empty());
    }

    #[test]
    fn a_stranded_run_is_a_gap_remedied_by_a_whole_workflow_rerun() {
        let dir = scratch_dir("stranded");
        fs::create_dir_all(dir.join("queue/r")).unwrap();
        let forge = dir.join("forge");
        testkit::write_exe(&forge, "#!/bin/sh\ncase \"$1\" in stranded-runners) echo 'STRANDED run=55 job=\"j\" label=ci-r-55-2 queued=900s runner=none vmid=?' ;; esac\n");
        let map = dir.join("map");
        fs::write(&map, "r|/srv/r-checkout\n").unwrap();
        let cfg = Config {
            seams: Seams { forge: forge.to_string_lossy().to_string(), ..Seams::production() },
            queue_dir: dir.join("queue"),
            repo_map: Some(map),
            ..test_config()
        };
        let probe = observe_queue_stranded_runs(&cfg);
        assert_eq!(probe.len(), 1);
        assert_eq!(probe[0].key, "queue-stranded-run:r:55");
        assert!(matches!(probe[0].raw, RawStatus::Gap { .. }));
        let steps = probe[0].remedy.steps();
        assert_eq!(steps[0].1, vec!["workflow-rerun", "/srv/r-checkout", "55"]);
    }

    #[test]
    fn a_green_batch_whose_head_is_on_the_landing_ref_is_landed_and_one_off_it_is_left() {
        let dir = scratch_dir("landed-batch");
        let repo = dir.join("checkout");
        let git = |args: &[&str]| {
            let st = std::process::Command::new("git").arg("-C").arg(&repo).args(args).stdout(Stdio::null()).status().unwrap();
            assert!(st.success(), "git {args:?}");
        };
        fs::create_dir_all(&repo).unwrap();
        git(&["init", "-q", "-b", "trunk"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "on"]);
        let on = String::from_utf8(std::process::Command::new("git").arg("-C").arg(&repo).args(["rev-parse", "HEAD"]).output().unwrap().stdout).unwrap().trim().to_string();
        git(&["checkout", "-q", "-b", "side"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "off"]);
        let off = String::from_utf8(std::process::Command::new("git").arg("-C").arg(&repo).args(["rev-parse", "HEAD"]).output().unwrap().stdout).unwrap().trim().to_string();
        git(&["checkout", "-q", "trunk"]);

        let lc = dir.join("lc");
        testkit::write_exe(
            &lc,
            &format!(
                "#!/bin/sh\ncat <<'EOF'\n[{{\"batch_id\":\"b-on\",\"repo\":\"r\",\"state\":\"GREEN\",\"version\":4,\"head\":\"{on}\"}},\n {{\"batch_id\":\"b-off\",\"repo\":\"r\",\"state\":\"GREEN\",\"version\":2,\"head\":\"{off}\"}},\n {{\"batch_id\":\"b-done\",\"repo\":\"r\",\"state\":\"LANDED\",\"version\":9,\"head\":\"{on}\"}}]\nEOF\n"
            ),
        );
        let sc = dir.join("sc");
        testkit::write_exe(&sc, "#!/bin/sh\necho trunk\n");
        let map = dir.join("map");
        fs::write(&map, format!("r|{}\n", repo.display())).unwrap();
        let cfg = Config {
            seams: Seams { spira_lc: lc.to_string_lossy().to_string(), spira_config: sc.to_string_lossy().to_string(), ..Seams::production() },
            repo_map: Some(map),
            ..test_config()
        };
        let checks = observe_landed_batches(&cfg);
        assert_eq!(checks.len(), 2, "only GREEN rows are observed");
        let on_check = checks.iter().find(|c| c.key == "batch-landed:r:b-on").unwrap();
        assert!(matches!(on_check.raw, RawStatus::Gap { .. }));
        assert_eq!(on_check.remedy.steps()[0].1, vec!["land", "b-on", "--expect", "GREEN", "--version", "4", "--actor", "reconciler", "--sha", on.as_str()]);
        let off_check = checks.iter().find(|c| c.key == "batch-landed:r:b-off").unwrap();
        assert!(matches!(off_check.raw, RawStatus::Satisfied));
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

    #[test]
    fn a_persisting_gap_is_mailed_once_per_streak() {
        let dir = scratch_dir("escalate-once");
        let calls = dir.join("calls");
        let stub = dir.join("stub-mail");
        testkit::write_exe(&stub, &format!("#!/usr/bin/env bash\ncat >/dev/null\necho x >> {}\n", calls.display()));
        let cfg = Config { grace_secs: 0, now_secs: 100, seams: stub_mail_seams(&stub), ..test_config() };
        let mut state = StateMap::new();
        let mut alerted = AlertedSinceMap::new();
        for _ in 0..3 {
            evaluate(&cfg, &mut state, &mut PendingMap::new(), &mut alerted, Check {
                key: "k".into(),
                raw: RawStatus::Gap { desired: "d".into(), observed: "o".into(), since_hint: None },
                remedy: Remedy::Escalate,
            });
        }
        assert_eq!(fs::read_to_string(&calls).unwrap().lines().count(), 1, "one note per gap streak, not one per pass");
        evaluate(&cfg, &mut state, &mut PendingMap::new(), &mut alerted, Check { key: "k".into(), raw: RawStatus::Satisfied, remedy: Remedy::Escalate });
        assert!(alerted.is_empty(), "a closed streak forgets it was alerted, so a new one alerts again");
    }

    fn fleet_seams(dir: &Path) -> Seams {
        let sc = dir.join("spira-config");
        testkit::write_exe(
            &sc,
            "#!/bin/sh\ncase \"$1 $2\" in\n  'rune partitions') printf 'a\\t\\nb\\t\\nc\\t\\n' ;;\n  'rune task') echo 'p-a p-b' ;;\n  'rune lane') echo 'p-c' ;;\n  'rune for-labels') echo \"p-$3\" ;;\nesac\n",
        );
        let claim = dir.join("spira-claim");
        testkit::write_exe(&claim, "#!/bin/sh\necho 5\n");
        let strand = dir.join("strand");
        testkit::write_exe(&strand, "#!/bin/sh\necho 0\n");
        Seams {
            spira_config: sc.to_string_lossy().to_string(),
            spira_claim: claim.to_string_lossy().to_string(),
            strand: strand.to_string_lossy().to_string(),
            ..Seams::production()
        }
    }

    #[test]
    fn an_epic_edge_is_a_gap_whose_remedy_converts_it_and_no_edge_is_satisfied() {
        let dir = scratch_dir("epic-edges");
        let bead = dir.join("bead");
        testkit::write_exe(&bead, "#!/bin/sh\nprintf 'sp-child sp-epic\\n'\n");
        let cfg = Config { seams: Seams { bead: bead.to_string_lossy().to_string(), ..Seams::production() }, ..test_config() };
        let checks = observe_epic_edges(&cfg);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].key, "epic-blocks:sp-child:sp-epic");
        assert!(matches!(&checks[0].raw, RawStatus::Gap { .. }));
        assert!(matches!(&checks[0].remedy, Remedy::Command { args, .. } if args == &["dep", "convert", "sp-child", "sp-epic"]));

        testkit::write_exe(&bead, "#!/bin/sh\nexit 0\n");
        assert!(observe_epic_edges(&cfg).is_empty());

        testkit::write_exe(&bead, "#!/bin/sh\nexit 1\n");
        let broken = observe_epic_edges(&cfg);
        assert!(matches!(&broken[0].raw, RawStatus::Unobservable { .. }));
    }

    #[test]
    fn a_fleet_with_no_ceiling_is_one_unobservable_check_however_many_partitions_report() {
        let dir = scratch_dir("fleet-no-ceiling");
        let cfg = Config { seams: fleet_seams(&dir), max_live_dwarves: String::new(), max_dwarves: "6".into(), ..test_config() };
        let checks = observe_fleet(&cfg);
        assert_eq!(checks.len(), 1, "an undeclared ceiling is one cause, not one per partition");
        assert_eq!(checks[0].key, "fleet");
        assert!(matches!(checks[0].raw, RawStatus::Unobservable { .. }));
    }

    #[test]
    fn a_declared_ceiling_bounds_the_desired_live_count_per_partition() {
        let dir = scratch_dir("fleet-ceiling");
        let cfg = Config { seams: fleet_seams(&dir), max_live_dwarves: "3".into(), max_dwarves: "6".into(), ..test_config() };
        let checks = observe_fleet(&cfg);
        assert_eq!(checks.len(), 3);
        for c in &checks {
            assert!(
                matches!(&c.raw, RawStatus::Gap { desired, observed, .. } if desired == "3" && observed == "0"),
                "5 ready under a ceiling of 3 with 0 live desires 3: {}",
                c.key
            );
        }
    }

    #[test]
    fn a_paused_task_pool_satisfies_task_partitions_but_not_a_lane() {
        let dir = scratch_dir("fleet-pool-zero");
        let cfg = Config { seams: fleet_seams(&dir), max_live_dwarves: "3".into(), max_dwarves: "0".into(), ..test_config() };
        let by_key: std::collections::BTreeMap<String, RawStatus> = observe_fleet(&cfg).into_iter().map(|c| (c.key, c.raw)).collect();
        assert_eq!(by_key["fleet:a"], RawStatus::Satisfied);
        assert_eq!(by_key["fleet:b"], RawStatus::Satisfied);
        assert!(matches!(by_key["fleet:c"], RawStatus::Gap { .. }), "a lane draws outside the pool");
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
        let units_install = bin("units-install", "#!/bin/sh\necho spira-round-template-prod.timer\n");
        let spira_lc = bin("lc", "#!/bin/sh\necho '{\"bead\":{\"bead_id\":\"sp-xn3nou\",\"state\":\"LANDED\"}}'\n");
        let mail = bin("mail", &format!("#!/bin/sh\ncat >> {}\necho --- >> {}\n", dir.join("mail-body").display(), dir.join("mail-body").display()));
        let ctrl_path = dir.join("control");
        let mut data = spira_ctrl::CtrlData::new();
        spira_ctrl::suspend(&mut data, "spira-round-template", "fails every run", owner, "2026-10-07", "operator");
        if let Some(u) = until {
            spira_ctrl::set_until(&mut data, "spira-round-template", u);
        }
        spira_ctrl::write_atomic(&ctrl_path, &data).unwrap();
        let cfg = Config {
            seams: Seams { systemctl, units_install, spira_lc, mail, ..Seams::production() },
            ctrl_path,
            reminders_path: dir.join("reminders.json"),
            store: fixture_store(),
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
            evaluate(&cfg, &mut state, &mut Default::default(), &mut Default::default(), c);
        }
        let calls = fs::read_to_string(dir.join("systemctl-calls")).unwrap_or_default();
        assert!(!calls.contains("enable") && !calls.contains("restart"), "a remedy ran against a suspended unit: {calls}");
        let status = stored(&cfg, "reconciler-status");
        assert!(status.contains("\"status\":\"deliberate\""), "{status}");
        assert!(status.contains("fails every run"), "{status}");
    }

    fn flaky_systemctl_fixture(tag: &str, body: &str) -> (testkit::TempDir, Config) {
        let (dir, cfg) = suspension_fixture(tag, "sp-never", None);
        fs::remove_file(&cfg.ctrl_path).unwrap();
        let systemctl = dir.join("systemctl");
        testkit::write_exe(&systemctl, body);
        let seams = Seams { systemctl: systemctl.to_string_lossy().to_string(), ..cfg.seams.clone() };
        (dir, Config { seams, ..cfg })
    }

    fn run_pass(cfg: &Config, state: &mut StateMap, now_secs: u64) {
        let cfg = Config { now_secs, ..cfg.clone() };
        for c in observe_units(&cfg) {
            evaluate(&cfg, state, &mut Default::default(), &mut Default::default(), c);
        }
    }

    #[test]
    fn one_failed_systemctl_read_is_not_escalated_when_the_next_pass_reads_active() {
        let (dir, cfg) = flaky_systemctl_fixture(
            "flaky-once",
            "#!/bin/sh\nm=\"$(dirname \"$0\")/failed-once\"\nif [ ! -f \"$m\" ]; then touch \"$m\"; exit 1; fi\ncase \"$2\" in is-enabled) echo enabled;; is-active) echo active;; esac\n",
        );
        let mut state = StateMap::new();
        run_pass(&cfg, &mut state, 100);
        run_pass(&cfg, &mut state, 160);
        assert!(!dir.join("mail-body").exists(), "a single failed read must not be mailed: {:?}", fs::read_to_string(dir.join("mail-body")));
    }

    #[test]
    fn a_systemctl_read_that_keeps_failing_escalates_and_names_what_systemctl_said() {
        let (dir, cfg) = flaky_systemctl_fixture(
            "flaky-always",
            "#!/bin/sh\necho 'Failed to connect to bus: No medium found' >&2\nexit 1\n",
        );
        let mut state = StateMap::new();
        run_pass(&cfg, &mut state, 100);
        assert!(!dir.join("mail-body").exists(), "the first failed read is held back");
        run_pass(&cfg, &mut state, 160);
        let mail = fs::read_to_string(dir.join("mail-body")).expect("a persistent failure must escalate");
        assert!(mail.contains("exit status 1") && mail.contains("Failed to connect to bus"), "{mail}");
    }

    #[test]
    fn a_timed_out_systemctl_read_is_named_as_a_timeout() {
        let (_dir, cfg) = flaky_systemctl_fixture("flaky-timeout", "#!/bin/sh\nexit 124\n");
        let checks = observe_units(&cfg);
        match &checks[0].raw {
            RawStatus::Unobservable { reason } => assert!(reason.contains("timed out"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn positive_control_an_unsuspended_timer_is_remedied() {
        let (dir, cfg) = suspension_fixture("unsuspended-unit", "sp-never", None);
        fs::remove_file(&cfg.ctrl_path).unwrap();
        let mut state = StateMap::new();
        for c in observe_units(&cfg) {
            evaluate(&cfg, &mut state, &mut Default::default(), &mut Default::default(), c);
        }
        let calls = fs::read_to_string(dir.join("systemctl-calls")).unwrap();
        assert!(calls.contains("enable --now spira-round-template-prod.timer"), "{calls}");
    }

    #[test]
    fn replay_a_disabled_reconciler_timer_is_enabled_and_the_effect_is_read() {
        let (dir, cfg) = suspension_fixture("replay-disabled-timer", "sp-never", None);
        fs::remove_file(&cfg.ctrl_path).unwrap();
        let state_file = dir.join("timer-on");
        let systemctl = dir.join("systemctl");
        testkit::write_exe(
            &systemctl,
            &format!(
                "#!/bin/sh\necho \"$*\" >> {c}\ncase \"$2\" in enable) touch {s};; is-enabled) [ -f {s} ] && {{ echo enabled; exit 0; }}; echo disabled; exit 1;; is-active) [ -f {s} ] && {{ echo active; exit 0; }}; echo inactive; exit 3;; esac\n",
                c = dir.join("systemctl-calls").display(),
                s = state_file.display()
            ),
        );
        let units_install = dir.join("units-install");
        testkit::write_exe(&units_install, "#!/bin/sh\necho spira-reconciler-prod.timer\n");
        let cfg = Config {
            store: fixture_store(),
            effect_passes: 2,
            release: "r-fixture".into(),
            seams: Seams {
                systemctl: systemctl.to_string_lossy().to_string(),
                units_install: units_install.to_string_lossy().to_string(),
                ..cfg.seams.clone()
            },
            ..cfg
        };
        let mut state = StateMap::new();
        let mut pending = PendingMap::new();
        let mut alerted = AlertedSinceMap::new();

        for c in observe_units(&cfg) {
            assert!(matches!(c.raw, RawStatus::Gap { .. }), "the disabled timer must read as a gap: {:?}", c.raw);
            evaluate(&cfg, &mut state, &mut pending, &mut alerted, c);
        }
        let calls = fs::read_to_string(dir.join("systemctl-calls")).unwrap();
        assert!(calls.contains("enable --now spira-reconciler-prod.timer"), "{calls}");
        let log = stored(&cfg, "reconciler-remedy");
        assert!(log.contains("\"event\":\"remedy\"") && log.contains("\"before\":1"), "{log}");

        let cfg2 = Config { now_secs: cfg.now_secs + 60, ..cfg.clone() };
        for c in observe_units(&cfg2) {
            assert_eq!(c.raw, RawStatus::Satisfied);
            evaluate(&cfg2, &mut state, &mut pending, &mut alerted, c);
        }
        let log = stored(&cfg, "reconciler-remedy");
        assert!(log.contains("\"event\":\"effect\"") && log.contains("\"after\":0"), "{log}");
        assert!(pending.is_empty());
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
        let cfg = Config { seams: Seams { spira_lc: "/bin/false".into(), ..cfg.seams.clone() }, ..cfg };
        remind_due_suspensions(&cfg, &load_ctrl(&cfg));
        assert!(!dir.join("mail-body").exists(), "reminded before until");
        let later = Config { now_iso: "2026-10-09T00:00:00Z".into(), ..cfg.clone() };
        remind_due_suspensions(&later, &load_ctrl(&later));
        assert!(fs::read_to_string(dir.join("mail-body")).unwrap().contains("2026-10-09"));
    }

    #[test]
    fn a_failed_send_is_retried_not_recorded() {
        let (dir, cfg) = suspension_fixture("reminder-retry", "sp-xn3nou", None);
        let broken = Config { seams: Seams { mail: "/bin/false".into(), ..cfg.seams.clone() }, ..cfg.clone() };
        remind_due_suspensions(&broken, &load_ctrl(&broken));
        remind_due_suspensions(&cfg, &load_ctrl(&cfg));
        assert!(dir.join("mail-body").exists());
    }

    #[test]
    fn a_composed_reminder_passes_the_lint_mail_send_applies() {
        let kinds = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/mail/kinds");
        let lint = |body: &str| {
            ::mail::lint::lint_check(
                &::mail::lint::LintInput {
                    from: reconciler_engine::mail::FROM,
                    subject: "reconciler: suspension of spira-cert-sweep-sample is due for review",
                    kind: "note",
                    default: "",
                    urgent: false,
                    body,
                    digest: false,
                },
                &kinds,
                "sp",
            )
        };
        let old = "The suspension is due.\n\nreason  faults\nowner   sp-f2nkn6\n";
        assert!(lint(old).is_err(), "the lint no longer rejects the old body shape");
        let body = reminder_body("spira-cert-sweep-sample", "its owner bead sp-f2nkn6 has landed", "faults when a release changes the image tag", "sp-f2nkn6");
        assert_eq!(lint(&body), Ok(()), "{body}");
    }

    #[test]
    fn a_repeated_send_failure_is_logged_once() {
        let (dir, cfg) = suspension_fixture("reminder-failure-once", "sp-xn3nou", None);
        let log = dir.join("recon.log");
        let broken = Config { seams: Seams { mail: "/bin/false".into(), ..cfg.seams.clone() }, log: log.clone(), ..cfg.clone() };
        for _ in 0..3 {
            remind_due_suspensions(&broken, &load_ctrl(&broken));
        }
        let text = fs::read_to_string(&log).unwrap_or_default();
        assert_eq!(text.matches("not sent").count(), 1, "{text}");
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

    fn main_health_repo(name: &str) -> (testkit::TempDir, std::path::PathBuf) {
        let dir = scratch_dir(name);
        let repo = dir.join("repo");
        let git = |args: &[&str]| {
            let st = std::process::Command::new("git").arg("-C").arg(&repo).args(args).output().unwrap();
            assert!(st.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&st.stderr));
        };
        fs::create_dir_all(&repo).unwrap();
        git(&["init", "-q", "-b", "main"]);
        fs::write(repo.join("f"), "x").unwrap();
        git(&["add", "f"]);
        git(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "base"]);
        git(&["update-ref", "refs/heads/local/main", "HEAD"]);
        let map = dir.join("repos");
        fs::write(&map, format!("spira|{}\n", repo.display())).unwrap();
        (dir, map)
    }

    fn stub(dir: &std::path::Path, name: &str, body: &str) -> String {
        let p = dir.join(name);
        testkit::write_exe(&p, &format!("#!/usr/bin/env bash\n{}\n", body));
        p.to_string_lossy().to_string()
    }

    fn main_health_cfg(dir: &std::path::Path, map: std::path::PathBuf, lint: String, guard: String, deadline: u64) -> Config {
        Config {
            spira_run: dir.to_path_buf(),
            repo_map: Some(map),
            fence_deadline_secs: deadline,
            seams: Seams { lint, guard, ..Seams::production() },
            ..test_config()
        }
    }

    fn main_health_by_key(cfg: &Config) -> std::collections::BTreeMap<String, RawStatus> {
        observe_main_health(cfg).into_iter().map(|c| (c.key, c.raw)).collect()
    }

    #[test]
    fn a_slow_guard_within_the_fence_deadline_is_satisfied_and_cached() {
        let (dir, map) = main_health_repo("main-health-slow");
        let cfg = main_health_cfg(&dir, map, stub(&dir, "green", "exit 0"), stub(&dir, "slow", "sleep 7; exit 0"), 15);
        let by_key = main_health_by_key(&cfg);
        assert_eq!(by_key["main-health:lifecycle-guard"], RawStatus::Satisfied);
        let cache = fs::read_to_string(dir.join("main-health-verdicts.json")).unwrap();
        assert!(cache.contains("lifecycle-guard"), "{}", cache);
    }

    #[test]
    fn a_guard_killed_at_the_fence_deadline_is_unobservable_naming_the_deadline_the_elapsed_time_and_the_log() {
        let (dir, map) = main_health_repo("main-health-hang");
        let cfg = main_health_cfg(&dir, map, stub(&dir, "green", "exit 0"), stub(&dir, "hang", "echo partial; sleep 6; exit 0"), 2);
        let by_key = main_health_by_key(&cfg);
        let RawStatus::Unobservable { reason } = &by_key["main-health:lifecycle-guard"] else {
            panic!("{:?}", by_key["main-health:lifecycle-guard"]);
        };
        assert!(reason.contains("2s"), "{}", reason);
        assert!(reason.contains("SPIRA_MAIN_HEALTH_FENCE_DEADLINE_SECS"), "{}", reason);
        let log = dir.join("main-health-lifecycle-guard.log");
        assert!(reason.contains(&log.display().to_string()), "{}", reason);
        let after = reason.split("after ").nth(1).expect("an elapsed figure");
        let elapsed: f64 = after.split('s').next().unwrap().parse().expect("elapsed parses");
        assert!(elapsed >= 2.0, "{}", reason);
        assert!(fs::read_to_string(&log).unwrap().contains("partial"));
        let cache = fs::read_to_string(dir.join("main-health-verdicts.json")).unwrap();
        assert!(!cache.contains("lifecycle-guard"), "{}", cache);
    }

    #[test]
    fn a_guard_that_dies_on_a_signal_or_exits_oddly_is_unobservable_with_its_output_in_the_log_and_uncached() {
        for (name, body, says) in [("odd", "echo oddout; exit 3", "exit 3"), ("sig", "echo sigout; kill -9 $$", "exit 137")] {
            let (dir, map) = main_health_repo(&format!("main-health-{}", name));
            let cfg = main_health_cfg(&dir, map, stub(&dir, "green", "exit 0"), stub(&dir, name, body), 15);
            let by_key = main_health_by_key(&cfg);
            let RawStatus::Unobservable { reason } = &by_key["main-health:lifecycle-guard"] else {
                panic!("{:?}", by_key["main-health:lifecycle-guard"]);
            };
            let log = dir.join("main-health-lifecycle-guard.log");
            assert!(reason.contains(says) || reason.contains("signal"), "{}", reason);
            assert!(reason.contains(&log.display().to_string()), "{}", reason);
            assert!(fs::read_to_string(&log).unwrap().contains(&format!("{}out", &name[..3])));
            assert!(!fs::read_to_string(dir.join("main-health-verdicts.json")).unwrap().contains("lifecycle-guard"));
        }
    }

    #[test]
    fn a_fence_already_judged_for_the_sha_is_not_run_again_while_another_is_unobservable() {
        let (dir, map) = main_health_repo("main-health-reuse");
        let first = main_health_cfg(&dir, map.clone(), stub(&dir, "green", "exit 0"), stub(&dir, "hang", "sleep 6"), 1);
        assert_eq!(main_health_by_key(&first)["main-health:lint"], RawStatus::Satisfied);
        let ran = dir.join("ran");
        let tripwire = stub(&dir, "tripwire", &format!("touch {}; exit 0", ran.display()));
        let second = main_health_cfg(&dir, map, tripwire, stub(&dir, "green2", "exit 0"), 15);
        let by_key = main_health_by_key(&second);
        assert!(!ran.exists(), "lint was judged for this sha and must not run again");
        assert_eq!(by_key["main-health:lint"], RawStatus::Satisfied);
        assert_eq!(by_key["main-health:lifecycle-guard"], RawStatus::Satisfied);
    }

    #[test]
    fn a_red_base_fence_is_a_gap_whose_output_lands_in_a_file_and_a_green_one_is_satisfied() {
        let (dir, map) = main_health_repo("main-health");
        let red = stub(&dir, "red", "echo planted violation\nexit 1");
        let green = stub(&dir, "green", "exit 0");
        let cfg = Config {
            spira_run: dir.to_path_buf(),
            repo_map: Some(map),
            seams: Seams { lint: green, guard: red, ..Seams::production() },
            ..test_config()
        };
        let by_key = main_health_by_key(&cfg);
        assert_eq!(by_key["main-health:lint"], RawStatus::Satisfied);
        assert!(matches!(&by_key["main-health:lifecycle-guard"], RawStatus::Gap { observed, .. } if observed.contains("main-health-lifecycle-guard.log")));
        assert!(fs::read_to_string(dir.join("main-health-lifecycle-guard.log")).unwrap().contains("planted violation"));
        assert!(!dir.join("main-health-tree").exists(), "the scratch checkout is removed");

        let ran = dir.join("ran");
        let tripwire = dir.join("tripwire");
        testkit::write_exe(&tripwire, &format!("#!/usr/bin/env bash\ntouch {}\nexit 0\n", ran.display()));
        let again = Config {
            seams: Seams {
                lint: tripwire.to_string_lossy().to_string(),
                guard: tripwire.to_string_lossy().to_string(),
                ..cfg.seams.clone()
            },
            ..cfg
        };
        let second: std::collections::BTreeMap<String, RawStatus> =
            observe_main_health(&again).into_iter().map(|c| (c.key, c.raw)).collect();
        assert_eq!(second, by_key, "an unchanged landing ref keeps its verdicts");
        assert!(!ran.exists(), "no fence re-runs on a sha already judged");
    }

    fn activation_fixture(name: &str) -> (testkit::TempDir, Config, String, String) {
        let dir = scratch_dir(name);
        let repo = dir.join("repo");
        let git = |args: &[&str]| {
            let o = std::process::Command::new("git").env("GIT_COMMITTER_DATE", "@1000 +0000").arg("-C").arg(&repo).args(args).output().unwrap();
            assert!(o.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        fs::create_dir_all(&repo).unwrap();
        git(&["init", "-q", "-b", "main"]);
        fs::write(repo.join("f"), "x").unwrap();
        git(&["add", "f"]);
        let commit = |msg: &str| git(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", msg]);
        commit("release");
        let release = git(&["rev-parse", "HEAD"]);
        commit("ahead");
        let main = git(&["rev-parse", "HEAD"]);
        git(&["update-ref", "refs/heads/local/main", "HEAD"]);
        let map = dir.join("repos");
        fs::write(&map, format!("spira|{}\n", repo.display())).unwrap();
        let red = dir.join("red");
        testkit::write_exe(&red, "#!/usr/bin/env bash\necho planted lint finding\nexit 1\n");
        let green = dir.join("green");
        testkit::write_exe(&green, "#!/usr/bin/env bash\nexit 0\n");
        let cfg = Config {
            spira_run: dir.to_path_buf(),
            repo_map: Some(map),
            release: release.clone(),
            activation_grace_secs: 600,
            seams: Seams { lint: red.to_string_lossy().to_string(), guard: green.to_string_lossy().to_string(), ..Seams::production() },
            ..test_config()
        };
        (dir, cfg, release, main)
    }

    fn lint_of(cfg: &Config) -> RawStatus {
        observe_main_health(cfg).into_iter().find(|c| c.key == "main-health:lint").expect("lint check").raw
    }

    #[test]
    fn main_ahead_of_the_release_within_grace_is_pending_not_red() {
        let (_dir, cfg, release, main) = activation_fixture("activation-within");
        let cfg = Config { now_secs: 1000 + 599, ..cfg };
        match lint_of(&cfg) {
            RawStatus::Deliberate { reason } => assert!(reason.contains(&release) && reason.contains(&main), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn main_ahead_of_the_release_past_grace_is_red_naming_both_shas() {
        let (_dir, cfg, release, main) = activation_fixture("activation-past");
        let cfg = Config { now_secs: 1000 + 601, ..cfg };
        match lint_of(&cfg) {
            RawStatus::Gap { observed, .. } => {
                assert!(observed.contains(&release) && observed.contains(&main) && observed.contains("601s"), "{observed}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_built_but_unactivated_release_containing_main_is_still_pending() {
        let (dir, cfg, release, main) = activation_fixture("activation-unflipped");
        let releases = dir.join("releases");
        fs::create_dir_all(releases.join(&release)).unwrap();
        fs::create_dir_all(releases.join(&main)).unwrap();
        std::os::unix::fs::symlink(releases.join(&release), releases.join("current")).unwrap();
        let cfg = Config { release: main.clone(), releases_dir: releases.clone(), now_secs: 1000 + 599, ..cfg };
        assert!(matches!(lint_of(&cfg), RawStatus::Deliberate { .. }));
        fs::remove_file(releases.join("current")).unwrap();
        std::os::unix::fs::symlink(releases.join(&main), releases.join("current")).unwrap();
        assert!(matches!(lint_of(&cfg), RawStatus::Gap { observed, .. } if observed.contains("lint red at")));
    }

    #[test]
    fn a_release_containing_main_keeps_the_lint_verdict() {
        let (_dir, cfg, _release, main) = activation_fixture("activation-contained");
        let cfg = Config { release: main, now_secs: 1000 + 100_000, ..cfg };
        assert!(matches!(lint_of(&cfg), RawStatus::Gap { observed, .. } if observed.contains("lint red at")));
    }

    fn stub_mail_seams(mail: &Path) -> Seams {
        Seams { mail: mail.to_string_lossy().to_string(), ..Seams::production() }
    }

    fn test_config() -> Config {
        Config {
            seams: Seams::production(),
            spira_run: PathBuf::from("/tmp"),
            spira_home: String::new(),
            log: PathBuf::from("/dev/null"),
            state_path: PathBuf::from("/dev/null"),
            alerted_path: PathBuf::from("/dev/null"),
            store: std::sync::Arc::new(tsd::store::Pusher::new(None)),
            effect_state: PathBuf::from("/dev/null"),
            mail_root: PathBuf::new(),
            release: "r-test".into(),
            shadow_kinds: Vec::new(),
            effect_passes: 2,
            lock_path: PathBuf::from("/dev/null"),
            spira_db: String::new(),
            bd_bin: String::new(),
            queue_dir: PathBuf::from("/dev/null"),
            repo_map: None,
            releases_dir: PathBuf::new(),
            cockpit_sessions: vec!["brain".into(), "hunk".into(), "chat".into()],
            cockpit_mail: String::new(),
            dolt_data: String::new(),
            max_live_dwarves: String::new(),
            max_dwarves: String::new(),
            disk_floor_pct: 15,
            grace_secs: 300,
            activation_grace_secs: 1800,
            fence_deadline_secs: 120,
            preflight_wall_secs: 240,
            now_secs: 0,
            now_iso: "2026-09-25T00:00:00Z".into(),
            desired_dir: PathBuf::from("/dev/null"),
            desired: DesiredState::default(),
            ctrl_path: PathBuf::from("/dev/null"),
            instance: "prod".into(),
            reminders_path: PathBuf::from("/dev/null"),
        }
    }

    #[test]
    fn parse_df_free_pct_reads_the_last_rows_total_and_available() {
        let out = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/sda1 1000 800 200 80% /\n";
        assert_eq!(parse_df_free_pct(out), Some(20));
    }

    #[test]
    fn parse_df_free_pct_refuses_what_it_cannot_read() {
        assert_eq!(parse_df_free_pct(""), None);
        assert_eq!(parse_df_free_pct("Filesystem 1024-blocks\n/dev/x 0 0 0 0% /\n"), None);
        assert_eq!(parse_df_free_pct("df: no such file\n"), None);
    }

    #[test]
    fn a_sequence_remedy_describes_and_runs_every_step() {
        let r = Remedy::Sequence(vec![("a".into(), vec!["1".into()]), ("b".into(), vec![])]);
        assert_eq!(r.describe(), "a 1 ; b ");
        assert_eq!(r.steps().len(), 2);
    }
}
