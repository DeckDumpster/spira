//! # batcher.forge — contract
//!
//! Unit `spira-rounds-forge-prod` (`batcher rounds-forge`, every 30 s, `TimeoutStartSec=3600`), pattern P2,
//! from [[scheduler-consolidation-2026-11-03]] §1 A. Paths are relative to `spira-releases/current`. It shares
//! the cut, the pure core and the round VM corpus with [[batcher-local]]; this page lists only what differs.
//!
//! **Code vs catalogue.** (1) The pass is per `land == queue` repo: *settle, then cut* (`batcher-cut/src/main.rs:1183-1189`).
//! Settle forks `queue verdict <repo>` and waits on CI (`main.rs:1256-1264`) — the same machine as [[queue-step]],
//! so this task must hand off to it, not run it. (2) The forge child is `forge pr-arm-queue` (`io.rs:1501-1515`);
//! the PR itself is opened by `queue batch open` (`io.rs:577-580`). (3) With `SPIRA_BATCHER_FORGE_ENABLE=0`
//! the pass returns at `main.rs:1306` before `page_held` (`main.rs:1315-1318`), so **`rounds-forge` never
//! writes the BATCHES HELD line** its config doc promises (`spira/conf.d/SPIRA_BATCHER_FORGE_ENABLE:4`); only the
//! retired combined `rounds` pass did. The task should restore it.
//!
//! ## Tick
//!
//! - Cadence 30 s, single-flight per queue repo; repos run independently (today they run in series, `main.rs:1217`).
//! - Admitted only while `SPIRA_BATCHER_FORGE_ENABLE != 0` (`main.rs:1299`, `:1204`). No pacing:
//!   pacing applies to queue.local only (`main.rs:1327`).
//! - Wakes (proposed names): `bead.certified`, `batch.settled` from [[queue-step]], `unit.ran` for its own legs.
//!
//! ## Observe
//!
//! Probe `batcher.forge.observe` (deadline 10 s), per repo:
//!
//! - **LcClient (S7a):** the same pool reads as [[batcher-local]] (`io.rs:212`, `:221`, `:244`, `:372-377`, `:495`, `:928`).
//! - **Batch projection (S7b):** today `queue batch show --repo <r>` → open batch PR, members (`io.rs:552-575`).
//!   An open batch means *no cut* (`main.rs:285-288`).
//! - **Config (S10):** registry `land/root/landref/forge` (`main.rs:142-167`), `SPIRA_LOOM_ADDR` (`main.rs:1132`).
//! - Git edge reads: branch tips (`io.rs:394`), `confirm_base` and `foreign_beads` (`io.rs:625-692`).
//! - Carried, not observed: the held-page marker `run/rounds-held/<repo>` (`main.rs:1238-1253`), base-moved, conflict streaks.
//!
//! ## Decide
//!
//! Same pure core as [[batcher-local]] (`batcher/src/core.rs:44-717`, `drive.rs:296`) minus pacing, plus
//! `pr_record` / `opened_event` (`batcher/src/core.rs`) and the held-page rule: page once per change of the
//! waiting CERTIFIED set while disabled (`main.rs:1235-1254`). Carry: per repo machine state, held set,
//! base-moved, conflict streaks. Fixtures: as [[batcher-local]], plus `main.rs` `drive_pass`/`plan` tests.
//!
//! **States:** `Idle`, `Disabled`, `Settling`, `Assembling`, `Corpus`, `Attributing`, `Opening`, `Open`.
//!
//! | State | Event | → State |
//! |---|---|---|
//! | Idle | observe: forge disabled | Disabled (`inbox.append` once per waiting set) |
//! | Disabled | observe: enabled | Idle |
//! | Idle | observe: open batch PR | Settling (hand-off: emit `batch.settle_wanted` to [[queue-step]]; `arm`) |
//! | Settling | `batch.settled` | Idle |
//! | Idle | observe: no batch, `should_cut` = None | Idle (silent) |
//! | Idle | observe: trigger | Assembling (`fetch`, `assemble`) |
//! | Assembling | merged empty / held key | Idle |
//! | Assembling | merged, no suites (`main.rs:616`) | Opening |
//! | Assembling | merged, suites | Corpus (`corpus` = unit.start) |
//! | Corpus | `unit.ran` red | Attributing (`withdraw` owners, re-run leg) |
//! | Attributing | `unit.ran` → land / blocked | Opening / Idle (verdict red, `main.rs:1016-1028`) |
//! | Corpus | `unit.ran` green | Opening |
//! | Opening | moved members (`main.rs:1032`) or foreign commits (`main.rs:1126`) | Idle (verdict red) |
//! | Opening | `batch_open` ok | Open (verdict green, metric) |
//! | Open | `batch.settled` | Idle |
//!
//! Illegal: a second `batch_open` while the projection shows one open is refused naming `Open`.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `batcher.forge.observe` | store | 10 s | tick | — |
//! | `batcher.forge.arm` | forge | 60 s (bounded, unverified) | repo | — (`io.rs:1501`) |
//! | `batcher.forge.fetch` / `.assemble` | git | 60 s / 120 s | repo + round_key | lock held (`main.rs:261-264`) |
//! | `batcher.forge.corpus` | unit (`unit.start`) | `SPIRA_BATCHER_WALL_SECS` | repo + round + run n | one live run (`vm.rs:370`) |
//! | `batcher.forge.withdraw` | store | 30 s | bead + version | not CERTIFIED (`io.rs:1178-1194`, `:851`) |
//! | `batcher.forge.shots` | container | 1500 s (`io.rs:1477-1478`) | repo + base + head | — best effort |
//! | `batcher.forge.batch_open` | forge | 120 s (unverified) | repo + head | batch already open (`io.rs:577-586`) |
//! | `batcher.forge.file` (judgement / incident) | store | 30 s | judgement ref | duplicate (`io.rs:2129`, `main.rs:461`) |
//! | `batcher.forge.verdict_write` | store | 5 s | repo + at | — (`io.rs:1434`) |
//! | `inbox.append` | store | 10 s | line | — |
//!
//! `batch.settle_wanted` is an Emit, not an effect: settlement belongs to [[queue-step]].
//!
//! ## Silence
//!
//! Idle tick, an open batch already being settled, or a disabled repo whose waiting set has not changed:
//! the probe only.
//!
//! ## Replaces
//!
//! `spira-rounds-forge-prod.{service,timer}`; children `queue verdict <repo>` (`main.rs:1258`),
//! `forge pr-arm-queue` (`io.rs:1503`), `queue batch show|open` (`io.rs:553`, `:580`), `timeout 1500 testenv shots`
//! (`io.rs:1477`), `systemd-run … round-vm run` (`vm.rs:381`), `timeout spira-lc` (`io.rs:197`), `bash lib.sh bead_reopen`
//! (`io.rs:1189`, `:149`), `bead.sh file` (`io.rs:2146`), `tsd-write` (`io.rs:2110`), `escape-classify.sh` (`io.rs:1212`).
//! Keys pinned: those of [[batcher-local]]'s `env_for` (`main.rs:187-223`) plus `SPIRA_BATCHER_FORGE_ENABLE`
//! (`main.rs:1299`) and `SPIRA_LOOM_ADDR` (`main.rs:1132`).
//!
//! ## Reverse
//!
//! Release rollback restores the unit; `Observe` mode (S4) shadows every effect; `SPIRA_BATCHER_FORGE_ENABLE=0`
//! remains the operator hold in both.
//!

