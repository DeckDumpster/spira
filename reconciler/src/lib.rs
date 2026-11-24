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

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
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
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use spira_store::lc_call::LcClient;
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
    pub store_gc_unit: String,
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
            store_gc_unit: name("dolt-store-gc.service")?,
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
    /// Seconds a job may sit queued before its runner is judged gone; also how long after CI was last in flight the one trailing sweep waits.
    pub stranded_min_age: u64,
    /// Per repo, the epoch CI was last seen in flight while a trailing sweep is still owed.
    pub stranded_seen_path: PathBuf,
    /// Builder starts in flight per rune: begun by the sentinel, no dwarf live yet.
    pub starting: std::collections::BTreeMap<String, u64>,
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
    /// The shared read every `unit_state` answer comes from; `Err` when it is unanswered.
    fn unit_listing(&self) -> Result<(), String> {
        Ok(())
    }
    fn free_pct(&self, path: &str) -> Option<i64>;
    /// One spira-lc verb; `Err` on a refusal or a store that does not answer.
    fn lc(&self, argv: &[&str]) -> Result<String, String>;
    /// `<bead> <tip> <epoch>` per CERTIFIED bead with a `spira/<bead>` branch in `repo`.
    fn certified_list(&self, repo: &Path) -> String;
    /// `rune task|reactive|partitions|for-labels ...` and `repo landref <name>`.
    fn config(&self, argv: &[&str]) -> String;
    /// `Err` when a listing it reads did not answer: nothing can be said of stranded runs.
    fn stranded_runners(&self, repo: &Path) -> Result<String, String>;
    /// Whether the harness has forge CI in flight for `repo_name`: an open forge batch or an open publish record.
    fn forge_ci_in_flight(&self, _repo_name: &str) -> Result<bool, String> {
        Err("this host cannot tell".into())
    }
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
    /// The runes the sentinel's last call pass left uncalled with ready work, each with its reason; `None` when this host cannot tell, `Err` when that pass cannot be read.
    fn uncalled(&self) -> Option<Result<BTreeMap<String, String>, String>> {
        None
    }
    /// Claimable beads under a partition as `(id, updated_at)`; `None` when this host cannot list them without `spira-claim`.
    fn ready_rows(&self, _labels: &str, _exclude: &str) -> Option<Vec<(String, String)>> {
        None
    }
    /// Whether a leg of `batch`'s round in `repo` is running; true when that cannot be told.
    fn round_legs_live(&self, _repo: &str, _batch: &str) -> bool {
        true
    }
    /// The tmux server: `Some(Some(identity))` when one answers, `Some(None)` when it is certainly down, `None` when only tmux can say.
    fn tmux_server(&self) -> Option<Option<String>> {
        None
    }
}

