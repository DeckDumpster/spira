//! # watch.skew — contract
//!
//! Replaces `spira-skew-prod` (`ExecStartPre=-skew refresh`, `ExecStart=skew check --escalate`), pattern P1. Both halves move: refresh and check are one tick.
//!
//! ## Tick
//!
//! - Hourly (`OnUnitActiveSec=1h`, `OnBootSec=10min`); single-flight.
//! - Order is kept: the refresh judgement first, then the check (unit `ExecStartPre`).
//!
//! ## Observe
//!
//! `watch.skew.observe` returns:
//!
//! ```text
//! { mode: local|release|checkout, activated, manifest_commit, base, tip,
//!   hotfix_line, verify: {ok, out}, queue_lock_held, tags[], tag_sidecar,
//!   decline_streak, stamps{key→condition} }
//! ```
//!
//! Each field comes from the component that decides it:
//!
//! - `current` symlink and `MANIFEST` `commit` (`skew/src/lib.rs:198-237`).
//! - Repo land mode from the registry `repo_land(home)`, in-process via `spira_config::repos` (`skew/src/real.rs:111-118`).
//! - `landref` and `rev_parse` / `is_ancestor` / `rev-list --count` through git (`real.rs:65-89,174-188`). The git class stays a child.
//! - `release verify <name> --no-pre-activate` (`real.rs:230-246`) and `release status`, whose `RUNNING UNLANDED` line names a hotfix (`real.rs:247-257`). Both are release-tool verbs; they become library calls into `release` (§2 sibling binaries).
//! - Queue lock: a non-blocking flock on `SPIRA_QUEUE_DIR/<repo>/lock` (`real.rs:401-417`).
//! - Release tags: the `.tags/<name>` sidecar, else `gh release list` (`real.rs:314-334`). Only artifact mode needs gh; under `queue.local` no gh is asked.
//! - Stamps today are files under `SPIRA_RUN`: `skew.escalated-<cksum>` and `skew.refresh-declined-<cksum>` (`lib.rs:575,828-850`, `real.rs:419-444`). They become carried state.
//!
//! ## Decide
//!
//! `decide(carry, snap) -> (Carry, Acts)` composes today's pure core over the `World` seam (`skew/src/ports.rs`):
//!
//! - `refresh_check_only` gives `LOCAL-SKEW` (`lib.rs:903-982`).
//! - `check_local` gives `LOCAL-BEHIND` / `LOCAL-TAMPERED` / `CANNOT-VERIFY` (`lib.rs:479-569`).
//! - `check` gives `NOT-LATEST` / `MANIFEST-MISMATCH` (`lib.rs:183-334`).
//! - `escalate` sends once per condition key (`lib.rs:574-610`).
//! - The decline streak (`lib.rs:832-850`) alarms at 3 passes and every 180 after.
//!
//! Carry: `{ escalated: {cksum(key)→condition}, declined: {repo→n} }`, emitted as `watch.skew.state`.
//!
//! Fixtures: `skew/src/tests.rs` (50 tests), e.g.:
//!
//! - `check_local_tampered_and_behind_both_fire` (`tests.rs:365`)
//! - `check_local_hotfix_is_clean` (`:405`)
//! - `escalate_sends_once_per_condition_per_run` (`:568`)
//! - `refresh_queue_local_mismatch_escalates_local_skew` (`:640`)
//! - `…during_a_land_local_does_not_escalate` (`:658`)
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.skew.observe` (probe) | git | 60 s | — | an unsourceable config. Today that is exit 3, could-not-check (`main.rs:39-41`) |
//! | `watch.skew.escalate` | store | 30 s | `watch.skew:<cksum(condition_key)>` | a key already in carry |
//! | `watch.skew.log` | host | 5 s | `watch.skew:refresh-declined:<repo>:<n>` | a path outside `SPIRA_RUN` |
//! | `watch.skew.refresh` | git | 300 s | `watch.skew:refresh:<repo>:<base sha>` | `queue.local`, which is check-only and never resets (`lib.rs:656-658`) |
//!
//! - **`watch.skew.escalate`** is an operator question. The mail today is `mail send operator --from "Skew check <skew@spira>" --subject "The Spira copy in force is not the code that landed" --kind question --default <action>`, with the body from stdin (`real.rs:335-360`, `lib.rs:594-597`).
//! - **`watch.skew.log`** appends `skew: refresh of <repo> has declined <n> consecutive passes<lock note>; <what>` to `run/skew-refresh.log` (`lib.rs:845-848`, log row `spira/watchers:185`).
//! - **`watch.skew.refresh`** applies to checkout and release mode only: fetch, ff-merge, `release build/verify/activate`, `overrides.sh apply` (`lib.rs:676-700`, `real.rs:258-282`).
//!   - Release activation is a long, host-changing leg. If it is ever exercised it should be a `unit.start` (unverified that prod ever takes this path; prod's home repo is `queue.local`).
//!
//! No inbox line is written today. Verdict lines go to stdout, i.e. `run/skew.log` (unit `StandardOutput`), which no watchd row reads. The task drops them; the decisions are in the journal.
//!
//! ## Silence
//!
//! An idle tick, where running matches base or is a recorded hotfix and no stamp changes, emits the probe only. The verdict "in effect" is not journaled.
//!
//! ## Replaces
//!
//! - `spira-skew-prod.service` and `spira-skew-prod.timer` (`TimeoutStartSec=300`).
//! - Children:
//!   - `bash -c '. lib.sh'` init guard (`real.rs:28`)
//!   - `git` (`real.rs:65-89,216`)
//!   - `release verify/status/build/activate` (`real.rs:236,250,261`)
//!   - `overrides.sh apply` (`real.rs:281`)
//!   - `units-install --diff` (`real.rs:288`; `skew units`, not on the timer)
//!   - `gh release list` (`real.rs:321`)
//!   - `mail send` (`real.rs:339`)
//!   - `date -u` (`real.rs:363`)
//! - Keys pinned: `SPIRA_RELEASES`, `SPIRA_RELEASE_REPO`, `SPIRA_GH_INTAKE_REPO`, `SPIRA_GH`, `SPIRA_RUN`, `SPIRA_QUEUE_DIR` (`real.rs:40`), `SPIRA_REPO` (`lib.rs:200`).
//!
//! Catalogue differs: skew forks no `spira-lc`. Its children are git, release, mail, gh, bash `lib.sh`, overrides.sh and date. The ExecStartPre `skew refresh` is part of this unit and moves with it.
//!
//! ## Reverse
//!
//! Release rollback restores the unit. Observe mode shadows `escalate`, `log` and `refresh`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::cfg;

use skew::ports::{StatusRow, World};

use super::pattern;

pub const NAME: &str = "watch.skew";
pub const STREAM: &str = "watch.skew";
pub const TICK: EventKindTag = "watch.skew.tick";
pub const STATE: EventKindTag = "watch.skew.state";
pub const OBSERVE: &str = "watch.skew.observe";
pub const ESCALATE: &str = "watch.skew.escalate";
pub const LOG: &str = "watch.skew.log";
pub const REFRESH: &str = "watch.skew.refresh";
pub const EFFECTS: &[&str] = &[OBSERVE, ESCALATE, LOG, REFRESH];
pub const KEYS: &[&str] = &["SPIRA_RELEASES", "SPIRA_RELEASE_REPO", "SPIRA_GH_INTAKE_REPO", "SPIRA_GH", "SPIRA_RUN", "SPIRA_QUEUE_DIR"];
/// `OnUnitActiveSec=1h`.
pub const CADENCE_MS: u64 = 3_600_000;
const OBSERVE_DEADLINE_MS: u64 = 60_000;
/// The decline streak alarms at this many passes, then every `DECLINE_EVERY` after (`lib.rs:832-850`).
pub const DECLINE_ALARM: u32 = 3;
pub const DECLINE_EVERY: u32 = 180;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub releases: String,
    pub release_repo: String,
    pub gh_intake_repo: String,
    pub gh: String,
    pub run: String,
    pub queue_dir: String,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { releases: cfg("SPIRA_RELEASES")?, release_repo: cfg("SPIRA_RELEASE_REPO")?, gh_intake_repo: cfg("SPIRA_GH_INTAKE_REPO")?, gh: cfg("SPIRA_GH")?, run: cfg("SPIRA_RUN")?, queue_dir: cfg("SPIRA_QUEUE_DIR")? })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Local,
    Release,
    Checkout,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verify {
    pub ok: bool,
    pub out: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The home repo's checkout, which the decline-streak log line names.
    pub repo: String,
    pub mode: Mode,
    pub activated: Option<String>,
    pub manifest_commit: Option<String>,
    pub base: Option<String>,
    pub tip: Option<String>,
    pub hotfix_line: Option<String>,
    pub verify: Verify,
    pub queue_lock_held: bool,
    pub tags: Vec<String>,
    pub tag_sidecar: Option<String>,
    pub decline_streak: u32,
    pub stamps: BTreeMap<String, String>,
    /// How many commits `tip` is ahead of the running commit, when it descends from it; `None`
    /// when it does not (or nothing is running behind it).
    #[serde(default)]
    pub behind: Option<u64>,
    /// Each release tag's commit, for the MANIFEST-MISMATCH check in release mode.
    #[serde(default)]
    pub tag_commits: BTreeMap<String, String>,
}

/// `{ escalated: {cksum(key)→condition}, declined: {repo→n} }`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub escalated: BTreeMap<String, String>,
    pub declined: BTreeMap<String, u32>,
}

/// The name the snapshot world gives the home repo; the snapshot is of that repo alone.
const HOME: &str = "home";
const ESCALATED: &str = "skew.escalated-";
const DECLINED: &str = "skew.refresh-declined-";
const WRITE_DEADLINE_MS: u64 = 5_000;
const ESCALATE_DEADLINE_MS: u64 = 30_000;
const REFRESH_DEADLINE_MS: u64 = 300_000;

/// The operator question `escalate` asks, as `watch.skew.escalate` carries it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub from: String,
    pub subject: String,
    pub body: String,
    pub default: String,
}

/// skew's `World` answered from one snapshot and the carry: every read the check and the refresh
/// judgement make comes from the snapshot, every stamp from the carry, and the two writes they
/// can make (the operator question and the refresh alarm line) are collected as acts.
struct SnapWorld<'a> {
    snap: &'a Snapshot,
    cfg: &'a Config,
    carry: std::cell::RefCell<Carry>,
    acts: std::cell::RefCell<Vec<EffectSpec>>,
}

impl SnapWorld<'_> {
    fn releases(&self) -> PathBuf {
        PathBuf::from(&self.cfg.releases)
    }

    fn running(&self) -> Option<&String> {
        self.snap.manifest_commit.as_ref()
    }

    fn act(&self, name: &str, key: String, input: String, deadline_ms: u64) {
        self.acts.borrow_mut().push(EffectSpec::keyed(name, key, input, deadline_ms));
    }
}

impl World for SnapWorld<'_> {
    fn repo_names(&self) -> Vec<String> {
        vec![HOME.into()]
    }
    fn repo_root(&self, name: &str) -> Option<PathBuf> {
        (name == HOME).then(|| PathBuf::from(&self.snap.repo))
    }
    fn repo_field(&self, _: &str, _: &str) -> Option<String> {
        None
    }
    fn same_repo(&self, a: &Path, b: &Path) -> bool {
        a == b
    }
    fn home_repo(&self) -> String {
        HOME.into()
    }
    fn landref(&self, _: &Path) -> Option<String> {
        self.snap.base.clone()
    }
    fn repo_land(&self, _: &str) -> String {
        match self.snap.mode {
            Mode::Local => "queue.local".into(),
            Mode::Release | Mode::Checkout => "queue".into(),
        }
    }
    fn ref_remote(&self, _: &str, _: &Path) -> Option<String> {
        None
    }
    fn ref_branch(&self, base: &str) -> String {
        base.rsplit('/').next().unwrap_or(base).into()
    }
    fn is_git_repo(&self, p: &Path) -> bool {
        p == Path::new(&self.snap.repo) && self.snap.tip.is_some()
    }
    fn harness_in(&self, _: &Path) -> Vec<String> {
        vec![]
    }
    fn harness_in_ref(&self, _: &Path, _: &str) -> Vec<String> {
        vec![]
    }
    fn changed_files(&self, _: &Path, _: &str, _: &str) -> Vec<String> {
        vec![]
    }
    fn tags_matching(&self, _: &Path, _: &str) -> Vec<String> {
        vec![]
    }
    fn local_tag_sidecars(&self, _: &Path) -> Vec<String> {
        self.snap.tags.clone()
    }
    fn rev_parse(&self, _: &Path, rev: &str, _: bool) -> Option<String> {
        if Some(rev) == self.snap.base.as_deref() {
            return self.snap.tip.clone();
        }
        if rev == "HEAD" {
            return self.running().cloned();
        }
        let tag = rev.strip_suffix("^{commit}").unwrap_or(rev);
        self.snap.tag_commits.get(tag).cloned()
    }
    fn rev_parse_short(&self, repo: &Path, rev: &str) -> Option<String> {
        self.rev_parse(repo, rev, false).map(|s| s.chars().take(7).collect())
    }
    fn is_ancestor(&self, _: &Path, _: &str, _: &str) -> bool {
        self.snap.behind.is_some()
    }
    fn rev_list_count(&self, _: &Path, _: &str) -> Option<u64> {
        self.snap.behind
    }
    fn current_branch(&self, _: &Path) -> String {
        String::new()
    }
    fn dirty_tracked(&self, _: &Path) -> String {
        String::new()
    }
    fn stash_push(&self, _: &Path, _: &str) -> Result<(), String> {
        Err("the snapshot world writes nothing".into())
    }
    fn restore_deleted(&self, _: &Path) -> Result<(), String> {
        Err("the snapshot world writes nothing".into())
    }
    fn fetch(&self, _: &Path, _: &str) -> bool {
        true
    }
    fn diff_status(&self, _: &Path, _: &str) -> Vec<StatusRow> {
        vec![]
    }
    fn show_file(&self, _: &Path, _: &str, _: &str) -> Option<Vec<u8>> {
        None
    }
    fn file_mode(&self, _: &Path, _: &str, _: &str) -> Option<String> {
        None
    }
    fn reset_mixed(&self, _: &Path, _: &str) -> bool {
        false
    }
    fn merge_ff_only(&self, _: &Path, _: &str) -> bool {
        false
    }
    fn release_verify_no_pre_activate(&self, _: &str, _: Option<&Path>) -> (bool, String) {
        (self.snap.verify.ok, self.snap.verify.out.clone())
    }
    fn release_status(&self) -> String {
        self.snap.hotfix_line.clone().unwrap_or_default()
    }
    fn release_build_verify_activate(&self, _: &str, _: &Path, _: &str, _: &Path) -> Result<(), String> {
        Err("the snapshot world writes nothing".into())
    }
    fn overrides_apply(&self, _: &Path) {}
    fn install_diff(&self, _: &Path) -> (i32, String) {
        (0, String::new())
    }
    fn which(&self, _: &str) -> Option<String> {
        None
    }
    fn is_executable(&self, _: &Path) -> bool {
        false
    }
    fn gh_release_list(&self, _: &str) -> Result<String, String> {
        Err("the snapshot carries the release tags".into())
    }
    fn mail_send_question(&self, subject: &str, body: &str, default_action: &str) -> Result<(), String> {
        let q = Question { from: FROM.into(), subject: subject.into(), body: body.into(), default: default_action.into() };
        self.act(ESCALATE, format!("watch.skew:{:016x}", fnv1a(subject)), serde_json::to_string(&q).unwrap_or_default(), ESCALATE_DEADLINE_MS);
        Ok(())
    }
    fn now_stamp(&self) -> String {
        String::new()
    }
    fn env(&self, k: &str) -> Result<Option<String>, String> {
        Ok(match k {
            "SPIRA_RELEASES" => Some(self.cfg.releases.clone()),
            "SPIRA_RELEASE_REPO" => Some(self.cfg.release_repo.clone()),
            "SPIRA_GH_INTAKE_REPO" => Some(self.cfg.gh_intake_repo.clone()),
            "SPIRA_GH" => Some(self.cfg.gh.clone()),
            "SPIRA_RUN" => Some(self.cfg.run.clone()),
            "SPIRA_QUEUE_DIR" => Some(self.cfg.queue_dir.clone()),
            "SPIRA_REPO" => Some(self.snap.repo.clone()),
            _ => None,
        })
    }
    fn is_symlink(&self, p: &Path) -> bool {
        p == self.releases().join("current") && self.snap.activated.is_some() && self.snap.mode != Mode::Checkout
    }
    fn readlink(&self, p: &Path) -> Option<String> {
        (p == self.releases().join("current")).then(|| self.snap.activated.clone()).flatten()
    }
    fn exists(&self, _: &Path) -> bool {
        true
    }
    fn read_to_string(&self, p: &Path) -> Option<String> {
        let name = self.snap.activated.as_deref()?;
        if p == self.releases().join(name).join("MANIFEST") {
            return self.snap.manifest_commit.as_ref().map(|c| format!("commit {c}\n"));
        }
        if p == self.releases().join(".tags").join(name) {
            return self.snap.tag_sidecar.clone();
        }
        None
    }
    fn write_staged(&self, _: &Path, _: &[u8], _: bool) -> Result<(), String> {
        Err("the snapshot world writes nothing".into())
    }
    fn remove_file(&self, _: &Path) {}
    fn mkdir_p(&self, _: &Path) {}
    fn queue_lock_held(&self, _: &str) -> bool {
        self.snap.queue_lock_held
    }
    fn stamp_read(&self, key: &str) -> Option<String> {
        let c = self.carry.borrow();
        if let Some(k) = key.strip_prefix(ESCALATED) {
            return c.escalated.get(k).cloned();
        }
        if key.starts_with(DECLINED) {
            return c.declined.get(&self.snap.repo).map(u32::to_string);
        }
        None
    }
    fn stamp_write(&self, key: &str, val: &str) {
        let mut c = self.carry.borrow_mut();
        if let Some(k) = key.strip_prefix(ESCALATED) {
            c.escalated.insert(k.into(), val.into());
        } else if key.starts_with(DECLINED) {
            match val.trim().parse::<u32>() {
                Ok(0) | Err(_) => c.declined.remove(&self.snap.repo),
                Ok(n) => c.declined.insert(self.snap.repo.clone(), n),
            };
        }
    }
    fn run_log_append(&self, name: &str, line: &str) {
        if name == skew::REFRESH_ALARM_LOG {
            self.act(LOG, format!("watch.skew:refresh-declined:{}:{:016x}", self.snap.repo, fnv1a(line)), line.into(), WRITE_DEADLINE_MS);
        }
    }
    fn index_lock_age_secs(&self, _: &Path) -> Option<u64> {
        None
    }
    fn remove_index_lock(&self, _: &Path) -> Result<(), String> {
        Ok(())
    }
    fn git_running_in(&self, _: &Path) -> bool {
        false
    }
    fn out(&self, _: &str) {}
    fn err(&self, _: &str) {}
}

/// Who the question is from (`skew/src/real.rs`, `send_question`).
pub const FROM: &str = "Skew check <skew@spira>";

/// The refresh judgement, then the check, then `escalate` once per condition key, all skew's own
/// functions run over the snapshot. Under `queue.local` the judgement is skew's check-only
/// `refresh`. Elsewhere a running commit behind its base is refreshed by `watch.skew.refresh`
/// (skew's `refresh` on the host) unless the queue lock is held: a land is in flight, so the pass
/// declines and the decline streak (skew's own, alarming at 3 and every 180 after) counts it.
pub fn decide(carry: &Carry, snap: &Snapshot, _now_ms: u64, cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    let w = SnapWorld { snap, cfg, carry: std::cell::RefCell::new(carry.clone()), acts: std::cell::RefCell::new(vec![]) };
    let repo = PathBuf::from(&snap.repo);
    match snap.mode {
        Mode::Local => {
            skew::refresh(&w, None);
        }
        Mode::Release | Mode::Checkout => match (w.running(), snap.tip.as_ref()) {
            (Some(running), Some(tip)) if running != tip => {
                if snap.queue_lock_held {
                    skew::decline_streak_bump(&w, &repo, "the queue lock is held; a land is in flight");
                } else {
                    w.act(REFRESH, format!("watch.skew:refresh:{}:{tip}", snap.repo), snap.repo.clone(), REFRESH_DEADLINE_MS);
                }
            }
            _ => skew::decline_streak_clear(&w, &repo),
        },
    }
    skew::check(&w, true);
    (w.carry.into_inner(), w.acts.into_inner())
}

/// The host's skew world, under the declared harness home.
fn real() -> Result<skew::real::Real, String> {
    Ok(skew::real::Real::new(spira_config::resolve::locate_home_for_process()?))
}

/// The snapshot from the host's skew world: the same reads `check` and `refresh` make there.
pub fn snapshot(w: &dyn World, cfg: &Config) -> Snapshot {
    let home = w.home_repo();
    let repo = w.repo_root(&home).filter(|p| w.is_git_repo(p)).unwrap_or_else(|| PathBuf::from(w.env("SPIRA_REPO").ok().flatten().unwrap_or_default()));
    let releases = PathBuf::from(&cfg.releases);
    let current = releases.join("current");
    let linked = w.is_symlink(&current);
    let mode = match (w.repo_land(&home).as_str(), linked) {
        ("queue.local", _) => Mode::Local,
        (_, true) => Mode::Release,
        (_, false) => Mode::Checkout,
    };
    let activated = linked.then(|| w.readlink(&current)).flatten();
    let manifest_commit = match &activated {
        Some(name) => w.read_to_string(&releases.join(name).join("MANIFEST")).and_then(|t| t.lines().find_map(|l| l.strip_prefix("commit ")).map(|c| c.trim().to_string())),
        None => w.rev_parse(&repo, "HEAD", false),
    };
    let base = if mode == Mode::Local { w.landref(Path::new(&home)) } else { w.landref(&repo) };
    let tip = base.as_ref().and_then(|b| w.rev_parse(&repo, b, false));
    let behind = match (&manifest_commit, &tip) {
        (Some(m), Some(t)) if m != t && w.is_ancestor(&repo, m, t) => w.rev_list_count(&repo, &format!("{m}..{t}")),
        _ => None,
    };
    let verify = match (&activated, mode) {
        (Some(name), Mode::Local) => {
            let (ok, out) = w.release_verify_no_pre_activate(name, Some(&releases));
            Verify { ok, out }
        }
        _ => Verify { ok: true, out: String::new() },
    };
    let hotfix_line = w.release_status().lines().find(|l| l.starts_with("RUNNING UNLANDED ")).map(str::to_string);
    let tag_sidecar = activated.as_ref().and_then(|n| w.read_to_string(&releases.join(".tags").join(n)));
    let (mut tags, mut tag_commits) = (vec![], BTreeMap::new());
    if mode == Mode::Release {
        tags = w.tags_matching(&repo, "spira-release-*");
        tags.extend(w.tags_matching(&repo, "spira-[0-9]*.[0-9]*.[0-9]*"));
        let dir = cfg.release_repo.strip_prefix("file://").unwrap_or(&cfg.release_repo);
        if tags.is_empty() && dir.starts_with('/') {
            tags = w.local_tag_sidecars(Path::new(dir));
        }
        for t in &tags {
            if let Some(c) = w.rev_parse(&repo, &format!("{t}^{{commit}}"), false) {
                tag_commits.insert(t.clone(), c);
            }
        }
    }
    let queue_lock_held = w.queue_lock_held(&home);
    Snapshot { repo: repo.display().to_string(), mode, activated, manifest_commit, base, tip, hotfix_line, verify, queue_lock_held, tags, tag_sidecar, decline_streak: 0, stamps: BTreeMap::new(), behind, tag_commits }
}

/// `watch.skew.observe`: the snapshot, from the host.
pub fn observe(_: &crate::Effects, _: &str, _: &EffectSpec) -> Result<String, String> {
    let cfg = Config::pinned()?;
    serde_json::to_string(&snapshot(&real()?, &cfg)).map_err(|e| e.to_string())
}

/// `watch.skew.escalate`: the operator question, through the mail library in-process.
pub fn escalate(_: &crate::Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
    let q: Question = serde_json::from_str(&spec.input).map_err(|e| e.to_string())?;
    real()?.mail_send_question(&q.subject, &q.body, &q.default).map(|_| q.subject)
}

/// `watch.skew.log`: one line to `run/skew-refresh.log`, the refresh alarm log row.
pub fn log(_: &crate::Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
    real()?.run_log_append(skew::REFRESH_ALARM_LOG, &spec.input);
    Ok(skew::REFRESH_ALARM_LOG.into())
}

/// `watch.skew.refresh`: skew's own refresh of the repo, on the host (fetch, stage-and-swap or
/// release build/verify/activate, then overrides). Under `queue.local` it is never asked.
pub fn refresh(_: &crate::Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
    match skew::refresh(&real()?, Some(&spec.input)) {
        0 => Ok(format!("refreshed {}", spec.input)),
        rc => Err(format!("skew refresh of {} exited {rc}", spec.input)),
    }
}

pub struct WatchSkew {
    pub config: Result<Config, String>,
}

impl WatchSkew {
    pub fn new(config: Config) -> Self {
        WatchSkew { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchSkew { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.skew/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Map { name: "escalated", drop: &[] }, runtime::rows::Part::Map { name: "declined", drop: &[] }];

impl Handler for WatchSkew {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            return pattern::tick(ev, OBSERVE, String::new(), OBSERVE_DEADLINE_MS, ev.at + CADENCE_MS);
        }
        pattern::on_answer(ctx, view, ev, OBSERVE, STATE, &self.config, decide)
    }
}