use std::collections::BTreeMap;

use batcher::core;
use runtime::event::*;
use runtime::units::{Exit, UnitRan};
use serde::{Deserialize, Serialize};

use super::batcher_local::{P2, digest, green, handle_p2, interests, leg_exit, page_line, select, table_step, unit_key, units_ran, Assembled, Leg, Policy, LEG_GRACE_MS};
use super::kit::{self, Act, Cadence, Shape};

pub use super::batcher_local::Member;

pub const NAME: &str = "batcher.forge";
pub const STREAM: &str = "task.batcher.forge";
pub const TICK: EventKindTag = "batcher.forge.tick";
pub const STATE: EventKindTag = "batcher.forge.state";
/// The hand-off of an open batch PR's settlement to [`super::queue_step`]: an Emit.
pub const SETTLE_WANTED: EventKindTag = "batch.settle_wanted";
/// [`super::queue_step`]'s answer: the batch settled. Read here as the batch projection no longer
/// showing the batch open, since an Emit lands on its emitter's stream, not this one.
pub const SETTLED: EventKindTag = "batch.settled";
pub const OBSERVE: &str = "batcher.forge.observe";
pub const ARM: &str = "batcher.forge.arm";
pub const FETCH: &str = "batcher.forge.fetch";
pub const ASSEMBLE: &str = "batcher.forge.assemble";
pub const CORPUS: &str = "batcher.forge.corpus";
pub const WITHDRAW: &str = "batcher.forge.withdraw";
pub const SHOTS: &str = "batcher.forge.shots";
pub const BATCH_OPEN: &str = "batcher.forge.batch_open";
pub const FILE: &str = "batcher.forge.file";
pub const VERDICT_WRITE: &str = "batcher.forge.verdict_write";
pub const INBOX_APPEND: &str = crate::watchers::INBOX_APPEND;

