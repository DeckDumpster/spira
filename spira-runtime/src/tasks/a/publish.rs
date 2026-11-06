//! # publish — contract
//!
//! Replaces `spira-publish-prod` (`queue publish-settle`, every 30 min). Pattern P2 of
//! [[scheduler-consolidation-2026-11-03]]. Paths are relative to `spira-releases/current/queue/src/`.
//! For every `queue.local` repo that publishes somewhere, it settles a stale publish PR and opens the
//! next one (`ops/publish.rs:420-461`, `settle_repo` `:463`). The open path, `publish_with`
//! (`ops/publish.rs:20`), is the same one [[queue-step]] runs in-process after each verdict pass
//! (`ops/simple.rs:522`); both callers become this task.
//!
//! **Code vs catalogue.** `release` is not a child: nothing in `ops/publish.rs` runs it (the one
//! `release` hit, `:516`, is a config-key name). There is no long-running leg in the process: the
//! forge's CI is the long leg, and its green/red is settled by [[queue-step]]
//! (`ops/verdict.rs:237`, `:544`), not here. P2 holds because the machine spans ticks (open → CI →
//! rerun → alarm), not because a unit runs; this contract has no `unit.start`.
//!
//! ## Tick
//!
//! - Cadence 30 min (OnUnitActiveSec=30min, OnBootSec=5min). Woken by `queue.step.publish_wanted{repo}`
//!   (replacing `ops/simple.rs:522`).
//! - Single-flight per repo; today the queue lock (`ops/publish.rs:474`, `lock.rs:142`); a busy lock
//!   is "retry next tick", not a failure (`:477`).
//!
//! ## Observe
//!
//! `publish.observe{repo}` → `Snapshot{mode, publish_mode, unpublished, stopped, publish_rec?, pr: Pr, open_publish_prs, forge_sha, head_sha, divergence, red_marker?, fix_forward_live, round_holds_hypervisor, deferred_ticks}`:
//!
//! - `mode`, `publish_mode`, `unpublished`, `publish`/`landref`: `Settings` (S10) from `spira_config::repos`
//!   (`real.rs:589-596`), replacing the `bash lib.sh` `Op::Context` fork (`real.rs:515`).
//! - `stopped`: `scoped_stop::stop_for(run, repo, "publish")` (`ops/publish.rs:54`).
//! - `publish_rec`: `queue/<repo>/publish` (`:491-492`); `red_marker`: `queue/<repo>/publish-red` (`:150`).
//! - `pr`: forge `pr-state` + `check-status` → `Pr{Open{ci,behind,tries}|Gone|Unknown}` (`:746-785`;
//!   `real.rs:1367`, `:1376`). `open_publish_prs`: forge `pr-list-open` (`:570`, `real.rs:1343`).
//! - `forge_sha`, `head_sha`: `git fetch` + `rev-parse` (`:97-115`); `divergence`: in-process
//!   `helpers::check_divergence` (`:116`, `ops/helpers.rs:346`).
//! - `fix_forward_live`: today the `Op::BeadLive` bash seam (`:155`, `real.rs:821`) → LcClient `show`.
//! - `round_holds_hypervisor`: `run/round-progress.json` (`:732-744`) else the **round/batch projection
//!   (S7b)** — today `round::live_records` + `spira-lc show-batch` per batch (`:710-729`, `real.rs:1622`).
//!
//! ## Decide
//!
//! Pure core exists: `decide(Option<Pr>) -> Settle` (`ops/publish.rs:391`) and `ci_of` (`:378`).
//! The new `decide(carry, snap)` wraps it with the deferral rule (`:506-527`) and the open-path guards
//! (`:39-50`, `:54`, `:89-96`, `:116-138`, `:142-146`, `:150-179`). Carry: `{deferred_ticks}` (today
//! `queue/<repo>/publish-deferred`, `:505`). Fixtures: `tests.rs:3808-3888` (publish-settle) and the
//! `decide` table tests.
//!
//! ### States (per repo)
//!
//! `Idle, Observing, Deferred, Adopting, Opening, Open, Rerun, Alarmed, Superseding, Refused`
//!
//! | state | event | → state | act |
//! |---|---|---|---|
//! | Idle | tick / `publish_wanted` | Observing | `publish.observe` |
//! | Observing | no record, one open publish PR on forge | Adopting | `publish.record` (adopt, `:563-643`) + `publish.pr_close` for older |
//! | Observing | `Settle::Open`, head ≠ forge, ancestor | Opening | `publish.push_branch` |
//! | Observing | `Settle::Open`, `publish_mode=push` | Opening | `publish.push_forge` (direct ff, `:224-245`) |
//! | Observing | head == forge / unpublished / stopped | Idle | — |
//! | Observing | diverged or cannot check | Refused | `publish.notify` (`ops/helpers.rs:346`) |
//! | Observing | red marker on same head, fix-forward live | Idle | — (wait, `:166-174`) |
//! | Observing | act ∉ {Wait,Parked,Alarm} and round holds hypervisor, ticks < max | Deferred | carry `deferred_ticks+1` |
//! | Observing | `Settle::Wait` / `Parked` | Open / Alarmed | — |
//! | Observing | `Settle::Rerun` | Rerun | `publish.rerun` (`:666`) |
//! | Observing | `Settle::Alarm` | Alarmed | `publish.notify` + `publish.record` (`alarmed=`, `:675-695`) |
//! | Observing | `Settle::Retire` / `Supersede` | Superseding | `publish.cancel_runs`, `publish.pr_close` (`:544-549`) |
//! | Superseding | `effect.succeeded` | Opening | `publish.push_branch` |
//! | Opening | push `effect.succeeded` | Opening | `publish.pr_create` |
//! | Opening | `pr_create` succeeded | Open | `publish.record` (`:305`) |
//! | Opening | any `effect.failed` | Refused | — (next tick re-observes) |
//! | Deferred | tick | Observing | — |
//! | Open | `queue.step` settles green/red | Idle | — (owned by [[queue-step]]) |
//! | Refused | tick | Observing | — |
//!
//! ## Act
//!
//! | effect | class | deadline | idempotency key | refuses |
//! |---|---|---|---|---|
//! | `publish.observe` | store+forge+git (probe) | 60 s | `repo/tick` | world not RUNNING |
//! | `publish.push_branch` | git | 60 s | `repo/branch/head` (`:251`) | forge not an ancestor of base (`:116-138`) |
//! | `publish.push_forge` | git | 60 s | `repo/forge_branch/head` (`:227`) | non-fast-forward |
//! | `publish.pr_create` | forge | 60 s | `repo/branch` (`:269`, `real.rs:1281`; preflight `real.rs:1262`) | a publish record already open (`:89-96`) |
//! | `publish.pr_close` | forge | 30 s | `repo/pr` (`:549`, `:629`) | — |
//! | `publish.cancel_runs` | forge | 30 s | `repo/branch` (`:544`, `real.rs:782`) | — (best effort) |
//! | `publish.rerun` | forge | 30 s | `repo/run` (`:666`, `real.rs:1410`) | `rerun=` already set (tries ≥ 1) |
//! | `publish.record` | host (queue dir; store later, unverified) | 2 s | `repo/pr` | — |
//! | `publish.notify` | store (in-process mail) | 5 s | `repo/pr/subject` (`:583`, `:687`, `ops/helpers.rs:298`) | — |
//!
//! PR bodies list member titles: today `timeout bdq show` (`:257`, `real.rs:361`) → LcClient `content`.
//! `czar_ok` (`:464`) forks `czar-fence.sh` only when `SPIRA_RUNE=czar` (`ops/mod.rs:206-216`,
//! `real.rs:962`); never true under the timer, so the task drops it (unverified that no czar path calls it).
//!
//! ## Silence
//!
//! An idle tick (`Settle::Wait`, or nothing to publish) emits only the probe. Today every repo prints
//! `publish-settle <repo>: pr=… -> Wait` each tick (`:534-537`) and `landing_log` lines (`:552-555`, `:309`);
//! those become journal events only on a transition.
//!
//! ## Replaces
//!
//! `spira-publish-prod.service` + `.timer`; the in-process `publish` call in `queue step`
//! (`ops/simple.rs:519-523`). Children: `bash lib.sh` (`real.rs:427`), `timeout spira-lc show-batch`
//! (`real.rs:1622`), `timeout bdq show` (`real.rs:361`), `forge` (`real.rs:1262-1460`), `git fetch/push`
//! (`ops/helpers.rs` `git_push_cmd`, `real.rs:718`), `czar-fence.sh` (`real.rs:962`). Pins:
//! `SPIRA_PUBLISH_SETTLE_DEFER_MAX` (`real.rs:540`), `SPIRA_FORGE`, `SPIRA_QUEUE_DIR`, `SPIRA_RUN`,
//! `SPIRA_MAIL_SESSION_MAILBOX` (`real.rs:531`), `SPIRA_QUEUE_LOCK_WAIT`, the repo registry
//! (`SPIRA_REPO_MAP`, publish forge, publish mode).
//!
//! ## Reverse
//!
//! Release rollback restores `spira-publish-prod.timer`; or `Observe` mode (S4). `queue publish` and
//! `queue publish-settle <repo>` remain operator verbs.
//!

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::kit::{self, Act, Cadence, Shape};

