//! # queue.step — contract
//!
//! Replaces `spira-verdict-prod` (`queue step --all`). Pattern P2 of [[scheduler-consolidation-2026-11-03]].
//! Paths are relative to `spira-releases/current/queue/src/`. Today one pass is, per queue-mode repo
//! under its step lock, **verdict → cut → publish** (`ops/simple.rs:514-526`): `verdict::pass`
//! (`:517`), a forked `batcher cut` (`:518` → `ops/simple.rs:357`, `real.rs:938`), and for
//! `queue.local` an in-process `publish` (`:522`).
//!
//! **Code vs catalogue.** `gate.sh` and `release` are not reached from `step --all`: `gate.sh` is
//! `queue submit` only (`ops/simple.rs:63`), `release` is `land-local` (`ops/land.rs:187`). testenv
//! appears only as `testenv suites observe-flake` (`real.rs:907`, called `ops/verdict.rs:1550`). The
//! real long legs are `batcher cut` and `batcher judgement-ci` (`real.rs:873`, called `ops/verdict.rs:2159`).
//!
//! ## Tick
//!
//! - Cadence 2 min (`spira-verdict-prod.timer` OnUnitActiveSec=2min, OnBootSec=60). Also woken by
//!   `forge.check_concluded{repo,pr}` (unverified: no such event exists yet; today CI conclusion is
//!   only seen by polling) and by `round.closed{repo}`.
//! - One machine **per repo** in `queue`/`queue.local` mode (`ops/simple.rs:477`); single-flight per
//!   repo replaces the step lock (`lock.rs:210`, `ops/simple.rs:409`). A busy repo is skipped, not
//!   failed (`ops/simple.rs:491`).
//!
//! ## Observe
//!
//! `queue.step.observe{repo}` → `Snapshot{mode, lc_ok, open_batch?, publish_rec?, pr_state, check, base_sha, lock_skips, provisional_reds}`:
//!
//! - repo list and per-repo settings: `Settings` (S10) replacing `Lib::repos` (`real.rs:614`) and the
//!   `bash lib.sh` `Op::Context` fork (`real.rs:515`, keys in `seam.rs:54-80`).
//! - `lc_ok`: LcClient `list --state IN_DELIVERY` (`real.rs:1882`), today `timeout spira-lc` (`real.rs:1482`).
//! - `open_batch`: the **batch projection (S7b)**, which `forge_batch::read` (`ops/forge_batch.rs:62`)
//!   answers today from the store's rounds door or the `queue/<repo>/batch` file.
//! - `publish_rec`: `queue/<repo>/publish` (`ops/verdict.rs:239`) — a file today; becomes a store record (unverified that S7 carries it).
//! - `pr_state`, `check`: forge `pr-state` / `check-status` (`real.rs:1367`, `:1376`; read at
//!   `ops/verdict.rs:264`, `:226`, `:1080`, `:1099`). These are forge reads inside the probe (class `forge`).
//! - `base_sha`: `git rev-parse <landref>` (`ops/verdict.rs:1508`).
//! - `provisional_reds`: `round::gate_red_at` per provisional member (`ops/verdict.rs:827-862`).
//!
//! ## Decide
//!
//! `decide(carry, snap) -> (carry, Vec<Act>)`, pure. Carry: `{lock_skips, fault_retries, red_streak, alarmed_head}`
//! (today files `lock-skips`, `publish-fault-retries`, `publish-red-streak`, `publish-missing-workflow`;
//! `ops/verdict.rs:866`, `:316-317`, `:332`). Existing pure cores reused: `normalize_status` (`:124`),
//! `infra_red` (`:148`), `classify_pending` (`:978`), `classify_fault_rerun` (`:997`). Fixtures: the
//! `tests.rs` step cases (`tests.rs:1905-2143`) and the verdict fixtures in `tests.rs`.
//!
//! ### States (per repo)
//!
//! `Idle, Observing, Settling, Pushing, Landing, Judging, Cutting, Publishing, Waiting`
//!
//! | state | event | → state | act |
//! |---|---|---|---|
//! | Idle | `queue.step.tick` | Observing | `queue.step.observe` |
//! | Observing | snapshot, no batch, no publish rec | Cutting | `queue.step.cut_wanted` |
//! | Observing | check `pending` | Waiting | `queue.step.record` (forge state) |
//! | Observing | check `green`, head = sealed, base unmoved | Pushing | `queue.step.push` (ff) |
//! | Observing | check `green`, base moved | Settling | `queue.step.push` (rebased head, `:1692`) |
//! | Observing | check `red`, batcher-owned | Judging | `queue.step.judgement` (unit.start) |
//! | Observing | check `harness_fault` | Settling | `queue.step.rerun` or `queue.step.notify` |
//! | Observing | PR `closed`/`merged` | Settling | `queue.step.reconcile` (`ops/reconcile.rs:22`) |
//! | Pushing | `effect.succeeded` | Landing | `queue.step.land` |
//! | Pushing | `effect.failed` | Idle | `queue.step.notify` (ff refused, `:301-311`) |
//! | Landing | `effect.succeeded` | Cutting | `queue.step.close_bead`, `queue.step.gh_closeout`, `queue.step.cut_wanted` |
//! | Judging | `unit.ran{ok, id=}` | Cutting | `queue.step.record` (judgement id, `:2167`) |
//! | Judging | `unit.ran{fail}` / deadline | Idle | `queue.step.notify` |
//! | Settling | outcome | Cutting | — |
//! | Cutting | emitted | Publishing (`queue.local`) / Idle | `queue.step.publish_wanted` |
//! | Publishing | emitted | Idle | — |
//! | Waiting | next tick | Observing | — |
//!
//! Any other (state, event) pair is refused naming the state (T3).
//!
//! ## Act
//!
//! | effect | class | deadline | idempotency key | refuses |
//! |---|---|---|---|---|
//! | `queue.step.observe` | store+forge (probe) | 30 s | `repo/tick` | world not RUNNING (`world guard` ExecStartPre) |
//! | `queue.step.push` | git | 60 s | `repo/remote/branch/head` | non-fast-forward (`:1528`, `:300`) |
//! | `queue.step.land` | store | 5 s | `batch_id/version` (`ops/verdict.rs:1643`, `real.rs:1857`) | state ≠ GREEN (`--expect GREEN`) |
//! | `queue.step.close_bead` | store | 5 s | `bead/sha` (`ops/helpers.rs:480`) | bead lacks submitted label (`ops/helpers.rs:476`) |
//! | `queue.step.gh_closeout` | forge | 30 s | `bead/sha` (`real.rs:690`, `gh-intake closeout`) | — (best effort) |
//! | `queue.step.pr_close` | forge | 30 s | `repo/pr` (`ops/verdict.rs:313`, `:947`, `:1332`) | — |
//! | `queue.step.cancel_runs` | forge | 30 s | `repo/branch` (`real.rs:782`) | — |
//! | `queue.step.rerun` | forge | 30 s | `repo/run` (`ops/verdict.rs:373`, `:1956`) | run already rerun once (`classify_fault_rerun`) |
//! | `queue.step.judgement` | unit | `unit.start`; ends on `unit.ran`, 600 s (unverified cap) | `repo/pr/head` | batcher off (`SPIRA_BATCHER_ENABLE=0`, `:2143`) |
//! | `queue.step.observe_flake` | container | 120 s (unverified) | `suite/sha` (`real.rs:907`) | — |
//! | `queue.step.eject` | store | 5 s | `batch/member/tip` (`ops/verdict.rs:849`) | concierge-owned batch (`:1052`) |
//! | `queue.step.reconcile` | store+forge | 60 s | `batch_id` | record opening (`ops/verdict.rs:84`) |
//! | `queue.step.sweep` | store+forge | 60 s | `repo/tick` (`ops/verdict.rs:906`) | file-era record |
//! | `queue.step.file_bug` | store | 10 s | `repo/head` (`ops/verdict.rs:687`, bash seam `Op::CreateBug`) | existing fix-forward marker |
//! | `queue.step.notify` | store (in-process mail) | 5 s | `repo/subject/head` (`real.rs:697`, `real.rs:914` forks `mail send`) | — |
//! | `queue.step.record` | host (queue dir) | 2 s | `repo/file` (`ops/verdict.rs:1124`) | — |
//! | `queue.step.cut_wanted` | — (Emit) | — | `repo/tick` | replaces the forked `batcher cut` (`real.rs:938`); wakes [[batcher-local]] / [[batcher-forge]] |
//! | `queue.step.publish_wanted` | — (Emit) | — | `repo/base_sha` | wakes [[publish]] instead of `ops/simple.rs:522` |
//!
//! Concierge-owned batches refuse the whole pass (`owner_refused`, `ops/verdict.rs:1052`; actor key `SPIRA_QUEUE_ACTOR`, `:1050`).
//!
//! ## Silence
//!
//! Idle tick (no open batch, no publish record, nothing new): one probe, no Emit, no Effect, no journal
//! line. Today the pass prints `step --all: stepped=…` every tick (`ops/simple.rs:495-505`); that line goes.
//! `Waiting` with an unchanged check status emits nothing (record only on change).
//!
//! ## Replaces
//!
//! `spira-verdict-prod.service` + `.timer`; the step lock (`lock.rs:210`); children: `bash lib.sh`
//! (`real.rs:427`), `timeout spira-lc` (`real.rs:1465-1520`), `timeout bdq` (`real.rs:361`), `forge`
//! (`real.rs:1318-1460`), `batcher cut` / `judgement-ci` (`real.rs:938`, `:873`), `mail send`
//! (`real.rs:914`), `gh-intake closeout` (`real.rs:690`), `testenv suites observe-flake` (`real.rs:907`),
//! `timeout hel reap-landed-branch` (`real.rs:667`, via `ops/helpers.rs:499`), `systemctl show`
//! (`real.rs:989`). Pins: `SPIRA_QUEUE_CI_MAXSEC[_<REPO>]`, `SPIRA_QUEUE_CI_IDLE_SEC[_<REPO>]`,
//! `SPIRA_QUEUE_INFRA_RETRIES`, `SPIRA_QUEUE_LOCK_WAIT`, `SPIRA_QUEUE_LOCK_STARVE_MAX`,
//! `SPIRA_QUEUE_CAPACITY_BACKOFF_{MIN,MAX}`, `SPIRA_BATCHER_ENABLE`, `SPIRA_FORGE`, `SPIRA_QUEUE_DIR`,
//! `SPIRA_MENDER_LABEL`, `SPIRA_RED_TRACKER_LABEL`, `SPIRA_INCIDENT_PRIORITY` (`seam.rs:57-80`),
//! `SPIRA_PUBLISH_RED_PRIORITY`, `SPIRA_PUBLISH_RED_ESCALATE_AFTER`, `SPIRA_MAIL_SESSION_MAILBOX`,
//! `SPIRA_LC_TIMEOUT`, `BD_TIMEOUT`, `SPIRA_REAP_TIMEOUT` (`real.rs:530-586`, `:355`, `:663`).
//!
//! ## Reverse
//!
//! Release rollback restores `spira-verdict-prod.timer`. Short of that, `Observe` mode (S4) journals
//! every act as `effect.shadowed`; `queue verdict <repo>` / `queue step <repo>` stay as operator verbs.
//!

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::kit::{self, Act, Cadence, Shape};

