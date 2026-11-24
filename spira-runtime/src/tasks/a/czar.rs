//! # czar — contract
//!
//! The queue-stall detector, moving from `czar-pass --pass` into the scheduler. Pattern P1, family A
//! of [[scheduler-consolidation-2026-11-03]]. Paths are relative to `spira-releases/current`
//! (0ef4f0f93); `cp` = `czar-pass/src/main.rs`.
//!
//! ## Tick
//!
//! - Interval 30 s (`spira-czar-pass-prod.timer`: `OnBootSec=60s`, `OnUnitActiveSec=30s`).
//! - Also woken by `landing.pass_completed` and `queue.step` outcome events, so a stall verdict
//!   rests on the events rather than on a log tail (proposed).
//! - Single-flight: today a non-blocking `flock` on `run/czar-pass.lock` skips an overlapping pass
//!   (`cp:572-581`); in the scheduler, one pending `czar.tick` timer per stream gives the same.
//! - World halted: today it reads `run/world.halted` and skips (`cp:566`). The task reads the world
//!   state from the scheduler instead.
//!
//! ## Observe
//!
//! `czar.observe` (probe, 10 s) returns one snapshot. Each field is read from the component that
//! decides it, not from raw state:
//!
//! | Field | Today (raw) | From (after the move) |
//! |---|---|---|
//! | `landing_lines` since the cursor: `no suites identified; leaving batch open`, `ejected 0, requeued N`, `queue_sort_rows: ranking failed` | tail of `run/landing.log` after the `czar-pass.swept` marker (`cp:586-589`, `645-668`) | `queue.step` events (`verdict.deadlock`, `verdict.requeued_all`, `queue.sort_failed`) (proposed names) |
//! | `landing_task` | last `landing: pass complete` line (`cp:840-850`) | the `landing` row of `scheduler status`: halted, last fire, last outcome, next due |
//! | `open_batch[repo]` (pr, branch, members) | `queue batch show --repo` fork, under `SPIRA_LC_TIMEOUT` (`cp:469-489`, `193`) | the S7b round/batch projection |
//! | `certified[repo]` | `spira-lc list --state CERTIFIED` fork (`cp:1240-1257`), then filtered by `git for-each-ref refs/heads/spira/*` (`cp:1217-1237`) | LcClient `list --state CERTIFIED`, with each row's repo from the store, not from the branch list (the store has the repo column; unverified) |
//! | `ci[repo]` (queued-since, run-id, run-conclusion, run-completed-at) | `forge batch-ci-status <repo> <branch>` (`cp:974`) and `forge run-id` (`cp:707`) | still `forge` (the edge; class `forge`), read inside the probe, only for a `queue` (forge) repo whose batch record has a `pr` and a branch |
//! | `base_ci[repo]` (run-conclusion, head-sha, run-url, red-suite) | `forge batch-ci-status <repo> <base>` (`cp:1126`); base from `SPIRA_REPO_MAP` col 4 (`cp:444`) | same; base from the registry; read only while that batch is in flight, then until a base run completed after the batch was last seen has a conclusion or the entry is older than `BASE_PENDING_MAX_AGE_MS` (`Carry.base_pending`) |
//! | `landing_unit_failed` | `systemctl --user is-failed spira-landing.service` (`cp:886`) | moot: `landing` becomes a task, so its health comes from `scheduler status` |
//! | `reconciler_last_pass` | `run/reconciler-pass.stamp` (`cp:1307`; `reconciler-engine/src/paths.rs:10`) and `systemctl show spira-runtime-<instance>.service` (`cp:1382-1396`) | the hosted `reconciler` handler's last pass, from `scheduler status` |
//!
//! An unreadable field is `Unobservable`, never satisfied (`cp:285-288`, sp-pu7v6). The snapshot keeps
//! that distinction per field.
//!
//! ## Decide
//!
//! `decide(carry, snap) -> (carry', acts)`, pure. Nine invariants, each a `reconciler_engine::core::step`
//! over a per-key `HysteresisState` (`cp:289-297`): `deadlock`, `attribution-failed`, `sort-failed`,
//! `loop-stalled`, `ci-stalled:<repo>`, `ci-red:<repo>`, `base-red:<repo>`, `pool-idle:<repo>`,
//! `reconciler-stale` (`cp:595-614`). A tenth registered class, `STARVED` (`cp:128`), has no detector.
//!
//! - **Carried state**: the `StateMap` of hysteresis per key (today `run/reconciler-state.json`,
//!   loaded `cp:592`, saved `cp:629`), the landing cursor (today `run/czar-pass.swept`, `cp:632`),
//!   and the per-class stage. The state file is the same path the reconciler engine uses
//!   (`reconciler-engine/src/paths.rs:6`); the task carries its own keys as an `Emit` instead.
//! - **Stage**: `SPIRA_CZAR_STAGE_<CLASS>` is `act` or anything else = shadow (`cp:179-186`). In
//!   shadow a remedy is a `CZAR-WOULD` line only (`cp:312-320`, `411-420`). Only `DEADLOCK` and
//!   `RECONCILER_STALE` have a registered default (`shadow`); the other eight resolve empty
//!   (`spira/conf.d/SPIRA_CZAR_STAGE_*`), and the prod config file sets none, so **in prod every
//!   class is shadow and czar-pass mutates nothing** (unverified for environment overrides).
//! - **Remedy ladder**: a deterministic remedy once per streak (`record_remedy`), then on
//!   `remedy_failed` an inference filing (`cp:700-748`, `893-920`, `1008-1040`).
//! - **Thresholds** (pinned config, S10): `SPIRA_LOOP_STALL_SECS` (`cp:194`), `SPIRA_CI_QUEUED_MAX_SECS`
//!   (`cp:195`), `SPIRA_CI_RED_MAX_SECS`, `SPIRA_BASE_CI_UNREADABLE_GRACE_SECS`,
//!   `SPIRA_RECONCILER_GRACE_SECS`, `SPIRA_QUEUE_ROUND_MIN_N`, `SPIRA_QUEUE_ROUND_STALL_SECS`,
//!   `SPIRA_SCOPE_LABEL`, `SPIRA_CZAR_LABEL`, `SPIRA_REPO_MAP`, `SPIRA_FORGE`, `SPIRA_INSTANCE`,
//!   `SPIRA_METRICS_ENDPOINT` (`cp:187-221`).
//! - **Fixtures**: the detector tests at `cp:1650-1780` (deadlock, attribution, sort, loop-stalled,
//!   ci, reconciler-stale), plus captured snapshots for base-red and pool-idle.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `czar.file_incident` | store | 30 s | `incident:queue-<ref>` (`cp:343`) | shadow stage; an open incident with the same ref (the `incident` binary dedupes on ref; unverified) |
//! | `czar.file_base_red` | store | 30 s | `incident:base-red:<repo>:<suites>` (`cp:390`, `1200`) | shadow stage. Files P0, express, against the red repo (`cp:369-395`) |
//! | `czar.rerun_workflow` | forge | 60 s | `<repo>:<run-id>` | shadow; a second try in the same streak (`remedy_failed`, `cp:700`, `1008`) |
//! | `czar.restart_landing` | unit | 10 s | `spira-landing:<streak since>` | shadow; second try in a streak. **Moot after the move**: `landing` is a task |
//! | `czar.status` | host | 5 s | `<key>:<tick at>` | nothing; the tsd push `append_status` per key per pass (`cp:292`) becomes an in-process metrics push |
//! | `inbox.append` | host | 10 s | line text | — replaces the `czar.log` CLASS/CZAR-WOULD lines (`cp:261-281`, `312-320`) only for gaps; the per-pass CLASS line becomes a metric, not a journal write |
//!
//! After an act-stage inference the code also runs `sentinel --call czar` (`cp:426-433`). In the
//! scheduler that is an `Emit(call.requested {rune: czar})` the `sentinel-call` task consumes, not an
//! effect and not a fork.
//!
//! ## Silence
//!
//! An idle tick (every key satisfied, no new landing events) emits only its probe. The per-class
//! `CLASS=… DETECTED=no` line and the tsd status push happen **every pass** today (`cp:616-626`,
//! `292`); those become metrics, not journal events. Carry is emitted only when a key's hysteresis
//! changes.
//!
//! ## Replaces
//!
//! `spira-czar-pass-prod.{service,timer}`; children per pass: `queue batch show` (per repo, memoised,
//! `cp:469-475`), `forge` ×(1–3 per repo), `incident file` (`cp:332`, `378`), `sentinel --call czar`
//! (`cp:428`), `systemctl` is-failed/reset-failed/start/show (`cp:886-901`, `1383`), `spira-lc list`
//! (`cp:1241`), `git for-each-ref` (`cp:1218`). The catalogue lists `incident.sh` and omits
//! `sentinel --call` and git: the code forks the `incident` binary and `sentinel`.
//! Files retired: `run/czar.log`, `run/czar-pass.swept`, `run/czar-pass.lock`; the czar's keys in
//! `run/reconciler-state.json`. The `--lint-default` verb (`cp:37-44`) is an operator tool and
//! stays in the binary.
//!
//! ## The port
//!
//! `decide` judges each invariant with the `czar_pass` library's detectors (the binary's own,
//! moved there) and advances its hysteresis with `reconciler_engine::core::step`, carried per
//! key; the ladder phase is the carry's reading of that state. A filing, a rerun or a shadow
//! line is made once per streak: when a key first becomes a gap, and again when the remedy it
//! tried did not close it. An act-stage inference also emits `call.requested`. The probe reads
//! the landing log, the reconciler's pass stamp, the open batches from the store projection,
//! the certified pool over `LcClient`, and the forge (the edge) for each open batch's CI and
//! each base. `czar.restart_landing` has a runner and is not asked for: the landing loop is a
//! task now, and its stall is filed rather than restarted.
//!
//! ## Reverse
//!
//! Release rollback restores the unit and timer. Per task: `Observe` mode (S4) journals each effect
//! as `effect.shadowed`, which is what the shadow stage already is; the stage keys stay as the
//! per-class switch inside Act mode.
//!