pub const NAME: &str = "publish";
pub const STREAM: &str = "task.publish";
pub const TICK: EventKindTag = "publish.tick";
pub const STATE: EventKindTag = "publish.state";
/// [`super::queue_step`]'s wake, after a verdict pass on a `queue.local` repo.
pub const PUBLISH_WANTED: EventKindTag = super::queue_step::PUBLISH_WANTED;
pub const OBSERVE: &str = "publish.observe";
pub const PUSH_BRANCH: &str = "publish.push_branch";
pub const PUSH_FORGE: &str = "publish.push_forge";
pub const PR_CREATE: &str = "publish.pr_create";
pub const PR_CLOSE: &str = "publish.pr_close";
pub const CANCEL_RUNS: &str = "publish.cancel_runs";
pub const RERUN: &str = "publish.rerun";
pub const RECORD: &str = "publish.record";
pub const NOTIFY: &str = "publish.notify";

pub const CADENCE_MS: u64 = 1_800_000;
pub const OBSERVE_DEADLINE_MS: u64 = 60_000;

pub const ACTS: &[(&str, u64)] = &[(PUSH_BRANCH, 60_000), (PUSH_FORGE, 60_000), (PR_CREATE, 60_000), (PR_CLOSE, 30_000), (CANCEL_RUNS, 30_000), (RERUN, 30_000), (RECORD, 2_000), (NOTIFY, 5_000)];
pub const EMITS: &[EventKindTag] = &[];
pub const EFFECTS: &[&str] = &[OBSERVE, PUSH_BRANCH, PUSH_FORGE, PR_CREATE, PR_CLOSE, CANCEL_RUNS, RERUN, RECORD, NOTIFY];
pub const KEYS: &[&str] = &["SPIRA_PUBLISH_SETTLE_DEFER_MAX", "SPIRA_FORGE", "SPIRA_QUEUE_DIR", "SPIRA_RUN", "SPIRA_MAIL_SESSION_MAILBOX", "SPIRA_QUEUE_LOCK_WAIT", "SPIRA_REPO_MAP"];
pub const UNREGISTERED: &[&str] = &[];