pub const CADENCE_MS: u64 = 30_000;
pub const OBSERVE_DEADLINE_MS: u64 = 10_000;

pub const ACTS: &[(&str, u64)] = &[
    (ARM, 60_000),
    (FETCH, 60_000),
    (ASSEMBLE, 120_000),
    (CORPUS, super::batcher_local::UNIT_START_DEADLINE_MS),
    (WITHDRAW, 30_000),
    (SHOTS, 1_500_000),
    (BATCH_OPEN, 120_000),
    (FILE, 30_000),
    (VERDICT_WRITE, 5_000),
    (INBOX_APPEND, 10_000),
];
pub const EMITS: &[EventKindTag] = &[SETTLE_WANTED];
pub const EFFECTS: &[&str] = &[OBSERVE, ARM, FETCH, ASSEMBLE, CORPUS, WITHDRAW, SHOTS, BATCH_OPEN, FILE, VERDICT_WRITE, INBOX_APPEND];
pub const KEYS: &[&str] = &[
    "SPIRA_RUN",
    "SPIRA_DB",
    "SPIRA_QUEUE_DIR",
    "SPIRA_FORGE",
    "SPIRA_HYPERVISOR_MEM_MIB",
    "SPIRA_ROUND_VM_MEM_MIB",
    "SPIRA_CI_RUNNER_MEM_MIB",
    "SPIRA_BATCH_MAXPAR",
    "SPIRA_BATCHER_WALL_SECS",
    "SPIRA_GIT_NAME",
    "SPIRA_GIT_EMAIL",
    "SPIRA_LC_TIMEOUT",
    "SPIRA_BATCHER_ORPHAN_REDRIVE_SECS",
    "SPIRA_BATCHER_FORGE_ENABLE",
    "SPIRA_LOOM_ADDR",
];
pub const UNREGISTERED: &[&str] = &["SPIRA_BATCHER_ROUND_SLOTS", "SPIRA_BATCHER_POLL_SECS", "SPIRA_BATCHER_MEND_DEADLINE_SECS", "SPIRA_VERDICTS", "ROUND_CPU_QUOTA"];

pub const SHAPE: Shape = Shape { name: NAME, stream: STREAM, tick: TICK, state: STATE, observe: OBSERVE, observe_deadline_ms: OBSERVE_DEADLINE_MS };

/// The batch projection's open batch PR.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    pub pr: u64,
    pub head: String,
    pub members: Vec<String>,
}

/// One admitted `queue` repository.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    pub pool: Vec<Member>,
    pub blockers: Vec<String>,
    pub basefail: Vec<String>,
    pub open_batch: Option<Batch>,
    pub branch_tips: BTreeMap<String, String>,
    pub base_sha: String,
    /// `confirm_base` agreed the base is what the cut was assembled on.
    pub base_confirmed: bool,
    /// Commits on the assembled head that belong to no member (`foreign_beads`).
    pub foreign: Vec<String>,
}

