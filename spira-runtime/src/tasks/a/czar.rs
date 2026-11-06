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
//! | `last_landing_pass_at` | last `landing: pass complete` line (`cp:840-850`) | the `landing` task's last `landing.pass_completed`, from S13 `scheduler status` |
//! | `open_batch[repo]` (pr, branch, members) | `queue batch show --repo` fork, under `SPIRA_LC_TIMEOUT` (`cp:469-489`, `193`) | the S7b round/batch projection |
//! | `certified[repo]` | `spira-lc list --state CERTIFIED --snapshot` fork (`cp:1240-1257`), then filtered by `git for-each-ref refs/heads/spira/*` (`cp:1217-1237`) | LcClient `list --state CERTIFIED --snapshot`, with each row's repo from the store, not from the branch list (the store has the repo column; unverified) |
//! | `ci[repo]` (queued-since, run-id, run-conclusion, run-completed-at) | `forge batch-ci-status <repo> <branch>` (`cp:974`) and `forge run-id` (`cp:707`) | still `forge` (the edge; class `forge`), read inside the probe |
//! | `base_ci[repo]` (run-conclusion, head-sha, run-url, red-suite) | `forge batch-ci-status <repo> <base>` (`cp:1126`); base from `SPIRA_REPO_MAP` col 4 (`cp:444`) | same; base from the registry |
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
//! ## Reverse
//!
//! Release rollback restores the unit and timer. Per task: `Observe` mode (S4) journals each effect
//! as `effect.shadowed`, which is what the shadow stage already is; the stage keys stay as the
//! per-class switch inside Act mode.
//!

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::kit::{self, Act, Cadence, Shape};

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

pub const ACTS: &[(&str, u64)] = &[(FILE_INCIDENT, 30_000), (FILE_BASE_RED, 30_000), (RERUN_WORKFLOW, 60_000), (RESTART_LANDING, 10_000), (STATUS, 5_000), (INBOX_APPEND, 10_000)];
pub const EMITS: &[EventKindTag] = &[CALL_REQUESTED];
pub const EFFECTS: &[&str] = &[OBSERVE, FILE_INCIDENT, FILE_BASE_RED, RERUN_WORKFLOW, RESTART_LANDING, STATUS, INBOX_APPEND];
pub const KEYS: &[&str] = &[
    "SPIRA_LOOP_STALL_SECS",
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
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Landing outcomes since the carried cursor.
    pub landing_events: Read<Vec<LandingEvent>>,
    pub last_landing_pass_at_ms: Read<Option<u64>>,
    pub open_batch: BTreeMap<String, Read<Option<Batch>>>,
    pub certified: BTreeMap<String, Read<Vec<String>>>,
    pub ci: BTreeMap<String, Read<Ci>>,
    pub base_ci: BTreeMap<String, Read<BaseCi>>,
    pub reconciler_last_pass_ms: Read<Option<u64>>,
    pub now_ms: u64,
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
pub struct KeyState {
    pub phase: Phase,
    pub since_ms: u64,
}

/// Carried between ticks: the hysteresis of every key, and the landing cursor.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub keys: BTreeMap<String, KeyState>,
    pub cursor_ms: Option<u64>,
}

/// Move `key` from `from` to `to`, or refuse naming the key and the phase.
pub fn advance(_key: &str, _from: Phase, _to: Phase) -> Result<Phase, String> {
    crate::not_ported!(NAME)
}

pub fn decide(_policy: &Policy, _carry: &Carry, _snap: &Snapshot) -> (Carry, Vec<Act>) {
    crate::not_ported!(NAME)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Czar {
    pub cadence: Cadence,
    pub policy: Policy,
}

impl Czar {
    pub fn new(policy: Policy) -> Self {
        Czar { cadence: Cadence::Every(CADENCE_MS), policy }
    }

    pub fn from_cfg() -> Self {
        Self::new(Policy::from_pinned(&kit::pin(NAME, KEYS)).unwrap_or_else(|e| panic!("{NAME} refuses to build: {e}")))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("czar/boot", STREAM, EventKind::new(TICK, ""))
    }
}

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
            return kit::on_tick(&SHAPE, serde_json::to_string(&carry).unwrap_or_default(), self.cadence.next_after(ev.at));
        }
        let Some(snap) = kit::observed::<Snapshot>(&SHAPE, view, ev) else { return vec![] };
        let (next, acts) = decide(&self.policy, &carry, &snap);
        kit::commands(&SHAPE, ACTS, EMITS, &carry, &next, acts)
    }
}