pub const SHAPE: Shape = Shape { name: NAME, stream: STREAM, tick: TICK, state: STATE, observe: OBSERVE, observe_deadline_ms: OBSERVE_DEADLINE_MS };

/// The publish PR as the forge reports it (`ops/publish.rs:746-785`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pr {
    Open {
        /// `ci_of`: `pending`, `green`, `red`, `untested`.
        ci: String,
        behind: bool,
        tries: u32,
    },
    Gone,
    #[default]
    Unknown,
}

/// `decide(Option<Pr>) -> Settle` (`ops/publish.rs:391`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Settle {
    Open,
    Wait,
    Parked,
    Rerun,
    Alarm,
    Retire,
    Supersede,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishRec {
    pub pr: u64,
    pub head: String,
    pub branch: String,
    pub rerun: bool,
    pub alarmed: bool,
}

/// One `queue.local` repository that publishes somewhere.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    pub mode: String,
    /// `pr` or `push`.
    pub publish_mode: String,
    pub unpublished: bool,
    pub stopped: bool,
    pub publish_rec: Option<PublishRec>,
    pub pr: Pr,
    pub open_publish_prs: Vec<u64>,
    pub forge_sha: Option<String>,
    pub head_sha: String,
    /// `None` when divergence could not be checked; `Some(true)` when the forge is not an ancestor.
    pub diverged: Option<bool>,
    pub red_marker: Option<String>,
    pub fix_forward_live: bool,
    pub round_holds_hypervisor: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub repos: BTreeMap<String, Repo>,
    pub world_running: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum State {
    #[default]
    Idle,
    Observing,
    Deferred,
    Adopting,
    Opening,
    Open,
    Rerun,
    Alarmed,
    Superseding,
    Refused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Tick,
    PublishWanted,
    AdoptOne,
    OpenAncestor,
    OpenPush,
    NothingToDo,
    DivergedOrUnchecked,
    RedFixForwardLive,
    RoundHoldsHypervisor,
    SettleWait,
    SettleParked,
    SettleRerun,
    SettleAlarm,
    SettleRetireOrSupersede,
    EffectSucceeded,
    PushSucceeded,
    PrCreated,
    EffectFailed,
    QueueStepSettled,
}