impl Repo {
    /// The pool as [`super::batcher_local`]'s cut reads it.
    fn local(&self) -> super::batcher_local::Repo {
        super::batcher_local::Repo { pool: self.pool.clone(), blockers: self.blockers.clone(), basefail: self.basefail.clone(), branch_tips: self.branch_tips.clone(), base_sha: self.base_sha.clone(), ..Default::default() }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// `SPIRA_BATCHER_FORGE_ENABLE` as pinned.
    pub enabled: bool,
    pub repos: BTreeMap<String, Repo>,
    pub now_ms: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum State {
    #[default]
    Idle,
    Disabled,
    Settling,
    Assembling,
    Corpus,
    Attributing,
    Opening,
    Open,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    ForgeDisabled,
    ForgeEnabled,
    OpenBatch,
    BatchSettled,
    NoTrigger,
    Cut,
    MergedEmpty,
    MergedNoSuites,
    MergedSuites,
    LegRed,
    LegGreen,
    RerunLands,
    RerunBlocked,
    MovedOrForeign,
    BatchOpened,
    /// A second `batch_open` while the projection shows one open.
    BatchOpen,
}

pub static TABLE: &[(State, Trigger, State)] = &[
    (State::Idle, Trigger::ForgeDisabled, State::Disabled),
    (State::Disabled, Trigger::ForgeDisabled, State::Disabled),
    (State::Disabled, Trigger::ForgeEnabled, State::Idle),
    (State::Idle, Trigger::OpenBatch, State::Settling),
    (State::Settling, Trigger::BatchSettled, State::Idle),
    (State::Idle, Trigger::NoTrigger, State::Idle),
    (State::Idle, Trigger::Cut, State::Assembling),
    (State::Assembling, Trigger::MergedEmpty, State::Idle),
    (State::Assembling, Trigger::MergedNoSuites, State::Opening),
    (State::Assembling, Trigger::MergedSuites, State::Corpus),
    (State::Corpus, Trigger::LegRed, State::Attributing),
    (State::Attributing, Trigger::RerunLands, State::Opening),
    (State::Attributing, Trigger::RerunBlocked, State::Idle),
    (State::Corpus, Trigger::LegGreen, State::Opening),
    (State::Opening, Trigger::MovedOrForeign, State::Idle),
    (State::Opening, Trigger::BatchOpened, State::Open),
    (State::Open, Trigger::BatchSettled, State::Idle),
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoCarry {
    pub state: State,
    pub round_key: Option<String>,
    /// The waiting CERTIFIED set last paged while disabled: page once per change of it.
    pub held: Vec<String>,
    pub base_moved: Option<(String, u64)>,
    pub conflict_streaks: BTreeMap<String, u32>,
    /// Round keys whose cut assembled nothing: not re-cut until the pool changes.
    #[serde(default)]
    pub held_keys: Vec<String>,
    #[serde(default)]
    pub members: Vec<(String, String)>,
    #[serde(default)]
    pub wt: String,
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub leg: Option<String>,
    #[serde(default)]
    pub leg_at_ms: u64,
    #[serde(default)]
    pub run: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub repos: BTreeMap<String, RepoCarry>,
}

/// What `batch_open` answers: the PR it opened.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Opened {
    #[serde(default)]
    pub pr: String,
}

pub fn step(state: State, event: Trigger) -> Result<State, String> {
    table_step(NAME, TABLE, state, event)
}

struct Machine<'a> {
    repo: &'a str,
    rc: RepoCarry,
    acts: Vec<Act>,
    now_ms: u64,
}

impl Machine<'_> {
    fn go(&mut self, on: Trigger) -> bool {
        step(self.rc.state, on).map(|to| self.rc.state = to).is_ok()
    }

    fn leg(&self) -> Leg {
        Leg { members: self.rc.members.clone(), wt: self.rc.wt.clone(), base: self.rc.base.clone(), round_key: self.rc.round_key.clone().unwrap_or_default(), ..Leg::on(self.repo) }
    }

    fn corpus(&mut self, run: u32) {
        self.rc.run = run;
        let unit = unit_key(self.repo, self.rc.round_key.as_deref().unwrap_or_default(), "corpus", run);
        self.rc.leg = Some(unit.clone());
        self.rc.leg_at_ms = self.now_ms;
        let leg = Leg { key: unit.clone(), unit, ..self.leg() };
        self.acts.push(leg.act(CORPUS));
    }