use std::collections::BTreeMap;

use reconciler_engine::core::{record_remedy, step, HysteresisState, RawStatus, Verdict};
use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::batcher_local;
use super::kit::{self, Act, Cadence, Shape};
use crate::Effects;

pub const NAME: &str = "czar";
pub const STREAM: &str = "task.czar";
pub const TICK: EventKindTag = "czar.tick";
pub const STATE: EventKindTag = "czar.state";
/// After an act-stage inference: the call of the czar rune, consumed by [`super::sentinel_call`].
pub const CALL_REQUESTED: EventKindTag = "call.requested";
pub const OBSERVE: &str = "czar.observe";
pub const FILE_INCIDENT: &str = "czar.file_incident";
pub const FILE_BASE_RED: &str = "czar.file_base_red";
pub const RERUN_WORKFLOW: &str = "czar.rerun_workflow";
pub const RESTART_LANDING: &str = "czar.restart_landing";
pub const STATUS: &str = "czar.status";
pub const INBOX_APPEND: &str = crate::watchers::INBOX_APPEND;

pub const CADENCE_MS: u64 = 30_000;
pub const OBSERVE_DEADLINE_MS: u64 = 10_000;
/// A batch that closed without landing never produces the base run that settles it; the entry expires instead.
pub const BASE_PENDING_MAX_AGE_MS: u64 = 6 * 3_600_000;

pub const ACTS: &[(&str, u64)] = &[(FILE_INCIDENT, 30_000), (FILE_BASE_RED, 30_000), (RERUN_WORKFLOW, 60_000), (RESTART_LANDING, 10_000), (STATUS, 5_000), (INBOX_APPEND, 10_000)];
pub const EMITS: &[EventKindTag] = &[CALL_REQUESTED];
pub const EFFECTS: &[&str] = &[OBSERVE, FILE_INCIDENT, FILE_BASE_RED, RERUN_WORKFLOW, RESTART_LANDING, STATUS, INBOX_APPEND];
pub const KEYS: &[&str] = &[
    "SPIRA_LOOP_STALL_SECS",
    "SPIRA_RUNTIME_SOCKET",
    "SPIRA_CI_QUEUED_MAX_SECS",
    "SPIRA_CI_RED_MAX_SECS",
    "SPIRA_BASE_CI_UNREADABLE_GRACE_SECS",
    "SPIRA_RECONCILER_GRACE_SECS",
    "SPIRA_QUEUE_ROUND_MIN_N",
    "SPIRA_QUEUE_ROUND_STALL_SECS",
    "SPIRA_SCOPE_LABEL",
    "SPIRA_CZAR_LABEL",
    "SPIRA_REPO_MAP",
    "SPIRA_FORGE",
    "SPIRA_GH",
    "SPIRA_INSTANCE",
    "SPIRA_METRICS_ENDPOINT",
    "SPIRA_CZAR_STAGE_DEADLOCK",
    "SPIRA_CZAR_STAGE_ATTRIBUTION_FAILED",
    "SPIRA_CZAR_STAGE_SORT_FAILED",
    "SPIRA_CZAR_STAGE_LOOP_STALLED",
    "SPIRA_CZAR_STAGE_CI_STALLED",
    "SPIRA_CZAR_STAGE_CI_RED",
    "SPIRA_CZAR_STAGE_BASE_RED",
    "SPIRA_CZAR_STAGE_POOL_IDLE",
    "SPIRA_CZAR_STAGE_RECONCILER_STALE",
    "SPIRA_CZAR_STAGE_STARVED",
];
pub const UNREGISTERED: &[&str] = &[];

/// The nine invariants with a detector; a per-repo class is keyed `<class>:<repo>`. `starved`
/// is registered and has none.
pub const CLASSES: &[&str] = &["deadlock", "attribution-failed", "sort-failed", "loop-stalled", "ci-stalled", "ci-red", "base-red", "pool-idle", "reconciler-stale"];

pub const SHAPE: Shape = Shape { name: NAME, stream: STREAM, tick: TICK, state: STATE, observe: OBSERVE, observe_deadline_ms: OBSERVE_DEADLINE_MS };

/// A field as the probe read it: unreadable is `Unobservable`, never satisfied (sp-pu7v6).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Read<T> {
    Observed(T),
    Unobservable(String),
}

impl<T: Default> Default for Read<T> {
    fn default() -> Self {
        Read::Observed(T::default())
    }
}

/// A landing outcome the czar's detectors read: `verdict.deadlock`, `verdict.requeued_all`,
/// `queue.sort_failed`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingEvent {
    pub kind: String,
    pub repo: String,
    pub at_ms: u64,
}

pub const DEADLOCK_EVENT: &str = "verdict.deadlock";
pub const REQUEUED_ALL_EVENT: &str = "verdict.requeued_all";
pub const SORT_FAILED_EVENT: &str = "queue.sort_failed";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    pub pr: Option<u64>,
    pub branch: String,
    pub members: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ci {
    pub queued_since_ms: Option<u64>,
    pub run_id: Option<String>,
    pub conclusion: Option<String>,
    pub completed_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseCi {
    pub conclusion: Option<String>,
    pub head_sha: String,
    pub run_url: Option<String>,
    pub red_suites: Vec<String>,
    #[serde(default)]
    pub completed_at_ms: Option<u64>,
    #[serde(default)]
    pub no_runs: bool,
}

/// GitHub's core budget, spent and not yet reset: why base CI cannot be read, said once.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpentBudget {
    pub reset: u64,
    pub top_spender: Option<(String, u64)>,
}

/// How far into which landing log file the czar has decided.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    pub dev: u64,
    pub ino: u64,
    pub offset: u64,
}

/// The `landing` row of `scheduler status`, times in the scheduler's milliseconds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingTask {
    pub halted: bool,
    pub last_fired_at_ms: Option<u64>,
    pub next_due_at_ms: Option<u64>,
    pub last_outcome: Option<String>,
}

/// The `landing` row out of a `scheduler status` document; no row is unobservable.
pub fn landing_task(doc: &str) -> Read<LandingTask> {
    let status = match spira_world::scheduler::parse_status(doc) {
        Ok(s) => s,
        Err(why) => return Read::Unobservable(why),
    };
    match status.tasks.iter().find(|t| t.name == "landing") {
        Some(t) => Read::Observed(LandingTask { halted: status.halted, last_fired_at_ms: t.last_fired_at, next_due_at_ms: t.next_due_at, last_outcome: t.last_outcome.clone() }),
        None => Read::Unobservable("scheduler status carries no `landing` task".into()),
    }
}

/// Whether the `landing` task fires: a halted scheduler is deliberate, a failed last fire or a fire
/// older than `stall_secs` is a gap. Firing quiet is healthy however long nothing lands.
pub fn loop_stalled(now_ms: u64, task: &Read<LandingTask>, stall_secs: u64) -> RawStatus {
    let t = match task {
        Read::Observed(t) => t,
        Read::Unobservable(why) => return unobservable(why.clone()),
    };
    if t.halted {
        return RawStatus::Deliberate { reason: "the scheduler is halted".into() };
    }
    let desired = format!("the landing task fires within {stall_secs}s");
    if matches!(t.last_outcome.as_deref(), Some("failed" | "timed_out")) {
        return RawStatus::Gap { desired, observed: format!("the landing task's last fire {}", t.last_outcome.as_deref().unwrap_or_default()), since_hint: t.last_fired_at_ms.map(|ms| ms / 1000) };
    }
    let (since, what) = match (t.last_fired_at_ms, t.next_due_at_ms) {
        (Some(at), _) => (at, "last fired"),
        (None, Some(due)) => (due, "never fired, due"),
        (None, None) => return RawStatus::Satisfied,
    };
    let age = now_ms.saturating_sub(since) / 1000;
    if age > stall_secs { RawStatus::Gap { desired, observed: format!("the landing task {what} {age}s ago"), since_hint: Some(since / 1000) } } else { RawStatus::Satisfied }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Landing outcomes since the carried cursor.
    pub landing_events: Read<Vec<LandingEvent>>,
    pub landing_task: Read<LandingTask>,
    /// The landing-log mark this snapshot was read to; `decide` carries it forward.
    #[serde(default)]
    pub mark: Mark,
    pub open_batch: BTreeMap<String, Read<Option<Batch>>>,
    pub certified: BTreeMap<String, Read<Vec<String>>>,
    /// Per repo, the open rounds the store's round record holds.
    #[serde(default)]
    pub open_rounds: BTreeMap<String, Read<u64>>,
    /// Per repo, the reason the batcher records for holding it.
    #[serde(default)]
    pub batcher_held: BTreeMap<String, String>,
    pub ci: BTreeMap<String, Read<Ci>>,
    pub base_ci: BTreeMap<String, Read<BaseCi>>,
    pub reconciler_last_pass_ms: Read<Option<u64>>,
    pub now_ms: u64,
    #[serde(default)]
    pub gh_budget: Option<SpentBudget>,
}