pub use spira_config::git_refs::Ref;

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
        "SPIRA_STRANDED_RUNNER_MIN_AGE",
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
            stranded_min_age: parsed::<u64>(get, "SPIRA_STRANDED_RUNNER_MIN_AGE")?,
            stranded_seen_path: paths::stranded_seen(&spira_run),
            starting: Default::default(),
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

    fn unit_listing(&self) -> Result<(), String> {
        match &self.host {
            Some(h) => h.unit_listing(),
            None => Ok(()),
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

    /// `requeue-orphans --apply` spares the rounds whose legs are live; with the dry run
    /// unreadable it applies nothing.
    fn orphan_apply_argv(&self, argv: &[&str]) -> Option<Vec<String>> {
        let dry: Vec<&str> = argv.iter().copied().filter(|a| *a != "--apply").collect();
        let seen: serde_json::Value = serde_json::from_str(self.lc(&dry).ok()?.trim()).ok()?;
        let mut out: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
        let repos = seen.get("repos").and_then(|r| r.as_object())?;
        for (batch, repo) in repos {
            let live = repo.as_str().is_some_and(|repo| self.host.as_ref().is_none_or(|h| h.round_legs_live(repo, batch)));
            if live {
                out.extend(["--live-round".to_string(), batch.clone()]);
            }
        }
        Some(out)
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

    /// The claimable beads under a partition, one read; an unreadable answer is no beads.
    fn ready_rows(&self, labels: &str, exclude: &str) -> Vec<(String, String)> {
        match self.host.as_ref().and_then(|h| h.ready_rows(labels, exclude)) {
            Some(rows) => rows,
            None => parse_ready_rows(&run_cmd(&self.seams.spira_claim, &["ready-count", labels, exclude, "--json"])),
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

    fn stranded_runners(&self, repo: &Path) -> Result<String, String> {
        match &self.host {
            Some(h) => h.stranded_runners(repo),
            None => run_cmd_stdout(&self.seams.forge, &["stranded-runners", &repo.to_string_lossy()]),
        }
    }

    fn forge_ci_in_flight(&self, repo_name: &str) -> Result<bool, String> {
        match &self.host {
            Some(h) => h.forge_ci_in_flight(repo_name),
            None => Err("no host".into()),
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
    /// Asks the sentinel's call pass to run, for a gap that is a refill it has not yet made.
    CallWanted,
    Command { program: String, args: Vec<String> },
    /// A command whose act the host records as `note` once it has run.
    Noted { note: String, program: String, args: Vec<String> },
    /// Every step runs, in order, whether or not an earlier one succeeded.
    Sequence(Vec<(String, Vec<String>)>),
}

impl Remedy {
    pub fn steps(&self) -> Vec<(&str, Vec<&str>)> {
        match self {
            Remedy::Escalate | Remedy::CallWanted => Vec::new(),
            Remedy::Command { program, args } | Remedy::Noted { program, args, .. } => vec![(program.as_str(), args.iter().map(String::as_str).collect())],
            Remedy::Sequence(steps) => steps
                .iter()
                .map(|(p, a)| (p.as_str(), a.iter().map(String::as_str).collect()))
                .collect(),
        }
    }

    pub fn note(&self) -> Option<String> {
        match self {
            Remedy::Noted { note, .. } => Some(note.clone()),
            _ => None,
        }
    }

    pub fn describe(&self) -> String {
        if matches!(self, Remedy::CallWanted) {
            return "asked the sentinel's call pass to run (call.wanted)".into();
        }
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

/// Stdout when the program exited zero; the reason otherwise.
pub fn run_cmd_stdout(program: &str, args: &[&str]) -> Result<String, String> {
    let out = spira_config::bounded::bounded(program).args(args).stderr(Stdio::null()).output().map_err(|e| format!("{program}: {e}"))?;
    if !out.status.success() {
        return Err(format!("{program} {} exited {}", args.join(" "), out.status));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("{program}: {e}"))
}

const WAIT_CANCEL_FLAG: &str = "--wait-cancel";
const WAIT_CANCEL_SECS: u64 = 120;
const WAIT_CANCEL_STEP_SECS: &str = "150";

/// A remedy step that waits on a cancel is given the wait plus the calls around it; every other
/// step keeps the plain call deadline.
fn remedy_step_deadline(args: &[&str]) -> &'static str {
    if args.contains(&WAIT_CANCEL_FLAG) { WAIT_CANCEL_STEP_SECS } else { spira_config::bounded::CALL_TIMEOUT_SECS }
}

/// One remedy step, `Err` naming its program, args and how it failed.
pub fn run_remedy_step(program: &str, args: &[&str]) -> Result<(), String> {
    let said = |why: String| format!("reconciler: remedy step {program} {} failed: {why}", args.join(" "));
    let status = spira_config::bounded::bounded_for(program, remedy_step_deadline(args))
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| said(e.to_string()))?;
    match status.code() {
        Some(0) => Ok(()),
        Some(c) => Err(said(format!("exit {c}"))),
        None => Err(said("killed by a signal".into())),
    }
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

fn run_cmd_err(program: &str, args: &[&str], deadline_secs: u64) -> Result<(), String> {
    let said = |why: String| format!("{} {}: {}", program, args.join(" "), why);
    let out = spira_config::bounded::bounded_for(program, &deadline_secs.to_string())
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| said(e.to_string()))?;
    if out.status.success() {
        return Ok(());
    }
    let code = match out.status.code() {
        Some(124) => format!("exit 124 (hit the {deadline_secs}s deadline)"),
        Some(c) => format!("exit {c}"),
        None => "killed by a signal".to_string(),
    };
    Err(said(format!("{code}: {}", String::from_utf8_lossy(&out.stderr).trim())))
}

fn clear_main_health_tree(git: &str, repo: &str, tree: &Path, deadline_secs: u64) -> Result<(), String> {
    let t = tree.to_string_lossy();
    let _ = run_cmd_err(git, &["-C", repo, "worktree", "unlock", &t], deadline_secs);
    let _ = run_cmd_err(git, &["-C", repo, "worktree", "remove", "--force", "--force", &t], deadline_secs);
    let _ = fs::remove_dir_all(tree);
    run_cmd_err(git, &["-C", repo, "worktree", "prune"], deadline_secs)
}

fn fresh_main_health_tree(git: &str, repo: &str, tree: &Path, sha: &str, deadline_secs: u64) -> Result<(), String> {
    clear_main_health_tree(git, repo, tree, deadline_secs)?;
    // batch-job: a full checkout of the spira tree, bounded by SPIRA_MAIN_HEALTH_FENCE_DEADLINE_SECS
    run_cmd_err(git, &["-C", repo, "worktree", "add", "--detach", &tree.to_string_lossy(), sha], deadline_secs)
}

// ──────────────────────────────────────────────────────────────────────────────
// Repo map — copied from czar-pass's own reader; the two binaries share the
// file's format but not a crate, so this stays a small, literal copy rather than a new
// cross-crate dependency for six lines of parsing.
// ──────────────────────────────────────────────────────────────────────────────

fn repo_map_rows(repo_map: &Option<PathBuf>) -> Vec<(String, PathBuf)> {
    let Some(content) = repo_map.as_ref().and_then(|p| fs::read_to_string(p).ok()) else { return Vec::new() };
    content
        .lines()
        .map(str::trim)
        .filter(|t| !t.starts_with('#') && !t.is_empty())
        .filter_map(|t| {
            let parts: Vec<&str> = t.splitn(6, '|').collect();
            (parts.len() >= 2 && !parts[1].trim().is_empty()).then(|| (parts[0].trim().to_string(), PathBuf::from(parts[1].trim())))
        })
        .collect()
}

fn repo_root(name: &str, repo_map: &Option<PathBuf>) -> Option<PathBuf> {
    repo_map_rows(repo_map).into_iter().find(|(n, _)| n == name).map(|(_, p)| p)
}

fn repo_map_names(repo_map: &Option<PathBuf>) -> Vec<String> {
    let mut names: Vec<String> = repo_map_rows(repo_map).into_iter().map(|(n, _)| n).collect();
    names.sort();
    names.dedup();
    names
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
    matches!(cfg.lc(&["show", owner]).and_then(|out| spira_store::lc_state::parse_show(&out)), Ok(Some(r)) if r.state == "LANDED")
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
    let mut to_read = Vec::new();
    for unit in unit_names.into_iter().filter(|u| !cfg.skip_units.iter().any(|s| s == u)) {
        if let Some((subject, reason)) = suspension_of(&ctrl, &cfg.instance, unit) {
            checks.push(Check {
                key: unit_key(cfg, unit),
                raw: RawStatus::Deliberate { reason: format!("suspended ({}): {}", subject, reason) },
                remedy: Remedy::Escalate,
            });
            continue;
        }
        to_read.push(unit);
    }
    match cfg.unit_listing() {
        Err(e) => {
            checks.push(Check {
                key: "units:listing".into(),
                raw: RawStatus::Unobservable { reason: format!("{e}; {} units not observed", to_read.len()) },
                remedy: Remedy::Escalate,
            });
            return checks;
        }
        Ok(()) => checks.push(Check { key: "units:listing".into(), raw: RawStatus::Satisfied, remedy: Remedy::Escalate }),
    }
    for unit in to_read {
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
    /// `(labels, ready beads as (id, updated_at), live, is_task, runes)` per builder partition.
    rows: Vec<FleetRow>,
    live_reactive: i64,
    live_total: i64,
    max_live: Option<i64>,
    pool: Option<i64>,
}

/// One partition: `(labels, ready, live, is_task, rune names)`.
type FleetRow = (String, Vec<ReadyBead>, i64, bool, Vec<String>);

/// A claimable bead: `(id, updated_at)`.
pub type ReadyBead = (String, String);

pub fn parse_ready_rows(json: &str) -> Vec<(String, String)> {
    let Ok(serde_json::Value::Array(rows)) = serde_json::from_str(json.trim()) else { return Vec::new() };
    let text = |r: &serde_json::Value, k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    rows.iter().map(|r| (text(r, "id"), text(r, "updated_at"))).filter(|(id, _)| !id.is_empty()).collect()
}

const GAP_BEADS_NAMED: usize = 5;

fn age_label(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// `"<n> (<id> updated <age> ago, …)"`: the beads that make a partition's desired count, at most five, then `+N more`.
fn desired_with_beads(desired: i64, beads: &[(String, String)], now_secs: u64) -> String {
    let named: Vec<String> = beads
        .iter()
        .take(GAP_BEADS_NAMED)
        .map(|(id, updated)| match clock::epoch_of_iso(updated) {
            Some(at) => format!("{id} updated {} ago", age_label(now_secs.saturating_sub(at))),
            None => id.clone(),
        })
        .collect();
    if named.is_empty() {
        return desired.to_string();
    }
    let more = beads.len().saturating_sub(GAP_BEADS_NAMED);
    let tail = if more > 0 { format!(", +{more} more") } else { String::new() };
    format!("{desired} ({}{tail})", named.join(", "))
}

fn count_of(out: &str) -> i64 {
    out.trim().parse().unwrap_or(0)
}

/// Observed fleet occupancy from the same tools the sentinel decides calls with: the runes
/// supplies the partitions, `spira-claim` the claimable work under each, `strand` the dwarves live.
fn fleet_readings(cfg: &Config) -> FleetReadings {
    let words = |out: String| out.split_whitespace().map(String::from).collect::<Vec<_>>();
    let task_runes = words(cfg.config(&["rune", "task"]));
    let live_of = |rune: &str| cfg.dwarf_count(rune) + cfg.starting.get(rune).map_or(0, |n| *n as i64);

    let reactive_runes: Vec<String> = words(cfg.config(&["rune", "reactive"])).into_iter().filter(|f| !task_runes.contains(f)).collect();
    let live_reactive: i64 = reactive_runes.iter().map(|f| live_of(f)).sum();
    let live_total = live_reactive + task_runes.iter().map(|f| live_of(f)).sum::<i64>();

    let mut rows = Vec::new();
    for line in cfg.config(&["rune", "partitions"]).lines() {
        let (labels, exclude) = line.split_once('\t').unwrap_or((line, ""));
        if labels.is_empty() {
            continue;
        }
        let ready = cfg.ready_rows(labels, exclude);
        let runes = cfg.config(&["rune", "for-labels", labels]);
        let runes: Vec<&str> = runes.split_whitespace().collect();
        let live = runes.iter().map(|f| live_of(f)).sum();
        let is_task = runes.iter().any(|f| task_runes.iter().any(|t| t == f));
        rows.push((labels.to_string(), ready, live, is_task, runes.iter().map(|r| r.to_string()).collect()));
    }

    FleetReadings {
        rows,
        live_reactive,
        live_total,
        max_live: cfg.max_live_dwarves.trim().parse().ok(),
        pool: cfg.max_dwarves.trim().parse().ok(),
    }
}

fn fleet_gap(desired_live: i64, ready: &[ReadyBead], live: i64, now_secs: u64) -> RawStatus {
    RawStatus::Gap { desired: desired_with_beads(desired_live, ready, now_secs), observed: live.to_string(), since_hint: None }
}

fn observe_fleet(cfg: &Config) -> Vec<Check> {
    let FleetReadings { rows, live_reactive, live_total, max_live, pool } = fleet_readings(cfg);

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

    let uncalled = if rows.is_empty() { None } else { cfg.host.as_ref().and_then(|h| h.uncalled()) };
    for (labels, ready, live, is_task, runes) in rows {
        let headroom = (max_live - live_reactive).max(0);
        // A task partition draws from the pool the sentinel bounds task starts by, so the pool
        // bounds what is desired of it; pool 0 is an operator pause and leaves nothing desired.
        // A reactive partition draws outside the pool.
        let free = if is_task { sentinel::plan::ck7_pool(pool, live_total - live_reactive) } else { None };
        let desired_live = (ready.len() as i64).min(headroom).min(free.map_or(i64::MAX, |f| live + f));
        let slot_free = live_total < max_live && free.is_none_or(|f| f > 0);
        let raw = if live >= desired_live || !slot_free {
            RawStatus::Satisfied
        } else {
            match &uncalled {
                Some(Err(e)) => RawStatus::Unobservable { reason: e.clone() },
                Some(Ok(left)) => match runes.iter().find_map(|r| left.get(r).map(|why| format!("{r}: {why}"))) {
                    Some(reason) => RawStatus::Deliberate { reason },
                    None => fleet_gap(desired_live, &ready, live, cfg.now_secs),
                },
                None => fleet_gap(desired_live, &ready, live, cfg.now_secs),
            }
        };
        checks.push(Check { key: format!("fleet:{}", labels), raw, remedy: Remedy::CallWanted });
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
    if !Path::new(&cfg.spira_home).ancestors().any(|dir| dir.join(".git").exists()) {
        return None;
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
// and prunes dangling podman images and unattached volumes (the store's own directory is
// instead collected online by starting `dolt-store-gc.service` without waiting on it); if that still does not clear the
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

/// The store's data directory is RAM-backed: reaping build output and podman storage frees
/// nothing there, so its remedy is an online collection of the live store.
fn disk_remedy(cfg: &Config, path: &str) -> Remedy {
    if !cfg.dolt_data.is_empty() && Path::new(path) == Path::new(&cfg.dolt_data) {
        let args = ["--user", "start", "--no-block", cfg.seams.store_gc_unit.as_str()].map(String::from).to_vec();
        return Remedy::Command { program: cfg.seams.systemctl.clone(), args };
    }
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
            RawStatus::Gap { .. } => disk_remedy(cfg, &path),
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

struct StrandedRun {
    run: String,
    job: String,
    label: String,
    queued: String,
    runner: String,
}

fn stranded_runs(out: &str) -> Vec<StrandedRun> {
    let mut runs: Vec<StrandedRun> = Vec::new();
    for line in out.lines().filter(|l| l.starts_with("STRANDED ")) {
        let field = |name: &str| line.split_whitespace().find_map(|w| w.strip_prefix(name)).unwrap_or_default().to_string();
        let run = field("run=");
        if run.is_empty() || runs.iter().any(|r| r.run == run) {
            continue;
        }
        let job = line.split_once("job=\"").and_then(|(_, rest)| rest.split_once("\" label=")).map(|(j, _)| j.to_string()).unwrap_or_default();
        runs.push(StrandedRun { run, job, label: field("label="), queued: field("queued="), runner: field("runner=") });
    }
    runs
}

fn load_stranded_seen(path: &Path) -> std::collections::BTreeMap<String, u64> {
    fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn observe_queue_stranded_runs(cfg: &Config) -> Vec<Check> {
    let mut checks = Vec::new();
    let mut seen = load_stranded_seen(&cfg.stranded_seen_path);
    let before = seen.clone();
    for repo_name in repo_map_names(&cfg.repo_map) {
        let Some(repo_path) = repo_root(&repo_name, &cfg.repo_map) else { continue };
        match cfg.forge_ci_in_flight(&repo_name) {
            Err(reason) => {
                checks.push(Check {
                    key: format!("queue-stranded-runs:{repo_name}"),
                    raw: RawStatus::Unobservable { reason: format!("cannot tell whether {repo_name} has forge CI in flight: {reason}") },
                    remedy: Remedy::Escalate,
                });
                continue;
            }
            Ok(true) => {
                seen.insert(repo_name.clone(), cfg.now_secs);
            }
            Ok(false) => match seen.get(&repo_name) {
                None => continue,
                Some(&last) if cfg.now_secs.saturating_sub(last) < cfg.stranded_min_age => continue,
                Some(_) => {
                    seen.remove(&repo_name);
                }
            },
        }
        let repo_str = repo_path.to_string_lossy().to_string();
        let out = match cfg.stranded_runners(&repo_path) {
            Ok(out) => out,
            Err(reason) => {
                checks.push(Check {
                    key: format!("queue-stranded-runs:{repo_name}"),
                    raw: RawStatus::Unobservable { reason: format!("cannot tell whether {repo_name} has stranded runs: {reason}") },
                    remedy: Remedy::Escalate,
                });
                continue;
            }
        };
        for StrandedRun { run, job, label, queued, runner } in stranded_runs(&out) {
            checks.push(Check {
                key: format!("{STRANDED_RUN_PREFIX}{repo_name}:{run}"),
                raw: RawStatus::Gap {
                    desired: "every queued job's runner label is carried by an online runner".into(),
                    observed: format!("run {run} queued on {label}, runner gone"),
                    since_hint: None,
                },
                remedy: Remedy::Noted {
                    note: format!("repo={repo_name} run={run} job=\"{job}\" label={label} queued={queued} runner={runner}"),
                    program: cfg.seams.forge.clone(),
                    args: vec!["workflow-rerun".into(), repo_str.clone(), run, WAIT_CANCEL_FLAG.into(), WAIT_CANCEL_SECS.to_string()],
                },
            });
        }
    }
    if seen != before {
        if let Err(e) = fs::write(&cfg.stranded_seen_path, serde_json::to_string(&seen).unwrap_or_else(|_| "{}".into())) {
            log_print(cfg, &format!("reconciler: cannot record stranded-sweep state in {}: {e}", cfg.stranded_seen_path.display()));
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

fn junk_row_ids(rows: &[spira_store::lc_state::Row], store_ids: &HashSet<String>) -> Vec<String> {
    rows.iter()
        .filter(|r| r.state == "READY" && !store_ids.contains(&r.bead_id))
        .map(|r| r.bead_id.clone())
        .collect()
}

const JUNK_IDS_PER_READ: usize = 100;

/// Which of `ids` the store holds a bead for: one bounded `content list --id` read per chunk.
fn store_bead_ids(cfg: &Config, ids: &[String]) -> Result<HashSet<String>, String> {
    let mut known = HashSet::new();
    for chunk in ids.chunks(JUNK_IDS_PER_READ) {
        let out = cfg.lc(&["content", "list", "--all", "--brief", "--json", "--limit", "0", "--id", &chunk.join(",")])?;
        let rows: Vec<serde_json::Value> = serde_json::from_str(out.trim()).map_err(|e| format!("content list: unparsable json: {e}"))?;
        known.extend(rows.iter().filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(String::from)));
    }
    if known.is_empty() && !ids.is_empty() {
        return Err(format!("the store knows none of {} READY ids", ids.len()));
    }
    Ok(known)
}

fn observe_junk_rows(cfg: &Config) -> Vec<Check> {
    let unobservable = |reason: String| {
        vec![Check {
            key: "junk-row".into(),
            raw: RawStatus::Unobservable { reason },
            remedy: Remedy::Escalate,
        }]
    };
    let rows = match cfg.lc(&["list", "--state", "READY"]).and_then(|out| spira_store::lc_state::parse_rows(&out)) {
        Ok(rows) => rows,
        Err(e) => return unobservable(e),
    };
    let ids: Vec<String> = rows.iter().map(|r| r.bead_id.clone()).collect();
    let store_ids = match store_bead_ids(cfg, &ids) {
        Ok(ids) => ids,
        Err(e) => return unobservable(e),
    };
    let junk = junk_row_ids(&rows, &store_ids);
    log_print(cfg, &format!("reconciler: invariant junk-row: {} READY row(s) read, {} without a bead", rows.len(), junk.len()));
    junk.into_iter()
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

fn fence_judge(program: &str) -> Result<String, String> {
    fs::canonicalize(program).map(|p| p.to_string_lossy().into_owned()).map_err(|e| format!("cannot resolve the fence program {program}: {e}"))
}

fn cached_verdict(entry: &serde_json::Value, class: &str, sha: &str, judge: &str) -> Option<RawStatus> {
    if entry.get("sha")?.as_str()? != sha || entry.get("judge")?.as_str()? != judge {
        return None;
    }
    match entry.get("observed").and_then(|o| o.as_str()) {
        None => Some(RawStatus::Satisfied),
        Some(observed) => Some(RawStatus::Gap {
            desired: format!("{} passes on {}", class, MAIN_REF),
            observed: observed.to_string(),
            since_hint: None,
        }),
    }
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
    let lint_pending = lint_awaiting_activation(cfg, &repo_str, &sha);
    let pending_check = lint_pending.map(|raw| Check { key: "main-health:lint".into(), raw, remedy: Remedy::Escalate });
    let lint_is_pending = pending_check.is_some();
    let judged = move |f: &Fence| !(lint_is_pending && f.class == "lint");
    let fences: Vec<Fence> = base_fences(cfg, "", &sha).into_iter().filter(judged).collect();
    let judges: std::collections::HashMap<&'static str, Result<String, String>> = fences.iter().map(|f| (f.class, fence_judge(&f.program))).collect();
    let cached = |cache: &serde_json::Map<String, serde_json::Value>, class: &str| -> Option<RawStatus> {
        let judge = judges.get(class)?.as_ref().ok()?;
        cached_verdict(cache.get(class)?, class, &sha, judge)
    };
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
    if let Err(e) = fresh_main_health_tree(git, &repo_str, &tree, &sha, cfg.fence_deadline_secs) {
        return unobservable(format!("cannot check out {} into {}: {}", sha, tree_str, e));
    }
    let mut checks: Vec<Check> = pending_check.into_iter().collect();
    for fence in base_fences(cfg, &tree_str, &sha).into_iter().filter(judged) {
        let judge = match &judges[fence.class] {
            Ok(j) => j.clone(),
            Err(reason) => {
                cache.remove(fence.class);
                checks.push(Check { key: format!("main-health:{}", fence.class), raw: RawStatus::Unobservable { reason: reason.clone() }, remedy: Remedy::Escalate });
                continue;
            }
        };
        if let Some(raw) = cached(&cache, fence.class) {
            checks.push(Check { key: format!("main-health:{}", fence.class), raw, remedy: Remedy::Escalate });
            continue;
        }
        let started = std::time::Instant::now();
        // batch-job: a fence over a whole checkout runs to SPIRA_MAIN_HEALTH_FENCE_DEADLINE_SECS (a hang guard), not the 5 s call deadline
        let out = spira_config::bounded::bounded_for(&judge, &cfg.fence_deadline_secs.to_string())
            .args(&fence.args)
            .current_dir(&tree)
            .stdin(Stdio::null())
            .output();
        let elapsed = started.elapsed().as_secs_f64();
        let log = paths::main_health_log(&cfg.spira_run, fence.class);
        let raw = match &out {
            Err(e) => RawStatus::Unobservable { reason: format!("{}: {}", judge, e) },
            Ok(o) if o.status.success() => RawStatus::Satisfied,
            Ok(o) => {
                let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
                let _ = fs::write(&log, text);
                match o.status.code() {
                    Some(1) => RawStatus::Gap {
                        desired: format!("{} passes on {}", fence.class, MAIN_REF),
                        observed: format!("{} red at {} by {} (output in {})", fence.class, sha, judge, log.display()),
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
                cache.insert(fence.class.to_string(), serde_json::json!({ "sha": sha, "judge": judge }));
            }
            RawStatus::Gap { observed, .. } => {
                cache.insert(fence.class.to_string(), serde_json::json!({ "sha": sha, "judge": judge, "observed": observed }));
            }
            _ => {
                cache.remove(fence.class);
            }
        }
        checks.push(Check { key: format!("main-health:{}", fence.class), raw, remedy: Remedy::Escalate });
    }
    let _ = fs::write(&cache_path, serde_json::Value::Object(cache).to_string());
    let _ = clear_main_health_tree(git, &repo_str, &tree, cfg.fence_deadline_secs);
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
    Remedy { key: String, steps: Vec<(String, Vec<String>)>, note: Option<String> },
    /// The redrive of the sentinel's call pass; only the scheduler can send it.
    CallWanted { key: String },
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

fn judge_pending(rules: &Rules, acts: &mut Vec<Act>, pending: &mut PendingMap, key: &str, after: f64, remedy: &Remedy) {
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
            if matches!(remedy, Remedy::CallWanted) {
                return;
            }
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
    judge_pending(rules, &mut acts, pending, &check.key, metric_now, &check.remedy);

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
                        if matches!(remedy, Remedy::CallWanted) {
                            acts.push(Act::CallWanted { key: check.key.clone() });
                        } else {
                            let steps = remedy.steps().into_iter().map(|(p, a)| (p.to_string(), a.into_iter().map(String::from).collect())).collect();
                            acts.push(Act::Remedy { key: check.key.clone(), steps, note: remedy.note() });
                        }
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
                        if argv.first() == Some(&"requeue-orphans") && argv.contains(&"--apply") {
                            match cfg.orphan_apply_argv(&argv) {
                                Some(probed) => {
                                    let _ = cfg.lc(&probed.iter().map(String::as_str).collect::<Vec<_>>());
                                }
                                None => log_print(cfg, "reconciler: requeue-orphans dry run unreadable; nothing applied"),
                            }
                            continue;
                        }
                        let _ = cfg.lc(&argv);
                        continue;
                    }
                    // FORK-EDGE: remedy program — declared remedies (systemctl enable/restart, forge, bead, queue) run as commands
                    if let Err(e) = run_remedy_step(program, &argv) {
                        log_print(cfg, &e);
                    }
                }
            }
            Act::CallWanted { key } => log_print(cfg, &format!("reconciler: {key} → call.wanted needs the scheduler; not sent")),
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
    let shadowed = is_shadowed(&rules.shadow_kinds, kind_of(key));
    let (fire, next) = should_alert(verdict, alerted.get(key).copied());
    if !shadowed {
        match next {
            Some(since) => alerted.insert(key.to_string(), since),
            None => alerted.remove(key),
        };
    }
    if !fire {
        return;
    }
    let subject = format!("RECONCILER: {} — {}", key, describe(verdict));
    let evidence = compose_alert(key, rules.now_secs, verdict, last_remedy);
    if shadowed {
        acts.push(Act::Log(format!("reconciler: {key} → escalation held (shadow)")));
        event(acts, "shadow-page", key, &subject, 1.0, None);
        return;
    }
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
        judge_pending(rules, &mut acts, pending, &key, if report.wanted.contains(&key) { 1.0 } else { 0.0 }, &Remedy::Escalate);
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
    let task = spira_store::lc_call::current_task();
    std::thread::scope(|scope| {
        let handles: Vec<_> = jobs
            .into_iter()
            .map(|(name, job)| {
                let task = task.clone();
                (name, scope.spawn(move || spira_store::lc_call::as_task_of(task.as_deref(), || observed(cfg, name, job))))
            })
            .collect();
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

pub const REMEDIES_LANE: &str = "remedies";

/// Why a pass must take no step: the world is halted, or the remedies lane is stopped on every
/// registered repo (cutover mode holds it while the halt stamp is lifted).
fn held(cfg: &Config) -> Option<String> {
    if world_halted(cfg) {
        return Some("reconciler: skipped — world is halted".to_string());
    }
    spira_config::scoped_stop::skip_if_stopped_registered(&cfg.spira_run, "reconciler", REMEDIES_LANE).unwrap_or_else(|e| Some(format!("reconciler: refused — {e}")))
}

/// One pass's reads: the groups, the suspensions due for a reminder, and the hold sweep (which
/// acts as it reads). Stamps the pass. `reminded` is the set of reminders already sent.
pub fn observe(cfg: &Config, reminded: &std::collections::BTreeSet<String>) -> Observation {
    if let Some(why) = held(cfg) {
        log_print(cfg, &why);
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

    if let Some(why) = held(&cfg) {
        log_print(&cfg, &why);
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

    #[test_id(tjabbj3)]
    #[test]
    fn seams_refuse_naming_spira_release_when_it_is_unset_or_lacks_a_tool() {
        let e = Seams::from_release(None).err().unwrap();
        assert!(e.contains("SPIRA_RELEASE is not set"), "{e}");
        let e = Seams::from_release(Some("nonexistent-release")).err().unwrap();
        assert!(e.contains("forge is not in SPIRA_RELEASE"), "{e}");
    }
    use std::time::SystemTime;

    // Real git in a TempDir: a locked registration is git's own state, which no model reproduces
    // (the exception law-a-unit-test-calls-no-binary allows, as queue/src/tests/round.rs uses).
    fn main_health_repo() -> (testkit::TempDir, String, String) {
        let d = testkit::TempDir::new("main-health-tree");
        let repo = d.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        testkit::git(&repo, &["init", "-q", "-b", "main"]);
        fs::write(repo.join("f"), "x").unwrap();
        testkit::git(&repo, &["add", "."]);
        testkit::git(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "x"]);
        let sha = testkit::git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
        (d, repo.to_string_lossy().to_string(), sha)
    }

    #[test_id(to15c29)]
    #[test]
    fn a_locked_half_initialised_registration_is_cleared_and_the_tree_added() {
        let (d, repo, sha) = main_health_repo();
        let tree = d.path().join("tree");
        let t = tree.to_string_lossy().to_string();
        testkit::git(Path::new(&repo), &["worktree", "add", "--detach", &t, &sha]);
        testkit::git(Path::new(&repo), &["worktree", "lock", "--reason", "initializing", &t]);
        fs::remove_dir_all(&tree).unwrap();
        fresh_main_health_tree("git", &repo, &tree, &sha, 60).unwrap();
        assert_eq!(testkit::git(&tree, &["rev-parse", "HEAD"]).trim(), sha);
    }

    #[test_id(t0sfc5t)]
    #[test]
    fn a_clean_path_is_added_and_a_second_pass_replaces_it() {
        let (d, repo, sha) = main_health_repo();
        let tree = d.path().join("tree");
        fresh_main_health_tree("git", &repo, &tree, &sha, 60).unwrap();
        fresh_main_health_tree("git", &repo, &tree, &sha, 60).unwrap();
        clear_main_health_tree("git", &repo, &tree, 60).unwrap();
        assert!(!tree.exists());
    }

    #[test_id(tgseq30)]
    #[test]
    fn a_failed_checkout_carries_gits_own_stderr() {
        let (d, repo, _) = main_health_repo();
        let tree = d.path().join("tree");
        let e = fresh_main_health_tree("git", &repo, &tree, "0123456789abcdef0123456789abcdef01234567", 60).unwrap_err();
        assert!(e.contains("fatal:") || e.contains("invalid reference"), "{e}");
    }

    fn two_builds() -> testkit::TempDir {
        let d = testkit::TempDir::new("judge-builds");
        for b in ["A", "B"] {
            fs::create_dir_all(d.path().join("releases").join(b).join("bin")).unwrap();
            fs::write(d.path().join("releases").join(b).join("bin/spira-lint"), "x").unwrap();
        }
        d
    }

    fn point_current(d: &testkit::TempDir, build: &str) {
        let link = d.path().join("releases/current");
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(d.path().join("releases").join(build), link).unwrap();
    }

    fn current_lint(d: &testkit::TempDir) -> String {
        d.path().join("releases/current/bin/spira-lint").to_string_lossy().into_owned()
    }

    #[test_id(ta01vrt)]
    #[test]
    fn a_red_from_one_build_is_rejudged_once_another_build_is_live() {
        let d = two_builds();
        point_current(&d, "A");
        let judge_a = fence_judge(&current_lint(&d)).unwrap();
        let entry = serde_json::json!({"sha": "S", "judge": judge_a, "observed": "lint red at S"});
        assert!(matches!(cached_verdict(&entry, "lint", "S", &judge_a), Some(RawStatus::Gap { .. })));
        point_current(&d, "B");
        let judge_b = fence_judge(&current_lint(&d)).unwrap();
        assert_ne!(judge_a, judge_b);
        assert!(cached_verdict(&entry, "lint", "S", &judge_b).is_none());
    }

    #[test_id(tl8ffgb)]
    #[test]
    fn a_green_is_a_hit_for_the_same_sha_and_judge_only() {
        let entry = serde_json::json!({"sha": "S", "judge": "j"});
        assert!(matches!(cached_verdict(&entry, "lint", "S", "j"), Some(RawStatus::Satisfied)));
        assert!(cached_verdict(&entry, "lint", "T", "j").is_none());
    }

    #[test_id(tecuaed)]
    #[test]
    fn an_entry_without_a_judge_is_a_miss() {
        let entry = serde_json::json!({"sha": "S"});
        assert!(cached_verdict(&entry, "lint", "S", "j").is_none());
    }

    #[test_id(tricx5k)]
    #[test]
    fn fence_judge_of_a_missing_program_names_it() {
        let e = fence_judge("nonexistent/bin/spira-lint").unwrap_err();
        assert!(e.contains("nonexistent/bin/spira-lint"), "{e}");
    }

    fn scratch_dir(name: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("reconciler-test-{}-{}", name, SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()))
    }

    #[test_id(tq0jsf2)]
    #[test]
    fn orphan_reading_is_a_gap_only_when_an_orphan_is_named() {
        let gap = orphan_reading(r#"{"orphans":["sp-a","sp-b"],"requeued":[],"failed":[]}"#);
        assert!(matches!(&gap, RawStatus::Gap { observed, .. } if observed == "sp-a sp-b"), "{gap:?}");
        assert_eq!(orphan_reading(r#"{"orphans":[],"requeued":[],"failed":[]}"#), RawStatus::Satisfied);
        assert!(matches!(orphan_reading(""), RawStatus::Unobservable { .. }));
        assert!(matches!(orphan_reading("{}"), RawStatus::Unobservable { .. }));
    }

    #[test_id(tch40l8)]
    #[test]
    fn repo_root_finds_the_named_repo() {
        let dir = scratch_dir("repo-root");
        let map = dir.join("repos");
        fs::write(&map, "spira | r/checkouts/spira | queue | origin/main | | \n").unwrap();
        assert_eq!(repo_root("spira", &Some(map)), Some(PathBuf::from("r/checkouts/spira")));
    }

    #[test_id(t0rvbpe)]
    #[test]
    fn repo_root_with_no_map_configured_is_none() {
        assert_eq!(repo_root("spira", &None), None);
    }

    #[test_id(t93wldb)]
    #[test]
    fn queue_repo_names_lists_only_directories() {
        let dir = scratch_dir("queue-repo-names");
        fs::create_dir_all(dir.join("spira")).unwrap();
        fs::write(dir.join("not-a-dir"), "x").unwrap();
        assert_eq!(queue_repo_names(&dir), vec!["spira".to_string()]);
    }

    #[test_id(t9lqubw)]
    #[test]
    fn queue_repo_names_missing_dir_is_empty() {
        let dir = scratch_dir("queue-repo-names-missing").join("does-not-exist");
        assert!(queue_repo_names(&dir).is_empty());
    }

    #[test_id(tv2dlw3)]
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

    #[test_id(tvo8hin)]
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

    #[test_id(tjpgp8x)]
    #[test]
    fn stranded_runs_reads_one_entry_per_run_and_ignores_other_lines() {
        let out = "STRANDED run=77 job=\"a b\" label=ci-r-77-1 queued=700s runner=none vmid=?\n\
                   STRANDED run=77 job=\"b\" label=ci-r-77-1 queued=700s runner=none vmid=?\n\
                   STRANDED run=78 job=\"a\" label=ci-r-78-2 queued=900s runner=x vmid=3\n\
                   noise run=99\n";
        let got: Vec<_> = stranded_runs(out).into_iter().map(|r| (r.run, r.job, r.label, r.queued, r.runner)).collect();
        let want = |a: &str, b: &str, c: &str, d: &str, e: &str| (a.to_string(), b.to_string(), c.to_string(), d.to_string(), e.to_string());
        assert_eq!(got, vec![want("77", "a b", "ci-r-77-1", "700s", "none"), want("78", "a", "ci-r-78-2", "900s", "x")]);
        assert!(stranded_runs("").is_empty());
    }

    #[test_id(tvutyjn)]
    #[test]
    fn junk_rows_are_ready_rows_without_a_bead() {
        let row = |id: &str, state: &str| spira_store::lc_state::Row {
            bead_id: id.into(),
            state: state.into(),
            ..Default::default()
        };
        let store: HashSet<String> = ["sp-real".to_string()].into();
        let rows = vec![row("sp-real", "READY"), row("prose key", "READY"), row("gone", "LANDED")];
        assert_eq!(junk_row_ids(&rows, &store), vec!["prose key".to_string()]);
    }

    #[test_id(t0hbyi2)]
    #[test]
    fn junk_rows_are_read_from_the_ready_rows_and_exactly_those_ids() {
        let dir = scratch_dir("junk-rows");
        let (mut cfg, _log) = tripwired(&dir, Some(quiet(None)));
        cfg.log = dir.join("reconciler.log");
        let checks = observe_junk_rows(&cfg);
        assert_eq!(checks.len(), 1, "{checks:?}");
        assert_eq!(checks[0].key, "junk-row:prose key");
        assert!(matches!(checks[0].raw, RawStatus::Gap { .. }));
        let logged = fs::read_to_string(&cfg.log).unwrap_or_default();
        assert!(logged.contains("invariant junk-row: 2 READY row(s) read, 1 without a bead"), "{logged}");
    }

    #[test_id(t6p4lx2)]
    #[test]
    fn a_content_read_that_knows_none_of_the_ready_ids_is_unobservable_not_a_gap_per_row() {
        let dir = scratch_dir("junk-rows-blank");
        let host: Arc<dyn Host> = Arc::new(Quiet { asked: Default::default(), server: None, blank_content: true, no_ready: false });
        let (cfg, _log) = tripwired(&dir, Some(host));
        let checks = observe_junk_rows(&cfg);
        assert_eq!(checks.len(), 1, "{checks:?}");
        assert!(matches!(&checks[0].raw, RawStatus::Unobservable { reason } if reason.contains("knows none of 2")), "{checks:?}");
    }

    #[test_id(tcz2qd0)]
    #[test]
    fn no_ready_rows_means_no_content_read_and_no_checks() {
        let dir = scratch_dir("junk-rows-none");
        let quiet = Arc::new(Quiet { asked: Default::default(), server: None, blank_content: false, no_ready: true });
        let host: Arc<dyn Host> = quiet.clone();
        let (cfg, _log) = tripwired(&dir, Some(host));
        assert!(observe_junk_rows(&cfg).is_empty());
        assert!(!quiet.asked.lock().unwrap().iter().any(|a| a.starts_with("content")));
    }

    #[test_id(ti46jp9)]
    #[test]
    fn reminder_due_reads_until_and_owner() {
        assert_eq!(reminder_due("sp-1", None, "2026-10-07", false), Due::NotDue);
        assert_eq!(reminder_due("sp-1", Some("2026-10-07"), "2026-10-07", false), Due::Until("2026-10-07".into()));
        assert_eq!(reminder_due("sp-1", Some("2026-10-08"), "2026-10-07", true), Due::OwnerLanded("sp-1".into()));
    }

    #[test_id(tjcogyh)]
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

    impl spira_store::lc_call::Transport for Store {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
            self.0.lock().unwrap().push(argv.to_vec());
            let out = match argv.iter().any(|a| a == "--apply") {
                false if argv[0] == "requeue-orphans" => r#"{"orphans":["sp-a","sp-b"],"repos":{"b-live":"r","b-dead":"r"}}"#.to_string(),
                _ => format!("answered {}", argv[0]),
            };
            Ok(spira_store::lc_call::Reply { code: 0, out, err: String::new(), read_ended: false })
        }
    }

    fn served_config() -> (Config, Arc<Store>) {
        let store = Arc::new(Store(std::sync::Mutex::default()));
        let mut cfg = test_config();
        cfg.seams.spira_lc = "nonexistent/spira-lc".into();
        cfg.lc_client = Some(LcClient::over(Arc::clone(&store) as Arc<dyn spira_store::lc_call::Transport>, true));
        (cfg, store)
    }

    #[test_id(t1yjqrj)]
    #[test]
    fn a_served_lc_read_goes_over_the_client_without_a_program() {
        let (cfg, store) = served_config();
        assert_eq!(cfg.lc(&["show", "sp-1"]), Ok("answered show".to_string()));
        assert_eq!(store.0.lock().unwrap().as_slice(), &[vec!["show".to_string(), "sp-1".into()]]);
    }

    #[test_id(t54b943)]
    #[test]
    fn a_composed_verb_goes_over_the_client_and_an_unserved_one_is_refused_unsent() {
        let (cfg, store) = served_config();
        assert_eq!(cfg.lc(&["hold", "sp-1", "ask", "q", "reconciler"]), Ok("answered hold".to_string()));
        assert_eq!(cfg.lc(&["content", "list", "--all"]), Ok("answered content".to_string()));
        let err = cfg.lc(&["deliver", "pr-merged", "sp-1"]).unwrap_err();
        assert!(err.contains("refused") && err.contains("nonexistent/spira-lc"), "{err}");
        assert_eq!(store.0.lock().unwrap().len(), 2, "the unserved verb was never sent");
    }

    #[test_id(tdagg7n)]
    #[test]
    fn a_served_lc_remedy_step_goes_over_the_client() {
        let (cfg, store) = served_config();
        let program = cfg.seams.spira_lc.clone();
        perform(&cfg, &mut AlertedSinceMap::new(), vec![Act::Remedy { note: None, key: "k".into(), steps: vec![(program, vec!["show".into(), "sp-1".into()])] }]);
        assert_eq!(store.0.lock().unwrap().as_slice(), &[vec!["show".to_string(), "sp-1".into()]]);
    }

    struct Ci {
        in_flight: std::collections::BTreeMap<&'static str, Result<bool, String>>,
        asked: std::sync::Mutex<Vec<String>>,
    }

    impl Host for Ci {
        fn unit_state(&self, _: &str, _: &str) -> Result<String, String> {
            Ok("active".into())
        }
        fn list_enable(&self) -> String {
            String::new()
        }
        fn free_pct(&self, _: &str) -> Option<i64> {
            Some(90)
        }
        fn lc(&self, _: &[&str]) -> Result<String, String> {
            Err("unexpected".into())
        }
        fn certified_list(&self, _: &Path) -> String {
            String::new()
        }
        fn config(&self, _: &[&str]) -> String {
            String::new()
        }
        fn stranded_runners(&self, repo: &Path) -> Result<String, String> {
            let name = repo.file_name().unwrap().to_string_lossy().to_string();
            let line = format!("STRANDED run=77 job=\"build and test\" label=ci-r-77-1 queued=400s runner=none vmid=? repo={name}\n");
            self.asked.lock().unwrap().push(name.clone());
            if name == "gamma" {
                return Err("org runner listing did not answer".into());
            }
            Ok(line)
        }
        fn forge_ci_in_flight(&self, repo: &str) -> Result<bool, String> {
            self.in_flight.get(repo).cloned().unwrap_or(Ok(false))
        }
        fn send_mail(&self, _: &Mail) -> Result<(), String> {
            Ok(())
        }
    }

    const REPOS: [&str; 3] = ["alpha", "beta", "gamma"];

    fn stranded_world(tag: &str, in_flight: &[(&'static str, Result<bool, String>)]) -> (Config, Arc<Ci>, testkit::TempDir) {
        let dir = scratch_dir(tag);
        let queue = dir.join("queue");
        let map: String = REPOS.iter().map(|r| format!("{r}|{}/{r}\n", dir.display())).collect();
        for r in REPOS {
            fs::create_dir_all(queue.join(r)).unwrap();
        }
        fs::write(dir.join("repos"), map).unwrap();
        let host = Arc::new(Ci { in_flight: in_flight.iter().cloned().collect(), asked: Default::default() });
        let mut cfg = test_config();
        cfg.queue_dir = queue;
        cfg.repo_map = Some(dir.join("repos"));
        cfg.stranded_seen_path = dir.join("seen.json");
        cfg.stranded_min_age = 600;
        cfg.host = Some(host.clone());
        (cfg, host, dir)
    }

    fn asked(host: &Ci) -> Vec<String> {
        std::mem::take(&mut *host.asked.lock().unwrap())
    }

    #[test_id(th9lcju)]
    #[test]
    fn with_no_forge_ci_in_flight_a_pass_asks_the_forge_about_no_runners() {
        let (cfg, host, _dir) = stranded_world("stranded-idle", &[]);
        assert!(observe_queue_stranded_runs(&cfg).is_empty());
        assert_eq!(asked(&host), Vec::<String>::new());
    }

    #[test_id(tgyy7br)]
    #[test]
    fn a_repo_with_ci_in_flight_is_asked_alone_and_a_stranded_run_is_still_a_gap() {
        let (cfg, host, _dir) = stranded_world("stranded-flight", &[("beta", Ok(true))]);
        let checks = observe_queue_stranded_runs(&cfg);
        assert_eq!(asked(&host), ["beta"]);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].key, "queue-stranded-run:beta:77");
        assert!(matches!(checks[0].raw, RawStatus::Gap { .. }));
    }

    #[test_id(t7hl8p7)]
    #[test]
    fn the_stranded_remedy_asks_the_rerun_to_wait_for_its_cancel() {
        let (cfg, _host, _dir) = stranded_world("stranded-args", &[("beta", Ok(true))]);
        let checks = observe_queue_stranded_runs(&cfg);
        let Remedy::Noted { args, .. } = &checks[0].remedy else { panic!("not a noted remedy") };
        assert_eq!(&args[2..], ["77", "--wait-cancel", "120"]);
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        assert_eq!(remedy_step_deadline(&argv), "150");
        assert_eq!(remedy_step_deadline(&["workflow-rerun", "r", "7"]), spira_config::bounded::CALL_TIMEOUT_SECS);
    }

    #[test_id(tgi1l9l)]
    #[test]
    fn one_sweep_follows_the_ci_once_the_min_age_has_passed_then_none() {
        let (mut cfg, host, _dir) = stranded_world("stranded-trailing", &[("beta", Ok(true))]);
        cfg.now_secs = 1_000;
        observe_queue_stranded_runs(&cfg);
        assert_eq!(asked(&host), ["beta"]);

        let idle = Arc::new(Ci { in_flight: std::collections::BTreeMap::new(), asked: Default::default() });
        cfg.host = Some(idle.clone());
        for (at, want) in [(1_000 + 599, 0), (1_000 + 600, 1), (1_000 + 601, 0)] {
            cfg.now_secs = at;
            observe_queue_stranded_runs(&cfg);
            assert_eq!(asked(&idle).len(), want, "at +{}", at - 1_000);
        }
    }

    #[test_id(tgtu37o)]
    #[test]
    fn a_trailing_sweep_owed_is_kept_on_disk_until_it_runs() {
        let (mut cfg, _, _dir) = stranded_world("stranded-restart", &[("beta", Ok(true))]);
        cfg.now_secs = 50;
        observe_queue_stranded_runs(&cfg);
        assert_eq!(load_stranded_seen(&cfg.stranded_seen_path), [("beta".to_string(), 50)].into());
        cfg.host = Some(Arc::new(Ci { in_flight: std::collections::BTreeMap::new(), asked: Default::default() }));
        cfg.now_secs = 50 + cfg.stranded_min_age;
        observe_queue_stranded_runs(&cfg);
        assert!(load_stranded_seen(&cfg.stranded_seen_path).is_empty(), "the swept entry is cleared");
    }

    #[test_id(td7e560)]
    #[test]
    fn a_host_that_cannot_tell_makes_no_call_and_is_unobservable_not_satisfied() {
        let (cfg, host, _dir) = stranded_world("stranded-unknown", &[("alpha", Err("no store view".into()))]);
        let checks = observe_queue_stranded_runs(&cfg);
        assert_eq!(asked(&host), Vec::<String>::new());
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].key, "queue-stranded-runs:alpha");
        assert!(matches!(&checks[0].raw, RawStatus::Unobservable { reason } if reason.contains("no store view")));
    }

    #[test_id(tsr0b01)]
    #[test]
    fn a_host_that_cannot_list_the_runners_is_unobservable_and_asks_no_remedy() {
        let (cfg, host, _dir) = stranded_world("stranded-err", &[("gamma", Ok(true))]);
        let checks = observe_queue_stranded_runs(&cfg);
        assert_eq!(asked(&host), ["gamma"]);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].key, "queue-stranded-runs:gamma");
        assert!(matches!(&checks[0].raw, RawStatus::Unobservable { reason } if reason.contains("org runner listing")));
        assert_eq!(checks[0].remedy, Remedy::Escalate);
    }

    #[test_id(tsr0b02)]
    #[test]
    fn a_stranded_run_is_rerun_with_a_note_naming_repo_run_job_and_label() {
        let (cfg, _host, _dir) = stranded_world("stranded-note", &[("beta", Ok(true))]);
        let checks = observe_queue_stranded_runs(&cfg);
        assert_eq!(checks[0].remedy.note().as_deref(), Some(r#"repo=beta run=77 job="build and test" label=ci-r-77-1 queued=400s runner=none"#));
    }

    #[test_id(tsr0b03)]
    #[test]
    fn a_decided_noted_remedy_carries_its_note_onto_the_act() {
        let (mut cfg, _host, _dir) = stranded_world("stranded-act", &[("beta", Ok(true))]);
        cfg.grace_secs = 0;
        let check = observe_queue_stranded_runs(&cfg).remove(0);
        let acts = decide(&cfg.rules(), &mut StateMap::new(), &mut PendingMap::new(), &mut AlertedSinceMap::new(), check);
        let noted = acts.iter().find_map(|a| match a {
            Act::Remedy { note, .. } => Some(note.clone()),
            _ => None,
        });
        assert_eq!(noted.flatten().as_deref(), Some(r#"repo=beta run=77 job="build and test" label=ci-r-77-1 queued=400s runner=none"#));
    }

    #[test_id(tsr0b04)]
    #[test]
    fn without_a_host_a_forge_that_exits_nonzero_is_unobservable_and_one_that_prints_is_read() {
        let (mut cfg, _host, _dir) = stranded_world("stranded-nohost", &[]);
        cfg.host = None;
        cfg.seams.forge = "false".into();
        assert!(cfg.stranded_runners(Path::new("r")).is_err());
        cfg.seams.forge = "echo".into();
        assert_eq!(cfg.stranded_runners(Path::new("r")).unwrap(), "stranded-runners r\n");
    }

    #[test_id(ttz1ba1)]
    #[test]
    fn stranded_repos_come_from_the_repo_map_not_the_queue_directory() {
        let (mut cfg, host, _dir) = stranded_world("stranded-source", &[("sp-ghost", Ok(true))]);
        fs::create_dir_all(cfg.queue_dir.join("sp-ghost")).unwrap();
        assert!(observe_queue_stranded_runs(&cfg).is_empty());
        assert_eq!(asked(&host), Vec::<String>::new());
        cfg.repo_map = None;
        assert!(observe_queue_stranded_runs(&cfg).is_empty());
    }

    struct Legs(&'static str, Arc<std::sync::Mutex<Vec<String>>>);

    impl Host for Legs {
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
            if argv.contains(&"--apply") {
                *self.1.lock().unwrap() = argv.iter().map(|a| a.to_string()).collect();
            }
            Ok(r#"{"orphans":["sp-a"],"repos":{"b-live":"r","b-dead":"r"}}"#.into())
        }
        fn certified_list(&self, _: &Path) -> String {
            String::new()
        }
        fn config(&self, _: &[&str]) -> String {
            String::new()
        }
        fn stranded_runners(&self, _: &Path) -> Result<String, String> {
            Ok(String::new())
        }
        fn send_mail(&self, _: &Mail) -> Result<(), String> {
            Ok(())
        }
        fn round_legs_live(&self, _: &str, batch: &str) -> bool {
            batch == self.0
        }
    }

    fn orphan_apply(cfg: &Config) {
        let program = cfg.seams.spira_lc.clone();
        perform(cfg, &mut AlertedSinceMap::new(), vec![Act::Remedy { note: None, key: "k".into(), steps: vec![(program, vec!["requeue-orphans".into(), "--actor".into(), "reconciler".into(), "--apply".into()])] }]);
    }

    fn applied(store: &Store) -> Vec<String> {
        store.0.lock().unwrap().iter().find(|a| a.iter().any(|x| x == "--apply")).cloned().unwrap_or_default()
    }

    #[test_id(tnaw3ha)]
    #[test]
    fn orphan_apply_spares_a_round_whose_leg_is_live() {
        let mut cfg = test_config();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        cfg.host = Some(Arc::new(Legs("b-live", Arc::clone(&seen))));
        orphan_apply(&cfg);
        assert_eq!(*seen.lock().unwrap(), ["requeue-orphans", "--actor", "reconciler", "--apply", "--live-round", "b-live"]);
    }

    #[test_id(top74jm)]
    #[test]
    fn orphan_apply_spares_every_round_when_liveness_cannot_be_told() {
        let (cfg, store) = served_config();
        orphan_apply(&cfg);
        let argv = applied(&store);
        assert!(argv.windows(2).any(|w| w == ["--live-round", "b-live"]) && argv.windows(2).any(|w| w == ["--live-round", "b-dead"]), "{argv:?}");
    }

    #[test_id(tmcucsc)]
    #[test]
    fn orphan_apply_without_a_readable_dry_run_applies_nothing() {
        struct Garbled(Arc<std::sync::Mutex<Vec<String>>>);
        impl spira_store::lc_call::Transport for Garbled {
            fn exchange(&self, argv: &[String], _: Option<&str>, _: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
                self.0.lock().unwrap().extend(argv.iter().cloned());
                Ok(spira_store::lc_call::Reply { code: 0, out: "not json".into(), err: String::new(), read_ended: false })
            }
        }
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut cfg = test_config();
        cfg.lc_client = Some(LcClient::over(Arc::new(Garbled(Arc::clone(&seen))) as Arc<dyn spira_store::lc_call::Transport>, true));
        orphan_apply(&cfg);
        assert!(!seen.lock().unwrap().iter().any(|a| a == "--apply"));
    }

    #[test_id(tx7f42d)]
    #[test]
    fn free_space_is_read_without_df() {
        assert!(free_pct("/").is_some_and(|p| (0..=100).contains(&p)));
        let root = testkit::TempDir::new("free-pct-none");
        assert_eq!(free_pct(root.join("no/such/path/at/all").to_str().unwrap()), None);
    }

    /// A world answered entirely by the host. Every program a `Seams` names is a script that
    /// records its name, so a launch is visible; `None` host means the pass launches.
    struct Quiet {
        asked: std::sync::Mutex<Vec<String>>,
        server: Option<Option<String>>,
        /// Whether a `content list --id` read answers `[]` for every id.
        blank_content: bool,
        no_ready: bool,
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
            if spira_store::lc_call::reads_whole_history(argv) {
                return Err("refused: reads every bead ever filed".into());
            }
            match argv {
                ["list", "--state", "READY"] if self.no_ready => Ok("[]".into()),
                ["list", "--state", "READY"] => Ok(r#"[{"bead_id":"sp-real","state":"READY"},{"bead_id":"prose key","state":"READY"}]"#.into()),
                ["content", "list", .., "--id", _] if self.blank_content => Ok("[]".into()),
                ["content", "list", .., "--id", _] => Ok(r#"[{"id":"sp-real"}]"#.into()),
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
                ["rune", "reactive"] => "czar\n".into(),
                ["rune", "partitions"] => "plan\tpoison\n".into(),
                ["rune", "for-labels", _] => "builder\n".into(),
                _ => String::new(),
            }
        }
        fn stranded_runners(&self, _: &Path) -> Result<String, String> {
            Ok(String::new())
        }
        fn send_mail(&self, _: &Mail) -> Result<(), String> {
            Ok(())
        }
        fn dwarf_count(&self, rune: &str) -> Option<i64> {
            Some(if rune == "builder" { 2 } else { 1 })
        }
        fn ready_rows(&self, labels: &str, exclude: &str) -> Option<Vec<(String, String)>> {
            assert_eq!((labels, exclude), ("plan", "poison"));
            Some((0..5).map(|i| (format!("sp-r{i}"), "2026-11-19T00:00:00Z".to_string())).collect())
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
            let body = format!("#!/bin/sh\necho \"{name} $*\" >> {log_text}\ncase \"$*\" in\n*list-panes*) echo health;;\n*rev-parse*) echo {SHA};;\n\"rune task\"*|\"rune for-labels\"*) echo builder;;\n\"rune reactive\"*) echo czar;;\n\"rune partitions\"*) printf 'plan\\\\tpoison\\\\n';;\nesac\nexit 0\n", SHA = "c".repeat(40));
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
        Arc::new(Quiet { asked: Default::default(), server, blank_content: false, no_ready: false })
    }

    #[test_id(to1n5tb)]
    #[test]
    fn a_quiet_world_launches_no_program_over_ten_ticks_and_the_unhosted_pass_does() {
        let dir = scratch_dir("quiet-ticks");
        let (cfg, log) = tripwired(&dir, Some(quiet(Some(Some("7:1.0".into())))));
        let mut cfg = cfg.with_memo(Arc::default());
        for tick in 0..10 {
            cfg.now_secs = 1_000 + tick * 60;
            let fleet = fleet_readings(&cfg);
            assert_eq!(fleet.live_total, 3);
            assert_eq!(fleet.rows.iter().map(|(l, r, live, t, _)| (l.as_str(), r.len(), *live, *t)).collect::<Vec<_>>(), vec![("plan", 5, 2, true)]);
            assert_eq!(observe_cockpit(&cfg).raw, RawStatus::Satisfied);
            let edges = observe_epic_edges(&cfg);
            assert_eq!(edges.len(), 1, "{edges:?}");
            assert_eq!(edges[0].key, "epic-blocks:sp-kid:sp-epic");
            assert_eq!(cfg.ref_read(Path::new("r"), "loose-tag"), Ref::Sha("c".repeat(40)), "the peel is git's, once");
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

    #[test_id(th0vyib)]
    #[test]
    fn a_home_outside_any_checkout_launches_no_git_and_one_inside_answers_as_before() {
        let dir = scratch_dir("main-branch");
        let (mut cfg, log) = tripwired(&dir, None);
        cfg.spira_home = dir.join("release").display().to_string();
        fs::create_dir_all(&cfg.spira_home).unwrap();
        assert_eq!(on_main_branch(&cfg), None);
        assert_eq!(launched(&log, "git"), 0, "{}", launches(&log));
        fs::create_dir_all(dir.join("release/.git")).unwrap();
        assert_eq!(on_main_branch(&cfg), Some(false), "the tripwire git answers a sha, which is not main");
        assert_eq!(launched(&log, "git"), 1);
    }

    #[test_id(t8qb575)]
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

    struct Listing {
        names: Vec<String>,
        listing: std::sync::Mutex<Result<(), String>>,
        disabled: Option<String>,
    }

    impl Listing {
        fn new(n: usize) -> Listing {
            let names = (0..n).map(|i| if i % 2 == 0 { format!("spira-u{i}.timer") } else { format!("spira-u{i}.service") }).collect();
            Listing { names, listing: std::sync::Mutex::new(Ok(())), disabled: None }
        }
        fn fail(&self) {
            *self.listing.lock().unwrap() = Err("ListUnits: no reply within 5s".into());
        }
        fn heal(&self) {
            *self.listing.lock().unwrap() = Ok(());
        }
    }

    impl Host for Listing {
        fn unit_listing(&self) -> Result<(), String> {
            self.listing.lock().unwrap().clone()
        }
        fn unit_state(&self, verb: &str, unit: &str) -> Result<String, String> {
            self.unit_listing()?;
            let down = self.disabled.as_deref() == Some(unit);
            Ok(match (verb, down) {
                ("is-enabled", true) => "disabled",
                ("is-enabled", false) => "enabled",
                (_, true) => "inactive",
                (_, false) => "active",
            }
            .into())
        }
        fn list_enable(&self) -> String {
            self.names.join("\n")
        }
        fn free_pct(&self, _: &str) -> Option<i64> {
            Some(90)
        }
        fn lc(&self, _: &[&str]) -> Result<String, String> {
            Err("unexpected".into())
        }
        fn certified_list(&self, _: &Path) -> String {
            String::new()
        }
        fn config(&self, _: &[&str]) -> String {
            String::new()
        }
        fn stranded_runners(&self, _: &Path) -> Result<String, String> {
            Ok(String::new())
        }
        fn send_mail(&self, _: &Mail) -> Result<(), String> {
            Ok(())
        }
    }

    fn listing_pass(host: &Arc<Listing>, now: u64, state: &mut StateMap, alerted: &mut AlertedSinceMap) -> (Vec<Check>, Vec<Mail>) {
        let mut cfg = test_config();
        cfg.host = Some(host.clone());
        let rules = Rules { now_secs: now, grace_secs: 60, effect_passes: 2, shadow_kinds: Vec::new() };
        let checks = observe_units(&cfg);
        let mut mails = Vec::new();
        for c in checks.clone() {
            for a in decide(&rules, state, &mut PendingMap::new(), alerted, c) {
                if let Act::Mail { mail, .. } = a {
                    mails.push(mail);
                }
            }
        }
        (checks, mails)
    }

    #[test_id(t87s4qi)]
    #[test]
    fn a_failed_unit_listing_raises_one_listing_mail_not_one_per_unit() {
        let host = Arc::new(Listing::new(21));
        host.fail();
        let (mut state, mut alerted) = (StateMap::new(), AlertedSinceMap::new());
        let (checks, mails) = listing_pass(&host, 1000, &mut state, &mut alerted);
        assert_eq!(checks.len(), 1);
        assert!(mails.is_empty(), "the first unobservable pass is held back");
        let (checks, mails) = listing_pass(&host, 1100, &mut state, &mut alerted);
        assert_eq!(checks.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(), ["units:listing"]);
        let subjects: Vec<&str> = mails.iter().map(|m| m.subject.as_str()).collect();
        assert_eq!(subjects, ["RECONCILER: units:listing — unobservable: ListUnits: no reply within 5s; 21 units not observed"]);
        let (checks, mails) = listing_pass(&host, 1200, &mut state, &mut alerted);
        assert_eq!(checks.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(), ["units:listing"]);
        assert!(mails.is_empty(), "a streak pages once");
    }

    #[test_id(t4t3533)]
    #[test]
    fn the_unobserved_count_leaves_out_skipped_units() {
        let host = Arc::new(Listing::new(21));
        host.fail();
        let mut cfg = test_config();
        cfg.host = Some(host.clone());
        cfg.skip_units = vec!["spira-u0.timer".into(), "spira-u1.service".into()];
        let checks = observe_units(&cfg);
        assert_eq!(checks.len(), 1);
        assert!(matches!(&checks[0].raw, RawStatus::Unobservable { reason } if reason.ends_with("; 19 units not observed")));
    }

    #[test_id(tz3fjj9)]
    #[test]
    fn one_diverging_timer_is_the_only_unit_gap_when_listing_answers() {
        let mut h = Listing::new(21);
        h.disabled = Some("spira-u4.timer".into());
        let host = Arc::new(h);
        let (checks, _) = listing_pass(&host, 1000, &mut StateMap::new(), &mut AlertedSinceMap::new());
        let gaps: Vec<&str> = checks.iter().filter(|c| matches!(c.raw, RawStatus::Gap { .. })).map(|c| c.key.as_str()).collect();
        assert_eq!(gaps, ["units-timer:spira-u4.timer"]);
        let rest: Vec<&str> = checks.iter().filter(|c| c.key == "units:listing").map(|c| c.key.as_str()).collect();
        assert_eq!(rest, ["units:listing"]);
        assert!(checks.iter().any(|c| c.key == "units:listing" && matches!(c.raw, RawStatus::Satisfied)));
    }

    #[test_id(tv1r0m3)]
    #[test]
    fn a_listing_that_answers_again_clears_the_streak_and_a_later_failure_pages_again() {
        let host = Arc::new(Listing::new(3));
        let (mut state, mut alerted) = (StateMap::new(), AlertedSinceMap::new());
        host.fail();
        listing_pass(&host, 1000, &mut state, &mut alerted);
        let (_, mails) = listing_pass(&host, 1100, &mut state, &mut alerted);
        assert_eq!(mails.len(), 1);
        assert!(alerted.contains_key("units:listing"));
        host.heal();
        let (checks, mails) = listing_pass(&host, 1200, &mut state, &mut alerted);
        assert!(mails.is_empty());
        assert!(checks.iter().any(|c| c.key == "units:listing" && matches!(c.raw, RawStatus::Satisfied)));
        assert!(!alerted.contains_key("units:listing"));
        host.fail();
        listing_pass(&host, 1300, &mut state, &mut alerted);
        let (_, mails) = listing_pass(&host, 1400, &mut state, &mut alerted);
        assert_eq!(mails.len(), 1);
    }

    #[test_id(tdlmg0p)]
    #[test]
    fn one_failing_listing_pass_from_default_state_sends_no_mail() {
        let host = Arc::new(Listing::new(21));
        host.fail();
        let (_, mails) = listing_pass(&host, 1000, &mut StateMap::new(), &mut AlertedSinceMap::new());
        assert!(mails.is_empty());
    }

    fn escalation_acts(shadow: &[&str]) -> Vec<Act> {
        let rules = Rules { now_secs: 100, grace_secs: 0, effect_passes: 2, shadow_kinds: shadow.iter().map(|k| k.to_string()).collect() };
        let raw = RawStatus::Gap { desired: "up".into(), observed: "down".into(), since_hint: None };
        decide(&rules, &mut StateMap::new(), &mut PendingMap::new(), &mut AlertedSinceMap::new(), Check { key: "fleet:a".into(), raw, remedy: Remedy::Escalate })
    }

    #[test_id(ti29zhf)]
    #[test]
    fn a_shadowed_kinds_page_is_recorded_not_sent_and_an_unshadowed_kinds_page_is_sent() {
        let sent = |acts: &[Act]| acts.iter().any(|a| matches!(a, Act::Mail { .. }));
        let recorded = |acts: &[Act]| acts.iter().any(|a| matches!(a, Act::Event(e) if e.event == "shadow-page" && e.key == "fleet:a"));
        let live = escalation_acts(&["disk"]);
        assert!(sent(&live) && !recorded(&live));
        let held = escalation_acts(&["fleet"]);
        assert!(!sent(&held) && recorded(&held));
    }

    #[test_id(tvu6xgh)]
    #[test]
    fn a_page_held_while_shadowed_is_still_sent_once_the_kind_is_unshadowed() {
        let raw = || RawStatus::Gap { desired: "up".into(), observed: "down".into(), since_hint: None };
        let mut alerted = AlertedSinceMap::new();
        let mut pass = |shadow: &[&str]| {
            let rules = Rules { now_secs: 100, grace_secs: 0, effect_passes: 2, shadow_kinds: shadow.iter().map(|k| k.to_string()).collect() };
            decide(&rules, &mut StateMap::new(), &mut PendingMap::new(), &mut alerted, Check { key: "fleet:a".into(), raw: raw(), remedy: Remedy::Escalate })
        };
        assert!(!pass(&["fleet"]).iter().any(|a| matches!(a, Act::Mail { .. })));
        assert!(pass(&[]).iter().any(|a| matches!(a, Act::Mail { .. })));
    }

    /// A fleet of one planned and one reactive partition: `live` dwarves under the planned rune,
    /// `reactive_live` under the reactive one, each with work ready.
    struct Fleet {
        planned_live: i64,
        reactive_live: i64,
        partition_rune: &'static str,
        ready: i64,
        named: Vec<(String, String)>,
        uncalled: Option<Result<std::collections::BTreeMap<String, String>, String>>,
    }

    impl Host for Fleet {
        fn uncalled(&self) -> Option<Result<std::collections::BTreeMap<String, String>, String>> {
            self.uncalled.clone()
        }
        fn unit_state(&self, _: &str, _: &str) -> Result<String, String> {
            Ok("active".into())
        }
        fn list_enable(&self) -> String {
            String::new()
        }
        fn free_pct(&self, _: &str) -> Option<i64> {
            Some(90)
        }
        fn lc(&self, _: &[&str]) -> Result<String, String> {
            Err("unexpected".into())
        }
        fn certified_list(&self, _: &Path) -> String {
            String::new()
        }
        fn config(&self, argv: &[&str]) -> String {
            match argv {
                ["rune", "task"] => "builder\n".into(),
                ["rune", "reactive"] => "ops\n".into(),
                ["rune", "partitions"] => format!("{}\t\n", self.partition_rune),
                ["rune", "for-labels", _] => format!("{}\n", self.partition_rune),
                _ => String::new(),
            }
        }
        fn stranded_runners(&self, _: &Path) -> Result<String, String> {
            Ok(String::new())
        }
        fn send_mail(&self, _: &Mail) -> Result<(), String> {
            Ok(())
        }
        fn dwarf_count(&self, rune: &str) -> Option<i64> {
            Some(if rune == "builder" { self.planned_live } else { self.reactive_live })
        }
        fn ready_rows(&self, _: &str, _: &str) -> Option<Vec<(String, String)>> {
            Some(if self.named.is_empty() { (0..self.ready).map(|i| (format!("sp-r{i}"), String::new())).collect() } else { self.named.clone() })
        }
    }

    fn reactive_check(planned_live: i64, ceiling: &str) -> RawStatus {
        let mut cfg = test_config();
        cfg.max_live_dwarves = ceiling.into();
        cfg.host = Some(Arc::new(Fleet { planned_live, reactive_live: 0, partition_rune: "ops", ready: 3, named: vec![], uncalled: None }));
        let checks = observe_fleet(&cfg);
        assert_eq!(checks.len(), 1, "{checks:?}");
        checks[0].raw.clone()
    }

    #[test_id(tpzfw90)]
    #[test]
    fn a_reactive_partition_with_ready_work_below_the_ceiling_is_a_gap() {
        assert!(matches!(reactive_check(3, "5"), RawStatus::Gap { .. }));
    }

    fn uncalled_check(uncalled: Option<Result<std::collections::BTreeMap<String, String>, String>>) -> RawStatus {
        let mut cfg = test_config();
        cfg.max_live_dwarves = "9".into();
        cfg.host = Some(Arc::new(Fleet { planned_live: 0, reactive_live: 0, partition_rune: "mender", ready: 4, named: vec![], uncalled }));
        observe_fleet(&cfg).remove(0).raw
    }

    #[test_id(tzxma1z)]
    #[test]
    fn a_partition_the_sentinel_left_uncalled_with_a_reason_is_deliberate_not_a_gap() {
        let left = BTreeMap::from([("mender".to_string(), "yielding — urgent bead(s) unclaimed: sp-x".to_string())]);
        match uncalled_check(Some(Ok(left))) {
            RawStatus::Deliberate { reason } => assert!(reason.starts_with("mender: ") && reason.contains("yielding"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    #[test_id(tzu4c5m)]
    #[test]
    fn a_partition_the_sentinel_did_not_account_for_stays_a_gap() {
        assert!(matches!(uncalled_check(Some(Ok(BTreeMap::from([("other".to_string(), "full".to_string())])))), RawStatus::Gap { .. }));
        assert!(matches!(uncalled_check(Some(Ok(BTreeMap::new()))), RawStatus::Gap { .. }));
    }

    #[test_id(t3fux1s)]
    #[test]
    fn an_unreadable_sentinel_pass_makes_the_gap_unobservable() {
        assert_eq!(uncalled_check(Some(Err("no sentinel pass yet".into()))), RawStatus::Unobservable { reason: "no sentinel pass yet".into() });
    }

    #[test_id(tafy6v7)]
    #[test]
    fn a_host_that_cannot_read_the_sentinel_leaves_the_gap_as_it_was() {
        assert!(matches!(uncalled_check(None), RawStatus::Gap { .. }));
    }

    #[test_id(tsbkz55)]
    #[test]
    fn a_reactive_partition_at_the_ceiling_is_satisfied() {
        assert_eq!(reactive_check(5, "5"), RawStatus::Satisfied);
    }

    fn desired_count(raw: &RawStatus) -> (String, String) {
        match raw {
            RawStatus::Gap { desired, observed, .. } => (desired.split_whitespace().next().unwrap().to_string(), observed.clone()),
            other => panic!("not a gap: {other:?}"),
        }
    }

    fn plan_fleet(named: &[(&str, &str)]) -> Config {
        let mut cfg = test_config();
        cfg.max_live_dwarves = "5".into();
        cfg.max_dwarves = "3".into();
        cfg.now_secs = 1_793_166_343;
        let named: Vec<(String, String)> = named.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        cfg.host = Some(Arc::new(Fleet { planned_live: 0, reactive_live: 0, partition_rune: "plan", ready: named.len() as i64, named, uncalled: None }));
        cfg
    }

    #[test_id(t4utjju)]
    #[test]
    fn a_fleet_gap_names_the_claimable_bead_behind_it_and_its_age() {
        let cfg = plan_fleet(&[("sp-x", "2026-10-28T03:45:43Z")]);
        let check = observe_fleet(&cfg).remove(0);
        assert_eq!(check.key, "fleet:plan");
        assert_eq!(check.raw, RawStatus::Gap { desired: "1 (sp-x updated 2h ago)".into(), observed: "0".into(), since_hint: None });
        let rules = Rules { now_secs: 100, grace_secs: 0, effect_passes: 2, shadow_kinds: vec![] };
        let (mut state, mut pending, mut alerted) = (StateMap::new(), PendingMap::new(), AlertedSinceMap::new());
        let mut subject = None;
        for pass in 0..4 {
            let rules = Rules { now_secs: 100 + pass, ..rules.clone() };
            let acts = decide(&rules, &mut state, &mut pending, &mut alerted, observe_fleet(&cfg).remove(0));
            subject = subject.or(acts.iter().find_map(|a| if let Act::Mail { mail, .. } = a { Some(mail.subject.clone()) } else { None }));
        }
        let subject = subject.expect("a page");
        assert!(subject.contains("sp-x"), "{subject}");
    }

    #[test_id(t812cg8)]
    #[test]
    fn ready_rows_read_from_the_json_and_an_unreadable_answer_is_no_beads() {
        assert_eq!(parse_ready_rows(r#"[{"id":"sp-a","updated_at":"2026-11-19T00:00:00Z"},{"id":"sp-b"}]"#), vec![("sp-a".into(), "2026-11-19T00:00:00Z".into()), ("sp-b".into(), String::new())]);
        assert!(parse_ready_rows("0").is_empty() && parse_ready_rows("").is_empty());
    }

    #[test_id(tt7x10k)]
    #[test]
    fn a_fleet_gap_names_at_most_five_beads_and_counts_the_rest() {
        let named: Vec<(String, String)> = (0..7).map(|i| (format!("sp-{i}"), String::new())).collect();
        let named: Vec<(&str, &str)> = named.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let mut cfg = plan_fleet(&named);
        cfg.max_dwarves = "9".into();
        cfg.max_live_dwarves = "9".into();
        match observe_fleet(&cfg).remove(0).raw {
            RawStatus::Gap { desired, .. } => assert_eq!(desired, "7 (sp-0, sp-1, sp-2, sp-3, sp-4, +2 more)"),
            other => panic!("{other:?}"),
        }
    }

    #[test_id(tvmkxyq)]
    #[test]
    fn a_lane_with_no_claimable_bead_is_satisfied_and_pages_nothing_even_after_a_gap_streak() {
        let rules = Rules { now_secs: 100, grace_secs: 0, effect_passes: 2, shadow_kinds: vec![] };
        let mut alerted = AlertedSinceMap::new();
        alerted.insert("fleet:plan".into(), 10);
        let check = observe_fleet(&plan_fleet(&[])).remove(0);
        assert_eq!(check.raw, RawStatus::Satisfied);
        let acts = decide(&rules, &mut StateMap::new(), &mut PendingMap::new(), &mut alerted, check);
        assert!(!acts.iter().any(|a| matches!(a, Act::Mail { .. })), "{acts:?}");
    }

    fn task_check(pool: &str, builders_live: i64) -> RawStatus {
        let mut cfg = test_config();
        cfg.max_live_dwarves = "5".into();
        cfg.max_dwarves = pool.into();
        cfg.host = Some(Arc::new(Fleet { planned_live: builders_live, reactive_live: 0, partition_rune: "builder", ready: 10, named: vec![], uncalled: None }));
        let checks = observe_fleet(&cfg);
        assert_eq!(checks.len(), 1, "{checks:?}");
        checks[0].raw.clone()
    }

    #[test_id(tjp22oq)]
    #[test]
    fn a_task_partition_above_the_pool_cap_is_satisfied() {
        assert_eq!(task_check("3", 4), RawStatus::Satisfied);
    }

    #[test_id(txrj7z1)]
    #[test]
    fn a_task_partition_at_the_pool_cap_is_satisfied() {
        assert_eq!(task_check("3", 3), RawStatus::Satisfied);
    }

    #[test_id(t3edafe)]
    #[test]
    fn a_task_partition_below_the_pool_cap_is_a_gap_desiring_the_pool() {
        assert_eq!(desired_count(&task_check("3", 2)), ("3".to_string(), "2".to_string()));
    }

    #[test_id(tt1znpv)]
    #[test]
    fn reactive_dwarves_do_not_spend_the_task_pool() {
        let mut cfg = test_config();
        cfg.max_live_dwarves = "9".into();
        cfg.max_dwarves = "3".into();
        cfg.host = Some(Arc::new(Fleet { planned_live: 2, reactive_live: 4, partition_rune: "builder", ready: 10, named: vec![], uncalled: None }));
        assert_eq!(desired_count(&observe_fleet(&cfg)[0].raw), ("3".to_string(), "2".to_string()));
    }

    #[test_id(to7uc8r)]
    #[test]
    fn a_paused_task_pool_is_satisfied() {
        assert_eq!(task_check("0", 0), RawStatus::Satisfied);
    }


    #[test_id(toesh2v)]
    #[test]
    fn a_task_partition_with_a_start_in_flight_is_satisfied_and_without_it_a_gap() {
        let check = |starting: &[(&str, u64)]| {
            let mut cfg = test_config();
            cfg.max_live_dwarves = "5".into();
            cfg.max_dwarves = "3".into();
            cfg.starting = starting.iter().map(|(r, n)| (r.to_string(), *n)).collect();
            cfg.host = Some(Arc::new(Fleet { planned_live: 2, reactive_live: 0, partition_rune: "builder", ready: 5, named: vec![], uncalled: None }));
            observe_fleet(&cfg)[0].raw.clone()
        };
        assert_eq!(check(&[("builder", 1)]), RawStatus::Satisfied);
        assert_eq!(check(&[]), RawStatus::Gap { desired: "3 (sp-r0, sp-r1, sp-r2, sp-r3, sp-r4)".into(), observed: "2".into(), since_hint: None });
    }

    fn redrive_pass(d: &mut (StateMap, PendingMap, AlertedSinceMap), now_secs: u64, gap: bool) -> Vec<Act> {
        let rules = Rules { now_secs, grace_secs: 100, effect_passes: 2, shadow_kinds: vec![] };
        let raw = if gap { RawStatus::Gap { desired: "3".into(), observed: "2".into(), since_hint: None } } else { RawStatus::Satisfied };
        decide(&rules, &mut d.0, &mut d.1, &mut d.2, Check { key: "fleet:a".into(), raw, remedy: Remedy::CallWanted })
    }

    fn mails(acts: &[Act]) -> Vec<&Mail> {
        acts.iter().filter_map(|a| if let Act::Mail { mail, .. } = a { Some(mail) } else { None }).collect()
    }

    #[test_id(tn35aki)]
    #[test]
    fn a_fleet_gap_past_grace_redrives_the_sentinel_then_pages_once_naming_the_redrive() {
        let mut d = Default::default();
        assert!(redrive_pass(&mut d, 1000, true).iter().all(|a| !matches!(a, Act::CallWanted { .. } | Act::Mail { .. })));
        let first = redrive_pass(&mut d, 1200, true);
        assert!(first.contains(&Act::CallWanted { key: "fleet:a".into() }));
        assert!(mails(&first).is_empty());
        let mut sent = Vec::new();
        for i in 1..=6 {
            let acts = redrive_pass(&mut d, 1200 + i * 100, true);
            sent.extend(mails(&acts).into_iter().cloned());
        }
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].mailbox, "concierge");
        assert!(sent[0].body.contains("last remedy tried: asked the sentinel's call pass to run (call.wanted)"), "{}", sent[0].body);
    }

    #[test_id(to9610a)]
    #[test]
    fn a_fleet_gap_that_closes_after_the_redrive_sends_no_mail() {
        let mut d = Default::default();
        redrive_pass(&mut d, 1000, true);
        assert!(redrive_pass(&mut d, 1200, true).contains(&Act::CallWanted { key: "fleet:a".into() }));
        for now in [1300, 1400, 1500, 1600] {
            assert!(mails(&redrive_pass(&mut d, now, false)).is_empty());
        }
    }

    #[test_id(tdwdmmj)]
    #[test]
    fn a_redrive_with_no_effect_is_paged_once_by_the_escalation_and_not_by_a_second_note() {
        let mut d = Default::default();
        let mut all = Vec::new();
        for now in [1000, 1200, 1300, 1400, 1500, 1600, 1700] {
            all.extend(redrive_pass(&mut d, now, true));
        }
        assert_eq!(mails(&all).len(), 1);
        assert!(!mails(&all)[0].subject.contains("had no effect"));
        assert!(all.iter().any(|a| matches!(a, Act::Event(e) if e.event == "no-effect")));
    }

    #[test_id(txuby76)]
    #[test]
    fn another_remedys_no_effect_still_sends_its_note() {
        let rules = Rules { now_secs: 0, grace_secs: 0, effect_passes: 1, shadow_kinds: vec![] };
        let mut d: (StateMap, PendingMap, AlertedSinceMap) = Default::default();
        let gap = || RawStatus::Gap { desired: "up".into(), observed: "down".into(), since_hint: None };
        let remedy = || Remedy::Command { program: "fix".into(), args: vec![] };
        let mut all = Vec::new();
        for now in [10, 20, 30, 40] {
            let rules = Rules { now_secs: now, ..rules.clone() };
            all.extend(decide(&rules, &mut d.0, &mut d.1, &mut d.2, Check { key: "disk:/".into(), raw: gap(), remedy: remedy() }));
        }
        assert!(mails(&all).iter().any(|m| m.subject.contains("had no effect")), "{all:?}");
    }

    #[test_id(tmud739)]
    #[test]
    fn the_shell_pass_logs_the_redrive_it_cannot_send() {
        let d = testkit::TempDir::new("redrive-log");
        let mut cfg = test_config();
        cfg.log = d.join("log");
        perform(&cfg, &mut AlertedSinceMap::new(), vec![Act::CallWanted { key: "fleet:a".into() }]);
        assert!(fs::read_to_string(&cfg.log).unwrap().contains("reconciler: fleet:a → call.wanted needs the scheduler; not sent"));
    }

    pub(super) fn test_config() -> Config {
        Config {
            seams: Seams::bare(),
            spira_run: PathBuf::from(system_paths::TMP),
            spira_home: String::new(),
            log: PathBuf::from(system_paths::DEV_NULL),
            state_path: PathBuf::from(system_paths::DEV_NULL),
            alerted_path: PathBuf::from(system_paths::DEV_NULL),
            store: std::sync::Arc::new(tsd::store::Pusher::new(None)),
            effect_state: PathBuf::from(system_paths::DEV_NULL),
            mail_root: PathBuf::new(),
            release: "r-test".into(),
            shadow_kinds: Vec::new(),
            effect_passes: 2,
            lock_path: PathBuf::from(system_paths::DEV_NULL),
            queue_dir: PathBuf::from(system_paths::DEV_NULL),
            repo_map: None,
            releases_dir: PathBuf::new(),
            cockpit_sessions: vec!["brain".into(), "hunk".into(), "chat".into()],
            cockpit_mail: String::new(),
            dolt_data: String::new(),
            max_live_dwarves: String::new(),
            max_dwarves: String::new(),
            disk_floor_pct: 15,
            untagged_images_max: 40,
            grace_secs: 300,
            activation_grace_secs: 1800,
            fence_deadline_secs: 120,
            preflight_wall_secs: 240,
            now_secs: 0,
            now_iso: "2026-09-25T00:00:00Z".into(),
            desired_dir: PathBuf::from(system_paths::DEV_NULL),
            desired: DesiredState::default(),
            ctrl_path: PathBuf::from(system_paths::DEV_NULL),
            instance: "prod".into(),
            reminders_path: PathBuf::from(system_paths::DEV_NULL),
            breaker: Err("not configured".into()),
            host: None,
            lc_client: None,
            skip_units: Vec::new(),
            memo: Arc::default(),
            stranded_min_age: 600,
            stranded_seen_path: PathBuf::from(system_paths::DEV_NULL),
            starting: Default::default(),
        }
    }

    #[test_id(txhrlrt)]
    #[test]
    fn a_full_store_data_directory_is_remedied_by_an_online_gc_not_by_reaping_build_output() {
        let (store, other) = (testkit::TempDir::new("disk-store"), testkit::TempDir::new("disk-other"));
        let mut cfg = test_config();
        cfg.dolt_data = store.path().display().to_string();
        cfg.desired.disk = Some(DiskSpec { floor_pct: 101, paths: vec![format!("{}/", cfg.dolt_data), other.path().display().to_string()] });
        let checks = observe_disk(&cfg);
        let remedy = |p: &Path| checks.iter().find(|c| c.key.starts_with(&format!("disk:{}", p.display()))).unwrap().remedy.describe();
        assert_eq!(remedy(store.path()), format!("{} --user start --no-block dolt-store-gc.service", cfg.seams.systemctl));
        assert!(remedy(other.path()).contains("image prune"), "other paths keep the podman remedy");
    }

    #[test_id(thlsd5t)]
    #[test]
    fn a_sequence_remedy_describes_and_runs_every_step() {
        let r = Remedy::Sequence(vec![("a".into(), vec!["1".into()]), ("b".into(), vec![])]);
        assert_eq!(r.describe(), "a 1 ; b ");
        assert_eq!(r.steps().len(), 2);
    }
}

#[cfg(test)]
mod lane_tests {
    use super::*;

    fn world(tag: &str) -> (testkit::TempDir, PathBuf, Vec<(&'static str, Option<String>)>) {
        let d = testkit::TempDir::new(tag);
        let run = d.path().join("run");
        std::fs::create_dir_all(&run).unwrap();
        let mut edits = spira_config::process::fixture_world_env(d.path(), &run, &["spira", "other"]);
        let release = d.path().join("release");
        std::fs::create_dir_all(release.join("bin")).unwrap();
        for tool in ["forge", "units-install", "spira-config", "spira-claim", "strand", "queue-helpers", "queue", "spira-lc", "bead", "target-reap", "rebuild", "layout", "spira-lint", "lifecycle-guard"] {
            testkit::write_exe(release.join("bin").join(tool), "#!/bin/sh\n");
        }
        edits.retain(|(k, _)| *k != "SPIRA_RELEASE");
        edits.push(("SPIRA_RELEASE", Some(release.display().to_string())));
        (d, run, edits)
    }

    fn stop(run: &Path, repo: &str) {
        spira_config::scoped_stop::pause_repo(run, repo, &[REMEDIES_LANE.to_string()], "cutover-mode", "cutover", "world stop", 1).unwrap();
    }

    #[test_id(tlo41i1)]
    #[test]
    fn a_pass_holds_only_when_the_remedies_lane_is_stopped_on_every_registered_repo() {
        let (_d, run, edits) = world("reconciler-held");
        let edits: Vec<(&str, Option<&str>)> = edits.iter().map(|(k, v)| (*k, v.as_deref())).collect();
        let _env = testkit::env(&edits);
        let cfg = Config { spira_run: run.clone(), ..tests::test_config() };
        assert_eq!(held(&cfg), None);
        stop(&run, "spira");
        assert_eq!(held(&cfg), None, "the other repo still takes remedies");
        stop(&run, "other");
        let why = held(&cfg).expect("stopped on both");
        assert!(why.starts_with("reconciler: skipped") && why.contains("world stop"), "{why}");
    }

    #[test_id(tzeje5u)]
    #[test]
    fn run_pass_stamps_and_takes_no_lock_while_the_remedies_lane_is_stopped_everywhere() {
        let (_d, run, edits) = world("reconciler-run-pass");
        let edits: Vec<(&str, Option<&str>)> = edits.iter().map(|(k, v)| (*k, v.as_deref())).collect();
        let _env = testkit::env(&edits);
        stop(&run, "spira");
        stop(&run, "other");
        assert_eq!(run_pass(), Ok(()));
        assert!(paths::pass_stamp(&run).exists(), "a held pass still stamps");
        assert!(!run.join("reconciler.lock").exists(), "a held pass went on to take its lock");
    }

    #[test_id(t33ikeb)]
    #[test]
    fn observe_reports_halted_and_stamps_while_the_remedies_lane_is_stopped_everywhere() {
        let (_d, run, edits) = world("reconciler-observe");
        let edits: Vec<(&str, Option<&str>)> = edits.iter().map(|(k, v)| (*k, v.as_deref())).collect();
        let _env = testkit::env(&edits);
        let cfg = Config { spira_run: run.clone(), ..tests::test_config() };
        stop(&run, "spira");
        stop(&run, "other");
        let seen = observe(&cfg, &Default::default());
        assert!(seen.halted && seen.groups.is_empty(), "a held pass observed nothing");
        assert!(paths::pass_stamp(&run).exists(), "a held pass still stamps");
    }
}
