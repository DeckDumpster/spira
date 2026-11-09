// reconciler: structural-invariant control loop — observe, diff against the declared
// desired state, act to close the gap with a deterministic remedy where one exists, else
// escalate to the Concierge. Shaped like a Kubernetes controller (design:
// wiki/projects/spira/designs/reconciler-2026-09-25.md).
//
// reconciler --pass, and the `reconciler` handler of spira-runtime, which runs the same
// observers against in-process reads (`Host`) and decides with the same `decide`.
//
// The pure diff-plus-hysteresis logic (grace periods, unobservable, remedy verification)
// lives in reconciler-engine and is shared with czar-pass; this crate owns observation
// (units, tmux, git, the queue) and the deterministic remedies for the resources czar-pass
// does not already watch: Fleet, Units, Store, Cockpit, Production/Release, Disk, and two
// Queue checks czar-pass's own detectors do not cover (the base gate itself is base-red,
// already watched there — reconciling it a second time here would file a second incident
// for the same fault).

use reconciler_engine::alert::{compose_alert, should_alert};
use reconciler_engine::core::{last_remedy, record_remedy, step, HysteresisState, RawStatus, Verdict};
use reconciler_engine::io::{load_alerted, load_state, push_status, save_alerted, save_state, status_record, AlertedSinceMap, StateMap, StatusRecord};
use reconciler_engine::{clock, mail, paths};
use reconciler_engine::effect::{
    append_event, compose_no_effect, is_shadowed, kind_of, load_pending, measure, save_pending, Outcome,
    PendingEffect, PendingMap, RemedyEvent,
};
use reconciler_engine::holds::{self, Outcome as HoldOutcome};
use spira_desired_state::compose::Composite;
use spira_desired_state::resource::{parse_resource, CockpitSpec, DiskSpec, FleetSpec, KindSpec, ReleaseSpec, UnitsSpec};
use spira_desired_state::store::FsStore;
pub mod hold_sweep;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use spira_config::lc_call::LcClient;
use std::time::{SystemTime, UNIX_EPOCH};

// ──────────────────────────────────────────────────────────────────────────────
// Desired state: the typed Composite this host last materialised via `spira
// apply`/`spira compose` (spira-desired-state) is the primary source for Fleet, Units,
// Cockpit and Release. A kind absent from the Composite — including "no Composite at all",
// the state of every install that has not yet adopted it — falls back to the bash/env
// default that kind always had, per resource rather than for the whole pass.
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Default, Clone)]
pub struct DesiredState {
    pub fleet: Option<FleetSpec>,
    pub units: Option<UnitsSpec>,
    pub cockpit: Option<CockpitSpec>,
    pub release: Option<ReleaseSpec>,
    pub disk: Option<DiskSpec>,
}

impl DesiredState {
    pub fn load(cfg_log: impl Fn(&str), dir: &Path) -> DesiredState {
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

// ──────────────────────────────────────────────────────────────────────────────
// Configuration — every program is a field of one injected Seams struct; every path is
// derived from a registered key.
// ──────────────────────────────────────────────────────────────────────────────

/// The programs a pass runs that the system, not the release, provides.
const SYSTEM_PROGRAMS: [&str; 4] = ["systemctl", "tmux", "git", "podman"];

/// Every program the pass runs, by name on the launcher's PATH, and the one unit it will
/// never remedy. A test injects its own; nothing here is read from the environment.
#[derive(Clone)]
pub struct Seams {
    pub systemctl: String,
    pub tmux: String,
    pub git: String,
    pub forge: String,
    pub units_install: String,
    pub spira_config: String,
    pub spira_claim: String,
    pub strand: String,
    pub queue_helpers: String,
    pub queue: String,
    pub spira_lc: String,
    pub bead: String,
    pub target_reap: String,
    pub podman: String,
    pub rebuild: String,
    pub layout: String,
    pub store_unit: String,
    pub lint: String,
    pub guard: String,
}

impl Seams {
    /// Every release tool from `$SPIRA_RELEASE`, or the refusal naming it.
    pub fn production() -> Result<Seams, String> {
        Seams::from_release(std::env::var(spira_config::RELEASE_ENV).ok().as_deref())
    }

    /// System programs (and the store's unit) by name on the launcher's PATH; every
    /// release-shipped tool from `release`, or the refusal naming `SPIRA_RELEASE`.
    pub fn from_release(release: Option<&str>) -> Result<Seams, String> {
        Seams::resolving(|n| if SYSTEM_PROGRAMS.contains(&n) || n.ends_with(".service") { Ok(n.to_string()) } else { spira_config::release_env::resolve_tool(n, release) })
    }

    /// Every program through `name`.
    pub fn resolving(name: impl Fn(&str) -> Result<String, String>) -> Result<Seams, String> {
        Ok(Seams {
            // FORK-EDGE: needs Units unit-file state (is-enabled) and enable; remedies run these as commands
            systemctl: name("systemctl")?,
            tmux: name("tmux")?,
            git: name("git")?,
            forge: name("forge")?,
            units_install: name("units-install")?,
            spira_config: name("spira-config")?,
            spira_claim: name("spira-claim")?,
            strand: name("strand")?,
            queue_helpers: name("queue-helpers")?,
            // FORK-EDGE: remedy program (queue lock clear is a declared remedy command)
            queue: name("queue")?,
            // FORK-EDGE: fixture stand-in — LcClient::for_bin runs it only for a declared SPIRA_LC_BIN or direct mode; production calls go over the socket (Config::lc)
            spira_lc: name("spira-lc")?,
            bead: name("bead")?,
            target_reap: name("target-reap")?,
            podman: name("podman")?,
            rebuild: name("rebuild")?,
            layout: name("layout")?,
            store_unit: name("dolt-beads.service")?,
            lint: name("spira-lint")?,
            guard: name("lifecycle-guard")?,
        })
    }

    /// Every program by its bare name, for a [`Config`] read only for its rules and never run,
    /// and for a test that injects the seams it exercises.
    pub fn bare() -> Seams {
        Seams::resolving(|n| Ok(n.to_string())).expect("bare names always resolve")
    }
}

#[derive(Clone)]
pub struct Config {
    pub seams: Seams,
    pub spira_run: PathBuf,
    pub spira_home: String,
    pub log: PathBuf,
    pub state_path: PathBuf,
    pub alerted_path: PathBuf,
    pub store: std::sync::Arc<tsd::store::Pusher>,
    pub effect_state: PathBuf,
    pub mail_root: PathBuf,
    pub release: String,
    pub shadow_kinds: Vec<String>,
    pub effect_passes: u32,
    pub lock_path: PathBuf,
    pub queue_dir: PathBuf,
    pub repo_map: Option<PathBuf>,
    pub releases_dir: PathBuf,
    pub cockpit_sessions: Vec<String>,
    pub cockpit_mail: String,
    pub dolt_data: String,
    pub max_live_dwarves: String,
    pub lanes_max_live: String,
    pub max_dwarves: String,
    pub disk_floor_pct: u32,
    pub untagged_images_max: u64,
    pub grace_secs: u64,
    pub activation_grace_secs: u64,
    pub fence_deadline_secs: u64,
    pub preflight_wall_secs: u64,
    pub now_secs: u64,
    pub now_iso: String,
    pub desired_dir: PathBuf,
    pub desired: DesiredState,
    pub ctrl_path: PathBuf,
    pub instance: String,
    pub reminders_path: PathBuf,
    pub breaker: Result<(u32, u32), String>,
    /// Reads with an in-process form; `None` launches the programs `seams` names.
    pub host: Option<Arc<dyn Host>>,
    /// The lifecycle store's client for a pass with no host; `None` resolves the configured one
    /// at the call.
    pub lc_client: Option<LcClient>,
    /// Units the Units invariant does not judge: a unit whose liveness something else owns.
    pub skip_units: Vec<String>,
    /// Answers to git questions, keyed by the shas that decide them, so a ref that did not move is not asked about twice.
    pub memo: Arc<std::sync::Mutex<std::collections::BTreeMap<String, String>>>,
}

pub const MAIN_REF: &str = "local/main";

/// The release the running harness copy is: the directory name above its `spira/` dir, or
/// `unknown` when the home cannot be located.
pub fn release_of(home: &str) -> String {
    release_name(Path::new(home))
}

fn release_name(home: &Path) -> String {
    home.parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// One message to a mailbox, as `mail send` takes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mail {
    pub mailbox: String,
    pub subject: String,
    pub kind: String,
    pub default: Option<String>,
    pub class: Option<String>,
    pub bead: Option<String>,
    pub body: String,
}

/// The reads and the one write an observer or the hold sweep makes that have an in-process
/// form. A `Config` with no host launches the programs its `Seams` name.
pub trait Host: Send + Sync {
    /// `is-enabled` or `is-active` of a user unit: one of systemctl's state words.
    fn unit_state(&self, verb: &str, unit: &str) -> Result<String, String>;
    /// The installed names of every unit the installer enables, one per line.
    fn list_enable(&self) -> String;
    fn free_pct(&self, path: &str) -> Option<i64>;
    /// One spira-lc verb; `Err` on a refusal or a store that does not answer.
    fn lc(&self, argv: &[&str]) -> Result<String, String>;
    /// `<bead> <tip> <epoch>` per CERTIFIED bead with a `spira/<bead>` branch in `repo`.
    fn certified_list(&self, repo: &Path) -> String;
    /// `rune task|lane|partitions|for-labels ...` and `repo landref <name>`.
    fn config(&self, argv: &[&str]) -> String;
    fn stranded_runners(&self, repo: &Path) -> String;
    fn send_mail(&self, mail: &Mail) -> Result<(), String>;
    /// What a ref names in `repo`, read without running git.
    fn ref_sha(&self, _repo: &Path, _name: &str) -> Ref {
        Ref::Unknown
    }
    /// The branch HEAD names in the checkout at `path` (`HEAD` when detached); `None` when unreadable.
    fn head_branch(&self, _path: &Path) -> Option<String> {
        None
    }
    /// Live dwarves of `rune`; `None` when this host cannot count them without `strand`.
    fn dwarf_count(&self, _rune: &str) -> Option<i64> {
        None
    }
    /// Claimable beads under a partition; `None` when this host cannot count them without `spira-claim`.
    fn ready_count(&self, _labels: &str, _exclude: &str) -> Option<i64> {
        None
    }
    /// The tmux server: `Some(Some(identity))` when one answers, `Some(None)` when it is certainly down, `None` when only tmux can say.
    fn tmux_server(&self) -> Option<Option<String>> {
        None
    }
}

/// What reading a ref without git came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ref {
    Sha(String),
    /// No loose or packed ref by any spelling git would try.
    Absent,
    /// A loose ref naming an object that may be an annotated tag: peel it before trusting it.
    Object(String),
    /// Not tellable this way (a revision expression, an unreadable layout): ask git.
    Unknown,
}

/// Answers to git questions held at most this many; past it they are dropped and asked again.
const MEMO_MAX: usize = 2048;

impl Config {
    /// Every registered key `from_lookup` reads.
    pub const KEYS: &'static [&'static str] = &[
        "COCKPIT_SESSIONS",
        "COCKPIT_MAIL",
        "SPIRA_METRICS_ENDPOINT",
        "SPIRA_MAIL",
        "SPIRA_RECONCILER_SHADOW_KINDS",
        "SPIRA_RECONCILER_EFFECT_PASSES",
        "SPIRA_QUEUE_DIR",
        "SPIRA_REPO_MAP",
        "SPIRA_RELEASES",
        "SPIRA_DOLT_DATA",
        "SPIRA_MAX_LIVE_DWARVES",
        "SPIRA_LANES_MAX_LIVE",
        "SPIRA_MAX_DWARVES",
        "SPIRA_DISK_FLOOR_PCT",
        "SPIRA_UNTAGGED_IMAGES_MAX",
        "SPIRA_RECONCILER_GRACE_SECS",
        "SPIRA_MAIN_HEALTH_ACTIVATION_GRACE_SECS",
        "SPIRA_MAIN_HEALTH_FENCE_DEADLINE_SECS",
        "SPIRA_PREFLIGHT_WALL_SECS",
        "SPIRA_CTRL",
        "SPIRA_INSTANCE",
        "SPIRA_DESIRED_DIR",
        "SPIRA_NO_PROGRESS_BREAKER_CLAIMS",
        "SPIRA_NO_PROGRESS_BREAKER_WINDOW_MINUTES",
    ];

