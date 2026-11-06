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

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::kit::{self, Act, Cadence, Shape};

pub use super::batcher_local::Member;

pub const NAME: &str = "batcher.forge";
pub const STREAM: &str = "task.batcher.forge";
pub const TICK: EventKindTag = "batcher.forge.tick";
pub const STATE: EventKindTag = "batcher.forge.state";
/// The hand-off of an open batch PR's settlement to [`super::queue_step`]: an Emit.
pub const SETTLE_WANTED: EventKindTag = "batch.settle_wanted";
/// [`super::queue_step`]'s answer: the batch settled.
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
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub repos: BTreeMap<String, RepoCarry>,
}

pub fn step(_state: State, _event: Trigger) -> Result<State, String> {
    crate::not_ported!(NAME)
}

pub fn decide(_carry: &Carry, _snap: &Snapshot) -> (Carry, Vec<Act>) {
    crate::not_ported!(NAME)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatcherForge {
    pub cadence: Cadence,
    pub pinned: BTreeMap<String, String>,
}

impl BatcherForge {
    pub fn new(pinned: BTreeMap<String, String>) -> Self {
        BatcherForge { cadence: Cadence::Every(CADENCE_MS), pinned }
    }

    pub fn from_cfg() -> Self {
        Self::new(kit::pin(NAME, KEYS))
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
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, _ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        let carry: Carry = kit::carry(&SHAPE, view);
        if ev.kind.tag == TICK {
            return kit::on_tick(&SHAPE, serde_json::to_string(&carry).unwrap_or_default(), self.cadence.next_after(ev.at));
        }
        let Some(snap) = kit::observed::<Snapshot>(&SHAPE, view, ev) else { return vec![] };
        let (next, acts) = decide(&carry, &snap);
        kit::commands(&SHAPE, ACTS, EMITS, &carry, &next, acts)
    }
}