/// `act`, or anything else: shadow, where a remedy is a `CZAR-WOULD` line only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    #[default]
    Shadow,
    Act,
}

/// The pinned thresholds and stages decide reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    pub stages: BTreeMap<String, Stage>,
    pub loop_stall_secs: u64,
    pub ci_queued_max_secs: u64,
    pub ci_red_max_secs: u64,
    pub base_ci_unreadable_grace_secs: u64,
    pub reconciler_grace_secs: u64,
    pub queue_round_min_n: u64,
    pub queue_round_stall_secs: u64,
}

impl Policy {
    /// The policy from the pinned keys; a threshold that does not parse refuses naming its key.
    pub fn from_pinned(pinned: &BTreeMap<String, String>) -> Result<Policy, String> {
        let secs = |k: &str| -> Result<u64, String> { pinned.get(k).ok_or_else(|| format!("{k} is not pinned"))?.trim().parse().map_err(|_| format!("{k} must be a non-negative integer")) };
        let stages = CLASSES
            .iter()
            .chain(&["starved"])
            .map(|c| {
                let key = format!("SPIRA_CZAR_STAGE_{}", c.to_uppercase().replace('-', "_"));
                let v = pinned.get(&key).ok_or_else(|| format!("{key} is not pinned"))?;
                Ok((c.to_string(), if v.trim() == "act" { Stage::Act } else { Stage::Shadow }))
            })
            .collect::<Result<_, String>>()?;
        Ok(Policy {
            stages,
            loop_stall_secs: secs("SPIRA_LOOP_STALL_SECS")?,
            ci_queued_max_secs: secs("SPIRA_CI_QUEUED_MAX_SECS")?,
            ci_red_max_secs: secs("SPIRA_CI_RED_MAX_SECS")?,
            base_ci_unreadable_grace_secs: secs("SPIRA_BASE_CI_UNREADABLE_GRACE_SECS")?,
            reconciler_grace_secs: secs("SPIRA_RECONCILER_GRACE_SECS")?,
            queue_round_min_n: secs("SPIRA_QUEUE_ROUND_MIN_N")?,
            queue_round_stall_secs: secs("SPIRA_QUEUE_ROUND_STALL_SECS")?,
        })
    }

    fn stage(&self, class: &str) -> Stage {
        self.stages.get(class).copied().unwrap_or_default()
    }
}

/// One key's place on the remedy ladder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Satisfied,
    Gap,
    Remedied,
    RemedyFailed,
    Inferred,
}

/// The legal moves of one key: the ladder, and back to satisfied from any rung when the
/// invariant holds again (`reconciler_engine::core::step` clears the streak).
pub static LADDER: &[(Phase, Phase)] = &[
    (Phase::Satisfied, Phase::Gap),
    (Phase::Gap, Phase::Remedied),
    (Phase::Remedied, Phase::RemedyFailed),
    (Phase::RemedyFailed, Phase::Inferred),
    (Phase::Inferred, Phase::Satisfied),
    (Phase::Gap, Phase::Satisfied),
    (Phase::Remedied, Phase::Satisfied),
    (Phase::RemedyFailed, Phase::Satisfied),
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyState {
    pub phase: Phase,
    pub since_ms: u64,
    /// The engine's own streak memory for this key.
    pub hysteresis: HysteresisState,
}

/// Carried between ticks: the hysteresis of every key, and the landing cursor.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub keys: BTreeMap<String, KeyState>,
    pub cursor_ms: Option<u64>,
    pub mark: Mark,
    /// Repos whose forge batch was open, with the epoch (ms) it was last seen: the base is read
    /// until a run completed after that epoch has a conclusion, or the entry is older than `BASE_PENDING_MAX_AGE_MS`.
    pub base_pending: BTreeMap<String, u64>,
}

/// Move `key` from `from` to `to`, or refuse naming the key and the phase.
pub fn advance(key: &str, from: Phase, to: Phase) -> Result<Phase, String> {
    if from == to || LADDER.contains(&(from, to)) {
        Ok(to)
    } else {
        Err(format!("{key}: {from:?} cannot move to {to:?}"))
    }
}

/// One pass of decide: the carry being built and the acts asked for.
struct Pass<'a> {
    policy: &'a Policy,
    now: u64,
    iso: String,
    next: Carry,
    acts: Vec<Act>,
    labels: String,
}

/// The remedy a gap gets this tick.
enum Remedy {
    /// Re-dispatch a workflow run: (repo, run id).
    Rerun(String, String),
    /// File an inference.
    File(czar_pass::Filing),
}

impl Pass<'_> {
    /// `key` judged by one engine step; the verdict, and whether this tick is the streak's first
    /// gap or the one its remedy was found to have failed on (the ticks a remedy is made on).
    fn judge(&mut self, key: &str, raw: RawStatus, grace: u64) -> (Verdict, bool) {
        let prev = self.next.keys.get(key).cloned().unwrap_or_default();
        let (v, h) = step(self.now, raw, grace, prev.hysteresis.clone());
        if !v.is_gap {
            if h == HysteresisState::default() {
                self.next.keys.remove(key);
            } else {
                self.next.keys.insert(key.into(), KeyState { phase: Phase::Satisfied, since_ms: v.since.unwrap_or(0) * 1000, hysteresis: h });
            }
            return (v, false);
        }
        let fresh = matches!(prev.phase, Phase::Satisfied) || v.remedy_failed;
        let phase = if v.remedy_failed { Phase::RemedyFailed } else if prev.phase == Phase::Satisfied { Phase::Gap } else { prev.phase };
        self.next.keys.insert(key.into(), KeyState { phase, since_ms: v.since.unwrap_or(self.now) * 1000, hysteresis: h });
        (v, fresh)
    }

    fn set_phase(&mut self, key: &str, to: Phase) {
        if let Some(k) = self.next.keys.get_mut(key) {
            if let Ok(p) = advance(key, k.phase, to) {
                k.phase = p;
            }
        }
    }

    /// The remedy of a gap that is fresh this tick, by stage: act files or reruns, shadow writes
    /// the `CZAR-WOULD` line only.
    fn remedy(&mut self, class: &str, key: &str, remedy: Remedy) {
        let act = self.policy.stage(class) == Stage::Act;
        match remedy {
            Remedy::Rerun(repo, run) => {
                let desc = format!("workflow-rerun {run} ({repo})");
                if act {
                    self.acts.push(Act::new(RERUN_WORKFLOW, format!("{repo}:{run}")).with_input(serde_json::json!({"repo": repo, "run_id": run}).to_string()));
                } else {
                    self.would(class, "det", &desc);
                }
                if let Some(k) = self.next.keys.get_mut(key) {
                    record_remedy(&mut k.hysteresis, self.now, &desc);
                }
                self.set_phase(key, Phase::Remedied);
            }
            Remedy::File(mut f) => {
                f.labels = self.labels.clone();
                if act {
                    let effect = if f.cause == "base-red" { FILE_BASE_RED } else { FILE_INCIDENT };
                    self.acts.push(Act::new(effect, f.reference.clone()).with_input(serde_json::to_string(&f).unwrap_or_default()));
                    self.acts.push(Act::new(CALL_REQUESTED, "czar").with_input(serde_json::json!({"rune": "czar"}).to_string()));
                } else {
                    self.would(class, "inference", &f.subject);
                }
                self.set_phase(key, Phase::Inferred);
            }
        }
    }

    fn would(&mut self, class: &str, kind: &str, desc: &str) {
        let line = czar_pass::would_line(&self.iso, class, kind, desc);
        self.acts.push(Act::new(INBOX_APPEND, &line).with_input(line));
    }

    fn status(&mut self, key: &str, v: &Verdict, before: Option<&KeyState>) {
        let after = self.next.keys.get(key);
        if after.map(|k| k.phase) != before.map(|k| k.phase) {
            let word = match &v.status {
                RawStatus::Satisfied => "satisfied",
                RawStatus::Gap { .. } => "gap",
                RawStatus::Unobservable { .. } => "unobservable",
                RawStatus::Deliberate { .. } => "deliberate",
            };
            self.acts.push(Act::new(STATUS, format!("{key}:{}", self.now)).with_input(serde_json::json!({"key": key, "status": word, "since": v.since}).to_string()));
        }
    }

    /// Judge `key`, remedy it on a fresh gap, and report its status when its phase moved.
    fn run(&mut self, class: &str, key: &str, raw: RawStatus, grace: u64, remedy: impl FnOnce(&Verdict) -> Remedy) {
        let before = self.next.keys.get(key).cloned();
        let unobservable = matches!(raw, RawStatus::Unobservable { .. });
        let (v, fresh) = self.judge(key, raw, grace);
        if v.is_gap && fresh && !(unobservable && class != "base-red") {
            let r = remedy(&v);
            self.remedy(class, key, r);
        }
        self.status(key, &v, before.as_ref());
    }
}

fn read<T: Clone>(r: Option<&Read<T>>) -> Result<Option<T>, String> {
    match r {
        None => Ok(None),
        Some(Read::Observed(t)) => Ok(Some(t.clone())),
        Some(Read::Unobservable(why)) => Err(why.clone()),
    }
}

fn unobservable(why: impl Into<String>) -> RawStatus {
    RawStatus::Unobservable { reason: why.into() }
}

