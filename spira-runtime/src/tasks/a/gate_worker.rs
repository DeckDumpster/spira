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
    /// Each slot's running leg, for the slots in `warming` or `gating` whose leg started.
    #[serde(default)]
    pub legs: BTreeMap<usize, Leg>,
    /// `SPIRA_GATE_TIMEOUT` and the folded `SPIRA_GATE_LOCK_WAIT`, as pinned, in seconds.
    #[serde(default)]
    pub gate_timeout: u64,
    #[serde(default)]
    pub lock_wait: u64,
    /// `$SPIRA_RUN`, where each leg writes its output and status.
    #[serde(default)]
    pub run_dir: String,
    #[serde(default)]
    pub now_ms: u64,
}

/// One leg as the probe found it: still running, or ended with its status (`None`: it ended
/// without writing one — killed, or never started) and, for a gate leg, `classify`'s answer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leg {
    pub running: bool,
    pub rc: Option<i32>,
    pub verdict: Option<String>,
    pub reason: Option<String>,
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
    /// The current leg's `unit.start` has succeeded, so an absent unit with no status is a
    /// leg that died, not one still to start.
    #[serde(default)]
    pub leg_started: bool,
}

/// One slot per `certify_par`, each single-flight.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "WrittenCarry", into = "WrittenCarry")]
pub struct Carry {
    pub slots: Vec<Slot>,
}

/// A carry as it is written (its state event, the observe's input, read back on every outcome):
/// the slots up to the last one in use, and how many slots there are. A wide pool of idle slots
/// is then a number, not one record each to write and parse.
#[derive(Serialize, Deserialize)]
struct WrittenCarry {
    slots: Vec<Slot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    width: Option<usize>,
}

impl From<Carry> for WrittenCarry {
    fn from(c: Carry) -> Self {
        let width = c.slots.len();
        let mut slots = c.slots;
        slots.truncate(slots.iter().rposition(|s| *s != Slot::default()).map_or(0, |i| i + 1));
        WrittenCarry { width: (slots.len() < width).then_some(width), slots }
    }
}

impl From<WrittenCarry> for Carry {
    fn from(w: WrittenCarry) -> Self {
        let mut slots = w.slots;
        slots.resize(slots.len().max(w.width.unwrap_or(0)), Slot::default());
        Carry { slots }
    }
}

fn word(state: State) -> String {
    serde_json::to_string(&state).unwrap_or_default().trim_matches('"').to_string()
}

pub fn step(state: State, event: Trigger) -> Result<State, String> {
    TABLE.iter().find(|(s, e, _)| *s == state && *e == event).map(|(_, _, to)| *to).ok_or_else(|| format!("{NAME}: {event:?} is not legal in state {}", word(state)))
}

/// `pick_key`'s order (`landing_pass::gateq::order_key`): fix branches first, then the repo
/// with the fewest running, then priority, then age. Every slot drains the one queue in the
/// one order, so `slot` does not change the answer.
pub fn pick(snap: &Snapshot, _slot: usize) -> Option<Job> {
    let running = |repo: &str| snap.running_by_repo.get(repo).copied().unwrap_or(0) as usize;
    snap.inbox.iter().min_by_key(|j| landing_pass::gateq::order_key(j.fix, running(&j.repo), i64::from(j.priority), u128::from(j.enqueued_ms))).cloned()
}

pub fn check_tip(job: &Job, tip: Option<&str>) -> TipCheck {
    match tip {
        None => TipCheck::Gone,
        Some(t) if t != job.tip => TipCheck::Moved,
        Some(_) => TipCheck::Proceed,
    }
}

/// Only the gate's own status may yield FAIL; not-found, not-started and killed are NO_VERDICT.
/// `gate_worker::classify` itself, read back as the verdict word and its reason.
pub fn classify(rc: i32, out: &str) -> GateRun {
    let run = gate_worker::classify(rc, out.to_string());
    GateRun { verdict: run.outcome.word().to_string(), reason: run.reason }
}

/// A slot's act input: the slot and its job.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotJob {
    pub slot: usize,
    pub job: Job,
}

