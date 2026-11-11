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
use runtime::units::{Exit, UnitRan};
use serde::{Deserialize, Serialize};

use super::batcher_local::{green, handle_p2, interests, table_step, unit_key, On, P2};
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
/// The judgement leg's cap (the contract's unverified 600 s): a leg with no `unit.ran` by then is failed.
pub const JUDGEMENT_CAP_MS: u64 = 600_000;

pub const ACTS: &[(&str, u64)] = &[
    (PUSH, 60_000),
    (LAND, 5_000),
    (CLOSE_BEAD, 5_000),
    (GH_CLOSEOUT, 30_000),
    (PR_CLOSE, 30_000),
    (CANCEL_RUNS, 30_000),
    (RERUN, 30_000),
    (JUDGEMENT, 10_000),
    (OBSERVE_FLAKE, 10_000),
    (EJECT, 10_000),
    (RECONCILE, 10_000),
    (SWEEP, 10_000),
    (FILE_BUG, 10_000),
    (NOTIFY, 5_000),
    (RECORD, 2_000),
];
pub const EMITS: &[EventKindTag] = &[CUT_WANTED, PUBLISH_WANTED];
/// Hand-offs that wake it at once, besides its tick: batcher.forge left a batch to settle.
pub const WAKES: &[EventKindTag] = &[super::batcher_forge::SETTLE_WANTED];
pub const EFFECTS: &[&str] = &[runtime::units::UNIT_FACTS, OBSERVE, PUSH, LAND, CLOSE_BEAD, GH_CLOSEOUT, PR_CLOSE, CANCEL_RUNS, RERUN, JUDGEMENT, OBSERVE_FLAKE, EJECT, RECONCILE, SWEEP, FILE_BUG, NOTIFY, RECORD];
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
/// Read by the queue's own verbs when a leg runs one, never by this task; `BD_TIMEOUT` is gone
/// with bd (the task's store writes ride `spira-lc`'s socket).
pub const UNREGISTERED: &[&str] = &["SPIRA_REAP_TIMEOUT", "SPIRA_QUEUE_ACTOR"];

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

impl Check {
    /// `check-status`'s first line, through `normalize_status`.
    pub fn of(status: &str) -> Check {
        match queue::ops::verdict::normalize_status(status.lines().next().unwrap_or("").trim()) {
            "pending" => Check::Pending,
            "green" => Check::Green,
            "red" => Check::Red,
            "harness_fault" => Check::HarnessFault,
            _ => Check::None,
        }
    }
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
    /// The snapshot asks nothing of this pass: a refused, settled, foreign-owned or unchecked
    /// batch (not in the contract's table, which gives `Observing` no quiet exit).
    Nothing,
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
    (State::Observing, Trigger::Nothing, State::Idle),
    (State::Pushing, Trigger::EffectSucceeded, State::Landing),
    (State::Pushing, Trigger::EffectFailed, State::Idle),
    (State::Landing, Trigger::EffectSucceeded, State::Cutting),
    // Not in the contract's table, which gives a refused land no exit: the store said no.
    (State::Landing, Trigger::EffectFailed, State::Idle),
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
    /// The batch this machine is acting on, as `id:version`, and the last one it settled: a
    /// settled batch the projection still shows is not acted on twice.
    #[serde(default)]
    pub batch: Option<String>,
    #[serde(default)]
    pub settled: Option<String>,
    /// The judgement leg's unit key and when it started.
    #[serde(default)]
    pub leg: Option<String>,
    #[serde(default)]
    pub leg_at_ms: u64,
    /// The base the judged batch was read on, for the cut its green judgement wants.
    #[serde(default)]
    pub base: String,
    /// The repo's mode when the judgement started (`queue.local` cuts and publishes, a forge repo cuts).
    #[serde(default)]
    pub mode: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub repos: BTreeMap<String, RepoCarry>,
}

/// Everything one act carries as its input.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub repo: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub batch: String,
    #[serde(default)]
    pub version: u64,
    #[serde(default)]
    pub pr: Option<u64>,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub head: String,
    #[serde(default)]
    pub members: Vec<String>,
    #[serde(default)]
    pub bead: String,
    /// RECORD's file under `queue/<repo>/`, NOTIFY's subject, FILE_BUG's title.
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub unit: String,
}

