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

use queue::ops::publish as core;
use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::batcher_local::{handle_p2, interests, table_step, On, P2};
use super::kit::{self, Act, Cadence, Shape};

pub const NAME: &str = "publish";
pub const STREAM: &str = "task.publish";
pub const TICK: EventKindTag = "publish.tick";
pub const STATE: EventKindTag = "publish.state";
/// [`super::queue_step`]'s wake, after a verdict pass on a `queue.local` repo. It lands on
/// queue.step's own stream, so this task's tick is what re-observes.
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
/// Hand-offs that wake it at once, besides its tick: queue.step wants a publish.
pub const WAKES: &[EventKindTag] = &[PUBLISH_WANTED];
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

impl Pr {
    /// The queue's own reading of this PR.
    fn core(&self) -> core::Pr {
        match self {
            Pr::Open { ci, behind, tries } => core::Pr::Open { ci: if ci == "untested" { core::Ci::Untested } else { core::ci_of(ci) }, behind: *behind, tries: u8::try_from(*tries).unwrap_or(u8::MAX) },
            Pr::Gone => core::Pr::Gone,
            Pr::Unknown => core::Pr::Unknown,
        }
    }
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
    // Nor a failed close (the next tick re-observes), nor the end of a direct push (nothing
    // is left open to settle).
    (State::Superseding, Trigger::EffectFailed, State::Refused),
    (State::Opening, Trigger::EffectSucceeded, State::Idle),
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoCarry {
    pub state: State,
    /// Today `queue/<repo>/publish-deferred`.
    pub deferred_ticks: u32,
    /// The PR this machine retired: a record still naming it is stale, not acted on twice.
    #[serde(default)]
    pub retired: Option<u64>,
    /// The head a refusal was last told about: one note per head.
    #[serde(default)]
    pub refused_head: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub repos: BTreeMap<String, RepoCarry>,
}

/// Everything one act carries as its input.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pub {
    pub repo: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub pr: u64,
    #[serde(default)]
    pub head: String,
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub branch: String,
    /// RECORD: the record's fields to set (`rerun`, `alarmed`), or `adopt`.
    #[serde(default)]
    pub record: BTreeMap<String, String>,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
}

impl Pub {
    fn on(repo: &str) -> Pub {
        Pub { repo: repo.into(), ..Pub::default() }
    }

    fn act(&self, effect: &str) -> Act {
        Act::new(effect, self.key.clone()).with_input(serde_json::to_string(self).unwrap_or_default())
    }
}

/// What `pr_create` answers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Created {
    #[serde(default)]
    pub pr: u64,
}

/// The pinned values `decide` reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub defer_max: u32,
}

impl Default for Policy {
    /// The registered default (`spira/conf.d/SPIRA_PUBLISH_SETTLE_DEFER_MAX`).
    fn default() -> Self {
        Policy { defer_max: 3 }
    }
}

impl Policy {
    pub fn pinned(pinned: &BTreeMap<String, String>) -> Policy {
        Policy { defer_max: pinned.get("SPIRA_PUBLISH_SETTLE_DEFER_MAX").and_then(|v| v.trim().parse().ok()).unwrap_or(Policy::default().defer_max) }
    }
}

pub fn step(state: State, event: Trigger) -> Result<State, String> {
    table_step(NAME, TABLE, state, event)
}

/// The pure core `decide(Option<Pr>) -> Settle`.
pub fn settle(pr: Option<&Pr>) -> Settle {
    match core::decide(pr.map(Pr::core)) {
        core::Settle::Open => Settle::Open,
        core::Settle::Wait => Settle::Wait,
        core::Settle::Parked => Settle::Parked,
        core::Settle::Rerun => Settle::Rerun,
        core::Settle::Alarm => Settle::Alarm,
        core::Settle::Retire => Settle::Retire,
        core::Settle::Supersede => Settle::Supersede,
    }
}

struct Machine<'a> {
    repo: &'a str,
    rc: RepoCarry,
    acts: Vec<Act>,
}