/// `complete`'s and `retain_output`'s input: the job, where its gate leg wrote, and when it ran.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filing {
    pub slot: usize,
    pub job: Job,
    /// The leg's status: `None` when it ended without one (killed, or never started).
    pub rc: Option<i32>,
    pub out_path: String,
    pub started_ms: u64,
    pub finished_ms: u64,
}

/// The status a leg that ended without writing one is classified under: killed.
const KILLED: i32 = 137;
/// The status of a gate leg whose start was refused: it did not start.
const NOT_STARTED: i32 = -1;

/// Where slot `slot`'s `leg` for `job` writes its output and its status.
pub fn leg_paths(run_dir: &str, slot: usize, leg: &str, job: &Job) -> (String, String) {
    let base = format!("{run_dir}/gate-worker/legs/{slot}/{leg}-{:016x}", runtime::event::fnv1a(&job.key()));
    (format!("{base}.out"), format!("{base}.rc"))
}

/// The leg's unit key: slot first so its outcome names the slot it belongs to.
fn leg_key(slot: usize, leg: &str, job: &Job) -> String {
    match leg {
        "warm" => format!("{slot}/warm/{}/{}", job.repo, job.tip),
        _ => format!("{slot}/gate/{}/{}/{}", job.repo, job.branch, job.tip),
    }
}

/// The slot a leg's unit key names.
fn slot_of_leg(key: &str) -> Option<usize> {
    key.split('/').next()?.parse().ok()
}

/// A leg as a transient unit: the command, its output and its status written beside it so the
/// probe can read how it ended after the unit is collected.
fn leg_act(snap: &Snapshot, slot: usize, leg: &str, job: &Job) -> Act {
    let (out, rc) = leg_paths(&snap.run_dir, slot, leg, job);
    let script = r#"out=$1 rc=$2; shift 2; mkdir -p "$(dirname "$out")"; rm -f "$rc"; "$@" > "$out" 2>&1; echo $? > "$rc.tmp" && mv "$rc.tmp" "$rc""#;
    let mut argv: Vec<String> = ["sh", "-c", script, "leg", &out, &rc].map(String::from).to_vec();
    let mut env = vec![];
    if leg == "warm" {
        argv.extend(["timeout", "-k", "10", &snap.gate_timeout.max(1).to_string(), "gate", "warm-tools", "--repo", &job.repo].map(String::from));
    } else {
        argv.extend(["gate.sh", &job.branch, &job.repo].map(String::from));
        env = vec![("SPIRA_GATE_LOCK_WAIT".to_string(), snap.lock_wait.to_string()), ("SPIRA_GATE_BEAD".to_string(), job.bead.clone())];
    }
    let spec = runtime::units::UnitSpec { task: NAME.into(), key: leg_key(slot, leg, job), argv, env, cwd: None };
    Act::new(UNIT_START, spec.key.clone()).with_input(serde_json::to_string(&spec).unwrap_or_default())
}

fn slot_input(slot: usize, job: &Job) -> String {
    serde_json::to_string(&SlotJob { slot, job: job.clone() }).unwrap_or_default()
}