pub const NAME: &str = "queue.step";
pub const STREAM: &str = "task.queue.step";
pub const TICK: EventKindTag = "queue.step.tick";
pub const STATE: EventKindTag = "queue.step.state";
/// Wakes [`super::batcher_local`] / [`super::batcher_forge`] instead of a forked `batcher cut`.
pub const CUT_WANTED: EventKindTag = "queue.step.cut_wanted";
/// Wakes [`super::publish`] instead of the in-process publish (`ops/simple.rs:522`).
pub const PUBLISH_WANTED: EventKindTag = "queue.step.publish_wanted";
pub const OBSERVE: &str = "queue.step.observe";
pub const PUSH: &str = "queue.step.push";
pub const LAND: &str = "queue.step.land";
pub const CLOSE_BEAD: &str = "queue.step.close_bead";
pub const GH_CLOSEOUT: &str = "queue.step.gh_closeout";
pub const PR_CLOSE: &str = "queue.step.pr_close";
pub const CANCEL_RUNS: &str = "queue.step.cancel_runs";
pub const RERUN: &str = "queue.step.rerun";
pub const JUDGEMENT: &str = "queue.step.judgement";
pub const OBSERVE_FLAKE: &str = "queue.step.observe_flake";
pub const EJECT: &str = "queue.step.eject";
pub const RECONCILE: &str = "queue.step.reconcile";
pub const SWEEP: &str = "queue.step.sweep";
pub const FILE_BUG: &str = "queue.step.file_bug";
pub const NOTIFY: &str = "queue.step.notify";
pub const RECORD: &str = "queue.step.record";