    fn open(&mut self) {
        self.rc.leg = None;
        let key = format!("{}:{}", self.repo, digest(self.rc.round_key.as_deref().unwrap_or_default()));
        self.acts.push(Leg { key, ..self.leg() }.act(BATCH_OPEN));
    }

    fn verdict(&mut self, verdict: &str, why: &str) {
        self.acts.push(Leg { key: format!("{}:{}", self.repo, self.now_ms), bead: verdict.into(), why: why.into(), ..Leg::on(self.repo) }.act(VERDICT_WRITE));
    }

    /// Back to Idle with the round forgotten.
    fn reset(&mut self) {
        let keep = (self.rc.state, self.rc.held.clone(), self.rc.base_moved.clone(), self.rc.conflict_streaks.clone(), self.rc.held_keys.clone());
        self.rc = RepoCarry { state: keep.0, held: keep.1, base_moved: keep.2, conflict_streaks: keep.3, held_keys: keep.4, ..RepoCarry::default() };
    }

    fn observe(&mut self, policy: &Policy, enabled: bool, repo: &Repo, ran: &[UnitRan]) {
        if !enabled && matches!(self.rc.state, State::Idle | State::Disabled) {
            self.go(Trigger::ForgeDisabled);
            // `page_held` (`main.rs:1235-1254`): once per change of the waiting CERTIFIED set.
            let waiting: Vec<String> = repo.pool.iter().filter(|m| m.state == "CERTIFIED").map(|m| m.id.clone()).collect();
            if !waiting.is_empty() && waiting != self.rc.held {
                let text = format!("BATCHES HELD {}: SPIRA_BATCHER_FORGE_ENABLE=0 with CERTIFIED work waiting: {}", self.repo, waiting.join(","));
                self.acts.push(Act::new(INBOX_APPEND, text.clone()).with_input(page_line(self.now_ms, &text)));
            }
            self.rc.held = waiting;
            return;
        }
        if self.rc.state == State::Disabled {
            self.go(Trigger::ForgeEnabled);
            self.rc.held.clear();
        }
        match self.rc.state {
            State::Idle => self.idle(policy, repo),
            State::Settling | State::Open if repo.open_batch.is_none() => {
                self.go(Trigger::BatchSettled);
                self.reset();
            }
            State::Corpus | State::Attributing => {
                let lost = self.now_ms.saturating_sub(self.rc.leg_at_ms) > policy.wall_ms + LEG_GRACE_MS;
                match leg_exit(NAME, self.rc.leg.as_deref(), ran) {
                    Some(exit) => self.leg_ended(exit),
                    None if lost => self.leg_ended(Exit::Unknown),
                    None => {}
                }
            }
            _ => {}
        }
    }

    fn idle(&mut self, policy: &Policy, repo: &Repo) {
        // `main.rs:285-288`: an open batch means no cut; its settlement is queue.step's.
        if let Some(b) = &repo.open_batch {
            self.go(Trigger::OpenBatch);
            let input = serde_json::json!({ "repo": self.repo, "pr": b.pr, "head": b.head }).to_string();
            self.acts.push(Act::new(SETTLE_WANTED, format!("{}:{}", self.repo, b.pr)).with_input(input));
            self.acts.push(Leg { key: self.repo.to_string(), ..Leg::on(self.repo) }.act(ARM));
            return;
        }
        match select(&repo.local(), policy.max_members) {
            Some((pool, key)) if !self.rc.held_keys.contains(&key) => {
                self.go(Trigger::Cut);
                self.rc.round_key = Some(key.clone());
                let members: Vec<(String, String)> = pool.iter().map(|m| (m.id.clone(), m.tip.clone())).collect();
                let versions = repo.pool.iter().filter(|m| members.iter().any(|(id, _)| id == &m.id)).map(|m| (m.id.clone(), m.version)).collect();
                self.acts.push(Leg { key: format!("{}:{}", self.repo, repo.base_sha), base: repo.base_sha.clone(), ..Leg::on(self.repo) }.act(FETCH));
                self.acts.push(Leg { key: format!("{}:{}", self.repo, digest(&key)), members, versions, round_key: key, base: repo.base_sha.clone(), ..Leg::on(self.repo) }.act(ASSEMBLE));
            }
            _ => {
                self.go(Trigger::NoTrigger);
            }
        }
    }