/// What the probe's snapshot moves each slot to. An idle slot claims the next job while host
/// admission has room; a slot whose leg ended moves on: a warm leg to the gate leg, a gate leg
/// to filing; a slot whose leg is gone with no status, after a restart, gives its claim back.
/// The CLAIM, TIP, DISCARD and COMPLETE outcomes move a slot in `on_outcome`.
pub fn decide(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<Act>) {
    let mut next = carry.clone();
    let mut acts = vec![];
    let held: Vec<String> = carry.slots.iter().filter_map(|s| s.job.as_ref().map(Job::key)).collect();
    let mut free = snap.free_slots;
    let mut queue = Snapshot { inbox: snap.inbox.iter().filter(|j| !held.contains(&j.key())).cloned().collect(), ..snap.clone() };
    for (i, slot) in next.slots.iter_mut().enumerate() {
        match (slot.state, slot.job.clone()) {
            (State::Idle, _) if free > 0 => {
                let Some(job) = pick(&queue, i) else { continue };
                queue.inbox.retain(|j| j.key() != job.key());
                *queue.running_by_repo.entry(job.repo.clone()).or_default() += 1;
                free -= 1;
                acts.push(Act::new(CLAIM, job.key()).with_input(slot_input(i, &job)));
                *slot = Slot { state: State::Claimed, job: Some(job), started_ms: Some(snap.now_ms), leg_started: false };
            }
            (State::Warming | State::Gating, Some(job)) if slot.leg_started => {
                let Some(leg) = snap.legs.get(&i).filter(|l| !l.running) else { continue };
                if slot.state == State::Warming && leg.rc.is_some() {
                    acts.push(leg_act(snap, i, "gate", &job));
                    *slot = Slot { state: State::Gating, leg_started: false, ..slot.clone() };
                } else if slot.state == State::Gating {
                    let (out_path, _) = leg_paths(&snap.run_dir, i, "gate", &job);
                    let filing = Filing { slot: i, job: job.clone(), rc: leg.rc, out_path, started_ms: slot.started_ms.unwrap_or(snap.now_ms), finished_ms: snap.now_ms };
                    let input = serde_json::to_string(&filing).unwrap_or_default();
                    if leg.verdict.as_deref() != Some("PASS") {
                        acts.push(Act::new(RETAIN_OUTPUT, format!("{}.out", job.key())).with_input(input.clone()));
                    }
                    acts.push(Act::new(COMPLETE, format!("done/{}", job.key())).with_input(input));
                    *slot = Slot { state: State::Filing, leg_started: false, ..slot.clone() };
                } else {
                    // A warm leg gone with no status: the scheduler (or the host) restarted.
                    acts.push(Act::new(RECOVER, i.to_string()).with_input(i.to_string()));
                    *slot = Slot::default();
                }
            }
            _ => {}
        }
    }
    (next, acts)
}

/// A slot's step on one of its own effects' outcomes: CLAIM → TIP; TIP → warm leg, or drop;
/// a leg's start recorded; DISCARD → idle; COMPLETE → idle (the drain goes on at once).
pub fn on_outcome(carry: &Carry, effect: &str, input: &str, ok: bool, body: &str, pins: &Snapshot) -> (Carry, Vec<Act>) {
    let mut next = carry.clone();
    let mut acts = vec![];
    if effect == UNIT_START {
        let Ok(spec) = serde_json::from_str::<runtime::units::UnitSpec>(input) else { return (next, acts) };
        let Some(i) = slot_of_leg(&spec.key) else { return (next, acts) };
        let Some(slot) = next.slots.get_mut(i) else { return (next, acts) };
        match (slot.state, ok) {
            (State::Warming | State::Gating, true) => slot.leg_started = true,
            // A warm leg that would not start costs only a cold build: on to the gate.
            (State::Warming, false) => {
                if let Some(job) = slot.job.clone() {
                    slot.state = State::Gating;
                    slot.leg_started = false;
                    acts.push(leg_act(pins, i, "gate", &job));
                }
            }
            (State::Gating, false) => {
                if let Some(job) = slot.job.clone() {
                    let (out_path, _) = leg_paths(&pins.run_dir, i, "gate", &job);
                    let f = Filing { slot: i, job: job.clone(), rc: Some(NOT_STARTED), out_path, started_ms: slot.started_ms.unwrap_or(0), finished_ms: slot.started_ms.unwrap_or(0) };
                    let input = serde_json::to_string(&f).unwrap_or_default();
                    acts.push(Act::new(RETAIN_OUTPUT, format!("{}.out", job.key())).with_input(input.clone()));
                    acts.push(Act::new(COMPLETE, format!("done/{}", job.key())).with_input(input));
                    slot.state = State::Filing;
                }
            }
            _ => {}
        }
        return (next, acts);
    }
    let Ok(sj) = serde_json::from_str::<SlotJob>(input) else { return (next, acts) };
    let Some(slot) = next.slots.get_mut(sj.slot) else { return (next, acts) };
    if slot.job.as_ref() != Some(&sj.job) {
        return (next, acts);
    }
    match (slot.state, effect, ok) {
        (State::Claimed, CLAIM, true) => acts.push(Act::new(TIP, sj.job.key()).with_input(input.to_string())),
        (State::Claimed, CLAIM, false) => *slot = Slot::default(),
        (State::Claimed, TIP, true) => match check_tip(&sj.job, Some(body.trim()).filter(|t| !t.is_empty())) {
            TipCheck::Proceed => {
                acts.push(leg_act(pins, sj.slot, "warm", &sj.job));
                slot.state = State::Warming;
            }
            TipCheck::Gone | TipCheck::Moved => {
                acts.push(Act::new(DISCARD, format!("{}/{}", sj.slot, sj.job.key())).with_input(input.to_string()));
                slot.state = State::Dropped;
            }
        },
        (State::Dropped, DISCARD, _) => *slot = Slot::default(),
        (State::Filing, COMPLETE, true) => *slot = Slot::default(),
        _ => {}
    }
    (next, acts)
}

