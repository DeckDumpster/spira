//! # gate-worker — contract
//!
//! Task id `gate.worker`, pattern P2, family A of [[scheduler-consolidation-2026-11-03]]. It drains
//! the local gate queue a branch at a time and files one verdict per job. The landing pass
//! (`landing-pass land`, `landing-pass/src/pass.rs:514,523`) enqueues and takes the verdicts; this
//! task only gates. Paths are relative to `spira-releases/current`.
//!
//! ## Tick
//!
//! - Cadence 30 s (`spira-gate-worker-prod.timer`: OnBootSec 60 s, OnUnitActiveSec 30 s).
//! - Also woken by `gate.queued`, which the landing task emits when it enqueues a job (today an
//!   `enqueue` file write, `landing-pass/src/gateq.rs:136`), and by `unit.ran` for its own legs.
//! - Up to N slots run at once, where N = `certify_par` (`gate-worker/src/lib.rs:186`,
//!   `gate-worker/src/main.rs:86`). Today the slots are N processes, each holding a flock
//!   (`lib.rs:203-212`, `main.rs:95-107`). In the scheduler there are N machine instances, each
//!   keyed `gate.worker/<slot>`. Each slot is single-flight.
//!
//! ## Observe
//!
//! The snapshot is `{ inbox: [Job], claimed: {slot: [Job]}, running_by_repo, free_slots }`.
//!
//! - **Gate queue.** Today it is files under `$SPIRA_RUN/gate-worker/{inbox,claimed/<slot>,done}`
//!   (`gateq.rs:86-107`), with `read_dir_jobs`, `running_by_repo` (`gateq.rs:197`) and `queued`
//!   (`gateq.rs:188`). No store verb owns it. The component that decides is the landing machine,
//!   so the snapshot comes from a **`gateq` projection that the landing task publishes**. The
//!   files are not read raw. *Gap: S7 has no such projection yet. The queue either joins the
//!   store or becomes this task's own journaled stream.*
//! - **Host gate admission.** The free slots of `run/gate-admission`
//!   (`landing-pass/src/real.rs:1124-1126`). This is host state with no store verb. It becomes
//!   the scheduler's own `container` class depth (S3) instead of a directory of slots.
//! - **Branch tip**, read when a job is claimed. `git rev-parse refs/heads/<branch>`
//!   (`main.rs:28-33`). This is a `git` class probe.
//! - **Config**, pinned once (S10): `SPIRA_CERTIFY_PAR` (`spira/conf.d/SPIRA_CERTIFY_PAR`, default
//!   2; `landing-pass/src/real.rs:172,210`), `SPIRA_GATE_TIMEOUT` (`main.rs:121`, default 2700),
//!   and `SPIRA_GATE_LOCK_WAIT` (`real.rs:171`, folded by `lib.rs:250-254` to max(explicit,
//!   2×timeout)). `load_context` gets them through the lib.sh seam (`bash`, `real.rs:33,86-93`);
//!   that fork goes away.
//!
//! ## Decide
//!
//! - `pick(snapshot, slot) -> Option<Job>` is `pick_key`, the order `claim` uses: fix branches
//!   first, then the repo with the fewest running jobs, then priority, then age
//!   (`gateq.rs:66-68,242-254`).
//! - `check_tip(job, tip) -> Proceed | Drop(gone|moved)` (`lib.rs:309-322`).
//! - `classify(rc, out) -> GateRun` (`lib.rs:259-267`). It is already pure and is reused as is.
//!   Only the gate's own status may yield FAIL. Not-found, not-started and killed give NO_VERDICT.
//! - Carried state per slot is `{ state, job, started_ms }`.
//! - Fixtures are `gate-worker/src/tests.rs`, which runs the `Worker` with fake Gate, Branches and
//!   Clock (364 lines).
//!
//! ### States
//!
//! `idle`, `claimed`, `warming`, `gating`, `filing`, `dropped` (transient, back to `idle`).
//!
//! | state × event | → state | effects |
//! |---|---|---|
//! | idle × tick/`gate.queued`, `pick` = Some, admission free | claimed | `gate.worker.claim` |
//! | idle × tick, inbox empty or no admission | idle | none (silent) |
//! | claimed × tip probe: gone or moved | dropped → idle | `gate.worker.discard` |
//! | claimed × tip probe: matches | warming | `unit.start` warm leg |
//! | warming × `unit.ran` (any rc) | gating | `unit.start` gate leg. A warm failure costs only a cold build (`lib.rs:323-333`) |
//! | gating × `unit.ran` rc, out | filing | `gate.worker.retain_output` if not PASS, then `gate.worker.complete` |
//! | gating × `unit.ran` killed or timed out | filing | the same, with `classify` giving NO_VERDICT `gate-killed` |
//! | filing × complete succeeded | idle | re-tick at once, because the drain continues (`lib.rs:292-307`) |
//! | claimed/warming/gating × scheduler restart | idle | `gate.worker.recover`: the slot's claims go back to the inbox (`gateq.rs:223-234`) |
//! | any other pair | refused, naming the state | none |
//!
//! `release_is_current` (`lib.rs:218-228`, `main.rs:138-144`) is dropped. The scheduler is the
//! release, so a superseded release is the whole process exiting.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `gate.worker.claim` | store (queue) | 5 s | `repo.branch.tip` (`gateq.rs:74-76`) | a job already claimed (it is a rename-once today, `gateq.rs:249`) |
//! | `gate.worker.discard` | store | 5 s | `slot/repo.branch.tip` | a job this slot does not hold |
//! | `unit.start` warm: `gate warm-tools --repo R` (`main.rs:19-24`) | unit | `SPIRA_GATE_TIMEOUT` (2700 s) | `warm/<repo>/<landref sha>` | an unknown repo |
//! | `unit.start` gate: `gate.sh <branch> <repo>` with `SPIRA_GATE_LOCK_WAIT` and `SPIRA_GATE_BEAD` (`landing-pass/src/real.rs:1106-1117`). It runs `gate`, which runs testenv/podman | unit (container ≤2 inside) | 300 s hard cap (`spira/gate.sh:27`) plus the lock wait (unverified that the wait sits inside gate.sh) | `gate/<repo>/<branch>/<tip>` | no free admission slot. That is `effect.deferred`, never a drop |
//! | `gate.worker.retain_output` | host | 5 s | the job file name with `.out` | none. It keeps the newest 300 (`gateq.rs:263-278`) |
//! | `gate.worker.complete` | store | 5 s | `done/<repo.branch.tip>` (`gateq.rs:280-285`) | a job not claimed by this slot |
//! | `gate.worker.recover` | store | 5 s | `slot` | none |
//!
//! The verdict count is pushed as a metric (`main.rs:146`) in-process; it is not an effect.
//!
//! ## Silence
//!
//! An idle tick reads the projection and finds no job or no free admission. It emits nothing,
//! journals nothing and arms the next tick. "nothing to do" (`main.rs:111`) stops being a log line.
//!
//! ## Replaces
//!
//! - `spira-gate-worker-prod.{service,timer}`. Its `ExecStartPre` was `world guard`. Its
//!   `TimeoutStartSec=infinity` goes away because deadlines now live on the effects.
//! - Children:
//!   - the N−1 `gate-worker run --worker` self re-execs (`main.rs:95-107`)
//!   - `bash` lib.sh context (`landing-pass/src/real.rs:33`)
//!   - `timeout gate warm-tools` (`main.rs:20`)
//!   - `gate.sh`, `gate`, then testenv and podman (`real.rs:1107`)
//!   - `git rev-parse` (`main.rs:31`)
//! - Pinned keys: `SPIRA_CERTIFY_PAR`, `SPIRA_GATE_TIMEOUT`, `SPIRA_GATE_LOCK_WAIT`, `SPIRA_RUN`,
//!   `SPIRA_HOME`.
//! - The flock files `gate-worker/worker.lock[.N]` (`lib.rs:192-198`) are retired.
//!
//! ## Reverse
//!
//! Release rollback restores the unit and timer. Per task, `Observe` mode journals `claim` and
//! `unit.start` as `effect.shadowed`. While it is shadowed, the old timer must run, or nothing
//! gates. The queue directory format is unchanged, so either side can drain it.
//!

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::kit::{self, Act, Cadence, Shape};