    fn leg_ended(&mut self, exit: Exit) {
        match (self.rc.state, green(exit)) {
            (State::Corpus, true) => {
                self.go(Trigger::LegGreen);
                self.open();
            }
            (State::Corpus, false) => {
                self.go(Trigger::LegRed);
                // A lone member owns any red: it is withdrawn while the rest re-run.
                if let [(bead, tip)] = &self.rc.members[..] {
                    self.acts.push(Leg { bead: bead.clone(), head: tip.clone(), key: bead.clone(), why: "the round's corpus was red".into(), ..Leg::on(self.repo) }.act(WITHDRAW));
                }
                let run = self.rc.run + 1;
                self.corpus(run);
            }
            (State::Attributing, true) => {
                self.go(Trigger::RerunLands);
                self.open();
            }
            (State::Attributing, false) => {
                self.go(Trigger::RerunBlocked);
                self.acts.push(Leg { key: format!("judgement:{}", digest(self.rc.round_key.as_deref().unwrap_or_default())), why: "the round's corpus was red twice".into(), ..self.leg() }.act(FILE));
                self.verdict("red", "local corpus red — held before reaching CI");
                self.reset();
            }
            _ => {}
        }
    }

    fn outcome(&mut self, effect: &str, ok: bool, body: &str, leg: &Leg) {
        match (self.rc.state, effect) {
            (State::Assembling, ASSEMBLE) => {
                let a: Assembled = if ok { serde_json::from_str(body).unwrap_or_default() } else { Assembled::default() };
                self.assembled(a, leg, ok);
            }
            (State::Corpus | State::Attributing, CORPUS) if !ok && self.rc.leg.as_deref() == Some(leg.unit.as_str()) => self.leg_ended(Exit::Unknown),
            (State::Opening, BATCH_OPEN) => {
                let o: Opened = if ok { serde_json::from_str(body).unwrap_or_default() } else { Opened::default() };
                if o.pr.is_empty() {
                    self.go(Trigger::MovedOrForeign);
                    self.verdict("red", body.trim());
                    self.reset();
                } else {
                    self.go(Trigger::BatchOpened);
                }
            }
            _ => {}
        }
    }

    fn assembled(&mut self, a: Assembled, leg: &Leg, ok: bool) {
        if !a.base.is_empty() && self.rc.base_moved.as_ref().map(|b| &b.0) != Some(&a.base) {
            self.rc.base_moved = Some((a.base.clone(), self.now_ms));
        }
        for (id, _) in &a.conflicts {
            let n = self.rc.conflict_streaks.get(id).copied().unwrap_or(0) + 1;
            if n >= core::CONFLICT_EJECT_ROUNDS {
                self.rc.conflict_streaks.remove(id);
                let version = leg.versions.get(id).copied().unwrap_or_default();
                self.acts.push(Leg { bead: id.clone(), key: format!("{id}:{version}"), why: format!("conflicted with the round {n} cuts running"), versions: [(id.clone(), version)].into(), ..Leg::on(self.repo) }.act(WITHDRAW));
            } else {
                self.rc.conflict_streaks.insert(id.clone(), n);
            }
        }
        let key = core::round_key(&a.merged);
        if a.merged.is_empty() || self.rc.held_keys.contains(&key) {
            if ok && !self.rc.held_keys.contains(&leg.round_key) {
                self.rc.held_keys.push(leg.round_key.clone());
                let over = self.rc.held_keys.len().saturating_sub(super::batcher_local::HELD_KEYS);
                self.rc.held_keys.drain(..over);
            }
            self.go(Trigger::MergedEmpty);
            self.reset();
            return;
        }
        self.rc.round_key = Some(key);
        self.rc.members = a.merged;
        self.rc.wt = a.wt;
        self.rc.base = a.base;
        if a.suites {
            self.go(Trigger::MergedSuites);
            self.corpus(1);
        } else {
            self.go(Trigger::MergedNoSuites);
            self.open();
        }
    }
}