pub static TABLE: &[(State, Trigger, State)] = &[
    (State::Idle, Trigger::Tick, State::Observing),
    (State::Idle, Trigger::PublishWanted, State::Observing),
    (State::Observing, Trigger::AdoptOne, State::Adopting),
    (State::Observing, Trigger::OpenAncestor, State::Opening),
    (State::Observing, Trigger::OpenPush, State::Opening),
    (State::Observing, Trigger::NothingToDo, State::Idle),
    (State::Observing, Trigger::DivergedOrUnchecked, State::Refused),
    (State::Observing, Trigger::RedFixForwardLive, State::Idle),
    (State::Observing, Trigger::RoundHoldsHypervisor, State::Deferred),
    (State::Observing, Trigger::SettleWait, State::Open),
    (State::Observing, Trigger::SettleParked, State::Alarmed),
    (State::Observing, Trigger::SettleRerun, State::Rerun),
    (State::Observing, Trigger::SettleAlarm, State::Alarmed),
    (State::Observing, Trigger::SettleRetireOrSupersede, State::Superseding),
    (State::Superseding, Trigger::EffectSucceeded, State::Opening),
    (State::Opening, Trigger::PushSucceeded, State::Opening),
    (State::Opening, Trigger::PrCreated, State::Open),
    (State::Opening, Trigger::EffectFailed, State::Refused),
    (State::Deferred, Trigger::Tick, State::Observing),
    (State::Open, Trigger::QueueStepSettled, State::Idle),
    (State::Refused, Trigger::Tick, State::Observing),
    // Not in the contract's table, which gives Adopting, Rerun and Alarmed no exit: the
    // adoption's record lands the PR as open, and the next tick re-observes a rerun or an alarm.
    (State::Adopting, Trigger::EffectSucceeded, State::Open),
    (State::Rerun, Trigger::Tick, State::Observing),
    (State::Alarmed, Trigger::Tick, State::Observing),
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoCarry {
    pub state: State,
    /// Today `queue/<repo>/publish-deferred`.
    pub deferred_ticks: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub repos: BTreeMap<String, RepoCarry>,
}

pub fn step(_state: State, _event: Trigger) -> Result<State, String> {
    crate::not_ported!(NAME)
}

/// The pure core `decide(Option<Pr>) -> Settle`.
pub fn settle(_pr: Option<&Pr>) -> Settle {
    crate::not_ported!(NAME)
}

/// The per-repo decision: the open-path guards, the deferral rule, then `settle`.
pub fn decide(_carry: &Carry, _snap: &Snapshot) -> (Carry, Vec<Act>) {
    crate::not_ported!(NAME)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Publish {
    pub cadence: Cadence,
    pub pinned: BTreeMap<String, String>,
}

impl Publish {
    pub fn new(pinned: BTreeMap<String, String>) -> Self {
        Publish { cadence: Cadence::Every(CADENCE_MS), pinned }
    }

    pub fn from_cfg() -> Self {
        Self::new(kit::pin(NAME, KEYS))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("publish/boot", STREAM, EventKind::new(TICK, ""))
    }
}

impl Handler for Publish {
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