    pub fn from_process(seams: Seams) -> Result<Config, String> {
        // SPIRA_RUN is resolved through the run-dir resolver, not a bare `cfg("SPIRA_RUN")`;
        // a config that fails to resolve is a named refusal, never a literal `/tmp/spira`.
        let spira_run = spira_config::resolve::run_dir_for_process()?;
        let spira_home = spira_config::resolve::locate_home_for_process()?.to_string_lossy().into_owned();
        let (now_secs, now_iso) = (clock::now_secs(), clock::now_iso());
        Config::from_lookup(&spira_config::process::cfg, seams, spira_run, spira_home, now_secs, now_iso)
    }

    /// `get` supplies every key in [`Config::KEYS`]; where the harness is and when this is are
    /// facts the caller knows, not keys.
    pub fn from_lookup(get: &dyn Fn(&str) -> Result<String, String>, seams: Seams, spira_run: PathBuf, spira_home: String, now_secs: u64, now_iso: String) -> Result<Config, String> {
        fn parsed<T: std::str::FromStr>(get: &dyn Fn(&str) -> Result<String, String>, key: &str) -> Result<T, String> {
            let raw = get(key)?;
            raw.trim().parse::<T>().map_err(|_| format!("{key} is not a valid value: {raw:?}"))
        }
        let sessions = get("COCKPIT_SESSIONS")?.split_whitespace().map(String::from).collect();
        let breaker = parsed::<u32>(get, "SPIRA_NO_PROGRESS_BREAKER_CLAIMS").and_then(|c| Ok((c, parsed::<u32>(get, "SPIRA_NO_PROGRESS_BREAKER_WINDOW_MINUTES")?)));
        let metrics = get("SPIRA_METRICS_ENDPOINT").ok().filter(|e| !e.trim().is_empty());
        Ok(Config {
            log: spira_run.join("reconciler.log"),
            state_path: paths::state(&spira_run),
            alerted_path: paths::alerted(&spira_run),
            store: Arc::new(tsd::store::Pusher::new(metrics)),
            effect_state: paths::effect_state(&spira_run),
            mail_root: PathBuf::from(get("SPIRA_MAIL")?),
            release: release_name(Path::new(&spira_home)),
            shadow_kinds: get("SPIRA_RECONCILER_SHADOW_KINDS")?.split_whitespace().map(String::from).collect(),
            effect_passes: match parsed::<u32>(get, "SPIRA_RECONCILER_EFFECT_PASSES")? {
                0 => 2,
                n => n,
            },
            lock_path: spira_run.join("reconciler.lock"),
            queue_dir: PathBuf::from(get("SPIRA_QUEUE_DIR")?),
            repo_map: Some(PathBuf::from(get("SPIRA_REPO_MAP")?)),
            releases_dir: PathBuf::from(get("SPIRA_RELEASES")?),
            cockpit_sessions: sessions,
            cockpit_mail: get("COCKPIT_MAIL")?,
            dolt_data: get("SPIRA_DOLT_DATA")?,
            max_live_dwarves: get("SPIRA_MAX_LIVE_DWARVES")?,
            lanes_max_live: get("SPIRA_LANES_MAX_LIVE")?,
            max_dwarves: get("SPIRA_MAX_DWARVES")?,
            disk_floor_pct: parsed::<u32>(get, "SPIRA_DISK_FLOOR_PCT")?,
            untagged_images_max: parsed::<u64>(get, "SPIRA_UNTAGGED_IMAGES_MAX")?,
            grace_secs: parsed::<u64>(get, "SPIRA_RECONCILER_GRACE_SECS")?,
            activation_grace_secs: parsed::<u64>(get, "SPIRA_MAIN_HEALTH_ACTIVATION_GRACE_SECS")?,
            fence_deadline_secs: parsed::<u64>(get, "SPIRA_MAIN_HEALTH_FENCE_DEADLINE_SECS")?,
            preflight_wall_secs: parsed::<u64>(get, "SPIRA_PREFLIGHT_WALL_SECS")?,
            now_secs,
            now_iso,
            desired_dir: PathBuf::from(get("SPIRA_DESIRED_DIR")?),
            desired: DesiredState::default(),
            ctrl_path: PathBuf::from(get("SPIRA_CTRL")?),
            instance: get("SPIRA_INSTANCE")?,
            reminders_path: paths::reminders(&spira_run),
            breaker,
            host: None,
            lc_client: None,
            skip_units: Vec::new(),
            memo: Arc::default(),
            seams,
            spira_run,
            spira_home,
        })
    }

    pub fn with_desired_state(mut self) -> Config {
        self.desired = DesiredState::load(|msg| eprintln!("{}", msg), &self.desired_dir);
        self
    }

    pub fn with_memo(mut self, memo: Arc<std::sync::Mutex<std::collections::BTreeMap<String, String>>>) -> Config {
        self.memo = memo;
        self
    }

    pub fn with_host(mut self, host: Arc<dyn Host>) -> Config {
        self.host = Some(host);
        self
    }

    /// What `decide` needs of this pass, without the reads.
    pub fn rules(&self) -> Rules {
        Rules { now_secs: self.now_secs, grace_secs: self.grace_secs, effect_passes: self.effect_passes, shadow_kinds: self.shadow_kinds.clone() }
    }

    fn unit_state(&self, verb: &str, unit: &str) -> Result<String, String> {
        match &self.host {
            Some(h) => h.unit_state(verb, unit),
            None => systemctl_state(&self.seams.systemctl, verb, unit),
        }
    }

    fn list_enable(&self) -> String {
        match &self.host {
            Some(h) => h.list_enable(),
            None => run_cmd(&self.seams.units_install, &["--list-enable"]),
        }
    }

    fn free_pct(&self, path: &str) -> Option<i64> {
        match &self.host {
            Some(h) => h.free_pct(path),
            None => free_pct(path),
        }
    }

    pub fn lc(&self, argv: &[&str]) -> Result<String, String> {
        match &self.host {
            Some(h) => h.lc(argv),
            None => {
                let bin = &self.seams.spira_lc;
                if !lc_served(argv) {
                    return Err(format!("{bin} {}: refused: the store does not serve this verb (it runs as its caller)", argv.join(" ")));
                }
                self.lc_client()?.answer(argv).map_err(|e| format!("{bin} {}: {e}", argv.join(" ")))
            }
        }
    }