pub const CADENCE_MS: u64 = 120_000;
pub const OBSERVE_DEADLINE_MS: u64 = 30_000;

pub const ACTS: &[(&str, u64)] = &[
    (PUSH, 60_000),
    (LAND, 5_000),
    (CLOSE_BEAD, 5_000),
    (GH_CLOSEOUT, 30_000),
    (PR_CLOSE, 30_000),
    (CANCEL_RUNS, 30_000),
    (RERUN, 30_000),
    (JUDGEMENT, 600_000),
    (OBSERVE_FLAKE, 120_000),
    (EJECT, 5_000),
    (RECONCILE, 60_000),
    (SWEEP, 60_000),
    (FILE_BUG, 10_000),
    (NOTIFY, 5_000),
    (RECORD, 2_000),
];
pub const EMITS: &[EventKindTag] = &[CUT_WANTED, PUBLISH_WANTED];
pub const EFFECTS: &[&str] = &[OBSERVE, PUSH, LAND, CLOSE_BEAD, GH_CLOSEOUT, PR_CLOSE, CANCEL_RUNS, RERUN, JUDGEMENT, OBSERVE_FLAKE, EJECT, RECONCILE, SWEEP, FILE_BUG, NOTIFY, RECORD];
pub const KEYS: &[&str] = &[
    "SPIRA_QUEUE_CI_MAXSEC",
    "SPIRA_QUEUE_CI_IDLE_SEC",
    "SPIRA_QUEUE_INFRA_RETRIES",
    "SPIRA_QUEUE_LOCK_WAIT",
    "SPIRA_QUEUE_LOCK_STARVE_MAX",
    "SPIRA_QUEUE_CAPACITY_BACKOFF_MIN",
    "SPIRA_QUEUE_CAPACITY_BACKOFF_MAX",
    "SPIRA_BATCHER_ENABLE",
    "SPIRA_FORGE",
    "SPIRA_QUEUE_DIR",
    "SPIRA_MENDER_LABEL",
    "SPIRA_RED_TRACKER_LABEL",
    "SPIRA_INCIDENT_PRIORITY",
    "SPIRA_PUBLISH_RED_PRIORITY",
    "SPIRA_PUBLISH_RED_ESCALATE_AFTER",
    "SPIRA_MAIL_SESSION_MAILBOX",
    "SPIRA_LC_TIMEOUT",
];
pub const UNREGISTERED: &[&str] = &["BD_TIMEOUT", "SPIRA_REAP_TIMEOUT", "SPIRA_QUEUE_ACTOR"];