/// The nine detectors over the snapshot, each key's hysteresis advanced once.
pub fn decide(policy: &Policy, carry: &Carry, snap: &Snapshot) -> (Carry, Vec<Act>) {
    let now = snap.now_ms / 1000;
    let mut p = Pass { policy, now, iso: czar_pass::iso_of(now), next: carry.clone(), acts: vec![], labels: String::new() };
    // The probe reads the landing outcomes after the carried cursor; the cursor moves to the last.
    let cursor = carry.cursor_ms.unwrap_or(0);
    p.next.mark = snap.mark.clone();
    let events: Result<Vec<LandingEvent>, String> = match &snap.landing_events {
        Read::Observed(e) => Ok(e.clone()),
        Read::Unobservable(why) => Err(why.clone()),
    };
    if let Ok(e) = &events {
        if let Some(last) = e.iter().map(|e| e.at_ms).max() {
            p.next.cursor_ms = Some(last.max(cursor));
        }
    }
    let seen = |kind: &str| events.as_ref().map(|e| e.iter().any(|x| x.kind == kind));
    let line_class = |p: &mut Pass, class: &str, kind: &str, raw: fn(bool) -> RawStatus, filing: (&str, &str), body: &str| {
        let raw = match seen(kind) {
            Ok(s) => raw(s),
            Err(why) => unobservable(why),
        };
        let (reference, subject) = filing;
        p.run(class, class, raw, 0, |_| Remedy::File(czar_pass::Filing::queue(class, reference, subject, body, "")));
    };
    // deadlock: a rerun of the open batch's run first, then an inference.
    {
        let raw = match seen(DEADLOCK_EVENT) {
            Ok(s) => czar_pass::deadlock(s),
            Err(why) => unobservable(why),
        };
        let rerun = snap.open_batch.iter().find_map(|(repo, b)| match (b, snap.ci.get(repo)) {
            (Read::Observed(Some(_)), Some(Read::Observed(Ci { run_id: Some(r), .. }))) => Some((repo.clone(), r.clone())),
            _ => None,
        });
        let (reference, subject) = czar_pass::deadlock_filing();
        let body = "verdict left the batch PR open after a red with no attributable suite.\nThe queue cannot advance until the batch is closed or requeued by hand.\n";
        p.run("deadlock", "deadlock", raw, 0, |v| match rerun {
            Some((repo, run)) if !v.remedy_failed => Remedy::Rerun(repo, run),
            _ => Remedy::File(czar_pass::Filing::queue("deadlock", reference, subject, body, "")),
        });
    }
    line_class(&mut p, "attribution-failed", REQUEUED_ALL_EVENT, czar_pass::attribution_failed, czar_pass::attribution_filing(), "Attribution found no branch to isolate the offender.\nThe entire batch was requeued with the failing member still in it; this loops.\n");
    line_class(&mut p, "sort-failed", SORT_FAILED_EVENT, czar_pass::sort_failed, czar_pass::sort_filing(), "The queue sorter failed and fell back to unranked order.\nPriority ordering is suspended until the sorter recovers.\n");
    {
        let raw = loop_stalled(snap.now_ms, &snap.landing_task, policy.loop_stall_secs);
        let stall = policy.loop_stall_secs;
        p.run("loop-stalled", "loop-stalled", raw, 0, |v| {
            let age = v.since.map_or(0, |s| now.saturating_sub(s));
            Remedy::File(czar_pass::Filing::queue("loop-stalled", "loop-stalled", &format!("QUEUE: landing loop stalled — landing task not firing for {age}s"), &format!("The landing task has not fired cleanly in the last {age}s (threshold: {stall}s), or its last fire failed.\n"), ""))
        });
    }
    for (repo, batch) in &snap.open_batch {
        if matches!(batch, Read::Observed(Some(_))) && snap.ci.contains_key(repo) {
            p.next.base_pending.insert(repo.clone(), snap.now_ms);
        }
        let safe = czar_pass::repo_safe(repo);
        let (cis_key, cir_key) = (format!("ci-stalled:{safe}"), format!("ci-red:{safe}"));
        let (cis, cir, run_id) = match (batch, snap.ci.get(repo)) {
            (Read::Observed(None), _) => continue,
            (Read::Unobservable(why), _) => (unobservable(format!("{repo}: whether a batch is open: {why}")), unobservable(format!("{repo}: whether a batch is open: {why}")), None),
            (Read::Observed(Some(_)), None) => continue,
            (Read::Observed(Some(_)), Some(Read::Unobservable(why))) => (unobservable(format!("{repo}: batch-ci-status: {why}")), unobservable(format!("{repo}: batch-ci-status: {why}")), None),
            (Read::Observed(Some(_)), Some(Read::Observed(ci))) => (
                czar_pass::ci_stalled(now, repo, ci.queued_since_ms.map(|ms| ms / 1000), policy.ci_queued_max_secs),
                czar_pass::ci_red(now, repo, ci.conclusion.as_deref(), ci.completed_at_ms.map(|ms| ms / 1000), policy.ci_red_max_secs),
                ci.run_id.clone(),
            ),
        };
        let max = policy.ci_queued_max_secs;
        p.run("ci-stalled", &cis_key, cis, 0, |v| match &run_id {
            Some(run) if !v.remedy_failed => Remedy::Rerun(repo.clone(), run.clone()),
            _ => {
                let age = v.since.map_or(0, |s| now.saturating_sub(s));
                Remedy::File(czar_pass::Filing::queue("ci-stalled", &format!("ci-stalled-{safe}"), &format!("QUEUE: CI job queued with no runner for {age}s ({repo})"), &format!("A CI job for the open batch in {repo} has been queued for {age}s (threshold: {max}s).\nCancel the stuck run and re-dispatch the whole workflow.\n"), ""))
            }
        });
        p.run("ci-red", &cir_key, cir, 0, |v| {
            let age = v.since.map_or(0, |s| now.saturating_sub(s));
            Remedy::File(czar_pass::Filing::queue("ci-red", &format!("ci-red-{safe}"), &format!("QUEUE: CI run completed red, verdict not acting for {age}s ({repo})"), &format!("CI run completed red for the open batch in {repo}.\nRed for {age}s; verdict has not acted.\n"), ""))
        });
    }
    for (repo, since) in &carry.base_pending {
        let in_flight = matches!(snap.open_batch.get(repo), Some(Read::Observed(Some(_)))) && snap.ci.contains_key(repo);
        let landed = matches!(snap.base_ci.get(repo), Some(Read::Observed(b)) if b.no_runs || (b.conclusion.is_some() && b.completed_at_ms.is_some_and(|at| at > *since)));
        let expired = snap.now_ms.saturating_sub(*since) > BASE_PENDING_MAX_AGE_MS;
        if !in_flight && (landed || expired) {
            p.next.base_pending.remove(repo);
        }
    }
    let watched: Vec<String> = snap.base_ci.keys().chain(p.next.base_pending.keys()).map(|r| czar_pass::repo_safe(r)).collect();
    p.next.keys.retain(|k, _| k.strip_prefix("base-red:").is_none_or(|r| r.starts_with("gh-budget") || watched.iter().any(|w| w == r)));
    let unreadable = snap.base_ci.values().any(|b| !matches!(b, Read::Observed(_)));
    if let (Some(spent), true) = (&snap.gh_budget, unreadable) {
        let until = crate::api_budget::clock_of(spent.reset);
        let who = spent.top_spender.as_ref().map_or("no caller is named by the panel".to_string(), |(c, n)| format!("top spender in the last hour: {c} ({n} calls)"));
        let why = format!("the GitHub API budget is spent until {until}");
        let body = format!("Base CI status could not be read for the repositories whose read failed: {why} (epoch {}).\n{who}.\nA base whose status cannot be read is treated as failed, not green; it is read again after the reset.\n", spent.reset);
        p.run("base-red", "base-red:gh-budget", unobservable(why), policy.base_ci_unreadable_grace_secs, |_| Remedy::File(czar_pass::Filing::base_red(&format!("gh-budget:{}", spent.reset), &format!("QUEUE: the GitHub API budget is spent until {until} — {who}"), &body, "", "")));
    }
    for (repo, base) in &snap.base_ci {
        if snap.gh_budget.is_some() && !matches!(base, Read::Observed(_)) {
            continue;
        }
        let safe = czar_pass::repo_safe(repo);
        let key = format!("base-red:{safe}");
        match base {
            Read::Observed(b) if b.no_runs => {
                p.judge(&key, RawStatus::Satisfied, 0);
            }
            Read::Observed(b) if b.conclusion.is_some() => {
                let raw = czar_pass::base_red(repo, "base", b.conclusion.as_deref());
                let suites = czar_pass::suite_key(&b.red_suites);
                let body = format!("{repo}'s own base gate run completed red.\n\nfailing suites   {suites}\nbase sha         {}\nrun url          {}\n", b.head_sha, b.run_url.as_deref().unwrap_or("(no run url)"));
                p.run("base-red", &key, raw, 0, |_| Remedy::File(czar_pass::Filing::base_red(&format!("{repo}:{suites}"), &format!("QUEUE: {repo}'s base is red — {suites}"), &body, repo, "")));
            }
            other => {
                let why = match other {
                    Read::Unobservable(w) => w.clone(),
                    _ => format!("{repo}'s base CI status returned no run information"),
                };
                let body = format!("{repo}'s base CI status could not be read: {why}.\nA base whose status cannot be read is treated as failed, not green.\n");
                p.run("base-red", &key, unobservable(why), policy.base_ci_unreadable_grace_secs, |_| Remedy::File(czar_pass::Filing::base_red(&format!("{repo}:unreadable"), &format!("QUEUE: {repo}'s base CI status could not be read"), &body, repo, "")));
            }
        }
    }
    for (repo, certified) in &snap.certified {
        let safe = czar_pass::repo_safe(repo);
        let key = format!("pool-idle:{safe}");
        let raw = match (read(snap.open_batch.get(repo)), certified, snap.open_rounds.get(repo)) {
            (_, _, Some(Read::Observed(rounds))) if *rounds > 0 => RawStatus::Satisfied,
            _ if snap.batcher_held.contains_key(repo) => RawStatus::Satisfied,
            (Err(why), _, _) => unobservable(format!("{repo}: {why}")),
            (_, Read::Unobservable(why), _) => unobservable(format!("{repo}: {why}")),
            (_, _, Some(Read::Unobservable(why))) => unobservable(format!("{repo}: {why}")),
            (_, _, None) => unobservable(format!("{repo}: open rounds not read")),
            (Ok(open), Read::Observed(c), Some(Read::Observed(_))) => czar_pass::pool_idle(repo, open.flatten().is_some(), c.len() as u64, policy.queue_round_min_n),
        };
        let min_n = policy.queue_round_min_n;
        p.run("pool-idle", &key, raw, policy.queue_round_stall_secs, |v| {
            let age = v.since.map_or(0, |s| now.saturating_sub(s));
            Remedy::File(czar_pass::Filing::queue("pool-idle", &format!("pool-idle-{safe}"), &format!("QUEUE: {repo}'s certified pool idle {age}s with no round cut"), &format!("{repo}'s certified pool has sat at the round threshold ({min_n}) for {age}s with no round open and the batcher not held.\n"), ""))
        });
    }
    {
        let raw = match &snap.reconciler_last_pass_ms {
            Read::Observed(at) => czar_pass::reconciler_stale(now, at.map(|ms| ms / 1000), policy.reconciler_grace_secs),
            Read::Unobservable(why) => unobservable(why.clone()),
        };
        let grace = policy.reconciler_grace_secs;
        p.run("reconciler-stale", "reconciler-stale", raw, grace, |_| Remedy::File(czar_pass::Filing::queue("reconciler-stale", "reconciler-stale", "RECONCILER: no pass completed — reconciler is not running", &format!("The reconciler has not completed a pass within {grace}s.\n"), "")));
    }
    let Pass { next, acts, .. } = p;
    (next, acts)
}