    fn lc_client(&self) -> Result<LcClient, String> {
        match &self.lc_client {
            Some(c) => Ok(c.clone()),
            None => LcClient::for_bin(&self.seams.spira_lc),
        }
    }

    fn ref_read(&self, repo: &Path, name: &str) -> Ref {
        match self.host.as_ref().map_or(Ref::Unknown, |h| h.ref_sha(repo, name)) {
            Ref::Object(object) => {
                let peeled = self.remembered(Some(format!("peel:{}:{object}", repo.display())), || {
                    Some(run_cmd(&self.seams.git, &["-C", &repo.to_string_lossy(), "rev-parse", "--verify", "--quiet", &format!("{object}^{{commit}}")]).trim().to_string()).filter(|sha| !sha.is_empty())
                });
                match peeled {
                    Some(sha) if !sha.is_empty() => Ref::Sha(sha),
                    _ => Ref::Unknown,
                }
            }
            other => other,
        }
    }

    fn dwarf_count(&self, rune: &str) -> i64 {
        match self.host.as_ref().and_then(|h| h.dwarf_count(rune)) {
            Some(n) => n,
            None => count_of(&run_cmd(&self.seams.strand, &["dwarf-count", rune])),
        }
    }

    fn ready_count(&self, labels: &str, exclude: &str) -> i64 {
        match self.host.as_ref().and_then(|h| h.ready_count(labels, exclude)) {
            Some(n) => n,
            None => count_of(&run_cmd(&self.seams.spira_claim, &["ready-count", labels, exclude])),
        }
    }

    fn ref_sha(&self, repo: &Path, name: &str) -> Option<String> {
        match self.ref_read(repo, name) {
            Ref::Sha(s) => Some(s),
            _ => None,
        }
    }

    /// The answer to a git question, asked of git only when no answer is held for `key` (the shas
    /// that decide it). Without a key every question is asked.
    fn remembered(&self, key: Option<String>, ask: impl FnOnce() -> Option<String>) -> Option<String> {
        let Some(key) = key else { return ask() };
        if let Some(held) = self.memo.lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
            return Some(held.clone());
        }
        let answer = ask()?;
        let mut memo = self.memo.lock().unwrap_or_else(|p| p.into_inner());
        if memo.len() >= MEMO_MAX {
            memo.clear();
        }
        memo.insert(key, answer.clone());
        Some(answer)
    }

    fn certified_list(&self, repo: &Path) -> String {
        match &self.host {
            Some(h) => h.certified_list(repo),
            None => run_cmd(&self.seams.queue_helpers, &["certified-list", &repo.to_string_lossy()]),
        }
    }

    fn config(&self, argv: &[&str]) -> String {
        match &self.host {
            Some(h) => h.config(argv),
            None => run_cmd(&self.seams.spira_config, argv),
        }
    }

    fn stranded_runners(&self, repo: &Path) -> String {
        match &self.host {
            Some(h) => h.stranded_runners(repo),
            None => run_cmd(&self.seams.forge, &["stranded-runners", &repo.to_string_lossy()]),
        }
    }

    pub fn send_mail(&self, mail: &Mail) -> Result<(), String> {
        match &self.host {
            Some(h) => h.send_mail(mail),
            None => send_mail_in_process(mail),
        }
    }
}

fn send_mail_in_process(m: &Mail) -> Result<(), String> {
    mail::send(&mail::Message {
        mailbox: &m.mailbox,
        from: mail::FROM,
        subject: &m.subject,
        kind: &m.kind,
        default: m.default.as_deref(),
        class: m.class.as_deref(),
        bead: m.bead.as_deref(),
        body: &m.body,
    })
}

/// Free percent of a filesystem the way `df -kP` reports it: available over total, in 1 KiB
/// blocks, read in-process.
pub fn free_pct(path: &str) -> Option<i64> {
    let c = std::ffi::CString::new(path).ok()?;
    // SAFETY: a zeroed statvfs is a valid out-parameter, and `c` is a NUL-terminated path.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let frsize = s.f_frsize as u128;
    let total = s.f_blocks as u128 * frsize / 1024;
    let avail = s.f_bavail as u128 * frsize / 1024;
    (total > 0).then(|| (avail * 100 / total) as i64)
}

/// The verbs the lifecycle socket does not serve: they prove a landing from git in the
/// caller's own checkout (spira-lc composed.rs), and the reconciler never sends them.
pub const LC_UNSERVED: &[&str] = &["deliver", "content-on-base", "close-on-land", "content-landed"];

/// Whether `argv` is a verb the store serves over its socket.
pub fn lc_served(argv: &[&str]) -> bool {
    argv.first().is_some_and(|v| !LC_UNSERVED.contains(v) && !v.starts_with("admin-"))
}

pub fn log_print(cfg: &Config, msg: &str) {
    let line = format!("{} spira: {}\n", clock::iso_of(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)), msg);
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Remedy {
    /// No deterministic fix — a gap here always escalates to the Concierge.
    Escalate,
    Command { program: String, args: Vec<String> },
    /// Every step runs, in order, whether or not an earlier one succeeded.
    Sequence(Vec<(String, Vec<String>)>),
}

impl Remedy {
    pub fn steps(&self) -> Vec<(&str, Vec<&str>)> {
        match self {
            Remedy::Escalate => Vec::new(),
            Remedy::Command { program, args } => vec![(program.as_str(), args.iter().map(String::as_str).collect())],
            Remedy::Sequence(steps) => steps
                .iter()
                .map(|(p, a)| (p.as_str(), a.iter().map(String::as_str).collect()))
                .collect(),
        }
    }