pub const SHAPE: Shape = Shape { name: NAME, stream: STREAM, tick: TICK, state: STATE, observe: OBSERVE, observe_deadline_ms: OBSERVE_DEADLINE_MS };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenBatch {
    pub id: String,
    pub version: u64,
    pub pr: Option<u64>,
    pub branch: String,
    /// The head the batch was sealed at.
    pub sealed_head: String,
    pub members: Vec<String>,
    /// The batch was opened by the batcher (red goes to judgement), not by the Concierge.
    pub batcher_owned: bool,
    pub owner: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishRec {
    pub pr: u64,
    pub head: String,
}

/// `normalize_status` of the forge check.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    #[default]
    None,
    Pending,
    Green,
    Red,
    HarnessFault,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    #[default]
    None,
    Open,
    Closed,
    Merged,
}

/// One repository in `queue` or `queue.local` mode.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    /// `queue` or `queue.local`.
    pub mode: String,
    pub lc_ok: bool,
    pub open_batch: Option<OpenBatch>,
    pub publish_rec: Option<PublishRec>,
    pub pr_state: PrState,
    pub check: Check,
    pub base_sha: String,
    /// The base the batch was sealed on; differing from `base_sha` is a moved base.
    pub sealed_base: Option<String>,
    pub lock_skips: u32,
    pub provisional_reds: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub repos: BTreeMap<String, Repo>,
    /// The world is RUNNING; otherwise every repo's pass refuses.
    pub world_running: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum State {
    #[default]
    Idle,
    Observing,
    Settling,
    Pushing,
    Landing,
    Judging,
    Cutting,
    Publishing,
    Waiting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Tick,
    NoBatchNoPublish,
    CheckPending,
    CheckGreenSealedBaseUnmoved,
    CheckGreenBaseMoved,
    CheckRedBatcherOwned,
    CheckHarnessFault,
    PrClosedOrMerged,
    EffectSucceeded,
    EffectFailed,
    JudgementRanOk,
    JudgementRanFailed,
    Outcome,
    EmittedLocal,
    EmittedForge,
    Emitted,
    /// `unit.ran` of any unit.
    UnitRan,
}