/// The labels a filing carries: the scope label, then the czar's (`infer`'s `labels`).
fn labels(pinned: &BTreeMap<String, String>) -> String {
    let get = |k: &str| pinned.get(k).cloned().unwrap_or_default();
    let (scope, czar) = (get("SPIRA_SCOPE_LABEL"), get("SPIRA_CZAR_LABEL"));
    if scope.is_empty() { czar } else { format!("{scope},{czar}") }
}

/// `snap` with each repo the batcher holds, and the reason it recorded; a hold with no reason is not one.
pub fn with_batcher_holds(mut snap: Snapshot, batcher: &batcher_local::Carry) -> Snapshot {
    snap.batcher_held = batcher.repos.iter().filter(|(_, rc)| rc.state == batcher_local::State::Held).filter_map(|(n, rc)| rc.skip_why.clone().map(|w| (n.clone(), w))).collect();
    snap
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Czar {
    pub cadence: Cadence,
    pub policy: Policy,
    pub pinned: BTreeMap<String, String>,
}

impl Czar {
    pub fn new(policy: Policy) -> Self {
        Czar { cadence: Cadence::Every(CADENCE_MS), policy, pinned: BTreeMap::new() }
    }

    pub fn from_settings(settings: &crate::Settings) -> Self {
        let pinned = kit::pin(settings, NAME, KEYS);
        Czar { pinned: pinned.clone(), ..Self::new(Policy::from_pinned(&pinned).unwrap_or_else(|e| panic!("{NAME} refuses to build: {e}"))) }
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("czar/boot", STREAM, EventKind::new(TICK, ""))
    }
}

/// The observe probe's input: the carry (which T3 reads back) and the pinned config.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ObserveIn {
    #[serde(flatten)]
    pub carry: Carry,
    #[serde(default)]
    pub pinned: BTreeMap<String, String>,
}

pub const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Map { name: "keys", drop: &[] }, runtime::rows::Part::Field("cursor_ms"), runtime::rows::Part::Field("mark"), runtime::rows::Part::Map { name: "base_pending", drop: &[] }];

impl Handler for Czar {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, _ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        let carry: Carry = kit::carry(&SHAPE, view);
        if ev.kind.tag == TICK {
            let input = ObserveIn { carry: carry.clone(), pinned: self.pinned.clone() };
            return kit::on_tick(&SHAPE, serde_json::to_string(&input).unwrap_or_default(), self.cadence.next_after(ev.at));
        }
        let Some(snap) = kit::observed::<Snapshot>(&SHAPE, view, ev) else { return vec![] };
        let snap = with_batcher_holds(snap, &kit::carry::<batcher_local::Carry>(&batcher_local::SHAPE, view));
        let (next, mut acts) = decide(&self.policy, &carry, &snap);
        let labels = labels(&self.pinned);
        for a in acts.iter_mut().filter(|a| a.effect == FILE_INCIDENT || a.effect == FILE_BASE_RED) {
            if let Ok(mut f) = serde_json::from_str::<czar_pass::Filing>(&a.input) {
                f.labels = if a.effect == FILE_BASE_RED { format!("{}plan", if self.pinned.get("SPIRA_SCOPE_LABEL").is_some_and(|s| !s.is_empty()) { format!("{},", self.pinned["SPIRA_SCOPE_LABEL"]) } else { String::new() }) } else { labels.clone() };
                a.input = serde_json::to_string(&f).unwrap_or_default();
            }
        }
        kit::commands(&SHAPE, ACTS, EMITS, &carry, &next, acts)
    }
}

// ---------------------------------------------------------------------------------------------
// The runners.

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// The landing log's outcomes from the bytes past `mark`, and the mark they were read to. A file unchanged since the mark is stat'ed and not opened. When the marked
/// file has been rotated, it is read from the mark to its end, then each newer file from 0.
fn landing_log(
    run: &std::path::Path,
    mark: &Mark,
) -> (Read<Vec<LandingEvent>>, Mark) {
    use std::os::unix::fs::MetadataExt;
    let live = run.join("landing.log");
    let unreadable = |path: &std::path::Path, e: std::io::Error| {
        let why = format!("{}: {e}", path.display());
        (Read::Unobservable(why), mark.clone())
    };
    let live_meta = match std::fs::metadata(&live) {
        Ok(m) => Some(m),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return unreadable(&live, e),
    };
    if let Some(m) = &live_meta {
        if m.dev() == mark.dev && m.ino() == mark.ino && m.len() == mark.offset {
            return (Read::Observed(vec![]), mark.clone());
        }
    }
    let mut chain: Vec<(std::path::PathBuf, std::fs::Metadata)> =
        queue::ops::landing_log_rotated(run)
            .into_iter()
            .filter_map(|p| std::fs::metadata(&p).ok().map(|m| (p, m)))
            .collect();
    chain.extend(live_meta.map(|m| (live.clone(), m)));
    let marked = chain
        .iter()
        .position(|(_, m)| m.dev() == mark.dev && m.ino() == mark.ino && m.len() >= mark.offset);
    let (from, mut start) = match marked {
        Some(i) => (i, mark.offset),
        None => (chain.len().saturating_sub(1), 0),
    };
    if chain.is_empty() {
        return (Read::Observed(vec![]), Mark::default());
    }
    if marked.is_none() && chain.last().is_some_and(|(p, _)| *p != live) {
        return (Read::Observed(vec![]), Mark::default());
    }
    let (mut events, mut next) = (vec![], mark.clone());
    for (path, meta) in &chain[from..] {
        let (found, offset) = match scan_landing_log(path, start) {
            Ok(r) => r,
            Err(e) => return unreadable(path, e),
        };
        events.extend(found);
        next = Mark {
            dev: meta.dev(),
            ino: meta.ino(),
            offset,
        };
        start = 0;
    }
    (Read::Observed(events), next)
}

/// The outcomes in `path` from byte `start` to its last whole line, and the offset read to.
fn scan_landing_log(path: &std::path::Path, start: u64) -> std::io::Result<(Vec<LandingEvent>, u64)> {
    use std::io::{BufRead, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(start))?;
    let mut reader = std::io::BufReader::new(f);
    let at = |l: &str| {
        l.get(..20)
            .and_then(czar_pass::parse_iso_to_epoch)
            .map(|s| s * 1000)
    };
    let (mut events, mut offset, mut buf) = (vec![], start, vec![]);
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 || buf.last() != Some(&b'\n') {
            break;
        }
        offset += n as u64;
        let line = String::from_utf8_lossy(&buf);
        let l = line.trim_end_matches(['\n', '\r']);
        let kind = if l.contains(czar_pass::DEADLOCK_LINE) {
            DEADLOCK_EVENT
        } else if czar_pass::requeued_all(l) {
            REQUEUED_ALL_EVENT
        } else if l.contains(czar_pass::SORT_FAILED_LINE) {
            SORT_FAILED_EVENT
        } else {
            continue;
        };
        if let Some(at_ms) = at(l) {
            events.push(LandingEvent { kind: kind.into(), repo: String::new(), at_ms });
        }
    }
    Ok((events, offset))
}