    pub fn describe(&self) -> String {
        self.steps().iter().map(|(p, a)| format!("{} {}", p, a.join(" "))).collect::<Vec<_>>().join(" ; ")
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub key: String,
    pub raw: RawStatus,
    pub remedy: Remedy,
}

pub fn run_cmd(program: &str, args: &[&str]) -> String {
    spira_config::bounded::bounded(program)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default()
}

pub fn run_cmd_ok(program: &str, args: &[&str]) -> bool {
    spira_config::bounded::bounded(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ──────────────────────────────────────────────────────────────────────────────
// Repo map — copied from czar-pass's own reader; the two binaries share the
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
/// The words `is-enabled` and `is-active` print for a unit they could tell about.
pub fn is_state_word(word: &str) -> bool {
    const KNOWN: &[&str] = &[
        "enabled", "disabled", "static", "masked", "linked", "alias", "generated", "indirect", "enabled-runtime",
        "active", "inactive", "failed", "activating", "deactivating", "reloading",
    ];
    KNOWN.contains(&word)
}

fn systemctl_state(systemctl: &str, verb: &str, unit: &str) -> Result<String, String> {
    let out = spira_config::bounded::bounded(systemctl)
        .args(["--user", verb, unit])
        .output()
        .map_err(|e| format!("{} {} {}: {}", systemctl, verb, unit, e))?;
    let word = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if is_state_word(&word) {
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

pub fn load_ctrl(cfg: &Config) -> spira_ctrl::CtrlData {
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
    matches!(cfg.lc(&["show", owner]).and_then(|out| spira_config::lc_state::parse_show(&out)), Ok(Some(r)) if r.state == "LANDED")
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

/// One suspension that has fallen due and not been reminded of yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reminder {
    pub marker: String,
    pub subject: String,
    pub why: String,
    pub mail: Mail,
}

/// Every suspension declared now (`live`, by marker) and those of them that are due and not in `sent`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderPlan {
    pub live: std::collections::BTreeSet<String>,
    pub due: Vec<Reminder>,
}

pub fn plan_reminders(cfg: &Config, ctrl: &spira_ctrl::CtrlData, sent: &std::collections::BTreeSet<String>) -> ReminderPlan {
    let today = &cfg.now_iso[..cfg.now_iso.len().min(10)];
    let mut plan = ReminderPlan::default();
    for subject in ctrl.keys() {
        let Some((owner, until)) = spira_ctrl::declared(ctrl, subject) else { continue };
        let marker = format!("{}|{}|{}", subject, owner, until.unwrap_or(""));
        plan.live.insert(marker.clone());
        if sent.contains(&marker) {
            continue;
        }
        let why = match reminder_due(owner, until, today, owner_landed(cfg, owner)) {
            Due::NotDue => continue,
            Due::Until(u) => format!("its until date {} has come", u),
            Due::OwnerLanded(o) => format!("its owner bead {} has landed", o),
        };
        let reason = spira_ctrl::reason(ctrl, subject).unwrap_or("");
        let mail = Mail {
            mailbox: "concierge".into(),
            subject: format!("reconciler: suspension of {} is due for review", subject),
            kind: "note".into(),
            default: None,
            class: None,
            bead: None,
            body: reminder_body(subject, &why, reason, owner),
        };
        plan.due.push(Reminder { marker, subject: subject.to_string(), why, mail });
    }
    plan
}

/// Mails the Concierge once per (subject, owner, until) declaration when it falls due. The
/// sent set is persisted; a failed send is not recorded, so it is retried next pass.
fn remind_due_suspensions(cfg: &Config, ctrl: &spira_ctrl::CtrlData) {
    let mut sent: std::collections::BTreeSet<String> = fs::read_to_string(&cfg.reminders_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let plan = plan_reminders(cfg, ctrl, &sent);
    let live = plan.live;
    let before = sent.clone();
    sent.retain(|k| live.contains(k));
    let failed_path = cfg.reminders_path.with_extension("failed.json");
    let mut failed: std::collections::BTreeMap<String, String> = fs::read_to_string(&failed_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let failed_before = failed.clone();
    for r in plan.due {
        match cfg.send_mail(&r.mail) {
            Ok(()) => {
                log_print(cfg, &format!("reconciler: reminded — {}: {}", r.subject, r.why));
                sent.insert(r.marker);
            }
            Err(e) => {
                if failed.get(&r.marker) != Some(&e) {
                    log_print(cfg, &format!("reconciler: reminder for {} not sent: {}", r.subject, e));
                }
                failed.insert(r.marker, e);
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
            manifest_names = cfg.list_enable();
            manifest_names.lines().map(str::trim).filter(|l| !l.is_empty()).collect()
        }
    };
    let ctrl = load_ctrl(cfg);
    let mut checks = Vec::new();
    for unit in unit_names.into_iter().filter(|u| !cfg.skip_units.iter().any(|s| s == u)) {
        if let Some((subject, reason)) = suspension_of(&ctrl, &cfg.instance, unit) {
            checks.push(Check {
                key: unit_key(cfg, unit),
                raw: RawStatus::Deliberate { reason: format!("suspended ({}): {}", subject, reason) },
                remedy: Remedy::Escalate,
            });
            continue;
        }
        let enabled = cfg.unit_state("is-enabled", unit);
        let active = cfg.unit_state("is-active", unit);
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
            // Never restart the store: a gap here always
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

        // A daemon row: several *.service rows were found
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
    live_total: i64,
    max_live: Option<i64>,
    lanes_max: Option<i64>,
    pool: Option<i64>,
}

fn count_of(out: &str) -> i64 {
    out.trim().parse().unwrap_or(0)
}

/// Observed fleet occupancy from the same tools the sentinel decides calls with: the runes
/// supplies the partitions, `spira-claim` the claimable work under each, `strand` the dwarves live.
fn fleet_readings(cfg: &Config) -> FleetReadings {
    let words = |out: String| out.split_whitespace().map(String::from).collect::<Vec<_>>();
    let task_runes = words(cfg.config(&["rune", "task"]));
    let live_of = |rune: &str| cfg.dwarf_count(rune);

    let lane_runes: Vec<String> = words(cfg.config(&["rune", "lane"])).into_iter().filter(|f| !task_runes.contains(f)).collect();
    let live_lanes: i64 = lane_runes.iter().map(|f| live_of(f)).sum();
    let live_total = live_lanes + task_runes.iter().map(|f| live_of(f)).sum::<i64>();

    let mut rows = Vec::new();
    for line in cfg.config(&["rune", "partitions"]).lines() {
        let (labels, exclude) = line.split_once('\t').unwrap_or((line, ""));
        if labels.is_empty() {
            continue;
        }
        let ready = cfg.ready_count(labels, exclude);
        let runes = cfg.config(&["rune", "for-labels", labels]);
        let runes: Vec<&str> = runes.split_whitespace().collect();
        let live = runes.iter().map(|f| live_of(f)).sum();
        let is_task = runes.iter().any(|f| task_runes.iter().any(|t| t == f));
        rows.push((labels.to_string(), ready, live, is_task));
    }

    FleetReadings {
        rows,
        live_lanes,
        live_total,
        max_live: cfg.max_live_dwarves.trim().parse().ok(),
        lanes_max: cfg.lanes_max_live.trim().parse().ok(),
        pool: cfg.max_dwarves.trim().parse().ok(),
    }
}

fn observe_fleet(cfg: &Config) -> Vec<Check> {
    let FleetReadings { rows, live_lanes, live_total, max_live, lanes_max, pool } = fleet_readings(cfg);

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
            // partition: fanning N incident.sh filings out of a single pass can
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
        // Mirrors the sentinel's CHECK7: a task partition needs a fleet slot; a lane needs a
        // lane share, and may spill past the ceiling by at most the lane cap.
        let slot_free = if is_task {
            live_total < max_live
        } else {
            lanes_max.map_or(live_total < max_live, |m| live_lanes < m && live_total < max_live + m)
        };
        let raw = if live >= desired_live || deliberately_paused || !slot_free {
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

/// A satisfied cockpit is not asked of tmux again for this long while the same server answers: nothing on disk
/// says a session or pane changed, so a killed session is found at most this late.
const COCKPIT_RECHECK_SECS: u64 = 600;

fn observe_cockpit(cfg: &Config) -> Check {
    // `layout down` records this: the operator dismissed the dashboards on purpose, and a
    // deliberate state is not a fault (law-a-deliberate-state-is-not-a-fault). `layout up`
    // clears it the moment it rebuilds anything, so this is stale for no longer than the next
    // time the operator — or this very remedy — asks for the cockpit.
    if cfg.spira_run.join("cockpit.down").exists() {
        return Check { key: "cockpit".into(), raw: RawStatus::Satisfied, remedy: Remedy::Escalate };
    }

    let server = cfg.host.as_ref().and_then(|h| h.tmux_server());
    let down = match &server {
        Some(answer) => answer.is_none(),
        None => !run_cmd_ok(&cfg.seams.tmux, &["list-sessions"]),
    };
    if down {
        return Check {
            key: "cockpit".into(),
            raw: RawStatus::Gap { desired: "tmux server up".into(), observed: "no server".into(), since_hint: None },
            remedy: rebuild_remedy(cfg),
        };
    }
    let satisfied = Check { key: "cockpit".into(), raw: RawStatus::Satisfied, remedy: Remedy::Escalate };
    let held = server.flatten().map(|id| format!("cockpit:{id}:{}:{}", cfg.cockpit_sessions.join(","), cfg.cockpit_mail));
    let recently = |at: &String| at.parse::<u64>().is_ok_and(|at| cfg.now_secs.saturating_sub(at) < COCKPIT_RECHECK_SECS);
    if held.as_ref().is_some_and(|k| cfg.memo.lock().unwrap_or_else(|p| p.into_inner()).get(k).is_some_and(recently)) {
        return satisfied;
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

    // The Composite's CockpitSpec.dashboards is the declared set; an install that
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

    if let Some(k) = held {
        let mut memo = cfg.memo.lock().unwrap_or_else(|p| p.into_inner());
        if memo.len() >= MEMO_MAX {
            memo.clear();
        }
        memo.insert(k, cfg.now_secs.to_string());
    }
    satisfied
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
    if let Some(branch) = cfg.host.as_ref().and_then(|h| h.head_branch(Path::new(&cfg.spira_home))) {
        return Some(branch == "main");
    }
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

    // The Composite's ReleaseSpec names the declared target explicitly; an install
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

/// Without a Composite DiskSpec: the root filesystem, the store's data directory, and
/// podman's storage root. A default-set path that cannot be read (podman not installed, the
/// store in server mode elsewhere) is simply omitted — there is no declared expectation it exist.
fn default_disk_paths(cfg: &Config) -> Vec<String> {
    let mut paths = vec!["/".to_string()];
    if !cfg.dolt_data.is_empty() {
        paths.push(cfg.dolt_data.clone());
    }
    let graph_root = podman_graph_root(cfg);
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
        let raw = match cfg.free_pct(&path) {
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

/// podman's storage root, or empty when podman is absent or did not answer. A long-lived
/// host remembers only a root podman named: the root is podman's configuration, not its
/// state, while one slow or failed `podman info` is not an answer to keep.
fn podman_graph_root(cfg: &Config) -> String {
    let held = cfg.host.as_ref().map(|_| "podman-graph-root".to_string());
    cfg.remembered(held, || Some(run_cmd(&cfg.seams.podman, &["info", "--format", "{{.Store.GraphRoot}}"])).filter(|r| !r.trim().is_empty())).unwrap_or_default()
}

/// The positive control on testenv's own image cleanup: a store holding more dangling
/// images than the configured bound is a gap, escalated, because the disk under it fills
/// silently otherwise. No podman storage root (podman absent) is no check, as for the disk.
fn observe_untagged_images(cfg: &Config) -> Vec<Check> {
    let podman = &cfg.seams.podman;
    if podman_graph_root(cfg).trim().is_empty() {
        return Vec::new();
    }
    let out = spira_config::bounded::bounded(podman)
        .args(["images", "--quiet", "--filter", "dangling=true"])
        .stderr(Stdio::null())
        .output();
    let raw = match out {
        Ok(o) if o.status.success() => {
            let n = String::from_utf8_lossy(&o.stdout).lines().filter(|l| !l.trim().is_empty()).count() as u64;
            if n <= cfg.untagged_images_max {
                RawStatus::Satisfied
            } else {
                RawStatus::Gap {
                    desired: format!("<= {} untagged images", cfg.untagged_images_max),
                    observed: format!("{n} untagged images"),
                    since_hint: None,
                }
            }
        }
        _ => RawStatus::Unobservable { reason: "podman could not list its dangling images".into() },
    };
    vec![Check { key: "images:untagged".into(), raw, remedy: Remedy::Escalate }]
}

// ──────────────────────────────────────────────────────────────────────────────
// ──────────────────────────────────────────────────────────────────────────────
// Queue: two structural checks czar-pass's own detectors do not already cover. The base
// gate itself is base-red, already watched by czar-pass — reconciling it here too would
// file a second incident for the same fault, so it is deliberately not repeated.
// ──────────────────────────────────────────────────────────────────────────────

fn branch_mergeable(git: &str, repo: &Path, base: &str, branch: &str) -> Option<bool> {
    let out = spira_config::bounded::bounded(git)
        .arg("-C")
        .arg(repo)
        .args(["-c", &format!("attr.tree={base}"), "merge-tree", "--write-tree", "--no-messages", base, branch])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().next().unwrap_or("");
    let tree_id = first.len() >= 40 && first.bytes().all(|b| b.is_ascii_hexdigit());
    match out.status.code() {
        Some(0) if tree_id => Some(true),
        Some(1) if tree_id => Some(false),
        _ => None,
    }
}

fn mergeable_remembered(cfg: &Config, repo: &Path, base: &str, branch: &str) -> Option<bool> {
    let key = cfg.ref_sha(repo, base).zip(cfg.ref_sha(repo, &format!("refs/heads/{branch}"))).map(|(b, t)| format!("merge:{}:{b}:{t}", repo.display()));
    cfg.remembered(key, || branch_mergeable(&cfg.seams.git, repo, base, branch).map(|m| m.to_string())).map(|m| m == "true")
}

fn observe_queue_mergeable(cfg: &Config) -> Vec<Check> {
    let mut checks = Vec::new();
    for repo_name in queue_repo_names(&cfg.queue_dir) {
        let repo_path = match repo_root(&repo_name, &cfg.repo_map) {
            Some(p) => p,
            None => continue,
        };
        let base = cfg.config(&["repo", "landref", &repo_name]);
        let base = base.trim();
        if base.is_empty() {
            continue;
        }
        let out = cfg.certified_list(&repo_path);
        for line in out.lines() {
            let Some(id) = line.split_whitespace().next() else { continue };
            let branch = format!("spira/{id}");
            let branch = branch.as_str();
            let key = format!("queue-mergeable:{}:{}", repo_name, id);
            let raw = match mergeable_remembered(cfg, &repo_path, base, branch) {
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
        let out = cfg.stranded_runners(&repo_path);
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
    let key = cfg.ref_sha(repo, landref).map(|l| format!("anc:{}:{head}:{l}", repo.display()));
    cfg.remembered(key, || head_on_ref_asked(cfg, repo, head, landref).map(|a| a.to_string())).map(|a| a == "true")
}

fn head_on_ref_asked(cfg: &Config, repo: &Path, head: &str, landref: &str) -> Option<bool> {
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
    let out = cfg.lc(&["list", "--batches", "--window", "1"]).unwrap_or_default();
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
        let landref = cfg.config(&["repo", "landref", &g.repo]);
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
    let raw = orphan_reading(&cfg.lc(&["requeue-orphans", "--actor", actor]).unwrap_or_default());
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
    let out = cfg.lc(&["content", "list", "--all", "--brief", "--json", "--limit", "0"])?;
    let rows: Vec<serde_json::Value> = serde_json::from_str(out.trim()).map_err(|e| format!("content list: unparsable json: {e}"))?;
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
    let rows = match cfg.lc(&["list"]).and_then(|out| spira_config::lc_state::parse_rows(&out)) {
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

/// The JSON object a `plan` verb printed, skipping any notice lines before it.
fn plan_json(out: &str) -> Result<serde_json::Value, String> {
    out.char_indices()
        .filter(|&(i, c)| c == '{' && (i == 0 || out.as_bytes()[i - 1] == b'\n'))
        .find_map(|(i, _)| serde_json::from_str(&out[i..]).ok())
        .ok_or_else(|| format!("plan: no JSON in {:?}", out.chars().take(120).collect::<String>()))
}

#[derive(Deserialize)]
enum EdgeKind {
    #[serde(rename = "blocks")]
    Blocks,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct PlanEdge {
    issue_id: String,
    depends_on_id: String,
    #[serde(rename = "type")]
    kind: EdgeKind,
}

/// `<child> <epic>` per open epic with a `blocks` edge from a dependent, from the store's planning reads.
fn epic_edges_read(cfg: &Config) -> Result<String, String> {
    let epics = plan_json(&cfg.lc(&["plan", "items", "--type", "epic", "--open"])?)?;
    let ids: Vec<&str> = epics["items"].as_array().into_iter().flatten().filter_map(|i| i["id"].as_str()).collect();
    let mut out = String::new();
    for chunk in ids.chunks(100) {
        let edges = plan_json(&cfg.lc(&["plan", "edges", "--ids", &chunk.join(","), "--dependents"])?)?;
        let edges: Vec<PlanEdge> = serde_json::from_value(edges["edges"].clone()).map_err(|e| format!("plan edges: {e}"))?;
        for e in edges.iter().filter(|e| matches!(e.kind, EdgeKind::Blocks)) {
            out.push_str(&format!("{} {}\n", e.issue_id, e.depends_on_id));
        }
    }
    Ok(out)
}

fn observe_epic_edges(cfg: &Config) -> Vec<Check> {
    let out = match epic_edges_read(cfg) {
        Ok(out) => out,
        Err(reason) => return vec![Check { key: "epic-blocks-edge".into(), raw: RawStatus::Unobservable { reason }, remedy: Remedy::Escalate }],
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
    let release = match cfg.ref_read(Path::new(repo), &active_release(cfg)) {
        Ref::Sha(sha) => sha,
        Ref::Absent => return None,
        Ref::Unknown | Ref::Object(_) => run_cmd(git, &["-C", repo, "rev-parse", "--verify", "--quiet", &format!("{}^{{commit}}", active_release(cfg))]).trim().to_string(),
    };
    let release = release.as_str();
    if release.is_empty() {
        return None;
    }
    let contained = cfg.remembered(Some(format!("anc:{repo}:{sha}:{release}")), || Some(run_cmd_ok(git, &["-C", repo, "merge-base", "--is-ancestor", sha, release]).to_string()));
    if contained.as_deref() != Some("false") {
        return None;
    }
    let oldest = cfg.remembered(Some(format!("oldest:{repo}:{release}:{sha}")), || Some(run_cmd(git, &["-C", repo, "log", "--reverse", "--format=%ct", &format!("{}..{}", release, sha)]))).unwrap_or_default();
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
    let sha = match cfg.ref_read(&repo, MAIN_REF) {
        Ref::Sha(sha) => sha,
        Ref::Absent => String::new(),
        Ref::Unknown | Ref::Object(_) => run_cmd(git, &["-C", &repo_str, "rev-parse", "--verify", "--quiet", &format!("{}^{{commit}}", MAIN_REF)]).trim().to_string(),
    };
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
// decide: run one Check through the pure engine and say what to do about the verdict.
// `evaluate` performs it in this process; spira-runtime turns the same acts into effects.
// ──────────────────────────────────────────────────────────────────────────────

/// What a pass needs to know to decide, apart from the reads: when it is and how the
/// hysteresis is tuned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rules {
    pub now_secs: u64,
    pub grace_secs: u64,
    pub effect_passes: u32,
    pub shadow_kinds: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventRow {
    pub event: String,
    pub key: String,
    pub action: String,
    pub before: f64,
    pub after: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Act {
    Log(String),
    Status(StatusRecord),
    Event(EventRow),
    /// Every step runs, in order, whether or not an earlier one succeeded.
    Remedy { key: String, steps: Vec<(String, Vec<String>)> },
    /// `unalert` is the escalation to forget when the mail does not go, so the next pass sends it again.
    Mail { mail: Mail, unalert: Option<String>, what: String },
}

/// What a pass remembers of each invariant, between passes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Book {
    pub state: StateMap,
    pub pending: PendingMap,
    pub alerted: AlertedSinceMap,
    pub reminded: std::collections::BTreeSet<String>,
}

/// The invariant metric every remedy here should lower: 0 when satisfied, 1 while it is
/// not (a gap or an unreadable input).
fn gap_metric(raw: &RawStatus) -> f64 {
    if matches!(raw, RawStatus::Satisfied) { 0.0 } else { 1.0 }
}

fn event(acts: &mut Vec<Act>, event: &str, key: &str, action: &str, before: f64, after: Option<f64>) {
    acts.push(Act::Event(EventRow { event: event.into(), key: key.into(), action: action.into(), before, after }));
}

fn note_concierge(subject: String, body: String, what: String) -> Act {
    Act::Mail { mail: Mail { mailbox: "concierge".into(), subject, kind: "note".into(), default: None, class: None, bead: None, body }, unalert: None, what }
}

fn judge_pending(rules: &Rules, acts: &mut Vec<Act>, pending: &mut PendingMap, key: &str, after: f64) {
    let Some(prior) = pending.remove(key) else { return };
    match measure(prior.clone(), after, rules.effect_passes) {
        Outcome::Pending(p) => {
            pending.insert(key.to_string(), p);
        }
        Outcome::Effect { before, after } => {
            event(acts, "effect", key, &prior.action, before, Some(after));
        }
        Outcome::NoEffect { before, after } => {
            event(acts, "no-effect", key, &prior.action, before, Some(after));
            acts.push(Act::Log(format!("reconciler: {} → remedy had no effect", key)));
            let subject = format!("RECONCILER: remedy for {} had no effect", key);
            let body = format!("## Note\n\n{}", compose_no_effect(key, &prior, after, rules.now_secs, rules.effect_passes));
            acts.push(note_concierge(subject, body, format!("{key} → no-effect mail")));
        }
    }
}

pub fn decide(rules: &Rules, state: &mut StateMap, pending: &mut PendingMap, alerted: &mut AlertedSinceMap, check: Check) -> Vec<Act> {
    let mut acts = Vec::new();
    let prev = state.remove(&check.key).unwrap_or_default();
    let prev_remedy = last_remedy(&prev).map(str::to_string);
    let metric_now = gap_metric(&check.raw);
    let first_unobservable_unit_read =
        kind_of(&check.key) == "units" && matches!(check.raw, RawStatus::Unobservable { .. }) && prev == HysteresisState::default();
    let (mut verdict, mut next) = step(rules.now_secs, check.raw, rules.grace_secs, prev);
    if first_unobservable_unit_read {
        verdict.is_gap = false;
    }
    acts.push(Act::Status(status_record(&check.key, &verdict)));
    judge_pending(rules, &mut acts, pending, &check.key, metric_now);

    if verdict.is_gap {
        if verdict.remedy_failed {
            escalate(rules, &mut acts, alerted, &check.key, &verdict, prev_remedy.as_deref());
        } else {
            match &check.remedy {
                Remedy::Escalate => escalate(rules, &mut acts, alerted, &check.key, &verdict, None),
                remedy => {
                    let desc = remedy.describe();
                    if is_shadowed(&rules.shadow_kinds, kind_of(&check.key)) {
                        acts.push(Act::Log(format!("reconciler: {} → remedy held (shadow): {}", check.key, desc)));
                        event(&mut acts, "shadow", &check.key, &desc, metric_now, None);
                        escalate(rules, &mut acts, alerted, &check.key, &verdict, None);
                    } else {
                        acts.push(Act::Log(format!("reconciler: {} → remedy: {}", check.key, desc)));
                        event(&mut acts, "remedy", &check.key, &desc, metric_now, None);
                        let steps = remedy.steps().into_iter().map(|(p, a)| (p.to_string(), a.into_iter().map(String::from).collect())).collect();
                        acts.push(Act::Remedy { key: check.key.clone(), steps });
                        record_remedy(&mut next, rules.now_secs, &desc);
                        pending.insert(
                            check.key.clone(),
                            PendingEffect { action: desc, metric: "gap".into(), before: metric_now, acted_at: rules.now_secs, passes_seen: 0 },
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
    acts
}

pub fn evaluate(cfg: &Config, state: &mut StateMap, pending: &mut PendingMap, alerted: &mut AlertedSinceMap, check: Check) {
    let acts = decide(&cfg.rules(), state, pending, alerted, check);
    perform(cfg, alerted, acts);
}

/// Carries out acts in this process: log lines, rows in the metrics store, the remedy programs
/// and the mail.
pub fn perform(cfg: &Config, alerted: &mut AlertedSinceMap, acts: Vec<Act>) {
    for act in acts {
        match act {
            Act::Log(line) => log_print(cfg, &line),
            Act::Status(row) => push_status(&cfg.store, &cfg.now_iso, &row),
            Act::Event(e) => append_event(
                &cfg.store,
                &RemedyEvent { ts: &cfg.now_iso, event: &e.event, kind: kind_of(&e.key), key: &e.key, action: &e.action, metric: "gap", before: e.before, after: e.after, release: &cfg.release },
            ),
            Act::Remedy { steps, .. } => {
                for (program, args) in &steps {
                    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
                    if *program == cfg.seams.spira_lc && lc_served(&argv) {
                        let _ = cfg.lc(&argv);
                        continue;
                    }
                    // FORK-EDGE: remedy program — declared remedies (systemctl enable/restart, forge, bead, queue) run as commands
                    let _ = run_cmd_ok(program, &argv);
                }
            }
            Act::Mail { mail, unalert, what } => {
                if let Err(e) = cfg.send_mail(&mail) {
                    log_print(cfg, &format!("reconciler: {what} failed: {e}"));
                    if let Some(key) = unalert {
                        alerted.remove(&key);
                    }
                }
            }
        }
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
fn escalate(rules: &Rules, acts: &mut Vec<Act>, alerted: &mut AlertedSinceMap, key: &str, verdict: &Verdict, last_remedy: Option<&str>) {
    let (fire, next) = should_alert(verdict, alerted.get(key).copied());
    match next {
        Some(since) => alerted.insert(key.to_string(), since),
        None => alerted.remove(key),
    };
    if !fire {
        return;
    }
    let subject = format!("RECONCILER: {} — {}", key, describe(verdict));
    let evidence = compose_alert(key, rules.now_secs, verdict, last_remedy);
    acts.push(Act::Log(format!("reconciler: {} → escalate", key)));
    let body = format!("## Alert\n{subject}\n\nThe reconciler found a structural gap that has outlasted its grace period.\n\n{evidence}");
    acts.push(Act::Mail {
        mail: Mail { mailbox: "concierge".into(), subject, kind: "alert".into(), default: None, class: None, bead: None, body },
        unalert: Some(key.to_string()),
        what: format!("{key} → escalation mail"),
    });
}

// ──────────────────────────────────────────────────────────────────────────────
// Hold sweep: every held bead visited, moved one step for its kind (reconciler_engine::holds).
// Throttled to its own cadence — it costs a store read per held bead — and its remedies are
// telemetered and measured like any other, under the kind `holds`.
// ──────────────────────────────────────────────────────────────────────────────

const HOLD_SWEEP_INTERVAL_SECS: u64 = 600;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepOutcome {
    Done,
    Shadowed,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepTaken {
    pub key: String,
    pub desc: String,
    pub outcome: StepOutcome,
}

/// What one hold sweep did, or why it could not.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldsReport {
    /// False while the sweep's own cadence says it is not due.
    pub ran: bool,
    pub breaker_unconfigured: Option<String>,
    pub cannot_tell: Option<String>,
    pub visited: usize,
    pub wanted: Vec<String>,
    pub skipped: Vec<String>,
    pub taken: Vec<StepTaken>,
}

pub fn sweep_holds(cfg: &Config) -> HoldsReport {
    let state_path = cfg.spira_run.join("reconciler-holds.json");
    let mut state = holds::load_state(&state_path);
    if cfg.now_secs.saturating_sub(state.last_pass) < HOLD_SWEEP_INTERVAL_SECS {
        return HoldsReport::default();
    }
    let lc = |argv: &[&str]| cfg.lc(argv);
    let send = |m: &Mail| cfg.send_mail(m);
    let mut live = hold_sweep::Live { lc: &lc, send: &send, mail_root: cfg.mail_root.clone(), actor: "reconciler".to_string(), now: cfg.now_secs };
    let shadow = is_shadowed(&cfg.shadow_kinds, "holds");
    let breaker = cfg.breaker.clone().and_then(|(claims, minutes)| holds::Breaker::new(claims, minutes));
    let mut report = HoldsReport { ran: true, ..HoldsReport::default() };
    let breaker = match breaker {
        Ok(b) => Some(b),
        Err(e) => {
            report.breaker_unconfigured = Some(e);
            None
        }
    };
    match holds::sweep(cfg.now_secs, &mut live, &mut state, shadow, breaker) {
        Err(e) => {
            report.cannot_tell = Some(e);
            state.last_pass = cfg.now_secs;
        }
        Ok(r) => {
            report.visited = r.visited;
            report.wanted = r.wanted.into_iter().collect();
            report.skipped = r.skipped;
            report.taken = r
                .taken
                .iter()
                .map(|t| StepTaken {
                    key: t.key.clone(),
                    desc: format!("{} {}", t.action.name(), t.bead),
                    outcome: match &t.outcome {
                        HoldOutcome::Done => StepOutcome::Done,
                        HoldOutcome::Shadowed => StepOutcome::Shadowed,
                        HoldOutcome::Failed(e) => StepOutcome::Failed(e.clone()),
                    },
                })
                .collect();
        }
    }
    let _ = holds::save_state(&state_path, &state);
    report
}

pub fn judge_holds(rules: &Rules, pending: &mut PendingMap, report: &HoldsReport) -> Vec<Act> {
    let mut acts = Vec::new();
    if !report.ran {
        return acts;
    }
    if let Some(e) = &report.breaker_unconfigured {
        acts.push(Act::Log(format!("reconciler: holds: no-progress breaker unconfigured, waits not lifted: {e}")));
    }
    if let Some(e) = &report.cannot_tell {
        acts.push(Act::Log(format!("reconciler: holds: cannot tell, nothing acted on: {e}")));
        return acts;
    }
    let judged: Vec<String> = pending.keys().filter(|k| k.starts_with("holds:")).cloned().collect();
    for key in judged {
        judge_pending(rules, &mut acts, pending, &key, if report.wanted.contains(&key) { 1.0 } else { 0.0 });
    }
    for note in &report.skipped {
        acts.push(Act::Log(format!("reconciler: holds: skipped {note}")));
    }
    for t in &report.taken {
        let desc = &t.desc;
        match &t.outcome {
            StepOutcome::Done => {
                acts.push(Act::Log(format!("reconciler: {} → remedy: {desc}", t.key)));
                event(&mut acts, "remedy", &t.key, desc, 1.0, None);
                pending.insert(t.key.clone(), PendingEffect { action: desc.clone(), metric: "gap".into(), before: 1.0, acted_at: rules.now_secs, passes_seen: 0 });
            }
            StepOutcome::Shadowed => {
                acts.push(Act::Log(format!("reconciler: {} → remedy held (shadow): {desc}", t.key)));
                event(&mut acts, "shadow", &t.key, desc, 1.0, None);
            }
            StepOutcome::Failed(e) => acts.push(Act::Log(format!("reconciler: {} → {desc} failed: {e}", t.key))),
        }
    }
    acts.push(Act::Log(format!("reconciler: holds: visited {}, acted {}", report.visited, report.taken.len())));
    acts
}

fn run_hold_sweep(cfg: &Config, pending: &mut PendingMap) {
    let report = sweep_holds(cfg);
    let acts = judge_holds(&cfg.rules(), pending, &report);
    perform(cfg, &mut AlertedSinceMap::new(), acts);
}

// ──────────────────────────────────────────────────────────────────────────────
// Main pass
// ──────────────────────────────────────────────────────────────────────────────

/// The invariants, in the order their checks are decided.
pub const INVARIANTS: [&str; 14] = [
    "units", "fleet", "cockpit", "release", "disk", "untagged-images", "queue-mergeable", "queue-lock-age",
    "queue-stranded-runs", "lc-orphans", "batch-landed", "junk-row", "epic-edges", "main-health",
];

/// Invariants whose keys name a thing that comes and goes: the key of one no longer reported
/// is dropped from the state.
const VANISHING: [(&str, &str); 4] = [
    ("queue-stranded-runs", STRANDED_RUN_PREFIX),
    ("batch-landed", LANDED_BATCH_PREFIX),
    ("junk-row", JUNK_ROW_PREFIX),
    ("epic-edges", EPIC_EDGE_PREFIX),
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub name: String,
    pub checks: Vec<Check>,
}

/// Forgets the state of any vanishing key its invariant did not report this pass.
pub fn prune(state: &mut StateMap, groups: &[Group]) {
    for (name, prefix) in VANISHING {
        let seen = groups.iter().find(|g| g.name == name).map(|g| g.checks.as_slice()).unwrap_or_default();
        state.retain(|k, _| !k.starts_with(prefix) || seen.iter().any(|c| &c.key == k));
    }
}

fn wall_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn observed(cfg: &Config, name: &str, observe: impl FnOnce() -> Vec<Check>) -> Vec<Check> {
    log_print(cfg, &format!("reconciler: invariant {name}: observing"));
    let started = wall_secs();
    let checks = observe();
    log_print(cfg, &format!("reconciler: invariant {name}: {} check(s), {}s", checks.len(), wall_secs().saturating_sub(started)));
    checks
}

/// Every invariant observed once, each on its own thread, in `INVARIANTS` order.
pub fn observe_groups(cfg: &Config) -> Vec<Group> {
    type Job<'a> = (&'static str, Box<dyn FnOnce() -> Vec<Check> + Send + 'a>);
    let jobs: Vec<Job> = vec![
        ("units", Box::new(|| observe_units(cfg))),
        ("fleet", Box::new(|| observe_fleet(cfg))),
        ("cockpit", Box::new(|| vec![observe_cockpit(cfg)])),
        ("release", Box::new(|| vec![observe_release(cfg)])),
        ("disk", Box::new(|| observe_disk(cfg))),
        ("untagged-images", Box::new(|| observe_untagged_images(cfg))),
        ("queue-mergeable", Box::new(|| observe_queue_mergeable(cfg))),
        ("queue-lock-age", Box::new(|| observe_queue_lock_age(cfg))),
        ("queue-stranded-runs", Box::new(|| observe_queue_stranded_runs(cfg))),
        ("lc-orphans", Box::new(|| vec![observe_lc_orphans(cfg)])),
        ("batch-landed", Box::new(|| observe_landed_batches(cfg))),
        ("junk-row", Box::new(|| observe_junk_rows(cfg))),
        ("epic-edges", Box::new(|| observe_epic_edges(cfg))),
        ("main-health", Box::new(|| observe_main_health(cfg))),
    ];
    debug_assert!(jobs.iter().map(|(n, _)| *n).eq(INVARIANTS));
    std::thread::scope(|scope| {
        let handles: Vec<_> = jobs.into_iter().map(|(name, job)| (name, scope.spawn(move || observed(cfg, name, job)))).collect();
        handles
            .into_iter()
            .map(|(name, h)| match h.join() {
                Ok(checks) => Group { name: name.into(), checks },
                Err(_) => {
                    log_print(cfg, &format!("reconciler: invariant {name}: panicked"));
                    Group {
                        name: name.into(),
                        checks: vec![Check {
                            key: format!("invariant:{name}"),
                            raw: RawStatus::Unobservable { reason: format!("the {name} observer panicked") },
                            remedy: Remedy::Escalate,
                        }],
                    }
                }
            })
            .collect()
    })
}

/// Everything one pass reads, for a caller that decides elsewhere.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub now_secs: u64,
    pub halted: bool,
    pub groups: Vec<Group>,
    pub reminders: ReminderPlan,
    pub holds: HoldsReport,
}

pub fn world_halted(cfg: &Config) -> bool {
    cfg.spira_run.join("world.halted").exists()
}

/// One pass's reads: the groups, the suspensions due for a reminder, and the hold sweep (which
/// acts as it reads). Stamps the pass. `reminded` is the set of reminders already sent.
pub fn observe(cfg: &Config, reminded: &std::collections::BTreeSet<String>) -> Observation {
    if world_halted(cfg) {
        log_print(cfg, "reconciler: skipped — world is halted");
        stamp_pass(cfg);
        return Observation { now_secs: cfg.now_secs, halted: true, ..Observation::default() };
    }
    let groups = observe_groups(cfg);
    let reminders = plan_reminders(cfg, &load_ctrl(cfg), reminded);
    let holds = sweep_holds(cfg);
    stamp_pass(cfg);
    Observation { now_secs: cfg.now_secs, halted: false, groups, reminders, holds }
}

/// The checks of every group in decision order.
pub fn checks_of(groups: Vec<Group>) -> Vec<Check> {
    INVARIANTS.iter().flat_map(|name| groups.iter().filter(move |g| g.name == *name).flat_map(|g| g.checks.clone())).collect()
}

pub fn run_pass() -> Result<(), String> {
    let cfg = Config::from_process(Seams::production()?)?.with_desired_state();

    if world_halted(&cfg) {
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

    let groups = observe_groups(&cfg);
    prune(&mut state, &groups);
    let checks = checks_of(groups);

    remind_due_suspensions(&cfg, &load_ctrl(&cfg));

    let n = checks.len();
    for check in checks {
        evaluate(&cfg, &mut state, &mut pending, &mut alerted, check);
    }

    run_hold_sweep(&cfg, &mut pending);

    let _ = save_state(&cfg.state_path, &state);
    let _ = save_pending(&cfg.effect_state, &pending);
    let _ = save_alerted(&cfg.alerted_path, &alerted);

    log_print(&cfg, &format!("reconciler: complete ({} checks, {}s)", n, wall_secs().saturating_sub(cfg.now_secs)));

    stamp_pass(&cfg);
    drop(lock_file);
    Ok(())
}

fn stamp_pass(cfg: &Config) {
    let _ = fs::write(paths::pass_stamp(&cfg.spira_run), format!("{}\n", wall_secs()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seams_refuse_naming_spira_release_when_it_is_unset_or_lacks_a_tool() {
        let e = Seams::from_release(None).err().unwrap();
        assert!(e.contains("SPIRA_RELEASE is not set"), "{e}");
        let e = Seams::from_release(Some("/nonexistent-release")).err().unwrap();
        assert!(e.contains("forge is not in SPIRA_RELEASE"), "{e}");
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
        let map = dir.join("repos");
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
    fn stranded_runs_reads_one_entry_per_run_and_ignores_other_lines() {
        let out = "STRANDED run=77 job=\"a\" label=ci-r-77-1 queued=700s runner=none vmid=?\n\
                   STRANDED run=77 job=\"b\" label=ci-r-77-1 queued=700s runner=none vmid=?\n\
                   STRANDED run=78 job=\"a\" label=ci-r-78-2 queued=900s runner=x vmid=3\n\
                   noise run=99\n";
        assert_eq!(stranded_runs(out), vec![("77".to_string(), "ci-r-77-1".to_string()), ("78".to_string(), "ci-r-78-2".to_string())]);
        assert!(stranded_runs("").is_empty());
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

    /// The lifecycle store's answers, and every argv it was asked.
    struct Store(std::sync::Mutex<Vec<Vec<String>>>);

    impl spira_config::lc_call::Transport for Store {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: std::time::Duration) -> Result<spira_config::lc_call::Reply, String> {
            self.0.lock().unwrap().push(argv.to_vec());
            Ok(spira_config::lc_call::Reply { code: 0, out: format!("answered {}", argv[0]), err: String::new() })
        }
    }

    fn served_config() -> (Config, Arc<Store>) {
        let store = Arc::new(Store(std::sync::Mutex::default()));
        let mut cfg = test_config();
        cfg.seams.spira_lc = "/nonexistent/spira-lc".into();
        cfg.lc_client = Some(LcClient::over(Arc::clone(&store) as Arc<dyn spira_config::lc_call::Transport>, true));
        (cfg, store)
    }

    #[test]
    fn a_served_lc_read_goes_over_the_client_without_a_program() {
        let (cfg, store) = served_config();
        assert_eq!(cfg.lc(&["show", "sp-1"]), Ok("answered show".to_string()));
        assert_eq!(store.0.lock().unwrap().as_slice(), &[vec!["show".to_string(), "sp-1".into()]]);
    }

    #[test]
    fn a_composed_verb_goes_over_the_client_and_an_unserved_one_is_refused_unsent() {
        let (cfg, store) = served_config();
        assert_eq!(cfg.lc(&["hold", "sp-1", "ask", "q", "reconciler"]), Ok("answered hold".to_string()));
        assert_eq!(cfg.lc(&["content", "list", "--all"]), Ok("answered content".to_string()));
        let err = cfg.lc(&["deliver", "pr-merged", "sp-1"]).unwrap_err();
        assert!(err.contains("refused") && err.contains("/nonexistent/spira-lc"), "{err}");
        assert_eq!(store.0.lock().unwrap().len(), 2, "the unserved verb was never sent");
    }

    #[test]
    fn a_served_lc_remedy_step_goes_over_the_client() {
        let (cfg, store) = served_config();
        let program = cfg.seams.spira_lc.clone();
        perform(&cfg, &mut AlertedSinceMap::new(), vec![Act::Remedy { key: "k".into(), steps: vec![(program, vec!["requeue-orphans".into(), "--actor".into(), "reconciler".into(), "--apply".into()])] }]);
        assert_eq!(store.0.lock().unwrap().as_slice(), &[vec!["requeue-orphans".to_string(), "--actor".into(), "reconciler".into(), "--apply".into()]]);
    }

    #[test]
    fn free_space_is_read_without_df() {
        assert!(free_pct("/").is_some_and(|p| (0..=100).contains(&p)));
        assert_eq!(free_pct("/no/such/path/at/all"), None);
    }

    /// A world answered entirely by the host. Every program a `Seams` names is a script that
    /// records its name, so a launch is visible; `None` host means the pass launches.
    struct Quiet {
        asked: std::sync::Mutex<Vec<String>>,
        server: Option<Option<String>>,
    }

    impl Host for Quiet {
        fn unit_state(&self, _: &str, _: &str) -> Result<String, String> {
            Ok("active".into())
        }
        fn list_enable(&self) -> String {
            String::new()
        }
        fn free_pct(&self, _: &str) -> Option<i64> {
            Some(90)
        }
        fn lc(&self, argv: &[&str]) -> Result<String, String> {
            self.asked.lock().unwrap().push(argv.join(" "));
            match argv {
                ["plan", "items", ..] => Ok("notice\n{\"watermark\":1,\"items\":[{\"id\":\"sp-epic\"}]}".into()),
                ["plan", "edges", ..] => Ok(r#"{"watermark":1,"edges":[{"issue_id":"sp-kid","depends_on_id":"sp-epic","type":"blocks"},{"issue_id":"sp-sub","depends_on_id":"sp-epic","type":"parent-child"}]}"#.into()),
                _ => Err("unexpected".into()),
            }
        }
        fn certified_list(&self, _: &Path) -> String {
            String::new()
        }
        fn config(&self, argv: &[&str]) -> String {
            match argv {
                ["rune", "task"] => "builder\n".into(),
                ["rune", "lane"] => "czar\n".into(),
                ["rune", "partitions"] => "plan\tpoison\n".into(),
                ["rune", "for-labels", _] => "builder\n".into(),
                _ => String::new(),
            }
        }
        fn stranded_runners(&self, _: &Path) -> String {
            String::new()
        }
        fn send_mail(&self, _: &Mail) -> Result<(), String> {
            Ok(())
        }
        fn dwarf_count(&self, rune: &str) -> Option<i64> {
            Some(if rune == "builder" { 2 } else { 1 })
        }
        fn ready_count(&self, labels: &str, exclude: &str) -> Option<i64> {
            assert_eq!((labels, exclude), ("plan", "poison"));
            Some(5)
        }
        fn tmux_server(&self) -> Option<Option<String>> {
            self.server.clone()
        }
        fn ref_sha(&self, _: &Path, name: &str) -> Ref {
            match name {
                "loose-tag" => Ref::Object("a".repeat(40)),
                _ => Ref::Unknown,
            }
        }
    }

    /// A config whose every program is a script that appends its name and argv to the returned
    /// log and answers as a healthy world would.
    fn tripwired(dir: &Path, host: Option<Arc<dyn Host>>) -> (Config, PathBuf) {
        let log = dir.join("launched");
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let log_text = log.display().to_string();
        let bin_text = bin.display().to_string();
        let seams = Seams::resolving(|name| {
            let script = format!("{bin_text}/{name}");
            let body = format!("#!/bin/sh\necho \"{name} $*\" >> {log_text}\ncase \"$*\" in\n*list-panes*) echo health;;\n*rev-parse*) echo {SHA};;\n\"rune task\"*|\"rune for-labels\"*) echo builder;;\n\"rune lane\"*) echo czar;;\n\"rune partitions\"*) printf 'plan\\\\tpoison\\\\n';;\nesac\nexit 0\n", SHA = "c".repeat(40));
            testkit::write_exe(&script, &body);
            Ok(script)
        })
        .unwrap();
        let mut cfg = test_config();
        cfg.seams = seams;
        cfg.host = host;
        (cfg, log)
    }

    fn launched(log: &Path, program: &str) -> usize {
        launches(log).lines().filter(|l| l.split(' ').next() == Some(program)).count()
    }

    fn launches(log: &Path) -> String {
        fs::read_to_string(log).unwrap_or_default()
    }

    fn quiet(server: Option<Option<String>>) -> Arc<dyn Host> {
        Arc::new(Quiet { asked: Default::default(), server })
    }

    #[test]
    fn a_quiet_world_launches_no_program_over_ten_ticks_and_the_unhosted_pass_does() {
        let dir = scratch_dir("quiet-ticks");
        let (cfg, log) = tripwired(&dir, Some(quiet(Some(Some("7:1.0".into())))));
        let mut cfg = cfg.with_memo(Arc::default());
        for tick in 0..10 {
            cfg.now_secs = 1_000 + tick * 60;
            let fleet = fleet_readings(&cfg);
            assert_eq!(fleet.live_total, 3);
            assert_eq!(fleet.rows, vec![("plan".to_string(), 5, 2, true)]);
            assert_eq!(observe_cockpit(&cfg).raw, RawStatus::Satisfied);
            let edges = observe_epic_edges(&cfg);
            assert_eq!(edges.len(), 1, "{edges:?}");
            assert_eq!(edges[0].key, "epic-blocks:sp-kid:sp-epic");
            assert_eq!(cfg.ref_read(Path::new("/r"), "loose-tag"), Ref::Sha("c".repeat(40)), "the peel is git's, once");
        }
        let seen = launches(&log);
        assert_eq!(launched(&log, "git"), 1, "one peel for the one tag sha, never again: {seen}");
        assert_eq!(launched(&log, "tmux"), cfg.cockpit_sessions.len() + 2, "one look at the sessions and the panes, on the first tick only: {seen}");
        assert_eq!(seen.lines().count(), 1 + cfg.cockpit_sessions.len() + 2, "no strand, spira-claim or bead launch: {seen}");

        let oracle_dir = scratch_dir("quiet-oracle");
        let (bare, log) = tripwired(&oracle_dir, None);
        let _ = fleet_readings(&bare);
        let _ = observe_cockpit(&bare);
        for program in ["strand", "spira-claim", "tmux"] {
            assert!(launched(&log, program) > 0, "the unhosted pass must launch {program}: {}", launches(&log));
        }
    }

    #[test]
    fn a_healthy_cockpit_is_asked_of_tmux_once_per_window_and_a_down_server_is_never_held() {
        let dir = scratch_dir("cockpit-window");
        let (cfg, log) = tripwired(&dir, Some(quiet(Some(Some("7:1.0".into())))));
        let mut cfg = cfg.with_memo(Arc::default());
        for now in [0, 60, 599] {
            cfg.now_secs = now;
            let _ = observe_cockpit(&cfg);
        }
        let first = launched(&log, "tmux");
        assert!(first >= 1, "the first sight of a server asks tmux");
        cfg.now_secs = 601;
        let _ = observe_cockpit(&cfg);
        assert!(launched(&log, "tmux") > first, "a window later it asks again");

        let down_dir = scratch_dir("cockpit-down");
        let (down, log) = tripwired(&down_dir, Some(quiet(Some(None))));
        let down = down.with_memo(Arc::default());
        for _ in 0..3 {
            assert!(matches!(observe_cockpit(&down).raw, RawStatus::Gap { observed, .. } if observed == "no server"));
        }
        assert_eq!(launches(&log), "", "a server the host knows is down needs no tmux to say so");
    }

    fn test_config() -> Config {
        Config {
            seams: Seams::bare(),
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
            queue_dir: PathBuf::from("/dev/null"),
            repo_map: None,
            releases_dir: PathBuf::new(),
            cockpit_sessions: vec!["brain".into(), "hunk".into(), "chat".into()],
            cockpit_mail: String::new(),
            dolt_data: String::new(),
            max_live_dwarves: String::new(),
            lanes_max_live: String::new(),
            max_dwarves: String::new(),
            disk_floor_pct: 15,
            untagged_images_max: 40,
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
            breaker: Err("not configured".into()),
            host: None,
            lc_client: None,
            skip_units: Vec::new(),
            memo: Arc::default(),
        }
    }

    #[test]
    fn a_sequence_remedy_describes_and_runs_every_step() {
        let r = Remedy::Sequence(vec![("a".into(), vec!["1".into()]), ("b".into(), vec![])]);
        assert_eq!(r.describe(), "a 1 ; b ");
        assert_eq!(r.steps().len(), 2);
    }
}