impl Step {
    fn on(repo: &str, mode: &str) -> Step {
        Step { repo: repo.into(), mode: mode.into(), ..Step::default() }
    }

    fn act(&self, effect: &str) -> Act {
        Act::new(effect, self.key.clone()).with_input(serde_json::to_string(self).unwrap_or_default())
    }
}

/// The pinned values `decide` reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub infra_retries: u64,
}

impl Default for Policy {
    /// The registered default (`spira/conf.d/SPIRA_QUEUE_INFRA_RETRIES`).
    fn default() -> Self {
        Policy { infra_retries: 2 }
    }
}

impl Policy {
    pub fn pinned(pinned: &BTreeMap<String, String>) -> Policy {
        Policy { infra_retries: pinned.get("SPIRA_QUEUE_INFRA_RETRIES").and_then(|v| v.trim().parse().ok()).unwrap_or(Policy::default().infra_retries) }
    }
}

pub fn step(state: State, event: Trigger) -> Result<State, String> {
    table_step(NAME, TABLE, state, event)
}

struct Machine<'a> {
    repo: &'a str,
    mode: String,
    rc: RepoCarry,
    acts: Vec<Act>,
    now_ms: u64,
}

impl Machine<'_> {
    fn go(&mut self, on: Trigger) -> bool {
        step(self.rc.state, on).map(|to| self.rc.state = to).is_ok()
    }

    fn step_of(&self) -> Step {
        let (batch, version) = self.rc.batch.as_deref().and_then(|b| b.rsplit_once(':')).map_or((String::new(), 0), |(b, v)| (b.to_string(), v.parse().unwrap_or(0)));
        Step { batch, version, ..Step::on(self.repo, &self.mode) }
    }

    fn notify(&mut self, subject: String, body: String, head: &str) {
        self.acts.push(Step { key: format!("{}/{subject}/{head}", self.repo), subject, body, head: head.into(), ..self.step_of() }.act(NOTIFY));
    }

    /// `Cutting` and on: a cut is wanted, and a local repo's publish after it.
    fn cut(&mut self, base: &str) {
        let input = serde_json::json!({ "repo": self.repo, "mode": self.mode }).to_string();
        self.acts.push(Act::new(CUT_WANTED, format!("{}/{}", self.repo, self.now_ms)).with_input(input.clone()));
        if self.mode == "queue.local" {
            self.go(Trigger::EmittedLocal);
            self.acts.push(Act::new(PUBLISH_WANTED, format!("{}/{base}", self.repo)).with_input(input));
            self.go(Trigger::Emitted);
        } else {
            self.go(Trigger::EmittedForge);
        }
        self.rc.settled = self.rc.batch.take().or(self.rc.settled.take());
        self.rc.leg = None;
    }

    fn judgement_failed(&mut self, why: &str) {
        self.go(Trigger::JudgementRanFailed);
        let b = self.step_of();
        self.notify(format!("judgement for {} failed", b.batch), why.to_string(), &b.batch);
        self.rc.settled = self.rc.batch.take();
        self.rc.leg = None;
        self.rc.base.clear();
        self.rc.mode.clear();
    }

    /// The judgement leg ended with `exit` (its `unit.ran`, on this task's stream): a green
    /// judgement is recorded and the cut wanted on the base the batch was judged on.
    fn judged(&mut self, exit: Exit) {
        if green(exit) {
            self.go(Trigger::JudgementRanOk);
            let leg = self.rc.leg.clone().unwrap_or_default();
            self.acts.push(Step { subject: "judgement".into(), body: leg, key: format!("{}/judgement", self.repo), ..self.step_of() }.act(RECORD));
            let base = std::mem::take(&mut self.rc.base);
            self.rc.mode.clear();
            self.cut(&base);
        } else {
            self.judgement_failed("the batcher's judgement of a red batch failed; the batch waits for a hand");
        }
    }

    fn observe(&mut self, policy: &Policy, repo: &Repo) {
        if self.rc.state == State::Judging {
            // The leg's end arrives as its own `unit.ran` (`on_ran`); none by the cap is a failure.
            if self.now_ms.saturating_sub(self.rc.leg_at_ms) > JUDGEMENT_CAP_MS {
                self.judgement_failed("the batcher's judgement of a red batch did not finish in time; the batch waits for a hand");
            }
            return;
        }
        if !matches!(self.rc.state, State::Idle | State::Waiting) {
            return;
        }
        self.go(Trigger::Tick);
        let Some(b) = &repo.open_batch else {
            // No batch: the repo's cut and publish are their own tasks' ticks; nothing is wanted
            // until a batch here settles (silence: an idle repo asks for nothing).
            self.go(Trigger::NoBatchNoPublish);
            if self.mode == "queue.local" {
                self.go(Trigger::EmittedLocal);
                self.go(Trigger::Emitted);
            } else {
                self.go(Trigger::EmittedForge);
            }
            self.rc.last_check = Check::None;
            return;
        };
        let id = format!("{}:{}", b.id, b.version);
        // `owner_refused` (`ops/verdict.rs:1052`): a Concierge-owned batch refuses the pass.
        if self.rc.settled.as_deref() == Some(id.as_str()) || (!b.batcher_owned && b.owner != "batcher") {
            self.go(Trigger::Nothing);
            return;
        }
        self.rc.batch = Some(id);
        let at = Step { pr: b.pr, branch: b.branch.clone(), head: b.sealed_head.clone(), members: b.members.clone(), ..self.step_of() };
        if matches!(repo.pr_state, PrState::Closed | PrState::Merged) {
            self.go(Trigger::PrClosedOrMerged);
            self.acts.push(Step { key: b.id.clone(), ..at }.act(RECONCILE));
            return;
        }
        match repo.check {
            Check::Pending => {
                self.go(Trigger::CheckPending);
                if self.rc.last_check != Check::Pending {
                    self.acts.push(Step { key: format!("{}/forge-state", self.repo), subject: "forge-state".into(), body: "pending".into(), ..at }.act(RECORD));
                }
            }
            Check::Green if repo.sealed_base.as_deref().is_none_or(|s| s == repo.base_sha) => {
                self.go(Trigger::CheckGreenSealedBaseUnmoved);
                self.acts.push(Step { key: format!("{}/{}/{}", self.repo, b.branch, b.sealed_head), ..at }.act(PUSH));
            }
            Check::Green => {
                // The base moved under a green batch: the queue's own pass rebuilds it on the new
                // base (`base_moved`, `ops/verdict.rs:1692`); its sweep is that leg.
                self.go(Trigger::CheckGreenBaseMoved);
                self.acts.push(Step { key: format!("{}/{}", self.repo, self.now_ms), ..at }.act(SWEEP));
            }
            Check::Red if b.batcher_owned => {
                self.go(Trigger::CheckRedBatcherOwned);
                let unit = unit_key(self.repo, &format!("{}/{}", b.id, b.sealed_head), "judgement", 1);
                self.rc.leg = Some(unit.clone());
                self.rc.leg_at_ms = self.now_ms;
                self.rc.base = repo.base_sha.clone();
                self.rc.mode = self.mode.clone();
                self.acts.push(Step { key: unit.clone(), unit, ..at }.act(JUDGEMENT));
            }
            Check::HarnessFault => {
                self.go(Trigger::CheckHarnessFault);
                if queue::ops::verdict::classify_fault_rerun(u64::from(self.rc.fault_retries), policy.infra_retries) {
                    self.rc.fault_retries += 1;
                    self.acts.push(Step { key: format!("{}/{}", self.repo, b.branch), ..at }.act(RERUN));
                } else {
                    let (subject, head) = (format!("batch {} harness fault — retries spent", b.id), b.sealed_head.clone());
                    self.acts.push(Step { key: format!("{}/{subject}/{head}", self.repo), subject, body: format!("CI on {} never tested the head after {} re-runs", b.branch, self.rc.fault_retries), ..at }.act(NOTIFY));
                }
            }
            _ => {
                self.rc.batch = None;
                self.go(Trigger::Nothing);
            }
        }
        self.rc.last_check = repo.check;
    }

    fn outcome(&mut self, effect: &str, ok: bool, body: &str, s: &Step) {
        match (self.rc.state, effect) {
            (State::Pushing, PUSH) if ok => {
                self.go(Trigger::EffectSucceeded);
                self.acts.push(Step { key: format!("{}/{}", s.batch, s.version), ..s.clone() }.act(LAND));
            }
            (State::Pushing, PUSH) => {
                self.go(Trigger::EffectFailed);
                self.notify(format!("batch {} not landed — the fast-forward was refused", s.batch), body.trim().to_string(), &s.head);
                self.rc.batch = None;
            }
            (State::Landing, LAND) if ok => {
                self.go(Trigger::EffectSucceeded);
                for m in &s.members {
                    self.acts.push(Step { bead: m.clone(), key: format!("{m}/{}", s.head), ..s.clone() }.act(CLOSE_BEAD));
                    self.acts.push(Step { bead: m.clone(), key: format!("{m}/{}", s.head), ..s.clone() }.act(GH_CLOSEOUT));
                }
                self.cut(&s.head);
            }
            (State::Landing, LAND) => {
                self.go(Trigger::EffectFailed);
                self.notify(format!("batch {} pushed but not landed on the store", s.batch), body.trim().to_string(), &s.head);
                self.rc.batch = None;
            }
            (State::Judging, JUDGEMENT) if !ok => self.judgement_failed(&format!("the judgement did not start: {}", body.trim())),
            (State::Settling, RECONCILE | SWEEP | RERUN | NOTIFY) => {
                self.go(Trigger::Outcome);
                self.cut(&s.head);
            }
            _ => {}
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

/// A repo not yet carried waits a pass rather than push the record past [`crate::CARRY_BYTES`].
pub fn admits<T: Serialize>(carry: &T) -> bool {
    serde_json::to_string(carry).is_ok_and(|s| s.len() <= crate::CARRY_BYTES)
}

pub fn decide(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<Act>) {
    decide_with(&Policy::default(), carry, snap, 0)
}

/// `decide` with the pinned policy and the observe's time.
pub fn decide_with(policy: &Policy, carry: &Carry, snap: &Snapshot, now_ms: u64) -> (Carry, Vec<Act>) {
    let (mut next, mut acts) = (carry.clone(), vec![]);
    if !snap.world_running {
        return (next, acts);
    }
    for (name, repo) in snap.repos.iter().filter(|(_, r)| r.lc_ok) {
        let mut m = Machine { repo: name, mode: repo.mode.clone(), rc: carry.repos.get(name).cloned().unwrap_or_default(), acts: vec![], now_ms };
        m.observe(policy, repo);
        let before = next.clone();
        store(&mut next, name, m.rc);
        if !carry.repos.contains_key(name) && !admits(&next) {
            next = before;
            continue;
        }
        acts.extend(m.acts);
    }
    (next, acts)
}

/// The judgement leg's end, delivered on this task's stream: the repo judging it moves at once.
pub fn on_ran(carry: &Carry, ran: &UnitRan, now_ms: u64) -> Option<(Carry, Vec<Act>)> {
    let key = ran.key_for(NAME)?;
    let (repo, rc) = carry.repos.iter().find(|(_, rc)| rc.state == State::Judging && rc.leg.as_deref() == Some(key.as_str()))?;
    let mut next = carry.clone();
    let mut m = Machine { repo, mode: rc.mode.clone(), rc: rc.clone(), acts: vec![], now_ms };
    m.judged(ran.exit);
    let acts = m.acts;
    store(&mut next, repo, m.rc);
    Some((next, acts))
}

pub fn on_outcome(carry: &Carry, effect: &str, ok: bool, body: &str, s: &Step, now_ms: u64) -> (Carry, Vec<Act>) {
    let mut next = carry.clone();
    let mut m = Machine { repo: &s.repo, mode: s.mode.clone(), rc: carry.repos.get(&s.repo).cloned().unwrap_or_default(), acts: vec![], now_ms };
    m.outcome(effect, ok, body, s);
    let acts = m.acts;
    store(&mut next, &s.repo, m.rc);
    (next, acts)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueStep {
    pub cadence: Cadence,
    pub pinned: BTreeMap<String, String>,
    pub policy: Policy,
    interests: Vec<EventKindTag>,
}

impl QueueStep {
    pub fn new(pinned: BTreeMap<String, String>) -> Self {
        QueueStep { cadence: Cadence::Every(CADENCE_MS), policy: Policy::pinned(&pinned), pinned, interests: interests(TICK, WAKES) }
    }

    pub fn from_settings(settings: &crate::Settings) -> Self {
        Self::new(kit::pin(settings, NAME, KEYS))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("queue.step/boot", STREAM, EventKind::new(TICK, ""))
    }
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Map { name: "repos", drop: &[] }];

impl Handler for QueueStep {
    fn interests(&self) -> &[EventKindTag] {
        &self.interests
    }

    fn handle(&self, _ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        let observed = |carry: &Carry, body: &str| serde_json::from_str::<Snapshot>(body).ok().map(|snap| decide_with(&self.policy, carry, &snap, ev.at));
        let outcome = |carry: &Carry, effect: &str, ok: bool, body: &str, input: &str| match serde_json::from_str::<Step>(input) {
            Ok(s) => on_outcome(carry, effect, ok, body, &s, ev.at),
            Err(_) => (carry.clone(), vec![]),
        };
        let ran = |carry: &Carry, r: &UnitRan| on_ran(carry, r, ev.at);
        let legs = |carry: &Carry| carry.repos.values().filter(|rc| rc.state == State::Judging).filter_map(|rc| rc.leg.clone()).collect();
        handle_p2(&P2 { shape: &SHAPE, cadence: &self.cadence, acts: ACTS, emits: EMITS, effects: EFFECTS, wakes: WAKES }, view, ev, On { observed, outcome, ran, legs })
    }
}

/// The runners: forge verbs through `forge::client` (gh its only child), git as a child through
/// `runtime::ports::Proc`, the store over `spira-lc`'s socket, mail in-process, and the queue's
/// own verbs (`verdict`, `batch sweep`, `eject`) and the batcher's judgement as unit legs.
pub mod edge {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use queue::ports::Lc as _;
    use runtime::ports::{Proc, ProcSpec, SysProc};
    use spira_config::lc_call::LcClient;
    use spira_config::process::cfg;
    use spira_config::repos::Registry;

    use super::super::batcher_local::edge::start_unit;
    use super::{Check, OpenBatch, PrState, PublishRec, Repo, Snapshot, Step};
    use crate::{Effects, EffectSpec};

    pub const ACTOR: &str = "queue.step";

    pub fn step(spec: &EffectSpec) -> Result<Step, String> {
        serde_json::from_str(&spec.input).map_err(|e| format!("{}: bad input: {e}", spec.name))
    }

    pub fn registry() -> Result<Registry, String> {
        let home = spira_config::resolve::locate_home_for_process()?;
        Ok(Registry::from_env(Default::default(), &home))
    }

    pub fn root(reg: &Registry, repo: &str) -> Result<PathBuf, String> {
        reg.root(repo).map(PathBuf::from).ok_or_else(|| format!("{repo}: the registry has no row for it"))
    }

    pub fn forge() -> Result<PathBuf, String> {
        cfg("SPIRA_FORGE").map(PathBuf::from)
    }

    /// One forge verb, in this process.
    pub fn forge_call(repo: &Path, verb: &str, args: &[&str], input: Option<&[u8]>) -> Result<forge::client::Reply, String> {
        Ok(forge::client::run(&forge::client::route_of(&forge()?), verb, repo, args, input))
    }

    /// One git command in `dir`, a child through the Proc port; its stdout, or why it failed.
    pub fn git(dir: &Path, args: &[&str], deadline: Duration) -> Result<String, String> {
        let mut all = vec!["-C".to_string(), dir.display().to_string()];
        all.extend(args.iter().map(|a| a.to_string()));
        let out = SysProc.run(&ProcSpec { prog: "git".into(), args: all, stdin: vec![], deadline })?;
        if out.code == Some(0) {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
        }
    }

    pub fn lc(e: &Effects) -> LcClient {
        LcClient::socket(&e.world.lc_socket)
    }

    fn json<T: serde::Serialize>(v: &T) -> Result<String, String> {
        serde_json::to_string(v).map_err(|e| e.to_string())
    }

    /// `queue/<repo>/<name>` as `key=value` lines.
    pub fn queue_kv(e: &Effects, repo: &str, name: &str) -> Option<std::collections::BTreeMap<String, String>> {
        let text = std::fs::read_to_string(e.world.queue_dir.join(repo).join(name)).ok()?;
        Some(text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect())
    }

    /// The probe: per `queue`/`queue.local` repo, the open batch from the store's batch view, its
    /// PR state and check from the forge, the publish record and the base.
    pub fn observe(e: &Effects, _: &str, _: &EffectSpec) -> Result<String, String> {
        let reg = registry()?;
        let lc = lc(e);
        let view = store_view::Projection::hot(lc.clone())?;
        view.batches_current().map_err(|e| e.to_string())?;
        let lc_ok = match store_view::shared() {
            Some(held) => held.rows(&store_view::RowQuery { states: &["IN_DELIVERY"], ..Default::default() }).is_ok(),
            None => lc.call(&["list", "--state", "IN_DELIVERY"]).is_ok_and(|r| r.code == 0),
        };
        let mut snap = Snapshot { world_running: !e.world.run_dir.join("world.halted").exists(), ..Snapshot::default() };
        for name in reg.names().into_iter().filter(|n| reg.land_queued(n)) {
            let dir = root(&reg, &name)?;
            let mode = reg.land(&name);
            let open_batch = view.batch_view(&name).map_err(|e| e.to_string())?.value.open.map(|r| OpenBatch {
                version: r.get("version").and_then(|v| v.parse().ok()).unwrap_or_default(),
                pr: r.get("pr").and_then(|v| v.trim_start_matches('#').parse().ok()),
                branch: r.get("branch").unwrap_or_default().to_string(),
                sealed_head: r.get("head").unwrap_or_default().to_string(),
                members: r.get("members").unwrap_or_default().split([' ', ',']).filter(|m| !m.is_empty()).map(|m| m.split([':', '@']).next().unwrap_or(m).to_string()).collect(),
                batcher_owned: r.get("owner") == Some("batcher"),
                owner: r.get("owner").unwrap_or_default().to_string(),
                id: r.id,
            });
            let (mut pr_state, mut check) = (PrState::None, Check::None);
            if let Some(b) = &open_batch {
                if let Some(pr) = b.pr.map(|p| p.to_string()) {
                    pr_state = match forge_call(&dir, "pr-state", &[&pr], None)?.stdout().unwrap_or_default().trim() {
                        "OPEN" | "open" => PrState::Open,
                        "MERGED" | "merged" => PrState::Merged,
                        "CLOSED" | "closed" => PrState::Closed,
                        _ => PrState::None,
                    };
                    check = Check::of(&forge_call(&dir, "check-status", &[&pr, &b.branch], None)?.stdout().unwrap_or_default());
                }
            }
            let publish_rec = queue_kv(e, &name, "publish").and_then(|kv| Some(PublishRec { pr: kv.get("pr")?.trim_start_matches('#').parse().ok()?, head: kv.get("head").cloned().unwrap_or_default() }));
            let landref = spira_config::repos::landref(&reg, &name).unwrap_or_default();
            let base_sha = git(&dir, &["rev-parse", &landref], Duration::from_secs(10)).unwrap_or_default();
            let sealed_base = open_batch.as_ref().and_then(|_| queue_kv(e, &name, "batch")).and_then(|kv| kv.get("base").cloned());
            snap.repos.insert(name, Repo { mode, lc_ok, open_batch, publish_rec, pr_state, check, base_sha, sealed_base, ..Repo::default() });
        }
        json(&snap)
    }

    /// The fast-forward of the land ref to the batch's sealed head; non-fast-forward is refused.
    pub fn push(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let s = step(spec)?;
        let reg = registry()?;
        let dir = root(&reg, &s.repo)?;
        let landref = spira_config::repos::landref(&reg, &s.repo).ok_or_else(|| format!("{}: no land ref", s.repo))?;
        match spira_config::repos::ref_remote(&landref, Some(&s.repo)) {
            Some(remote) => git(&dir, &["push", &remote, &format!("{}:refs/heads/{}", s.head, spira_config::repos::ref_branch(&landref))], Duration::from_secs(55))?,
            None => {
                git(&dir, &["merge-base", "--is-ancestor", &landref, &s.head], Duration::from_secs(10)).map_err(|_| format!("{landref} is not an ancestor of {} — non-fast-forward, refused", s.head))?;
                git(&dir, &["update-ref", &format!("refs/heads/{landref}"), &s.head], Duration::from_secs(10))?
            }
        };
        Ok("{}".into())
    }

    /// The batch's LANDED on the store, refused unless it is GREEN at that version.
    pub fn land(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let s = step(spec)?;
        let lc = queue::real::RealLc { bin: None, client: Some(lc(e)) };
        lc.land_batch(&s.batch, &s.version.to_string(), ACTOR, &s.head).map_err(|(rc, why)| format!("spira-lc land refused for {} (rc={rc}): {why}", s.batch))?;
        Ok("{}".into())
    }

    /// A landed member's bead, closed with the landing's reason (`land_close_reason`).
    pub fn close_bead(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let s = step(spec)?;
        let r = lc(e).call_stdin(&["close", &s.bead, "--reason-file", "-", "--actor", ACTOR], &queue::ops::helpers::land_close_reason(&s.head)).map_err(|e| e.to_string())?;
        if r.code != 0 {
            return Err(format!("close {}: {}", s.bead, r.why()));
        }
        Ok("{}".into())
    }

    /// `gh-intake closeout` for the landed member, as a unit: only its binary has it.
    pub fn gh_closeout(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let s = step(spec)?;
        let bin = spira_config::release_env::tool("gh-intake")?;
        start_unit(e, super::NAME, &format!("closeout.{}", s.bead), vec![bin, "closeout".into(), s.bead.clone(), s.head.clone()], vec![], None)
    }

    pub fn pr_close(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let s = step(spec)?;
        let dir = root(&registry()?, &s.repo)?;
        let pr = s.pr.ok_or("no PR to close")?.to_string();
        let r = forge_call(&dir, "pr-close", &[&pr], None)?;
        r.ok().then(|| "{}".to_string()).ok_or_else(|| format!("pr-close {pr}: {}", r.err.trim()))
    }

    pub fn cancel_runs(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let s = step(spec)?;
        let dir = root(&registry()?, &s.repo)?;
        queue::ops::helpers::cancel_branch_runs(&forge()?, &dir, &s.branch, ACTOR).then(|| "{}".to_string()).ok_or_else(|| format!("{}: not every run on {} was cancelled", s.repo, s.branch))
    }

    /// The branch's last run, re-run once (`workflow-rerun`); the forge may decide against it.
    pub fn rerun(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let s = step(spec)?;
        let dir = root(&registry()?, &s.repo)?;
        let run = forge_call(&dir, "run-id", &[&s.branch], None)?.stdout().map(|o| o.trim().to_string()).filter(|r| !r.is_empty()).ok_or_else(|| format!("no run on {}", s.branch))?;
        let r = forge_call(&dir, "workflow-rerun", &[&run], None)?;
        r.ok().then(|| "{}".to_string()).ok_or_else(|| format!("workflow-rerun {run}: {}", r.err.trim()))
    }

    /// `batcher judgement-ci`, as a unit: the CI-only judgement of a batcher-owned red.
    pub fn judgement(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let s = step(spec)?;
        if cfg("SPIRA_BATCHER_ENABLE")?.trim() == "0" {
            return Err("SPIRA_BATCHER_ENABLE=0: no judgement".into());
        }
        let dir = root(&registry()?, &s.repo)?;
        let pr = s.pr.map(|p| p.to_string()).unwrap_or_default();
        let status = forge_call(&dir, "check-status", &[&pr, &s.branch], None)?.out;
        let suites = status.lines().filter_map(|l| l.strip_prefix("red-suite: ")).collect::<Vec<_>>().join(",");
        let bin = spira_config::release_env::tool("batcher")?;
        let argv = vec![bin, "judgement-ci".into(), s.repo.clone(), "--suites".into(), suites, "--members".into(), s.members.join(","), "--evidence".into(), format!("CI red on PR {pr} at {}", s.head)];
        start_unit(e, super::NAME, &s.unit, argv, vec![], None)
    }

    /// `testenv suites observe-flake <suite> <sha>`, as a unit.
    pub fn observe_flake(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let s = step(spec)?;
        let bin = spira_config::release_env::tool("testenv")?;
        start_unit(e, super::NAME, &format!("flake.{}.{}", s.subject, s.head), vec![bin, "suites".into(), "observe-flake".into(), s.subject.clone(), s.head.clone()], vec![], None)
    }

    /// `queue <args>`, as a unit: the queue's own verbs this task has no library entry for.
    fn queue_leg(e: &Effects, key: &str, args: &[&str]) -> Result<String, String> {
        let bin = spira_config::release_env::tool("queue")?;
        start_unit(e, super::NAME, key, std::iter::once(bin).chain(args.iter().map(|a| a.to_string())).collect(), vec![], None)
    }

    pub fn eject(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let s = step(spec)?;
        queue_leg(e, &format!("eject.{}.{}", s.batch, s.bead), &["eject", &s.bead, "--reason", &s.body, &s.repo])
    }

    /// A closed or merged batch PR: the queue's verdict pass reconciles it (`ops/reconcile.rs`).
    pub fn reconcile(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let s = step(spec)?;
        queue_leg(e, &format!("reconcile.{}", s.batch), &["verdict", &s.repo])
    }

    pub fn sweep(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let s = step(spec)?;
        queue_leg(e, &format!("sweep.{}.{}", s.repo, s.key.rsplit('/').next().unwrap_or("0")), &["batch", "sweep", "--repo", &s.repo])
    }

    /// A bug for the repo's red head, filed over the store's work door.
    pub fn file_bug(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let s = step(spec)?;
        let r = lc(e).call_stdin(&["work", "-", "file", &s.subject, "--for", "builder", "--repo", &s.repo, "--body-file", "-", "--actor", ACTOR], &s.body).map_err(|e| e.to_string())?;
        if r.code != 0 {
            return Err(format!("work file {:?}: {}", s.subject, r.why()));
        }
        Ok(r.out.trim().to_string())
    }

    /// Mail to the session mailbox (`SPIRA_MAIL_SESSION_MAILBOX`), in-process.
    pub fn notify(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let s = step(spec)?;
        let env = e.mail.as_deref().ok_or("no mail environment configured")?;
        let mailbox = cfg("SPIRA_MAIL_SESSION_MAILBOX")?;
        let args = mail::cmds::SendArgs { mailbox: &mailbox, from: Some("queue <queue@spira>"), subject: &s.subject, kind: "note", default: "", class: "", bead: "", urgent: false, digest: false, dry_run: false, requested_by: "" };
        let bd = mail::bead::BdCli { bdq: String::new(), conn_retries: 1 };
        mail::cmds::send(&bd, env, &args, format!("## {}\n{}\n", s.repo, s.body)).map(|o| o.delivered_path.display().to_string())
    }

    /// `queue/<repo>/<subject>`, written whole.
    pub fn record(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let s = step(spec)?;
        if s.subject.is_empty() || s.subject.contains('/') {
            return Err(format!("record: bad file name {:?}", s.subject));
        }
        let dir = e.world.queue_dir.join(&s.repo);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let (tmp, path) = (dir.join(format!(".{}.tmp", s.subject)), dir.join(&s.subject));
        std::fs::write(&tmp, format!("{}\n", s.body)).and_then(|_| std::fs::rename(&tmp, &path)).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok("{}".into())
    }
}