fn forge_status(forge: &str, repo_path: &str, branch: &str) -> Result<String, String> {
    czar_pass::batch_ci_status(forge, repo_path, branch)
}

/// A registered repository as `observe` read it.
pub struct RepoRow {
    pub name: String,
    pub land: String,
    pub root: String,
    pub landref: Option<String>,
}

/// The forge's reads for the snapshot: each repo's batch CI and its base CI. `forge` is called
/// with (checkout path, branch).
pub fn forge_reads(rows: &[RepoRow], batches: &BTreeMap<String, Read<Option<Batch>>>, carry: &Carry, forge: &dyn Fn(&str, &str) -> Result<String, String>) -> (BTreeMap<String, Read<Ci>>, BTreeMap<String, Read<BaseCi>>) {
    let (mut ci, mut base_ci) = (BTreeMap::new(), BTreeMap::new());
    for row in rows {
        let batch = batches.get(&row.name);
        let in_flight = row.land == "queue" && matches!(batch, Some(Read::Observed(Some(b))) if b.pr.is_some() && !b.branch.is_empty());
        let trailing = carry.base_pending.contains_key(&row.name) && matches!(batch, Some(Read::Observed(_)));
        if let (true, Some(Read::Observed(Some(b)))) = (in_flight, batch) {
            let read = match forge(&row.root, &b.branch) {
                Ok(out) => {
                    let num = |k: &str| czar_pass::parse_field(&out, k).and_then(|v| v.trim().parse::<u64>().ok()).map(|s| s * 1000);
                    Read::Observed(Ci { queued_since_ms: num("queued-since"), run_id: czar_pass::parse_field(&out, "run-id").filter(|r| !r.is_empty()), conclusion: czar_pass::parse_field(&out, "run-conclusion"), completed_at_ms: num("run-completed-at") })
                }
                Err(err) => Read::Unobservable(err),
            };
            ci.insert(row.name.clone(), read);
        }
        if let (true, Some(base)) = ((in_flight || trailing), row.landref.as_deref()) {
            let branch = base.split_once('/').map_or(base, |(_, b)| b);
            let read = match forge(&row.root, branch) {
                Ok(out) if czar_pass::parse_field(&out, "run-id").is_some() => Read::Observed(BaseCi {
                    conclusion: czar_pass::parse_field(&out, "run-conclusion"),
                    head_sha: czar_pass::parse_field(&out, "head-sha").unwrap_or_else(|| "unknown".into()),
                    run_url: czar_pass::parse_field(&out, "run-url"),
                    red_suites: out.lines().filter_map(|l| l.strip_prefix("red-suite: ").map(str::to_string)).collect(),
                    completed_at_ms: czar_pass::parse_field(&out, "run-completed-at").and_then(|v| v.trim().parse::<u64>().ok()).map(|s| s * 1000),
                    no_runs: false,
                }),
                Ok(out) if czar_pass::parse_field(&out, "runs").as_deref() == Some("0") => Read::Observed(BaseCi { no_runs: true, ..BaseCi::default() }),
                Ok(_) => Read::Observed(BaseCi::default()),
                Err(err) => Read::Unobservable(err),
            };
            base_ci.insert(row.name.clone(), read);
        }
    }
    (ci, base_ci)
}