pub static TABLE: &[(State, Trigger, State)] = &[
    (State::Idle, Trigger::Tick, State::Observing),
    (State::Observing, Trigger::NoBatchNoPublish, State::Cutting),
    (State::Observing, Trigger::CheckPending, State::Waiting),
    (State::Observing, Trigger::CheckGreenSealedBaseUnmoved, State::Pushing),
    (State::Observing, Trigger::CheckGreenBaseMoved, State::Settling),
    (State::Observing, Trigger::CheckRedBatcherOwned, State::Judging),
    (State::Observing, Trigger::CheckHarnessFault, State::Settling),
    (State::Observing, Trigger::PrClosedOrMerged, State::Settling),
    (State::Pushing, Trigger::EffectSucceeded, State::Landing),
    (State::Pushing, Trigger::EffectFailed, State::Idle),
    (State::Landing, Trigger::EffectSucceeded, State::Cutting),
    (State::Judging, Trigger::JudgementRanOk, State::Cutting),
    (State::Judging, Trigger::JudgementRanFailed, State::Idle),
    (State::Settling, Trigger::Outcome, State::Cutting),
    (State::Cutting, Trigger::EmittedLocal, State::Publishing),
    (State::Cutting, Trigger::EmittedForge, State::Idle),
    (State::Publishing, Trigger::Emitted, State::Idle),
    (State::Waiting, Trigger::Tick, State::Observing),
];

/// Carried per repo, replacing the `lock-skips`, `publish-fault-retries`, `publish-red-streak`
/// and `publish-missing-workflow` files.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoCarry {
    pub state: State,
    pub lock_skips: u32,
    pub fault_retries: u32,
    pub red_streak: u32,
    pub alarmed_head: Option<String>,
    /// The check last recorded, so `Waiting` records only on change.
    pub last_check: Check,
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
pub struct QueueStep {
    pub cadence: Cadence,
    pub pinned: BTreeMap<String, String>,
}

impl QueueStep {
    pub fn new(pinned: BTreeMap<String, String>) -> Self {
        QueueStep { cadence: Cadence::Every(CADENCE_MS), pinned }
    }

    pub fn from_cfg() -> Self {
        Self::new(kit::pin(NAME, KEYS))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("queue.step/boot", STREAM, EventKind::new(TICK, ""))
    }
}

impl Handler for QueueStep {
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