impl Machine<'_> {
    fn go(&mut self, on: Trigger) -> bool {
        step(self.rc.state, on).map(|to| self.rc.state = to).is_ok()
    }

    fn notify(&mut self, pr: u64, subject: String, body: String) {
        self.acts.push(Pub { key: format!("{}/{pr}/{subject}", self.repo), pr, subject, body, ..Pub::on(self.repo) }.act(NOTIFY));
    }

    fn observe(&mut self, policy: &Policy, r: &Repo) {
        if !matches!(self.rc.state, State::Idle | State::Deferred | State::Refused | State::Rerun | State::Alarmed) {
            return;
        }
        self.go(Trigger::Tick);
        let rec = r.publish_rec.as_ref().filter(|rec| Some(rec.pr) != self.rc.retired);
        // `:39-50`, `:54`: a repo that does not publish, or whose publish lane is stopped.
        if r.mode != "queue.local" || r.unpublished || r.stopped {
            self.go(Trigger::NothingToDo);
            return;
        }
        if r.publish_rec.is_some() && rec.is_none() {
            // The record still names the PR this machine retired: the next one is in flight.
            self.go(Trigger::NothingToDo);
            return;
        }
        // `:563-643`: no record, but publish PRs open on the forge: adopt the newest, close the rest.
        if rec.is_none() && !r.open_publish_prs.is_empty() {
            self.go(Trigger::AdoptOne);
            let newest = r.open_publish_prs.iter().copied().max().unwrap_or_default();
            self.acts.push(Pub { pr: newest, key: format!("{}/{newest}", self.repo), record: [("adopt".to_string(), newest.to_string())].into(), ..Pub::on(self.repo) }.act(RECORD));
            for older in r.open_publish_prs.iter().copied().filter(|p| *p != newest) {
                self.acts.push(Pub { pr: older, key: format!("{}/{older}", self.repo), ..Pub::on(self.repo) }.act(PR_CLOSE));
            }
            return;
        }
        if rec.is_none() && r.forge_sha.as_deref() == Some(r.head_sha.as_str()) {
            self.go(Trigger::NothingToDo);
            return;
        }
        // `:166-174`: red on this very head with its fix-forward live: wait for the fix.
        if r.red_marker.as_deref() == Some(r.head_sha.as_str()) && r.fix_forward_live {
            self.go(Trigger::RedFixForwardLive);
            return;
        }
        let act = settle(rec.map(|_| &r.pr));
        // `:506-527`: an act that writes is deferred while a round holds the hypervisor.
        if !matches!(act, Settle::Wait | Settle::Parked | Settle::Alarm) && r.round_holds_hypervisor && self.rc.deferred_ticks < policy.defer_max {
            self.go(Trigger::RoundHoldsHypervisor);
            self.rc.deferred_ticks += 1;
            return;
        }
        self.rc.deferred_ticks = 0;
        let pr = rec.map_or(0, |rec| rec.pr);
        match act {
            Settle::Open => self.open(r),
            Settle::Wait => {
                // The open PR's CI is queue.step's to settle (`settle_publish`).
                self.go(Trigger::SettleWait);
                self.go(Trigger::QueueStepSettled);
            }
            Settle::Parked => {
                self.go(Trigger::SettleParked);
            }
            Settle::Rerun => {
                self.go(Trigger::SettleRerun);
                let branch = rec.map(|r| r.branch.clone()).unwrap_or_default();
                self.acts.push(Pub { pr, branch, key: format!("{}/{pr}", self.repo), record: [("rerun".to_string(), "1".to_string())].into(), ..Pub::on(self.repo) }.act(RERUN));
            }
            Settle::Alarm => {
                self.go(Trigger::SettleAlarm);
                self.notify(pr, format!("publish PR {pr} for {} untested twice", self.repo), format!("CI never tested {} after one re-run; the record is parked until a hand moves it.", r.head_sha));
                self.acts.push(Pub { pr, key: format!("{}/{pr}", self.repo), record: [("alarmed".to_string(), "1".to_string())].into(), ..Pub::on(self.repo) }.act(RECORD));
            }
            Settle::Retire | Settle::Supersede => {
                self.go(Trigger::SettleRetireOrSupersede);
                let branch = rec.map(|r| r.branch.clone()).unwrap_or_default();
                self.rc.retired = Some(pr);
                self.acts.push(Pub { pr, branch: branch.clone(), key: format!("{}/{branch}", self.repo), ..Pub::on(self.repo) }.act(CANCEL_RUNS));
                self.acts.push(Pub { pr, branch, head: r.head_sha.clone(), base: r.forge_sha.clone().unwrap_or_default(), key: format!("{}/{pr}", self.repo), ..Pub::on(self.repo) }.act(PR_CLOSE));
            }
        }
    }

    /// `Settle::Open`: guard the push (`:116-138`), then publish by PR or by fast-forward.
    fn open(&mut self, r: &Repo) {
        if r.forge_sha.as_deref() == Some(r.head_sha.as_str()) {
            self.go(Trigger::NothingToDo);
            return;
        }
        if r.diverged != Some(false) {
            self.go(Trigger::DivergedOrUnchecked);
            if self.rc.refused_head.as_deref() != Some(r.head_sha.as_str()) {
                self.rc.refused_head = Some(r.head_sha.clone());
                let why = if r.diverged.is_none() { "divergence could not be checked" } else { "the forge branch is not an ancestor of the local head" };
                self.notify(0, format!("publish refused for {}", self.repo), format!("{why} ({} vs {}); nothing was pushed.", r.forge_sha.clone().unwrap_or_default(), r.head_sha));
            }
            return;
        }
        self.rc.refused_head = None;
        let at = Pub { head: r.head_sha.clone(), base: r.forge_sha.clone().unwrap_or_default(), ..Pub::on(self.repo) };
        if r.publish_mode == "push" {
            self.go(Trigger::OpenPush);
            self.acts.push(Pub { key: format!("{}/forge/{}", self.repo, r.head_sha), ..at }.act(PUSH_FORGE));
        } else {
            self.go(Trigger::OpenAncestor);
            self.acts.push(Pub { key: format!("{}/branch/{}", self.repo, r.head_sha), ..at }.act(PUSH_BRANCH));
        }
    }

    fn outcome(&mut self, effect: &str, ok: bool, body: &str, p: &Pub) {
        match (self.rc.state, effect, ok) {
            (State::Adopting, RECORD, true) => {
                self.go(Trigger::EffectSucceeded);
                self.go(Trigger::QueueStepSettled);
            }
            (State::Superseding, PR_CLOSE, true) => {
                self.go(Trigger::EffectSucceeded);
                self.acts.push(Pub { key: format!("{}/branch/{}", self.repo, p.head), ..p.clone() }.act(PUSH_BRANCH));
            }
            (State::Superseding, PR_CLOSE, false) => {
                self.go(Trigger::EffectFailed);
                self.rc.retired = None;
            }
            (State::Opening, PUSH_BRANCH, true) => {
                self.go(Trigger::PushSucceeded);
                let branch: Pub = serde_json::from_str(body).unwrap_or_default();
                self.acts.push(Pub { branch: branch.branch, key: format!("{}/{}", self.repo, p.head), ..p.clone() }.act(PR_CREATE));
            }
            (State::Opening, PR_CREATE, true) => {
                let c: Created = serde_json::from_str(body).unwrap_or_default();
                self.go(Trigger::PrCreated);
                self.acts.push(Pub { pr: c.pr, key: format!("{}/{}", self.repo, c.pr), ..p.clone() }.act(RECORD));
                // The opened PR's CI is queue.step's to settle.
                self.go(Trigger::QueueStepSettled);
            }
            (State::Opening, PUSH_FORGE, true) => {
                self.go(Trigger::EffectSucceeded);
            }
            (State::Opening, PUSH_BRANCH | PUSH_FORGE | PR_CREATE, false) => {
                self.go(Trigger::EffectFailed);
                self.notify(p.pr, format!("publish {} failed for {}", effect.trim_start_matches("publish."), self.repo), body.trim().to_string());
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

/// The per-repo decision: the open-path guards, the deferral rule, then `settle`.
pub fn decide(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<Act>) {
    decide_with(&Policy::default(), carry, snap)
}

pub fn decide_with(policy: &Policy, carry: &Carry, snap: &Snapshot) -> (Carry, Vec<Act>) {
    let (mut next, mut acts) = (carry.clone(), vec![]);
    if !snap.world_running {
        return (next, acts);
    }
    for (name, repo) in &snap.repos {
        let mut m = Machine { repo: name, rc: carry.repos.get(name).cloned().unwrap_or_default(), acts: vec![] };
        m.observe(policy, repo);
        store(&mut next, name, m.rc);
        acts.extend(m.acts);
    }
    (next, acts)
}

pub fn on_outcome(carry: &Carry, effect: &str, ok: bool, body: &str, p: &Pub) -> (Carry, Vec<Act>) {
    let mut next = carry.clone();
    let mut m = Machine { repo: &p.repo, rc: carry.repos.get(&p.repo).cloned().unwrap_or_default(), acts: vec![] };
    m.outcome(effect, ok, body, p);
    let acts = m.acts;
    store(&mut next, &p.repo, m.rc);
    (next, acts)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Publish {
    pub cadence: Cadence,
    pub pinned: BTreeMap<String, String>,
    pub policy: Policy,
    interests: Vec<EventKindTag>,
}

impl Publish {
    pub fn new(pinned: BTreeMap<String, String>) -> Self {
        Publish { cadence: Cadence::Every(CADENCE_MS), policy: Policy::pinned(&pinned), pinned, interests: interests(TICK, WAKES) }
    }

    pub fn from_settings(settings: &crate::Settings) -> Self {
        Self::new(kit::pin(settings, NAME, KEYS))
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
        &self.interests
    }

    fn handle(&self, _ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        let observed = |carry: &Carry, body: &str| serde_json::from_str::<Snapshot>(body).ok().map(|snap| decide_with(&self.policy, carry, &snap));
        let outcome = |carry: &Carry, effect: &str, ok: bool, body: &str, input: &str| match serde_json::from_str::<Pub>(input) {
            Ok(p) => on_outcome(carry, effect, ok, body, &p),
            Err(_) => (carry.clone(), vec![]),
        };
        // Publish starts no unit leg, so no `unit.ran` is its own.
        let ran = |_: &Carry, _: &runtime::units::UnitRan| None;
        handle_p2(&P2 { shape: &SHAPE, cadence: &self.cadence, acts: ACTS, emits: EMITS, effects: EFFECTS, wakes: WAKES }, view, ev, On { observed, outcome, ran })
    }
}

/// The runners: git as a child through the Proc port, forge verbs through `forge::client`, the
/// record in the queue directory, mail in-process; the shared plumbing is queue.step's.
pub mod edge {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use super::super::queue_step::edge::{forge, forge_call, git, queue_kv, registry, root};
    use super::{Created, Pr, Pub, PublishRec, Repo, Snapshot};
    use crate::{Effects, EffectSpec};

    fn input(spec: &EffectSpec) -> Result<Pub, String> {
        serde_json::from_str(&spec.input).map_err(|e| format!("{}: bad input: {e}", spec.name))
    }

    fn json<T: serde::Serialize>(v: &T) -> Result<String, String> {
        serde_json::to_string(v).map_err(|e| e.to_string())
    }

    /// The remote and branch a repo publishes to (`spira_publish_forge`), or why it does not.
    fn target(reg: &spira_config::repos::Registry, repo: &str) -> Result<(String, String), String> {
        spira_config::repos::publish_forge(reg, repo, &BTreeMap::new()).ok_or_else(|| format!("{repo} publishes nowhere"))
    }

    /// The probe: per `queue.local` repo, its record, its PR on the forge, the two heads and
    /// whether a round holds the hypervisor.
    pub fn observe(e: &Effects, _: &str, _: &EffectSpec) -> Result<String, String> {
        let reg = registry()?;
        let rounds = store_view::Projection::configured(super::super::queue_step::edge::lc(e))?;
        rounds.refresh_batches().map_err(|e| e.to_string())?;
        let mut snap = Snapshot { world_running: !e.world.run_dir.join("world.halted").exists(), ..Snapshot::default() };
        let local: Vec<String> = reg.names().into_iter().filter(|n| reg.land(n) == "queue.local").collect();
        let holds = local.iter().any(|n| rounds.round_view(n).is_ok_and(|v| !v.value.rounds.is_empty()));
        for name in local {
            let dir = root(&reg, &name)?;
            let landref = spira_config::repos::landref(&reg, &name).unwrap_or_default();
            let head_sha = git(&dir, &["rev-parse", &landref], Duration::from_secs(10))?;
            let target = spira_config::repos::publish_forge(&reg, &name, &BTreeMap::new());
            let publish_mode = match spira_config::repos::publish_mode(&reg, &name) {
                spira_config::PublishMode::Push => "push",
                _ => "pr",
            };
            let forge_sha = target.as_ref().and_then(|(remote, branch)| {
                git(&dir, &["fetch", "--quiet", remote, branch], Duration::from_secs(30)).ok()?;
                git(&dir, &["rev-parse", &format!("{remote}/{branch}")], Duration::from_secs(10)).ok()
            });
            let diverged = forge_sha.as_ref().map(|f| git(&dir, &["merge-base", "--is-ancestor", f, &head_sha], Duration::from_secs(10)).is_err());
            let kv = queue_kv(e, &name, "publish");
            let publish_rec = kv.as_ref().and_then(|kv| Some(PublishRec { pr: kv.get("pr")?.trim_start_matches('#').parse().ok()?, head: kv.get("head").cloned().unwrap_or_default(), branch: kv.get("branch").cloned().unwrap_or_default(), rerun: kv.get("rerun").is_some_and(|v| v == "1"), alarmed: kv.get("alarmed").is_some_and(|v| v == "1") }));
            let pr = match &publish_rec {
                None => Pr::Unknown,
                Some(rec) => {
                    let n = rec.pr.to_string();
                    match forge_call(&dir, "pr-state", &[&n], None)?.stdout().map(|s| s.trim().to_ascii_uppercase()) {
                        Some(s) if s == "OPEN" => {
                            let status = forge_call(&dir, "check-status", &[&n, &rec.branch], None)?.stdout().unwrap_or_default();
                            let first = status.lines().next().unwrap_or("").trim();
                            let ci = match queue::ops::verdict::normalize_status(first) {
                                "harness_fault" => "untested",
                                "green" => "green",
                                "red" => "red",
                                _ => "pending",
                            };
                            Pr::Open { ci: ci.into(), behind: rec.head != head_sha, tries: u32::from(rec.rerun) + u32::from(rec.alarmed) }
                        }
                        Some(_) => Pr::Gone,
                        None => Pr::Unknown,
                    }
                }
            };
            let open_publish_prs = forge_call(&dir, "pr-list-open", &[], None)?.stdout().unwrap_or_default().lines().filter_map(|l| l.split_once(' ')).filter(|(_, b)| b.starts_with("spira/publish/")).filter_map(|(n, _)| n.trim_start_matches('#').parse().ok()).collect();
            let red_marker = queue_kv(e, &name, "publish-red").and_then(|kv| kv.get("head").cloned());
            let fix_forward_live = queue_kv(e, &name, "publish-red").and_then(|kv| kv.get("bead").cloned()).is_some_and(|bead| {
                super::super::queue_step::edge::lc(e).answer(&["show", &bead]).ok().is_some_and(|o| super::fix_forward_live(&o))
            });
            let stopped = spira_config::scoped_stop::stop_for(&e.world.run_dir, &name, "publish").is_some();
            snap.repos.insert(name.clone(), Repo { mode: "queue.local".into(), publish_mode: publish_mode.into(), unpublished: target.is_none(), stopped, publish_rec, pr, open_publish_prs, forge_sha, head_sha, diverged, red_marker, fix_forward_live, round_holds_hypervisor: holds });
        }
        json(&snap)
    }

    /// The head pushed to a fresh `spira/publish/<stamp>` branch; answers the branch.
    pub fn push_branch(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let p = input(spec)?;
        let reg = registry()?;
        let (remote, _) = target(&reg, &p.repo)?;
        let branch = format!("spira/publish/{}", spira_config::vtime::now_epoch());
        git(&root(&reg, &p.repo)?, &["push", &remote, &format!("{}:refs/heads/{branch}", p.head)], Duration::from_secs(55))?;
        json(&Pub { branch, ..p })
    }

    /// `publish_mode = push`: the forge branch fast-forwarded to the head (`:224-245`).
    pub fn push_forge(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let p = input(spec)?;
        let reg = registry()?;
        let (remote, branch) = target(&reg, &p.repo)?;
        git(&root(&reg, &p.repo)?, &["push", &remote, &format!("{}:refs/heads/{branch}", p.head)], Duration::from_secs(55))?;
        Ok("{}".into())
    }

    /// The publish PR from the pushed branch onto the forge branch, preflighted.
    pub fn pr_create(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let p = input(spec)?;
        let reg = registry()?;
        let dir = root(&reg, &p.repo)?;
        let (_, forge_branch) = target(&reg, &p.repo)?;
        let pre = forge_call(&dir, "pr-preflight", &[&p.branch], None)?;
        if !pre.ok() {
            return Err(format!("pr-preflight {}: {}", p.branch, pre.err.trim()));
        }
        let range = if p.base.is_empty() { p.head.clone() } else { format!("{}..{}", p.base, p.head) };
        let subjects = git(&dir, &["log", "--format=%s", &range], Duration::from_secs(10)).unwrap_or_default();
        let body = format!("Publish queue: {} landed on the local base since the last publish, for {}.\n\nThese commits already fast-forwarded the base locally; CI here is confirmation, not the gate. A red run files a fix-forward bead — it never rolls back production.\n\n{}", p.head, p.repo, subjects.lines().map(|s| format!("- {s}\n")).collect::<String>());
        let title = format!("publish: {} for {}", &p.head[..p.head.len().min(12)], p.repo);
        let r = forge_call(&dir, "pr-create", &[&p.branch, &forge_branch, &title], Some(body.trim_end().as_bytes()))?;
        if !r.ok() {
            return Err(format!("pr-create {}: rc={}: {}", p.branch, r.code, r.err.trim()));
        }
        let pr = r.out.trim().trim_start_matches('#').parse().map_err(|_| format!("forge returned no PR number for {}", p.branch))?;
        json(&Created { pr })
    }

    pub fn pr_close(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let p = input(spec)?;
        let r = forge_call(&root(&registry()?, &p.repo)?, "pr-close", &[&p.pr.to_string()], None)?;
        r.ok().then(|| "{}".to_string()).ok_or_else(|| format!("pr-close {}: {}", p.pr, r.err.trim()))
    }

    pub fn cancel_runs(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let p = input(spec)?;
        let dir = root(&registry()?, &p.repo)?;
        queue::ops::helpers::cancel_branch_runs(&forge()?, &dir, &p.branch, "publish").then(|| "{}".to_string()).ok_or_else(|| format!("{}: not every run on {} was cancelled", p.repo, p.branch))
    }

    /// The publish branch's run, re-run once; the record then says `rerun=1`.
    pub fn rerun(e: &Effects, k: &str, spec: &EffectSpec) -> Result<String, String> {
        let p = input(spec)?;
        let dir = root(&registry()?, &p.repo)?;
        let run = forge_call(&dir, "run-id", &[&p.branch], None)?.stdout().map(|o| o.trim().to_string()).filter(|r| !r.is_empty()).ok_or_else(|| format!("no run on {}", p.branch))?;
        let r = forge_call(&dir, "workflow-rerun", &[&run], None)?;
        if !r.ok() {
            return Err(format!("workflow-rerun {run}: {}", r.err.trim()));
        }
        record(e, k, spec)
    }

    /// `queue/<repo>/publish`: a new PR's record, an adoption, or fields set on the open one.
    /// A whole record carries what queue's `settle_publish` refuses one without (`remote`,
    /// `forge_branch`) and the `members` its red path files the fix-forward for, as
    /// `queue publish` and `queue publish-settle`'s adoption wrote them.
    pub fn record(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let mut p = input(spec)?;
        let fresh = if p.record.is_empty() || p.record.contains_key("adopt") {
            let reg = registry()?;
            let dir = root(&reg, &p.repo)?;
            let (remote, forge_branch) = target(&reg, &p.repo)?;
            if p.record.contains_key("adopt") {
                adopted(&dir, &remote, &forge_branch, &mut p)?;
            }
            let members = range_members(e, &dir, &p.base, &p.head)?;
            Some(Fresh { remote, forge_branch, members })
        } else {
            None
        };
        write_record(&e.world.queue_dir, &p, fresh.as_ref(), spira_config::vtime::now_epoch())?;
        Ok("{}".into())
    }

    /// The adopted PR's branch from the forge's open list, and its head and base as the remote
    /// holds them (`adopt_forge_publish`).
    fn adopted(dir: &std::path::Path, remote: &str, forge_branch: &str, p: &mut Pub) -> Result<(), String> {
        let listed = forge_call(dir, "pr-list-open", &[], None)?.stdout().unwrap_or_default();
        p.branch = listed.lines().filter_map(|l| l.split_once(' ')).find(|(n, _)| n.trim_start_matches('#').parse::<u64>() == Ok(p.pr)).map(|(_, b)| b.trim().to_string()).ok_or_else(|| format!("{}: PR {} is not among the forge's open PRs", p.repo, p.pr))?;
        for b in [p.branch.as_str(), forge_branch] {
            git(dir, &["fetch", "--quiet", remote, b], Duration::from_secs(30))?;
        }
        p.head = git(dir, &["rev-parse", &format!("refs/remotes/{remote}/{}", p.branch)], Duration::from_secs(10))?;
        p.base = git(dir, &["rev-parse", &format!("refs/remotes/{remote}/{forge_branch}")], Duration::from_secs(10))?;
        Ok(())
    }

    /// The members of base..head from its land commits, a LANDED row's tip taken when it is in
    /// the range (queue's `range_members`).
    fn range_members(e: &Effects, dir: &std::path::Path, base: &str, head: &str) -> Result<Vec<queue::model::Member>, String> {
        use queue::ports::Lc as _;
        let range = if base.is_empty() { head.to_string() } else { format!("{base}..{head}") };
        let commits = queue::publish_range::parse_log(&git(dir, &["log", "-z", "--format=%H%x1f%P%x1f%s", &range], Duration::from_secs(10))?);
        let lc = queue::real::RealLc { bin: None, client: Some(super::super::queue_step::edge::lc(e)) };
        let rows = lc.bead_rows(Some("LANDED"))?.into_iter().map(|r| (r.bead_id.clone(), r)).collect();
        let ok = |args: &[&str]| git(dir, args, Duration::from_secs(10)).is_ok();
        let in_range = |t: &str| queue::ident::check("tip", t).is_ok() && ok(&["cat-file", "-e", &format!("{t}^{{commit}}")]) && ok(&["merge-base", "--is-ancestor", t, head]) && (base.is_empty() || !ok(&["merge-base", "--is-ancestor", t, base]));
        Ok(queue::publish_range::members(&commits, &rows, &in_range))
    }

    /// What a new record carries beyond the act's input: where it publishes and who is in it.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Fresh {
        pub remote: String,
        pub forge_branch: String,
        pub members: Vec<queue::model::Member>,
    }

    /// The record written under `queue_dir/<repo>/publish`: whole when `p.record` is empty (a
    /// new PR) or names an adoption, from `fresh`; else `p.record`'s fields set on the record
    /// there, the rest kept.
    pub fn write_record(queue_dir: &std::path::Path, p: &Pub, fresh: Option<&Fresh>, opened: u64) -> Result<(), String> {
        let path = queue_dir.join(&p.repo).join("publish");
        let rec = if p.record.is_empty() || p.record.contains_key("adopt") {
            let f = fresh.ok_or_else(|| format!("{}: a new publish record needs its remote, forge branch and members", p.repo))?;
            let mut rec = queue::records::Kv::default();
            for (k, v) in [("pr", p.pr.to_string()), ("head", p.head.clone()), ("base", p.base.clone()), ("members", queue::model::render_members(&f.members)), ("opened", opened.to_string()), ("branch", p.branch.clone()), ("remote", f.remote.clone()), ("forge_branch", f.forge_branch.clone())] {
                rec.push(k, &v);
            }
            rec
        } else {
            let mut rec = queue::records::read_kv(&path)?.unwrap_or_default();
            for (k, v) in &p.record {
                match rec.lines.iter_mut().find(|(f, _)| f == k) {
                    Some(l) => l.1 = v.clone(),
                    None => rec.push(k, v),
                }
            }
            rec
        };
        queue::records::write_atomic(&path, &rec.render())
    }

    pub fn notify(e: &Effects, k: &str, spec: &EffectSpec) -> Result<String, String> {
        let p = input(spec)?;
        let s = super::super::queue_step::Step { repo: p.repo, subject: p.subject, body: p.body, ..Default::default() };
        let spec = EffectSpec { name: spec.name.clone(), input: serde_json::to_string(&s).map_err(|e| e.to_string())?, deadline_ms: spec.deadline_ms };
        super::super::queue_step::edge::notify(e, k, &spec)
    }
}

/// Whether the publish-red fix-forward bead is still live, from `spira-lc show`'s typed row: a
/// row in any terminal state (LANDED, SUPERSEDED, DROPPED, DONE) is not, so the red is filed
/// again; an unreadable answer or no row is not live either.
pub fn fix_forward_live(show_json: &str) -> bool {
    spira_config::lc_state::parse_show(show_json).ok().flatten().is_some_and(|r| !r.terminal())
}