/// The pinned values a leg is built from: `$SPIRA_RUN`, `SPIRA_GATE_TIMEOUT`, and
/// `SPIRA_GATE_LOCK_WAIT` folded by `gate_worker::lock_wait` to at least twice the timeout.
pub fn pins(pinned: &BTreeMap<String, String>) -> Snapshot {
    let timeout = pinned.get("SPIRA_GATE_TIMEOUT").map(String::as_str);
    let gate_timeout = timeout.and_then(|t| t.trim().parse().ok()).filter(|t| *t > 0).unwrap_or(2_700);
    let lock_wait = gate_worker::lock_wait(timeout, pinned.get("SPIRA_GATE_LOCK_WAIT").map(String::as_str).filter(|v| !v.trim().is_empty()));
    Snapshot { run_dir: pinned.get("SPIRA_RUN").cloned().unwrap_or_default(), gate_timeout, lock_wait, ..Snapshot::default() }
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

    pub fn from_settings(settings: &crate::Settings) -> Self {
        let pinned = kit::pin(settings, NAME, KEYS);
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
        &[TICK, GATE_QUEUED, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT, runtime::units::UNIT_RAN]
    }

    fn handle(&self, _ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        // N slots, each single-flight; a carry recorded under a smaller N grows, never shrinks
        // past a slot still holding a job.
        let mut carry: Carry = kit::carry(&SHAPE, view);
        let keep = carry.slots.iter().rposition(|s| s.job.is_some()).map_or(0, |i| i + 1);
        carry.slots.resize(self.slots.max(keep), Slot::default());
        // One of its own legs ended (its `unit.ran`, routed to this stream by unit name): the
        // slot's status is read now, so the slot moves on at once rather than at the next tick.
        let own_leg_ran = runtime::units::UnitRan::from_event_kind(&ev.kind).is_some_and(|r| r.key_for(NAME).is_some());
        if ev.kind.tag == runtime::units::UNIT_RAN && !own_leg_ran {
            return vec![];
        }
        if ev.kind.tag == TICK || ev.kind.tag == GATE_QUEUED || own_leg_ran {
            let mut out = vec![Command::Probe(EffectSpec { name: OBSERVE.into(), input: serde_json::to_string(&carry).unwrap_or_default(), deadline_ms: OBSERVE_DEADLINE_MS })];
            if ev.kind.tag == TICK {
                out.push(Command::Timer { at: self.cadence.next_after(ev.at), kind: EventKind::new(TICK, "") });
            }
            return out;
        }
        let Some(spec) = crate::watchers::answered_spec(view, ev) else { return vec![] };
        if spec.name == OBSERVE {
            let Some(snap) = kit::observed::<Snapshot>(&SHAPE, view, ev) else { return vec![] };
            let (next, acts) = decide(&carry, &snap);
            return kit::commands(&SHAPE, ACTS, EMITS, &carry, &next, acts);
        }
        let ok = ev.kind.tag == EFFECT_SUCCEEDED;
        let (next, acts) = on_outcome(&carry, &spec.name, &spec.input, ok, &ev.kind.body, &pins(&self.pinned));
        let mut out = kit::commands(&SHAPE, ACTS, EMITS, &carry, &next, acts);
        // A filed verdict frees its slot: the drain goes on at once rather than a tick later.
        if spec.name == COMPLETE && ok {
            out.push(Command::Timer { at: ev.at, kind: EventKind::new(TICK, "") });
        }
        out
    }
}