fn store(carry: &mut Carry, repo: &str, rc: RepoCarry) {
    if rc == RepoCarry::default() {
        carry.repos.remove(repo);
    } else {
        carry.repos.insert(repo.to_string(), rc);
    }
}

pub fn decide(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<Act>) {
    decide_with(&Policy::default(), carry, snap, &[])
}

/// `decide` with the pinned policy and the `unit.ran`s seen since the carry was recorded.
pub fn decide_with(policy: &Policy, carry: &Carry, snap: &Snapshot, ran: &[UnitRan]) -> (Carry, Vec<Act>) {
    let (mut next, mut acts) = (carry.clone(), vec![]);
    for (name, repo) in &snap.repos {
        let mut m = Machine { repo: name, rc: carry.repos.get(name).cloned().unwrap_or_default(), acts: vec![], now_ms: snap.now_ms };
        m.observe(policy, snap.enabled, repo, ran);
        store(&mut next, name, m.rc);
        acts.extend(m.acts);
    }
    (next, acts)
}

pub fn on_outcome(carry: &Carry, effect: &str, ok: bool, body: &str, leg: &Leg, now_ms: u64) -> (Carry, Vec<Act>) {
    let mut next = carry.clone();
    let mut m = Machine { repo: &leg.repo, rc: carry.repos.get(&leg.repo).cloned().unwrap_or_default(), acts: vec![], now_ms };
    m.outcome(effect, ok, body, leg);
    let acts = m.acts;
    store(&mut next, &leg.repo, m.rc);
    (next, acts)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatcherForge {
    pub cadence: Cadence,
    pub pinned: BTreeMap<String, String>,
    pub policy: Policy,
    interests: [EventKindTag; 4],
}

impl BatcherForge {
    pub fn new(pinned: BTreeMap<String, String>) -> Self {
        BatcherForge { cadence: Cadence::Every(CADENCE_MS), policy: Policy::pinned(&pinned), pinned, interests: interests(TICK) }
    }

    pub fn from_settings(settings: &crate::Settings) -> Self {
        Self::new(kit::pin(settings, NAME, KEYS))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("batcher.forge/boot", STREAM, EventKind::new(TICK, ""))
    }
}

impl Handler for BatcherForge {
    fn interests(&self) -> &[EventKindTag] {
        &self.interests
    }

    fn handle(&self, _ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        let observed = |carry: &Carry, body: &str| serde_json::from_str::<Snapshot>(body).ok().map(|snap| decide_with(&self.policy, carry, &snap, &units_ran(view)));
        let outcome = |carry: &Carry, effect: &str, ok: bool, body: &str, input: &str| match serde_json::from_str::<Leg>(input) {
            Ok(leg) => on_outcome(carry, effect, ok, body, &leg, ev.at),
            Err(_) => (carry.clone(), vec![]),
        };
        handle_p2(&P2 { shape: &SHAPE, cadence: &self.cadence, acts: ACTS, emits: EMITS, effects: EFFECTS }, view, ev, observed, outcome)
    }
}

/// The runners. The cut, the corpus and the withdrawals are [`super::batcher_local`]'s seam; the
/// forge's own are the queue's arming, the shots and the PR.
pub mod edge {
    use batcher_cut::io;

    use super::super::batcher_local::edge::{env, leg};
    use super::{Batch, Member, Opened, Repo, Snapshot};
    use crate::{Effects, EffectSpec};

    fn json<T: serde::Serialize>(v: &T) -> Result<String, String> {
        serde_json::to_string(v).map_err(|e| e.to_string())
    }

    /// The probe: every `queue` repo's certified pool and open batch PR.
    pub fn observe(_: &Effects, _: &str, _: &EffectSpec) -> Result<String, String> {
        let env = env()?;
        io::lc_forget(&env);
        let reg = io::registry(&env);
        let enabled = spira_config::process::cfg("SPIRA_BATCHER_FORGE_ENABLE")?.trim() != "0";
        let mut snap = Snapshot { enabled, now_ms: spira_config::vtime::now_epoch() * 1000, ..Snapshot::default() };
        for name in reg.names().into_iter().filter(|n| reg.land(n) == "queue") {
            let repo = batcher_cut::find_repo(&env, &name)?;
            let pool: Vec<Member> = io::certified_pool(&env, &repo)?
                .into_iter()
                .map(|m| Member { state: "CERTIFIED".into(), sifted_tip: Some(m.tip.clone()), title: m.title.clone(), priority: m.priority.unwrap_or(u8::MAX), deps: m.blocked_by.clone(), stack: m.stack.iter().map(|(i, t)| format!("{i}:{t}")).collect(), base_fix: m.base_fix, express: m.express, certified_at: m.certified_at, id: m.id, tip: m.tip, ..Member::default() })
                .collect();
            let open_batch = io::read_open_batch(&env, &name)?.map(|b| Batch { pr: b.pr.trim_start_matches('#').parse().unwrap_or_default(), head: String::new(), members: b.members.into_iter().map(|(id, _)| id).collect() });
            let blockers = pool.iter().flat_map(|m| m.deps.clone()).collect();
            let branch_tips = pool.iter().map(|m| (m.id.clone(), m.tip.clone())).collect();
            snap.repos.insert(name, Repo { pool, blockers, open_batch, branch_tips, base_sha: io::resolve_base_sha(&repo)?, base_confirmed: true, ..Repo::default() });
        }
        json(&snap)
    }

    /// `forge pr-arm-queue`: the repo's merge queue is armed before a batch settles.
    pub fn arm(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        io::forge_arm_queue(&batcher_cut::find_repo(&env, &leg.repo)?)?;
        Ok("{}".into())
    }

    pub fn fetch(e: &Effects, k: &str, spec: &EffectSpec) -> Result<String, String> {
        super::super::batcher_local::edge::fetch(e, k, spec)
    }

    pub fn assemble(e: &Effects, k: &str, spec: &EffectSpec) -> Result<String, String> {
        super::super::batcher_local::edge::assemble(e, k, spec)
    }

    pub fn corpus(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        super::super::batcher_local::edge::corpus_for(super::NAME, e, spec)
    }

    pub fn withdraw(e: &Effects, k: &str, spec: &EffectSpec) -> Result<String, String> {
        super::super::batcher_local::edge::withdraw(e, k, spec)
    }

    /// `testenv shots` for the round's head, as the PR body's shots section; best effort.
    pub fn shots(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        let loom = spira_config::process::cfg("SPIRA_LOOM_ADDR").ok();
        Ok(io::batch_shots_section(&env.testenv_bin, &env.run, loom.as_deref(), &leg.repo, &leg.base, &leg.head))
    }

    /// The batch PR (`open_round_pr`): refused when a member moved since the round was built or
    /// the head carries a non-member's commits.
    pub fn batch_open(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        let repo = batcher_cut::find_repo(&env, &leg.repo)?;
        let members: Vec<batcher::core::Member> = leg.members.iter().map(|(id, tip)| batcher::core::Member { id: id.clone(), tip: tip.clone(), title: String::new(), priority: None, express: false, base_fix: false, certified_at: 0, stack: Default::default(), blocked_by: vec![] }).collect();
        let moved = io::moved_members(&repo, &leg.base, &members);
        if !moved.is_empty() {
            return Err(format!("refused to open — {} changed since the round was built (new patches); rebuild and retest", moved.join(", ")));
        }
        let head = io::head_of(std::path::Path::new(&leg.wt))?;
        let (pr, armed) = batcher_cut::open_round_pr(&env, &repo, &members, &head, spira_config::vtime::now_epoch(), None, None)?;
        if let Err(e) = armed {
            eprintln!("batcher.forge {}: PR {pr} opened, auto-merge not armed ({e}) — the reconciler arms it", leg.repo);
        }
        json(&Opened { pr })
    }

    /// The judgement a twice-red round files against its members.
    pub fn file(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        let repo = batcher_cut::find_repo(&env, &leg.repo)?;
        let j = batcher::core::Judgement { source: batcher::core::RedSource::Local, suites: vec![] };
        let ids: Vec<String> = leg.members.iter().map(|(id, _)| id.clone()).collect();
        io::file_judgement(&env, &repo, &j, &ids, &leg.why)
    }

    pub fn verdict_write(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        io::write_local_verdict(&env, &leg.repo, &leg.bead, &leg.why);
        Ok("{}".into())
    }
}
