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
        let raw = match &snap.last_landing_pass_at_ms {
            Read::Observed(at) => czar_pass::loop_stalled(now, at.map(|ms| ms / 1000), policy.loop_stall_secs),
            Read::Unobservable(why) => unobservable(why.clone()),
        };
        let stall = policy.loop_stall_secs;
        p.run("loop-stalled", "loop-stalled", raw, 0, |v| {
            let age = v.since.map_or(0, |s| now.saturating_sub(s));
            Remedy::File(czar_pass::Filing::queue("loop-stalled", "loop-stalled", &format!("QUEUE: landing loop stalled — no pass for {age}s"), &format!("No landing pass completed in the last {age}s (threshold: {stall}s).\n"), ""))
        });
    }
    for (repo, batch) in &snap.open_batch {
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
    for (repo, base) in &snap.base_ci {
        let safe = czar_pass::repo_safe(repo);
        let key = format!("base-red:{safe}");
        match base {
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
        let raw = match (read(snap.open_batch.get(repo)), certified) {
            (Ok(open), Read::Observed(c)) => czar_pass::pool_idle(repo, open.flatten().is_some(), c.len() as u64, policy.queue_round_min_n),
            (Err(why), _) => unobservable(format!("{repo}: {why}")),
            (_, Read::Unobservable(why)) => unobservable(format!("{repo}: {why}")),
        };
        let min_n = policy.queue_round_min_n;
        p.run("pool-idle", &key, raw, policy.queue_round_stall_secs, |v| {
            let age = v.since.map_or(0, |s| now.saturating_sub(s));
            Remedy::File(czar_pass::Filing::queue("pool-idle", &format!("pool-idle-{safe}"), &format!("QUEUE: {repo}'s certified pool idle {age}s with no round cut"), &format!("{repo}'s certified pool has sat at the round threshold ({min_n}) for {age}s with no batch open.\n"), ""))
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
        Registration::new(NAME, self)
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

/// The landing log's outcomes, and its last completed pass, read in-process.
fn landing_log(run: &std::path::Path) -> (Read<Vec<LandingEvent>>, Read<Option<u64>>) {
    let path = run.join("landing.log");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Read::Observed(vec![]), Read::Observed(None)),
        Err(e) => return (Read::Unobservable(format!("{}: {e}", path.display())), Read::Unobservable(format!("{}: {e}", path.display()))),
    };
    let at = |l: &str| l.get(..20).and_then(czar_pass::parse_iso_to_epoch).map(|s| s * 1000);
    let mut events = vec![];
    for l in text.lines() {
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
    let last = match text.lines().rfind(|l| l.contains(czar_pass::PASS_COMPLETE_LINE)) {
        None => Read::Observed(None),
        Some(l) => match at(l) {
            Some(ms) => Read::Observed(Some(ms)),
            None => Read::Unobservable(format!("landing.log timestamp unparseable: {:?}", l.get(..20))),
        },
    };
    (Read::Observed(events), last)
}

fn forge_status(forge: &str, repo_path: &str, branch: &str) -> Result<String, String> {
    czar_pass::batch_ci_status(forge, repo_path, branch)
}

/// `czar.observe`: the landing log, the reconciler's stamp, each queue repository's open batch
/// (the store projection), certified pool (`LcClient`) and CI (the forge).
pub fn observe(e: &Effects, input: &str) -> Result<String, String> {
    let req: ObserveIn = serde_json::from_str(input).unwrap_or_default();
    let run = e.world.run_dir.clone();
    let (landing_events, last_landing_pass_at_ms) = landing_log(&run);
    let cursor = req.carry.cursor_ms.unwrap_or(0);
    let landing_events = match landing_events {
        Read::Observed(e) => Read::Observed(e.into_iter().filter(|e| e.at_ms > cursor).collect()),
        other => other,
    };
    let mut snap = Snapshot { landing_events, last_landing_pass_at_ms, now_ms: now_ms(), ..Snapshot::default() };
    let stamp = run.join("reconciler-pass.stamp");
    snap.reconciler_last_pass_ms = match std::fs::read_to_string(&stamp) {
        Ok(t) => match t.trim().parse::<u64>() {
            Ok(at) => Read::Observed(Some(at * 1000)),
            Err(_) => Read::Unobservable(format!("{} unparseable", stamp.display())),
        },
        Err(_) => Read::Observed(None),
    };
    let lc = spira_config::lc_call::LcClient::socket(&e.world.lc_socket).within(std::time::Duration::from_secs(5));
    let views = store_view::Projection::configured(lc.clone());
    let refreshed = views.as_ref().map_err(Clone::clone).and_then(|v| v.refresh_batches().map_err(String::from));
    let home = spira_config::resolve::locate_home_for_process()?;
    let reg = spira_config::repos::Registry::from_env(std::env::vars().chain(req.pinned.clone()).collect(), &home);
    let forge = req.pinned.get("SPIRA_FORGE").cloned().unwrap_or_else(|| "forge".into());
    for name in reg.all() {
        let land = reg.land(&name);
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
        if let Read::Observed(Some(b)) = &batch {
            if !b.branch.is_empty() {
                let ci = match forge_status(&forge, &path, &b.branch) {
                    Ok(out) => {
                        let num = |k: &str| czar_pass::parse_field(&out, k).and_then(|v| v.trim().parse::<u64>().ok()).map(|s| s * 1000);
                        Read::Observed(Ci { queued_since_ms: num("queued-since"), run_id: czar_pass::parse_field(&out, "run-id").filter(|r| !r.is_empty()), conclusion: czar_pass::parse_field(&out, "run-conclusion"), completed_at_ms: num("run-completed-at") })
                    }
                    Err(err) => Read::Unobservable(err),
                };
                snap.ci.insert(name.clone(), ci);
            }
        }
        if let Some(base) = spira_config::repos::landref(&reg, &name).filter(|b| !b.is_empty()) {
            let branch = base.split_once('/').map_or(base.as_str(), |(_, b)| b).to_string();
            let read = match forge_status(&forge, &path, &branch) {
                Ok(out) if czar_pass::parse_field(&out, "run-id").is_some() => Read::Observed(BaseCi {
                    conclusion: czar_pass::parse_field(&out, "run-conclusion"),
                    head_sha: czar_pass::parse_field(&out, "head-sha").unwrap_or_else(|| "unknown".into()),
                    run_url: czar_pass::parse_field(&out, "run-url"),
                    red_suites: out.lines().filter_map(|l| l.strip_prefix("red-suite: ").map(str::to_string)).collect(),
                }),
                Ok(_) => Read::Observed(BaseCi::default()),
                Err(err) => Read::Unobservable(err),
            };
            snap.base_ci.insert(name.clone(), read);
        }
        if matches!(land.as_str(), "queue" | "queue.local") {
            let certified = match lc.call(&["list", "--state", "CERTIFIED", "--repo", name.as_str()]) {
                Ok(r) if r.code == 0 => match serde_json::from_str::<serde_json::Value>(&r.out) {
                    Ok(v) => Read::Observed(v.as_array().into_iter().flatten().filter(|r| r["holds"].as_array().is_none_or(|h| h.is_empty())).filter_map(|r| r["bead_id"].as_str().map(str::to_string)).collect()),
                    Err(err) => Read::Unobservable(format!("spira-lc list: {err}")),
                },
                Ok(r) => Read::Unobservable(r.why()),
                Err(err) => Read::Unobservable(err.to_string()),
            };
            snap.certified.insert(name.clone(), certified);
        }
        snap.open_batch.insert(name, batch);
    }
    serde_json::to_string(&snap).map_err(|e| e.to_string())
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