// ---------------------------------------------------------------------------------------------
// Runners: the queue is `landing_pass::gateq` and admission `landing_pass::real::admission_free`,
// in this process; the branch tip is a `git` child; the legs run as transient units.

fn queue(e: &crate::Effects) -> landing_pass::gateq::GateQueue {
    landing_pass::gateq::GateQueue::new(&e.world.run_dir)
}

fn task_job(j: &landing_pass::gateq::Job) -> Job {
    Job { repo: j.repo.clone(), branch: j.branch.clone(), tip: j.tip.clone(), bead: j.bead.clone(), fix: j.fix, priority: j.priority.clamp(0, i64::from(u8::MAX)) as u8, enqueued_ms: (j.queued / 1_000_000) as u64 }
}

fn slot_job(input: &str, effect: &str) -> Result<SlotJob, String> {
    serde_json::from_str(input).map_err(|e| format!("{effect}: bad input: {e}"))
}

fn read_leg(rc_path: &str, out_path: &str) -> (Option<i32>, String) {
    let rc = std::fs::read_to_string(rc_path).ok().and_then(|t| t.trim().parse().ok());
    (rc, std::fs::read_to_string(out_path).unwrap_or_default())
}

/// The snapshot: the queue's inbox, claims and per-repo running counts; free admission; and for
/// each slot whose leg started, whether it still runs and, once ended, how.
pub fn observe(e: &crate::Effects, input: &str) -> Result<String, String> {
    let carry: Carry = serde_json::from_str(input).unwrap_or_default();
    let mut pinned = BTreeMap::new();
    for k in KEYS {
        pinned.insert(k.to_string(), spira_config::process::cfg(k)?);
    }
    let par: usize = pinned["SPIRA_CERTIFY_PAR"].trim().parse().map_err(|_| "SPIRA_CERTIFY_PAR must be a positive integer".to_string())?;
    let q = queue(e);
    let mut snap = pins(&pinned);
    snap.run_dir = e.world.run_dir.display().to_string();
    snap.inbox = q.queued().iter().map(task_job).collect();
    snap.claimed = q.claimed_slots().into_iter().map(|s| (s, q.claimed(s).iter().map(task_job).collect())).collect();
    snap.running_by_repo = q.running_by_repo().into_iter().map(|(r, n)| (r, n as u32)).collect();
    snap.free_slots = landing_pass::real::admission_free(&e.world.run_dir.join("gate-admission"), par) as u32;
    snap.now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    for (i, slot) in carry.slots.iter().enumerate() {
        let (Some(job), true) = (&slot.job, slot.leg_started) else { continue };
        let leg = match slot.state {
            State::Warming => "warm",
            State::Gating => "gate",
            _ => continue,
        };
        let (out_path, rc_path) = leg_paths(&snap.run_dir, i, leg, job);
        let (rc, out) = read_leg(&rc_path, &out_path);
        let running = rc.is_none() && e.units.as_deref().ok_or("no unit manager configured")?.facts(&runtime::units::UnitHandle::of(NAME, &leg_key(i, leg, job)).map_err(|e| e.0)?).map_err(|e| e.0)?.state != runtime::units::UnitState::Absent;
        let run = (!running && leg == "gate").then(|| classify(rc.unwrap_or(KILLED), &out));
        snap.legs.insert(i, Leg { running, rc, verdict: run.as_ref().map(|r| r.verdict.clone()), reason: run.and_then(|r| r.reason) });
    }
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

/// `git rev-parse refs/heads/<branch>` in the job's repository; empty when it is gone.
pub fn tip(input: &str) -> Result<String, String> {
    let sj = slot_job(input, TIP)?;
    let home = spira_config::resolve::locate_home_for_process()?;
    let Some(root) = spira_config::repos::Registry::from_env(std::env::vars().collect(), &home).root(&sj.job.repo) else { return Ok(String::new()) };
    Ok(super::gate_check::git(std::path::Path::new(&root), &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{}", sj.job.branch)], 5).unwrap_or_default().trim().to_string())
}

/// Claim the chosen job for the slot: one rename, refused when it is already claimed.
pub fn claim(e: &crate::Effects, input: &str) -> Result<String, String> {
    let sj = slot_job(input, CLAIM)?;
    queue(e).claim_job(sj.slot, &sj.job.repo, &sj.job.branch, &sj.job.tip).map(|_| format!("slot {} claimed {}", sj.slot, sj.job.key())).ok_or_else(|| format!("{CLAIM}: {} is no longer queued — refused", sj.job.key()))
}

/// Drop the slot's claim on a job whose branch is gone or moved.
pub fn discard(e: &crate::Effects, input: &str) -> Result<String, String> {
    let sj = slot_job(input, DISCARD)?;
    let q = queue(e);
    let job = q.claimed_job(sj.slot, &sj.job.repo, &sj.job.branch, &sj.job.tip).ok_or_else(|| format!("{DISCARD}: slot {} does not hold {} — refused", sj.slot, sj.job.key()))?;
    q.discard(sj.slot, &job);
    Ok(format!("slot {} dropped {}", sj.slot, sj.job.key()))
}

fn filing(input: &str, effect: &str) -> Result<Filing, String> {
    serde_json::from_str(input).map_err(|e| format!("{effect}: bad input: {e}"))
}

/// The gate run as `gate_worker::classify` reads it from the leg's own status and output.
fn gate_run(f: &Filing) -> landing_pass::model::GateRun {
    let out = std::fs::read_to_string(&f.out_path).unwrap_or_default();
    gate_worker::classify(f.rc.unwrap_or(KILLED), out)
}

/// Keep a non-PASS gate's whole output (the newest 300 are kept).
pub fn retain_output(e: &crate::Effects, input: &str) -> Result<String, String> {
    let f = filing(input, RETAIN_OUTPUT)?;
    let q = queue(e);
    let job = q.claimed_job(f.slot, &f.job.repo, &f.job.branch, &f.job.tip).unwrap_or_else(|| landing_pass::gateq::Job::new(&f.job.repo, &f.job.branch, &f.job.bead, &f.job.tip, f.job.fix));
    q.retain_output(&job, &gate_run(&f).out).map(|p| p.display().to_string()).map_err(|e| format!("{RETAIN_OUTPUT}: {e}"))
}

/// File the verdict for the pass and free the slot's claim; the leg's files go with it.
pub fn complete(e: &crate::Effects, input: &str) -> Result<String, String> {
    let f = filing(input, COMPLETE)?;
    let q = queue(e);
    let job = q.claimed_job(f.slot, &f.job.repo, &f.job.branch, &f.job.tip).ok_or_else(|| format!("{COMPLETE}: slot {} does not hold {} — refused", f.slot, f.job.key()))?;
    let run = gate_run(&f);
    let word = run.outcome.word();
    q.complete(f.slot, &landing_pass::gateq::Done { job, run, started_ms: f.started_ms, finished_ms: f.finished_ms }).map_err(|e| format!("{COMPLETE}: {e}"))?;
    for leg in ["warm", "gate"] {
        let (out, rc) = leg_paths(&e.world.run_dir.display().to_string(), f.slot, leg, &f.job);
        let _ = std::fs::remove_file(out);
        let _ = std::fs::remove_file(rc);
    }
    Ok(format!("{} → {word}", f.job.key()))
}

/// A slot's claims go back to the inbox.
pub fn recover(e: &crate::Effects, input: &str) -> Result<String, String> {
    let slot: usize = input.trim().parse().map_err(|_| format!("{RECOVER}: bad slot {input:?}"))?;
    queue(e).recover(slot);
    Ok(format!("slot {slot} recovered"))
}