pub const NAME: &str = "gate.worker";
pub const STREAM: &str = "task.gate.worker";
pub const TICK: EventKindTag = "gate.worker.tick";
pub const STATE: EventKindTag = "gate.worker.state";
/// The landing task's wake when it enqueues a job.
pub const GATE_QUEUED: EventKindTag = "gate.queued";
pub const OBSERVE: &str = "gate.worker.observe";
/// The `git rev-parse refs/heads/<branch>` probe on claim. The contract names the probe
/// ("claimed × tip probe") but not the effect; this is its name.
pub const TIP: &str = "gate.worker.tip";
pub const CLAIM: &str = "gate.worker.claim";
pub const DISCARD: &str = "gate.worker.discard";
pub const RETAIN_OUTPUT: &str = "gate.worker.retain_output";
pub const COMPLETE: &str = "gate.worker.complete";
pub const RECOVER: &str = "gate.worker.recover";
/// The warm and gate legs: the Units port's start (S6), shared by every unit-starting task.
pub const UNIT_START: &str = runtime::units::UNIT_START;

pub const CADENCE_MS: u64 = 30_000;
pub const OBSERVE_DEADLINE_MS: u64 = 5_000;
/// `SPIRA_GATE_TIMEOUT`'s deadline is pinned per leg once ported; the start itself is quick.
pub const UNIT_START_DEADLINE_MS: u64 = 10_000;
/// `GATE_NOVERDICT`: the verdict of a run the gate itself did not choose.
pub const NO_VERDICT: &str = "NO_VERDICT";