/// `czar.observe`: the landing log, the reconciler's stamp, each queue repository's open batch
/// (the store projection), certified pool (`LcClient`) and CI (the forge).
pub fn observe(e: &Effects, input: &str) -> Result<String, String> {
    let req: ObserveIn = serde_json::from_str(input).unwrap_or_default();
    let run = e.world.run_dir.clone();
    let (landing_events, mark) = landing_log(&run, &req.carry.mark);
    let cursor = req.carry.cursor_ms.unwrap_or(0);
    let landing_events = match landing_events {
        Read::Observed(e) => Read::Observed(e.into_iter().filter(|e| e.at_ms > cursor).collect()),
        other => other,
    };
    let mut snap = Snapshot { landing_events, mark, now_ms: now_ms(), ..Snapshot::default() };
    snap.landing_task = match req.pinned.get("SPIRA_RUNTIME_SOCKET").filter(|p| !p.is_empty()) {
        Some(sock) => match runtime::socket::lc_call(std::path::Path::new(sock), &["scheduler", "status"]) {
            Ok(doc) => landing_task(&doc),
            Err(why) => Read::Unobservable(format!("scheduler status: {why}")),
        },
        None => Read::Unobservable("SPIRA_RUNTIME_SOCKET is not pinned".into()),
    };
    let stamp = run.join("reconciler-pass.stamp");
    snap.reconciler_last_pass_ms = match std::fs::read_to_string(&stamp) {
        Ok(t) => match t.trim().parse::<u64>() {
            Ok(at) => Read::Observed(Some(at * 1000)),
            Err(_) => Read::Unobservable(format!("{} unparseable", stamp.display())),
        },
        Err(_) => Read::Observed(None),
    };
    let lc = spira_store::lc_call::LcClient::socket(&e.world.lc_socket).within(std::time::Duration::from_secs(5));
    let views = store_view::Projection::hot(lc.clone());
    let refreshed = views.as_ref().map_err(Clone::clone).and_then(|v| v.batches_current().map_err(String::from));
    let home = spira_config::resolve::locate_home_for_process()?;
    let reg = spira_config::repos::Registry::from_env(std::env::vars().chain(req.pinned.clone()).collect(), &home);
    let gh_pinned = req.pinned.get("SPIRA_GH").cloned().unwrap_or_default();
    let metrics = req.pinned.get("SPIRA_METRICS_ENDPOINT").cloned();
    let forge = req.pinned.get("SPIRA_FORGE").cloned().unwrap_or_else(|| "forge".into());
    let mut rows = vec![];
    for name in reg.all() {
        let land = reg.land(&name);
        let landref = spira_config::repos::landref(&reg, &name).filter(|b| !b.is_empty());
        let Some(path) = reg.root(&name).filter(|p| !p.is_empty()) else { continue };
        let batch = match (&views, &refreshed) {
            (Ok(v), Ok(())) => match v.batch_view(&name) {
                Ok(view) => {
                    let shown = view.value.shown();
                    let kv: BTreeMap<&str, &str> = shown.lines().filter_map(|l| l.split_once('=')).collect();
                    Read::Observed((!shown.trim().is_empty()).then(|| Batch { pr: kv.get("pr").and_then(|p| p.trim().parse().ok()), branch: kv.get("branch").unwrap_or(&"").to_string(), members: kv.get("members").map(|m| m.split_whitespace().map(str::to_string).collect()).unwrap_or_default() }))
                }
                Err(err) => Read::Unobservable(format!("could not say: {err}")),
            },
            (Err(err), _) | (_, Err(err)) => Read::Unobservable(format!("could not say: {err}")),
        };
        if matches!(land.as_str(), "queue" | "queue.local") {
            let certified = certified_pool(&lc, &name);
            snap.certified.insert(name.clone(), certified);
            let rounds = match (&views, &refreshed) {
                (Ok(v), Ok(())) => match v.round_view(&name) {
                    Ok(view) => Read::Observed(view.value.rounds.len() as u64),
                    Err(err) => Read::Unobservable(format!("could not say: {err}")),
                },
                (Err(err), _) | (_, Err(err)) => Read::Unobservable(format!("could not say: {err}")),
            };
            snap.open_rounds.insert(name.clone(), rounds);
        }
        rows.push(RepoRow { name: name.clone(), land, root: path, landref });
        snap.open_batch.insert(name, batch);
    }
    (snap.ci, snap.base_ci) = forge_reads(&rows, &snap.open_batch, &req.carry, &|path, branch| forge_status(&forge, path, branch));
    if snap.base_ci.values().any(|b| matches!(b, Read::Unobservable(_))) {
        snap.gh_budget = spent_budget(&gh_pinned, metrics.as_deref(), snap.now_ms / 1000);
    }
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

/// Set when `gh api rate_limit` (not charged) says the core budget is zero until a future reset.
fn spent_budget(gh: &str, metrics: Option<&str>, now: u64) -> Option<SpentBudget> {
    let gh = if gh.is_empty() { "gh" } else { gh };
    let out = spira_config::bounded::bounded(gh).args(["api", "rate_limit"]).output().ok().filter(|o| o.status.success())?;
    let rate = crate::api_budget::core_rate(&String::from_utf8_lossy(&out.stdout)).filter(|r| r.remaining == 0 && r.reset > now)?;
    let top_spender = spira_otel::query::Store::new(metrics).ok().and_then(|s| s.export(crate::api_budget::CALLS, now.saturating_sub(3600)).ok()).and_then(|series| crate::api_budget::top_spender(&series));
    Some(SpentBudget { reset: rate.reset, top_spender })
}

/// The repo's unheld CERTIFIED beads: held rows when the process keeps the store's watch (a
/// refusal could not say), else the store's own list.
pub fn certified_pool(lc: &spira_store::lc_call::LcClient, repo: &str) -> Read<Vec<String>> {
    if let Some(held) = store_view::shared() {
        return match held.unheld("CERTIFIED", repo) {
            Ok(r) => Read::Observed(r.value),
            Err(err) => Read::Unobservable(format!("could not say: {err}")),
        };
    }
    match lc.call(&["list", "--state", "CERTIFIED", "--repo", repo]) {
        Ok(r) if r.code == 0 => match unheld_ids(&r.out) {
            Ok(ids) => Read::Observed(ids),
            Err(err) => Read::Unobservable(err),
        },
        Ok(r) => Read::Unobservable(r.why()),
        Err(err) => Read::Unobservable(err.to_string()),
    }
}

fn filing(input: &str) -> Result<czar_pass::Filing, String> {
    serde_json::from_str(input).map_err(|e| format!("czar filing: {e}"))
}

/// `czar.file_incident` and `czar.file_base_red`: the incident library, in-process.
pub fn file(input: &str) -> Result<String, String> {
    czar_pass::file(&filing(input)?)
}

/// `czar.rerun_workflow`: the forge library re-dispatches the run.
pub fn rerun_workflow(input: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(input).map_err(|e| format!("{RERUN_WORKFLOW}: {e}"))?;
    let repo = v["repo"].as_str().unwrap_or_default();
    let home = spira_config::resolve::locate_home_for_process()?;
    let reg = spira_config::repos::Registry::from_env(std::env::vars().collect(), &home);
    let path = reg.root(repo).filter(|p| !p.is_empty()).ok_or_else(|| format!("{RERUN_WORKFLOW}: {repo} has no checkout registered"))?;
    let forge = spira_config::process::cfg("SPIRA_FORGE").unwrap_or_else(|_| "forge".into());
    czar_pass::rerun_workflow(&forge, &path, v["run_id"].as_str().unwrap_or_default())
}

/// `czar.restart_landing`: reset and start the installed landing unit through the Units port.
pub fn restart_landing(e: &Effects) -> Result<String, String> {
    let units = e.units.as_deref().ok_or("no unit manager configured")?;
    let home = spira_config::resolve::locate_home_for_process()?;
    let instance = spira_config::resolve::resolve_instance(&std::env::vars().collect(), &home)?;
    let name = spira_config::unit::unit_name("landing", "service", &instance);
    let h = runtime::units::UnitHandle::named(&name);
    units.reset_failed(&h).map_err(|e| e.to_string())?;
    units.start_installed(&h).map_err(|e| e.to_string())?;
    Ok(format!("restarted {name}"))
}

/// `czar.status`: one key's status row in the metrics store.
pub fn status(e: &Effects, input: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(input).map_err(|e| format!("{STATUS}: {e}"))?;
    let fields: Vec<(String, serde_json::Value)> = ["key", "status", "since"].iter().map(|k| (k.to_string(), v[*k].clone())).collect();
    tsd::store::Pusher::new(e.world.metrics_endpoint.clone()).push("czar-status", &tsd::now_iso(), &fields).map_err(|e| e.to_string())?;
    Ok(v["key"].as_str().unwrap_or_default().to_string())
}

/// The ids of the rows in a `spira-lc list` answer that carry no hold, read through the typed
/// [`spira_store::lc_state::Row`], which takes `holds` as an array or as the JSON-encoded string
/// spira-lc may write. A held row is not pool: reading the string form as unheld miscounted it.
pub fn unheld_ids(list_json: &str) -> Result<Vec<String>, String> {
    Ok(spira_store::lc_state::parse_rows(list_json)?.into_iter().filter(|r| r.holds.is_empty()).map(|r| r.bead_id).collect())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    const NOW: u64 = 1_793_000_000_000;

    fn policy() -> Policy {
        Policy { stages: CLASSES.iter().map(|c| (c.to_string(), Stage::Shadow)).collect(), loop_stall_secs: 900, ci_queued_max_secs: 1_800, ci_red_max_secs: 3_600, base_ci_unreadable_grace_secs: 600, reconciler_grace_secs: 600, queue_round_min_n: 2, queue_round_stall_secs: 1_800 }
    }

    fn row(name: &str, land: &str) -> RepoRow {
        RepoRow { name: name.into(), land: land.into(), root: format!("r/{name}"), landref: Some("origin/main".into()) }
    }

    fn open(pr: Option<u64>, branch: &str) -> Read<Option<Batch>> {
        Read::Observed(Some(Batch { pr, branch: branch.into(), members: vec![] }))
    }

    fn counted(calls: &RefCell<Vec<(String, String)>>) -> impl Fn(&str, &str) -> Result<String, String> + '_ {
        move |path, branch| {
            calls.borrow_mut().push((path.into(), branch.into()));
            Ok("run-id: 7\nrun-conclusion: success\n".into())
        }
    }

    #[test_id(twypigu)]
    #[test]
    fn an_idle_pass_asks_the_forge_nothing() {
        let rows = [row("fa", "queue"), row("fb", "queue"), row("lo", "queue.local"), row("pr", "pr")];
        let batches: BTreeMap<_, _> = [("fa".to_string(), Read::Observed(None)), ("fb".to_string(), open(None, "spira/batch/b")), ("lo".to_string(), open(Some(3), "spira/round/1")), ("pr".to_string(), Read::Observed(None))].into();
        let calls = RefCell::new(vec![]);
        let (ci, base_ci) = forge_reads(&rows, &batches, &Carry::default(), &counted(&calls));
        assert_eq!(calls.borrow().len(), 0, "{:?}", calls.borrow());
        assert!(ci.is_empty() && base_ci.is_empty());
    }

    fn base_run(completed_s: u64) -> Result<String, String> {
        Ok(format!("run-id: 9\nrun-conclusion: success\nrun-completed-at: {completed_s}\n"))
    }

    #[test_id(towglzx)]
    #[test]
    fn an_in_flight_forge_batch_reads_its_branch_and_the_base_only() {
        let rows = [row("fa", "queue"), row("fb", "queue"), row("lo", "queue.local")];
        let batches: BTreeMap<_, _> = [("fa".to_string(), open(Some(12), "spira/batch/x")), ("fb".to_string(), Read::Observed(None)), ("lo".to_string(), open(Some(3), "spira/round/1"))].into();
        let calls = RefCell::new(vec![]);
        let (ci, base_ci) = forge_reads(&rows, &batches, &Carry::default(), &counted(&calls));
        assert_eq!(*calls.borrow(), vec![("r/fa".to_string(), "spira/batch/x".to_string()), ("r/fa".to_string(), "main".to_string())]);
        assert_eq!(ci.keys().collect::<Vec<_>>(), ["fa"]);
        assert_eq!(base_ci.keys().collect::<Vec<_>>(), ["fa"]);
    }

    #[test_id(tylfbfr)]
    #[test]
    fn the_base_is_read_after_the_batch_closes_until_its_landing_run_completes() {
        let rows = [row("fa", "queue")];
        let batches: BTreeMap<_, _> = [("fa".to_string(), Read::Observed(None))].into();
        let carry = Carry { base_pending: [("fa".to_string(), NOW)].into(), ..Carry::default() };
        let calls = RefCell::new(vec![]);
        let (_, base_ci) = forge_reads(&rows, &batches, &carry, &counted(&calls));
        assert_eq!(*calls.borrow(), vec![("r/fa".to_string(), "main".to_string())]);

        let snap = Snapshot { now_ms: NOW + 60_000, open_batch: batches.clone(), base_ci, ..Snapshot::default() };
        let older = Snapshot { base_ci: [("fa".to_string(), forge_reads(&rows, &batches, &carry, &|_, _| base_run(NOW / 1000 - 5)).1.remove("fa").unwrap())].into(), ..snap.clone() };
        let (still, _) = decide(&policy(), &carry, &older);
        assert!(still.base_pending.contains_key("fa"), "a run that finished before the batch was last seen does not settle it");

        let snap = Snapshot { base_ci: [("fa".to_string(), forge_reads(&rows, &batches, &carry, &|_, _| base_run(NOW / 1000 + 30)).1.remove("fa").unwrap())].into(), ..snap };
        let (next, _) = decide(&policy(), &carry, &snap);
        assert!(next.base_pending.is_empty());

        let calls = RefCell::new(vec![]);
        forge_reads(&rows, &batches, &next, &counted(&calls));
        assert_eq!(calls.borrow().len(), 0);
    }

    #[test_id(tk98p4j)]
    #[test]
    fn a_batch_closed_without_landing_stops_being_read_once_its_pending_entry_expires() {
        let carry = Carry { base_pending: [("fa".to_string(), NOW)].into(), ..Carry::default() };
        let at = |now_ms| Snapshot { now_ms, open_batch: [("fa".to_string(), Read::Observed(None))].into(), base_ci: [("fa".to_string(), Read::Observed(BaseCi::default()))].into(), ..Snapshot::default() };
        let (young, _) = decide(&policy(), &carry, &at(NOW + BASE_PENDING_MAX_AGE_MS));
        assert!(young.base_pending.contains_key("fa"));
        let (old, _) = decide(&policy(), &carry, &at(NOW + BASE_PENDING_MAX_AGE_MS + 1));
        assert!(old.base_pending.is_empty());
        let rows = [row("fa", "queue")];
        let calls = RefCell::new(vec![]);
        forge_reads(&rows, &[("fa".to_string(), Read::Observed(None))].into(), &old, &counted(&calls));
        assert_eq!(calls.borrow().len(), 0);
    }

    #[test_id(thakiru)]
    #[test]
    fn decide_records_an_in_flight_batch_as_pending() {
        let batch = Batch { pr: Some(12), branch: "spira/batch/x".into(), members: vec![] };
        let snap = Snapshot { now_ms: NOW, open_batch: [("fa".to_string(), Read::Observed(Some(batch)))].into(), ci: [("fa".to_string(), Read::Observed(Ci::default()))].into(), ..Snapshot::default() };
        let (next, _) = decide(&policy(), &Carry::default(), &snap);
        assert_eq!(next.base_pending.get("fa"), Some(&NOW));
    }

    #[test_id(tbjiufk)]
    #[test]
    fn an_unobservable_batch_view_makes_no_call_and_is_not_satisfied() {
        let rows = [row("fa", "queue")];
        let batches: BTreeMap<_, _> = [("fa".to_string(), Read::Unobservable("store down".to_string()))].into();
        let carry = Carry { base_pending: [("fa".to_string(), NOW)].into(), ..Carry::default() };
        let calls = RefCell::new(vec![]);
        let (ci, base_ci) = forge_reads(&rows, &batches, &carry, &counted(&calls));
        assert_eq!(calls.borrow().len(), 0);
        let snap = Snapshot { now_ms: NOW, open_batch: batches, ci, base_ci, ..Snapshot::default() };
        let (next, _) = decide(&policy(), &carry, &snap);
        assert!(next.base_pending.contains_key("fa"));
        for key in ["ci-stalled:fa", "ci-red:fa"] {
            let reason = &next.keys[key].hysteresis;
            assert!(*reason != HysteresisState::default() || next.keys[key].phase != Phase::Satisfied, "{key} is carried, not dropped as satisfied");
        }
        let (judged, _) = step(snap.now_ms / 1000, unobservable("x"), 0, HysteresisState::default());
        assert!(matches!(judged.status, RawStatus::Unobservable { .. }));
    }

    #[test_id(tjck0vz)]
    #[test]
    fn a_red_landing_run_still_files_base_red() {
        let carry = Carry { base_pending: [("fa".to_string(), NOW)].into(), ..Carry::default() };
        let red = BaseCi { conclusion: Some("failure".into()), head_sha: "abc".into(), red_suites: vec!["unit".into()], completed_at_ms: Some(NOW + 1_000), ..BaseCi::default() };
        let snap = Snapshot { now_ms: NOW + 5_000, open_batch: [("fa".to_string(), Read::Observed(None))].into(), base_ci: [("fa".to_string(), Read::Observed(red))].into(), ..Snapshot::default() };
        let mut pol = policy();
        pol.stages.insert("base-red".into(), Stage::Act);
        let (next, acts) = decide(&pol, &carry, &snap);
        assert!(acts.iter().any(|a| a.effect == FILE_BASE_RED));
        assert!(next.base_pending.is_empty());
    }

    fn base_read_decided(out: Result<String, String>) -> (BaseCi, Vec<Act>) {
        let rows = [row("fa", "queue")];
        let batches: BTreeMap<_, _> = [("fa".to_string(), Read::Observed(None))].into();
        let carry = Carry { base_pending: [("fa".to_string(), NOW)].into(), ..Carry::default() };
        let mut pol = policy();
        pol.stages.insert("base-red".into(), Stage::Act);
        let read = forge_reads(&rows, &batches, &carry, &|_, _| out.clone()).1.remove("fa").unwrap();
        let first = Snapshot { now_ms: NOW + 1_000, open_batch: batches.clone(), base_ci: [("fa".to_string(), read.clone())].into(), ..Snapshot::default() };
        let (c1, _) = decide(&pol, &carry, &first);
        let late = Snapshot { now_ms: NOW + 1_000 + (pol.base_ci_unreadable_grace_secs + 60) * 1_000, ..first };
        let (_, acts) = decide(&pol, &c1, &late);
        (match read { Read::Observed(b) => b, Read::Unobservable(_) => BaseCi::default() }, acts)
    }

    #[test_id(tkpt9de)]
    #[test]
    fn a_base_that_never_ran_its_workflow_is_satisfied_and_files_nothing() {
        let (b, acts) = base_read_decided(Ok("runs: 0\n".into()));
        assert!(b.no_runs);
        assert!(!acts.iter().any(|a| a.effect == FILE_BASE_RED), "{acts:?}");
    }

    #[test_id(tttuvy8)]
    #[test]
    fn a_failed_base_read_still_files_unreadable_with_the_forge_error() {
        let (_, acts) = base_read_decided(Err("forge batch-ci-status exited 1: HTTP 403".into()));
        let filed = acts.iter().find(|a| a.effect == FILE_BASE_RED).expect("files");
        assert!(filed.input.contains("HTTP 403") && filed.input.contains("base CI status could not be read"), "{filed:?}");
    }

    #[test_id(trl7yvh)]
    #[test]
    fn a_settled_landing_forgets_its_base_red_streak_so_the_next_red_files_again() {
        let mut pol = policy();
        pol.stages.insert("base-red".into(), Stage::Act);
        let red = |at| BaseCi { conclusion: Some("failure".into()), head_sha: "abc".into(), red_suites: vec!["unit".into()], completed_at_ms: Some(at), ..BaseCi::default() };
        let snap = |now, at| Snapshot { now_ms: now, open_batch: [("fa".to_string(), Read::Observed(None))].into(), base_ci: [("fa".to_string(), Read::Observed(red(at)))].into(), ..Snapshot::default() };
        let carry = Carry { base_pending: [("fa".to_string(), NOW)].into(), ..Carry::default() };
        let (c1, _) = decide(&pol, &carry, &snap(NOW + 5_000, NOW + 1_000));
        assert!(c1.keys.contains_key("base-red:fa") && c1.base_pending.is_empty());
        let idle = Snapshot { now_ms: NOW + 40_000, open_batch: [("fa".to_string(), Read::Observed(None))].into(), ..Snapshot::default() };
        let (c2, _) = decide(&pol, &c1, &idle);
        assert!(!c2.keys.contains_key("base-red:fa"), "an unwatched repo keeps no streak");
        let again = Carry { base_pending: [("fa".to_string(), NOW + 100_000)].into(), ..c2 };
        let (_, acts) = decide(&pol, &again, &snap(NOW + 200_000, NOW + 150_000));
        assert!(acts.iter().any(|a| a.effect == FILE_BASE_RED));
    }

    fn idle_pool(rounds: Read<u64>) -> Snapshot {
        Snapshot { open_batch: [("fa".to_string(), Read::Observed(None))].into(), certified: [("fa".to_string(), Read::Observed(vec!["a".into(), "b".into(), "c".into()]))].into(), open_rounds: [("fa".to_string(), rounds)].into(), ..Snapshot::default() }
    }

    fn two_passes(snap: Snapshot) -> (Carry, Vec<Act>, Vec<Act>) {
        let mut pol = policy();
        pol.stages.insert("pool-idle".into(), Stage::Act);
        let first = Snapshot { now_ms: NOW, ..snap.clone() };
        let (c1, a1) = decide(&pol, &Carry::default(), &first);
        let (c2, a2) = decide(&pol, &c1, &Snapshot { now_ms: NOW + (pol.queue_round_stall_secs + 1) * 1000, ..snap });
        (c2, a1, a2)
    }

    fn pool_filings(acts: &[Act]) -> usize {
        acts.iter().filter(|a| a.effect == FILE_INCIDENT && a.input.contains("pool-idle-fa")).count()
    }

    #[test_id(tkq6mqp)]
    #[test]
    fn an_open_round_is_not_a_pool_idle_stall() {
        let (next, a1, a2) = two_passes(idle_pool(Read::Observed(1)));
        assert_eq!(pool_filings(&a1) + pool_filings(&a2), 0);
        assert!(next.keys.get("pool-idle:fa").is_none_or(|k| k.phase != Phase::Gap), "{:?}", next.keys.get("pool-idle:fa"));
    }

    #[test_id(t55zmcz)]
    #[test]
    fn a_batcher_hold_is_not_a_pool_idle_stall() {
        let rc = batcher_local::RepoCarry { state: batcher_local::State::Held, skip_why: Some("the rounds lane is stopped (op: test)".into()), ..Default::default() };
        let snap = with_batcher_holds(idle_pool(Read::Observed(0)), &batcher_local::Carry { repos: [("fa".to_string(), rc)].into() });
        let (_, a1, a2) = two_passes(snap);
        assert_eq!(pool_filings(&a1) + pool_filings(&a2), 0);
    }

    #[test_id(trub7zd)]
    #[test]
    fn a_held_state_with_no_recorded_reason_still_pages() {
        let rc = batcher_local::RepoCarry { state: batcher_local::State::Held, skip_why: None, ..Default::default() };
        let snap = with_batcher_holds(idle_pool(Read::Observed(0)), &batcher_local::Carry { repos: [("fa".to_string(), rc)].into() });
        assert!(snap.batcher_held.is_empty());
    }

    #[test_id(txdw18k)]
    #[test]
    fn an_idle_pool_with_no_round_and_no_hold_files_once_past_the_stall() {
        let (_, a1, a2) = two_passes(idle_pool(Read::Observed(0)));
        assert_eq!(pool_filings(&a1), 0);
        assert_eq!(pool_filings(&a2), 1);
    }

    #[test_id(t70fsih)]
    #[test]
    fn an_unreadable_round_record_is_unobservable_not_satisfied() {
        let (next, a1, a2) = two_passes(idle_pool(Read::Unobservable("could not say: store down".into())));
        assert_eq!(pool_filings(&a1) + pool_filings(&a2), 0);
        let status = a1.iter().chain(&a2).filter(|a| a.effect == STATUS && a.input.contains("pool-idle:fa")).map(|a| a.input.clone()).collect::<Vec<_>>();
        assert!(status.iter().any(|i| i.contains("\"unobservable\"")), "{status:?} {:?}", next.keys.get("pool-idle:fa"));
    }
}