pub const ACTS: &[(&str, u64)] = &[(TIP, 5_000), (CLAIM, 5_000), (DISCARD, 5_000), (UNIT_START, UNIT_START_DEADLINE_MS), (RETAIN_OUTPUT, 5_000), (COMPLETE, 5_000), (RECOVER, 5_000)];
pub const EMITS: &[EventKindTag] = &[];
pub const EFFECTS: &[&str] = &[OBSERVE, TIP, CLAIM, DISCARD, RETAIN_OUTPUT, COMPLETE, RECOVER, UNIT_START];
pub const KEYS: &[&str] = &["SPIRA_CERTIFY_PAR", "SPIRA_GATE_TIMEOUT", "SPIRA_GATE_LOCK_WAIT", "SPIRA_RUN"];
/// `SPIRA_HOME` is the process's own home, not a `conf.d` key.
pub const UNREGISTERED: &[&str] = &["SPIRA_HOME"];

pub const SHAPE: Shape = Shape { name: NAME, stream: STREAM, tick: TICK, state: STATE, observe: OBSERVE, observe_deadline_ms: OBSERVE_DEADLINE_MS };

/// One gate job (`gateq.rs`): keyed `repo.branch.tip`.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Job {
    pub repo: String,
    pub branch: String,
    pub tip: String,
    pub bead: String,
    /// A fix branch goes first.
    pub fix: bool,
    pub priority: u8,
    pub enqueued_ms: u64,
}

impl Job {
    pub fn key(&self) -> String {
        format!("{}.{}.{}", self.repo, self.branch, self.tip)
    }
}

/// The `gateq` projection and the host gate admission.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub inbox: Vec<Job>,
    pub claimed: BTreeMap<usize, Vec<Job>>,
    pub running_by_repo: BTreeMap<String, u32>,
    /// Free slots of the host gate admission (the `container` class depth).
    pub free_slots: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    #[default]
    Idle,
    Claimed,
    Warming,
    Gating,
    Filing,
    Dropped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// tick or `gate.queued`, `pick` = Some, admission free.
    Picked,
    /// tick, inbox empty or no admission.
    Nothing,
    TipGoneOrMoved,
    Discarded,
    TipMatches,
    WarmRan,
    GateRan,
    GateKilledOrTimedOut,
    Completed,
    SchedulerRestart,
    GateQueued,
}

pub static TABLE: &[(State, Trigger, State)] = &[
    (State::Idle, Trigger::Picked, State::Claimed),
    (State::Idle, Trigger::Nothing, State::Idle),
    (State::Claimed, Trigger::TipGoneOrMoved, State::Dropped),
    (State::Dropped, Trigger::Discarded, State::Idle),
    (State::Claimed, Trigger::TipMatches, State::Warming),
    (State::Warming, Trigger::WarmRan, State::Gating),
    (State::Gating, Trigger::GateRan, State::Filing),
    (State::Gating, Trigger::GateKilledOrTimedOut, State::Filing),
    (State::Filing, Trigger::Completed, State::Idle),
    (State::Claimed, Trigger::SchedulerRestart, State::Idle),
    (State::Warming, Trigger::SchedulerRestart, State::Idle),
    (State::Gating, Trigger::SchedulerRestart, State::Idle),
];

/// `check_tip`: proceed, or drop a job whose branch is gone or moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TipCheck {
    Proceed,
    Gone,
    Moved,
}

/// `classify`'s answer: the verdict and the reason a non-verdict carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateRun {
    pub verdict: String,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    pub state: State,
    pub job: Option<Job>,
    pub started_ms: Option<u64>,
}

/// One slot per `certify_par`, each single-flight.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub slots: Vec<Slot>,
}

pub fn step(_state: State, _event: Trigger) -> Result<State, String> {
    crate::not_ported!(NAME)
}

/// `pick_key`'s order: fix branches first, then the repo with the fewest running, then
/// priority, then age.
pub fn pick(_snap: &Snapshot, _slot: usize) -> Option<Job> {
    crate::not_ported!(NAME)
}

pub fn check_tip(_job: &Job, _tip: Option<&str>) -> TipCheck {
    crate::not_ported!(NAME)
}

/// Only the gate's own status may yield FAIL; not-found, not-started and killed are NO_VERDICT.
pub fn classify(_rc: i32, _out: &str) -> GateRun {
    crate::not_ported!(NAME)
}

pub fn decide(_carry: &Carry, _snap: &Snapshot) -> (Carry, Vec<Act>) {
    crate::not_ported!(NAME)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateWorker {
    pub cadence: Cadence,
    /// `SPIRA_CERTIFY_PAR`: how many slots drain at once.
    pub slots: usize,
    pub pinned: BTreeMap<String, String>,
}

impl GateWorker {
    pub fn new(slots: usize, pinned: BTreeMap<String, String>) -> Self {
        GateWorker { cadence: Cadence::Every(CADENCE_MS), slots, pinned }
    }

    pub fn from_cfg() -> Self {
        let pinned = kit::pin(NAME, KEYS);
        let slots = pinned["SPIRA_CERTIFY_PAR"].trim().parse().ok().filter(|n| *n > 0).unwrap_or_else(|| panic!("{NAME} refuses to build: SPIRA_CERTIFY_PAR must be a positive integer"));
        Self::new(slots, pinned)
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("gate.worker/boot", STREAM, EventKind::new(TICK, ""))
    }
}

impl Handler for GateWorker {
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
