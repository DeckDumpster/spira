//! The in-process executor's core, synchronous and clock-agnostic: one FIFO of events, each
//! dispatched to its handlers, then one store transaction for the turn. Every id it writes is
//! derived from the event that caused it. History is shipped, never read back; a restart finds
//! only what the store kept (carried state, open effects, unfired schedule rows, the halt).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::event::*;
use crate::journal::{is_outcome, EffectRow, Journal, Poison, ScheduleRow, StateRow, Turn, CARRY_KEY, HALTED_KEY, HELD_KEY, SCHEDULER_TASK};
use crate::marker::{Mark, Marker, NullMarker};
use crate::offload::{Busy, Steps};
use crate::changes::Changes;
use crate::ports::Clock;
use crate::scheduler::Scheduler;
use crate::wide::{Fields, NullSink, TurnRss, Value, Wide, WideSink};
use crate::{Result, RuntimeError};

/// A stream found quarantined is asked again after this long, so a cleared row resumes it; a stream found healthy is not.
const POISON_RECHECK_MS: u64 = 300_000;

/// Memory-only events a stream keeps in the projection; older ones are dropped.
pub const LOCAL_KEEP: usize = 64;

pub const LOST_AT_RESTART: &str = "lost at restart";

/// Bytes of an effect's failure text its wide event keeps.
pub const EFFECT_DETAIL_MAX: usize = 512;

fn clip(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Hears whether a submitted event's turn was committed.
pub type Reply = tokio::sync::oneshot::Sender<std::result::Result<(), String>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Succeeded(String),
    Failed(String),
    TimedOut,
    /// The process that launched the effect ended before it answered; whether it ran is unknown.
    Lost,
}

impl Outcome {
    fn tag(&self) -> &'static str {
        match self {
            Outcome::Succeeded(_) => EFFECT_SUCCEEDED,
            Outcome::Failed(_) => EFFECT_FAILED,
            Outcome::TimedOut | Outcome::Lost => EFFECT_TIMED_OUT,
        }
    }

    fn detail(&self) -> String {
        match self {
            Outcome::Succeeded(d) | Outcome::Failed(d) => d.clone(),
            Outcome::TimedOut => String::new(),
            Outcome::Lost => LOST_AT_RESTART.into(),
        }
    }
}

/// An effect requested and not yet answered. `key` is the idempotency key: the id of the
/// request event, derived from the causing event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingEffect {
    pub key: String,
    /// The task that asked.
    pub task: String,
    pub stream: StreamKey,
    pub spec: EffectSpec,
    pub deadline_at: u64,
    pub requested_at: u64,
    pub correlation: CorrelationId,
}

#[derive(Serialize, Deserialize)]
struct RequestBody {
    spec: EffectSpec,
    deadline_at: u64,
    /// The task that asked: the one handler its outcome is offered to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct HeldBody {
    stream: StreamKey,
    task: String,
    spec: EffectSpec,
}

/// The ids of submitted events remembered, so the same id submitted twice dispatches once.
const SUBMITTED_KEEP: usize = 10_000;

/// The events whose wake is remembered, so an outcome that arrives long after its request still says what woke the run.
const WAKES_KEEP: usize = 10_000;

/// What started a run: the row's own periodic tick, any other timer or schedule row, or an event from outside or from another task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    Sweep,
    Timer,
    Signal,
}

impl Wake {
    pub fn as_str(self) -> &'static str {
        match self {
            Wake::Sweep => "sweep",
            Wake::Timer => "timer",
            Wake::Signal => "signal",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum LastOutcome {
    Succeeded,
    Failed,
    TimedOut,
    /// Its effector class was full; the effect is still waiting to run.
    Deferred,
    /// It was fired and its handler answered with nothing to do.
    Quiet,
}

/// The newest event a task emitted or noted: what it carries forward.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CarriedState {
    pub stream: String,
    pub tag: String,
    pub body_len: usize,
    /// The emitted body, for a reader that decodes it; never part of the status document.
    #[serde(skip)]
    pub body: String,
}

/// The commands a handler returned for one event, and how many the engine admitted: a refused
/// or held effect is returned and not admitted, an empty fire is neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct FireCommands {
    pub returned: usize,
    pub admitted: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TaskStatus {
    pub name: String,
    pub mode: Mode,
    pub plane: Plane,
    /// Each declared effect and the plane a halt reads for it.
    pub effects: Vec<(String, Plane)>,
    pub state: Option<CarriedState>,
    /// Engine clock when the task last handled an event, and that event's own time.
    pub last_fired_at: Option<u64>,
    pub last_due_at: Option<u64>,
    pub lag_ms: Option<u64>,
    /// The earliest schedule row or timer waiting for a tag the task handles.
    pub next_due_at: Option<u64>,
    pub last_outcome: Option<LastOutcome>,
    /// What its last fire returned and how much of it the engine took up.
    pub last_commands: Option<FireCommands>,
    /// Idempotency keys of its effects and probes with no outcome yet.
    pub in_flight: Vec<String>,
    /// Runs begun by an event of this wake, counting the events that started a run and not those it derived.
    pub runs_by_wake: BTreeMap<String, u64>,
    /// Runs the engine's sweep began, with no `store.changed` having named the row's reads, that went on to admit an act: the work a swallowed wake-up left behind.
    pub sweep_found: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ClassStatus {
    pub class: String,
    pub depth: usize,
    pub refusals: u64,
}

/// What the event shipper has lost: a full queue drops, a failed POST counts, neither is retried.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ShipStatus {
    #[serde(rename = "ship.dropped")]
    pub dropped: u64,
    #[serde(rename = "ship.failed")]
    pub failed: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SchedulerStatus {
    pub at: u64,
    #[serde(flatten)]
    pub ship: Option<ShipStatus>,
    pub halted: bool,
    pub held: usize,
    pub tasks: Vec<TaskStatus>,
    pub classes: Vec<ClassStatus>,
}

pub use crate::status_board::StatusBoard;

pub type Classifier = Arc<dyn Fn(&EffectSpec) -> String + Send + Sync>;

/// The class an effect runs under when no classifier is given.
pub const DEFAULT_CLASS: &str = "host";

#[derive(Default)]
struct Track {
    state: Option<CarriedState>,
    fired_at: Option<u64>,
    due_at: Option<u64>,
    outcome: Option<LastOutcome>,
    commands: Option<FireCommands>,
    runs_by_wake: BTreeMap<String, u64>,
    sweep_found: u64,
}

/// A task that declared its reads: what the engine knows of its tick, once the tick has run.
struct Ticking {
    wake: crate::event::Wake,
    stream: Option<StreamKey>,
    tag: String,
    body: String,
    last_tick_at: u64,
    last_rev: Option<u64>,
    signal_at: Option<u64>,
    due_at: Option<u64>,
    sweep_run: Option<EventId>,
    /// The row's `last_rev` before the sweep's run, and the revision that run was begun at.
    sweep_from: Option<u64>,
    sweep_at_rev: Option<u64>,
    /// A sweep's find, held until the next change or sweep says whether a wake was already carrying it.
    held: Option<Held>,
    /// Prefixes of the keys heard since the row's previous tick, and those heard before the sweep's own tick.
    heard: BTreeSet<String>,
    sweep_heard: BTreeSet<String>,
}

struct Held {
    ev: Event,
    rev: u64,
    from: Option<u64>,
}

/// The part of `key` up to and including its first `/`.
fn key_prefix(key: &str) -> String {
    key.split_inclusive('/').next().unwrap_or(key).to_string()
}

/// The in-process sweep: every `every_ms`, run the ticks of tasks whose reads moved unseen.
struct Sweep {
    every_ms: u64,
    changes: Arc<dyn Changes>,
    at: u64,
}

pub trait Executor {
    fn submit(&mut self, id: &str, stream: &str, kind: EventKind) -> Result<()>;
    fn run_until_idle(&mut self) -> Result<()>;
    fn pending_effects(&self) -> Vec<PendingEffect>;
    /// `false` when the effect already has an outcome: the first outcome wins.
    fn complete(&mut self, key: &str, outcome: Outcome) -> Result<bool>;
}


pub struct Engine {
    journal: Arc<dyn Journal>,
    clock: Arc<dyn Clock>,
    regs: Vec<Registration>,
    seed: u64,
    projection: Projection,
    scheduler: Scheduler,
    pending: BTreeMap<String, PendingEffect>,
    sink: Arc<dyn WideSink>,
    marker: Arc<dyn Marker>,
    busy: Arc<Busy>,
    steps: Arc<Steps>,
    stall_limit: u32,
    suspects: BTreeMap<(EventId, String), u32>,
    attempts: BTreeMap<String, u32>,
    children: BTreeMap<String, BTreeMap<String, u64>>,
    baseline: Vec<(String, Value)>,
    started: Instant,
    poison_seen: BTreeMap<StreamKey, bool>,
    quarantined_here: BTreeSet<StreamKey>,
    set_aside: BTreeSet<EventId>,
    poison_cleared_at: u64,
    /// Every event waiting for its handlers, in the order it was made; `true` is memory-only.
    queue: VecDeque<(Event, bool)>,
    queued_ids: BTreeSet<EventId>,
    /// Submitters waiting to hear their event's turn committed, by submission id.
    replies: BTreeMap<String, Reply>,
    /// Submissions dispatched whose turn is not yet committed.
    uncommitted: Vec<String>,
    submitted: VecDeque<String>,
    submitted_ids: BTreeSet<String>,
    seq: u64,
    timers: BTreeMap<String, ScheduleRow>,
    memory_only: BTreeSet<String>,
    local_seq: u64,
    halted: bool,
    /// The writes held since the halt, each an [`EFFECT_HELD`] event, in the order they were held.
    held: Vec<Event>,
    tracks: BTreeMap<String, Track>,
    owners: BTreeMap<String, String>,
    refusals: BTreeMap<String, u64>,
    classifier: Option<Classifier>,
    board: Option<Arc<StatusBoard>>,
    /// Events in the projection ahead of their own dispatch: what a handler wrote on its own
    /// stream, placed right after the event that caused it.
    early: BTreeSet<EventId>,
    /// What the event being dispatched has had written on its stream so far; `true` is local.
    written: Vec<(Event, bool)>,
    /// The task that requested each effect not yet answered, as far as this run has seen.
    requesters: BTreeMap<String, String>,
    /// Effects an executor has taken and not yet started: waiting for a permit or a worker.
    /// Their deadline is not running, so `tick` never times them out.
    queued: BTreeSet<String>,
    rss: TurnRss,
    /// Each task's newest own-stream emit per (stream, kind): the carry stored under [`CARRY_KEY`], or as the rows of its codec.
    carries: BTreeMap<String, BTreeMap<(String, String), Event>>,
    dirty_carry: BTreeSet<String>,
    codecs: BTreeMap<String, RowCodec>,
    dirty_halt: bool,
    /// What every state key last committed says, as [`carry_signature`] reads it: a key is
    /// written only when this changes.
    kept: BTreeMap<(String, String), String>,
    /// The signatures of the state rows the turn in progress will commit.
    staged: BTreeMap<(String, String), String>,
    /// What the turn in progress will commit.
    turn: Turn,
    /// Requests whose row has committed: only these are offered to an executor.
    committed: BTreeSet<String>,
    /// The schedule row a fired event came from, and the request an outcome answers: both are
    /// recorded in the turn that dispatches the event.
    fire_of: BTreeMap<EventId, String>,
    /// The timer behind each queued local tick.
    tick_of: BTreeMap<EventId, ScheduleRow>,
    resolves: BTreeMap<EventId, String>,
    /// The wake of each recent event, and whether the event began a run (`true`) or derives from one.
    wakes: BTreeMap<EventId, (Wake, bool)>,
    wake_order: VecDeque<EventId>,
    /// The tick each stream was booted with: its later fires are sweeps.
    boot_tags: BTreeMap<String, String>,
    ticking: BTreeMap<String, Ticking>,
    sweep: Option<Sweep>,
    /// The sweep-woken run each event belongs to, and the runs already counted as having found work.
    sweep_run: BTreeMap<EventId, EventId>,
    /// The highest `StoreChanged.rev` dispatched: what the source is known to have said.
    heard_rev: Option<u64>,
    sweep_counted: BTreeSet<EventId>,
}

impl Ticking {
    fn pending_at(&self) -> Option<u64> {
        self.signal_at.into_iter().chain(self.due_at).min()
    }
}

/// What a whole-carry row holds once its task is kept as rows: no events.
const LEGACY_SPENT: &str = "[]";

/// What a carry says without the ids, causes and times of the events that carry it: the same
/// state emitted again is no change, and writes nothing.
fn carry_signature<'a>(events: impl Iterator<Item = &'a Event>) -> String {
    let parts: Vec<(&str, &str, &str)> = events.map(|e| (e.stream.0.as_str(), e.kind.tag.as_str(), e.kind.body.as_str())).collect();
    serde_json::to_string(&parts).unwrap_or_default()
}

fn mix(seed: u64, parts: &[&str]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64 ^ seed;
    for p in parts {
        for b in p.bytes().chain(std::iter::once(0xff)) {
            h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

fn panic_message(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "handler panicked".into())
}

/// How `Engine::open` treats a journal that cannot tell: ask again after `first_ms`, doubling,
/// until the time spent, waits and asks together, reaches `bound_ms` (the last wait is cut short to fit),
/// then fail naming `store`, the attempts, the time spent and the journal's last words.
#[derive(Clone)]
pub struct BootRetry {
    pub first_ms: u64,
    pub ceiling_ms: u64,
    pub bound_ms: u64,
    pub store: String,
    pub sleep: Arc<dyn Fn(u64) + Send + Sync>,
}

impl BootRetry {
    /// Ask once: the policy of a caller that has no store to wait for.
    pub fn once() -> Self {
        BootRetry { first_ms: 0, ceiling_ms: 0, bound_ms: 0, store: String::new(), sleep: Arc::new(|_| {}) }
    }

    fn read<T>(&self, clock: &dyn Clock, mut ask: impl FnMut() -> Result<T>) -> Result<T> {
        let (mut wait, mut spent, mut attempts) = (self.first_ms, 0u64, 0u32);
        loop {
            let began = clock.now_ms();
            let answer = ask();
            attempts += 1;
            spent = spent.saturating_add(clock.now_ms().saturating_sub(began));
            match answer {
                Err(e) if e.is_cannot_tell() => {
                    if wait == 0 || spent >= self.bound_ms {
                        return Err(RuntimeError::new(format!("{}: still cannot tell after {attempts} attempt(s) over {spent} ms: {}", self.store, e.0)));
                    }
                    let wait_now = wait.min(self.bound_ms - spent);
                    (self.sleep)(wait_now);
                    spent += wait_now;
                    wait = wait.saturating_mul(2).min(self.ceiling_ms);
                }
                other => return other,
            }
        }
    }
}

impl Engine {
    pub fn open(journal: Arc<dyn Journal>, clock: Arc<dyn Clock>, regs: Vec<Registration>, seed: u64) -> Result<Self> {
        Self::open_with(journal, clock, regs, seed, BootRetry::once())
    }

    /// Rebuild from what the store kept: each task's carry, the halt, the effects still waiting
    /// for an outcome and the schedule rows not yet fired. No event is replayed.
    pub fn open_with(journal: Arc<dyn Journal>, clock: Arc<dyn Clock>, regs: Vec<Registration>, seed: u64, retry: BootRetry) -> Result<Self> {
        let codecs = regs.iter().filter_map(|r| r.rows.map(|c| (r.name.clone(), c))).collect();
        let ticking = regs.iter().filter_map(|r| r.wake.clone().map(|wake| (r.name.clone(), Ticking { wake, stream: None, tag: String::new(), body: String::new(), last_tick_at: 0, last_rev: None, signal_at: None, due_at: None, sweep_run: None, sweep_from: None, sweep_at_rev: None, held: None, heard: BTreeSet::new(), sweep_heard: BTreeSet::new() }))).collect();
        let mut e = Engine {
            journal,
            clock,
            regs,
            seed,
            projection: Projection::default(),
            scheduler: Scheduler::new(seed),
            pending: BTreeMap::new(),
            sink: Arc::new(NullSink),
            marker: Arc::new(NullMarker),
            busy: Arc::new(Busy::default()),
            steps: Arc::new(Steps::default()),
            stall_limit: 2,
            suspects: BTreeMap::new(),
            attempts: BTreeMap::new(),
            children: BTreeMap::new(),
            baseline: vec![],
            started: Instant::now(),
            poison_seen: BTreeMap::new(),
            quarantined_here: BTreeSet::new(),
            set_aside: BTreeSet::new(),
            poison_cleared_at: 0,
            queue: VecDeque::new(),
            queued_ids: BTreeSet::new(),
            replies: BTreeMap::new(),
            uncommitted: vec![],
            submitted: VecDeque::new(),
            submitted_ids: BTreeSet::new(),
            seq: 0,
            timers: BTreeMap::new(),
            memory_only: BTreeSet::new(),
            local_seq: 0,
            halted: false,
            held: vec![],
            tracks: BTreeMap::new(),
            owners: BTreeMap::new(),
            refusals: BTreeMap::new(),
            classifier: None,
            board: None,
            early: BTreeSet::new(),
            written: vec![],
            requesters: BTreeMap::new(),
            queued: BTreeSet::new(),
            rss: TurnRss::default(),
            carries: BTreeMap::new(),
            dirty_carry: BTreeSet::new(),
            codecs,
            dirty_halt: false,
            kept: BTreeMap::new(),
            staged: BTreeMap::new(),
            turn: Turn::default(),
            committed: BTreeSet::new(),
            fire_of: BTreeMap::new(),
            tick_of: BTreeMap::new(),
            resolves: BTreeMap::new(),
            wakes: BTreeMap::new(),
            wake_order: VecDeque::new(),
            boot_tags: BTreeMap::new(),
            ticking,
            sweep: None,
            sweep_run: BTreeMap::new(),
            heard_rev: None,
            sweep_counted: BTreeSet::new(),
        };
        let boot = retry.read(e.clock.as_ref(), || e.journal.boot())?;
        let mut legacy: BTreeMap<String, StateRow> = BTreeMap::new();
        let mut keyed: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        for row in boot.state {
            if e.codecs.contains_key(&row.task) {
                e.kept.insert((row.task.clone(), row.skey.clone()), row.value.clone());
                if row.skey == CARRY_KEY {
                    legacy.insert(row.task.clone(), row);
                } else {
                    if row.value.is_empty() {
                        e.turn.cleared.push((row.task.clone(), row.skey));
                    } else {
                        keyed.entry(row.task.clone()).or_default().insert(row.skey, row.value);
                    }
                }
                continue;
            }
            e.restore(&row)?;
        }
        e.restore_keyed(legacy, keyed)?;
        let lost: Vec<String> = boot.effects.iter().map(|r| r.request_id.clone()).collect();
        for row in boot.effects {
            e.restore_effect(row)?;
        }
        for key in lost {
            e.complete(&key, Outcome::Lost)?;
        }
        for row in boot.schedules {
            e.scheduler.insert(row);
        }
        Ok(e)
    }

    fn restore(&mut self, row: &StateRow) -> Result<()> {
        let bad = |what: &str, err: serde_json::Error| RuntimeError::new(format!("kept {what} of {} is not readable: {err}", row.task));
        match (row.task.as_str(), row.skey.as_str()) {
            (SCHEDULER_TASK, HALTED_KEY) => self.halted = row.value == "1",
            (SCHEDULER_TASK, HELD_KEY) => {
                self.held = serde_json::from_str(&row.value).map_err(|e| bad(HELD_KEY, e))?;
                for held in &self.held {
                    self.projection.push(held);
                }
            }
            (_, CARRY_KEY) => {
                let events: Vec<Event> = serde_json::from_str(&row.value).map_err(|e| bad(CARRY_KEY, e))?;
                for ev in events {
                    self.projection.push(&ev);
                    self.carry(&row.task, &ev.stream, &ev.kind);
                    self.carries.entry(row.task.clone()).or_default().insert((ev.stream.0.clone(), ev.kind.tag.clone()), ev);
                }
            }
            _ => {}
        }
        let sig = if row.skey == CARRY_KEY { carry_signature(self.carries.get(&row.task).map(|c| c.values()).into_iter().flatten()) } else { row.value.clone() };
        self.kept.insert((row.task.clone(), row.skey.clone()), sig);
        Ok(())
    }

    /// A task kept as rows is rebuilt from its rows alone; the whole-carry row of an older
    /// store is read only while no row of the task exists. A task whose codec keeps none of its
    /// rows has them deleted by the first turn, whichever task it is.
    fn restore_keyed(&mut self, mut legacy: BTreeMap<String, StateRow>, mut keyed: BTreeMap<String, BTreeMap<String, String>>) -> Result<()> {
        for task in self.codecs.keys().cloned().collect::<Vec<_>>() {
            let codec = self.codecs[&task];
            let Some(rows) = keyed.remove(&task).map(|r| r.into_iter().filter(|(_, v)| !v.is_empty()).collect::<BTreeMap<_, _>>()).filter(|r| !r.is_empty()) else {
                if let Some(row) = legacy.remove(&task) {
                    self.restore(&row)?;
                    self.kept.insert((task.clone(), CARRY_KEY.into()), row.value);
                    self.dirty_carry.insert(task);
                }
                continue;
            };
            if (codec.split)(&(codec.join)(&rows)).is_empty() {
                self.turn.cleared.extend(rows.into_keys().map(|skey| (task.clone(), skey)));
                continue;
            }
            let id = derive_id(format!("boot/{task}/{}/{}", codec.stream, codec.tag));
            let kind = EventKind::new(codec.tag, &(codec.join)(&rows));
            let ev = Event { id: EventId(id.clone()), stream: StreamKey(codec.stream.into()), kind, at: self.clock.now_ms(), cause: None, correlation: CorrelationId(id) };
            self.projection.push(&ev);
            self.carry(&task, &ev.stream, &ev.kind);
            self.carries.entry(task).or_default().insert((ev.stream.0.clone(), ev.kind.tag.clone()), ev);
        }
        Ok(())
    }

    fn restore_effect(&mut self, row: EffectRow) -> Result<()> {
        let class = self.class_of(&row.spec);
        self.refusals.entry(class).or_insert(0);
        self.owners.insert(row.request_id.clone(), row.task.clone());
        self.requesters.insert(row.request_id.clone(), row.task.clone());
        self.committed.insert(row.request_id.clone());
        let correlation = CorrelationId(row.request_id.clone());
        // A handler learns which effect an outcome answers by reading its request from the view.
        let body = RequestBody { spec: row.spec.clone(), deadline_at: row.deadline_at, task: Some(row.task.clone()) };
        let body = serde_json::to_string(&body).map_err(|e| RuntimeError::new(format!("kept effect {} of {} does not encode: {e}", row.request_id, row.task)))?;
        let kind = EventKind::new(EFFECT_REQUESTED, &body);
        self.projection.push(&Event { id: EventId(row.request_id.clone()), stream: row.stream.clone(), kind, at: row.requested_at, cause: None, correlation: correlation.clone() });
        self.pending.insert(row.request_id.clone(), PendingEffect { key: row.request_id, task: row.task, stream: row.stream, spec: row.spec, deadline_at: row.deadline_at, requested_at: row.requested_at, correlation });
        Ok(())
    }

    /// Names the effector class of an effect, for `status`.
    pub fn with_classifier(mut self, c: Classifier) -> Self {
        self.classifier = Some(c);
        self
    }

    /// Turns on the wake mechanism: a task with a [`crate::event::Wake`] runs when a
    /// `store.changed` names a key it reads, and every `every_ms` the engine runs, in memory,
    /// the ones whose reads moved since their last tick. Without it every task runs as today.
    pub fn with_sweep(mut self, every_ms: u64, changes: Arc<dyn Changes>) -> Self {
        let at = self.clock.now_ms().saturating_add(every_ms);
        self.sweep = Some(Sweep { every_ms, changes, at });
        self
    }

    /// Receives `status()` at the end of every `run_until_idle`.
    pub fn with_status_board(mut self, board: Arc<StatusBoard>) -> Self {
        self.board = Some(board);
        self
    }

    fn class_of(&self, spec: &EffectSpec) -> String {
        self.classifier.as_ref().map_or_else(|| DEFAULT_CLASS.to_string(), |c| c(spec))
    }

    /// The host's effector class `class` was full when it offered effect `key`.
    pub fn note_deferred(&mut self, key: &str, class: &str) {
        *self.refusals.entry(class.into()).or_insert(0) += 1;
        if let Some(owner) = self.owners.get(key) {
            self.tracks.entry(owner.clone()).or_default().outcome = Some(LastOutcome::Deferred);
        }
    }

    pub fn status(&self) -> SchedulerStatus {
        let rows: Vec<&ScheduleRow> = self.scheduler.rows().chain(self.timers.values()).collect();
        let tasks = self
            .regs
            .iter()
            .map(|r| {
                let t = self.tracks.get(&r.name);
                let interests = r.handler.interests();
                TaskStatus {
                    name: r.name.clone(),
                    mode: r.mode,
                    plane: r.plane,
                    effects: r.effects.iter().map(|e| (e.to_string(), r.plane_of(e))).collect(),
                    state: t.and_then(|t| t.state.clone()),
                    last_fired_at: t.and_then(|t| t.fired_at),
                    last_due_at: t.and_then(|t| t.due_at),
                    lag_ms: t.and_then(|t| Some(t.fired_at?.saturating_sub(t.due_at?))),
                    next_due_at: rows.iter().filter(|row| interests.contains(&row.kind.tag.as_str())).map(|row| row.at).chain(self.ticking.get(&r.name).filter(|t| t.stream.is_some()).and_then(|t| t.pending_at())).chain(self.ticking.get(&r.name).filter(|t| !t.wake.polls).and_then(|_| self.sweep.as_ref().map(|w| w.at))).min(),
                    last_outcome: t.and_then(|t| t.outcome),
                    last_commands: t.and_then(|t| t.commands),
                    in_flight: self.pending.keys().filter(|k| self.owners.get(*k) == Some(&r.name)).cloned().collect(),
                    runs_by_wake: t.map(|t| t.runs_by_wake.clone()).unwrap_or_default(),
                    sweep_found: t.map_or(0, |t| t.sweep_found),
                }
            })
            .collect();
        let mut classes: BTreeMap<String, ClassStatus> = self.refusals.iter().map(|(c, n)| (c.clone(), ClassStatus { class: c.clone(), depth: 0, refusals: *n })).collect();
        for p in self.pending.values() {
            let c = self.class_of(&p.spec);
            classes.entry(c.clone()).or_insert_with(|| ClassStatus { class: c, depth: 0, refusals: 0 }).depth += 1;
        }
        SchedulerStatus { at: self.clock.now_ms(), ship: self.ship_status(), halted: self.halted, held: self.held.len(), tasks, classes: classes.into_values().collect() }
    }

    fn ship_status(&self) -> Option<ShipStatus> {
        self.journal.shipped().map(|c| ShipStatus { dropped: c.dropped(), failed: c.failed() })
    }

    fn publish(&self) {
        if let Some(b) = &self.board {
            b.put(self.status());
        }
    }

    pub fn with_sink(mut self, sink: Arc<dyn WideSink>) -> Self {
        self.sink = sink;
        self
    }

    /// Fields every wide event carries (runtime version, config hash, ...).
    pub fn with_baseline(mut self, fields: Vec<(String, Value)>) -> Self {
        self.baseline = fields;
        self
    }

    /// The probe the host reads to tell a parked handler from a turn waiting on I/O.
    pub fn busy(&self) -> Arc<Busy> {
        self.busy.clone()
    }

    /// The step the turn is in, for the host to name when a turn overruns.
    pub fn steps(&self) -> Arc<Steps> {
        self.steps.clone()
    }

    pub fn with_busy(mut self, busy: Arc<Busy>) -> Self {
        self.busy = busy;
        self
    }

    /// Restarts a handler call may stall through before its stream is quarantined.
    pub fn with_stall_limit(mut self, limit: u32) -> Self {
        self.stall_limit = limit;
        self
    }

    /// Adopts the marker. One left by a previous run is a call that never returned: it counts
    /// as a stall of that (event, handler) and is reported as a restart.
    pub fn with_marker(mut self, marker: Arc<dyn Marker>) -> Result<Self> {
        self.marker = marker;
        if let Some(m) = self.marker.take()? {
            let stalls = m.stalls + 1;
            self.suspects.insert((m.event.clone(), m.handler.clone()), stalls);
            let w = self.unit("watchdog.restart", None);
            w.set("event.id", m.event.0.as_str());
            w.set("event.stream", m.stream.0.as_str());
            w.set("handler", m.handler.as_str());
            w.set("stall.count", stalls);
            w.set("outcome", "restarted");
            self.finish(&w);
        }
        Ok(self)
    }

    /// The executor has taken effect `key` and will start it once its class has a permit. Until
    /// [`note_attempt`](Self::note_attempt) its deadline does not run: queue wait never times
    /// an effect out.
    pub fn note_queued(&mut self, key: &str) {
        if self.pending.contains_key(key) {
            self.queued.insert(key.into());
        }
    }

    /// The programs the effect `key` started, written on its `effect` record when it completes.
    pub fn note_children(&mut self, key: &str, children: BTreeMap<String, u64>) {
        if !children.is_empty() {
            self.children.insert(key.into(), children);
        }
    }

    /// The executor started the runner for effect `key`; its deadline counts from now.
    pub fn note_attempt(&mut self, key: &str) {
        *self.attempts.entry(key.into()).or_insert(0) += 1;
        self.queued.remove(key);
        let now = self.clock.now_ms();
        if let Some(p) = self.pending.get_mut(key) {
            p.deadline_at = now.saturating_add(p.spec.deadline_ms);
        }
    }

    /// The deadline of every effect whose deadline is running.
    fn running_deadlines(&self) -> impl Iterator<Item = (u64, &String)> {
        self.pending.values().filter(|p| !self.queued.contains(&p.key)).map(|p| (p.deadline_at, &p.key))
    }

    fn unit(&self, unit: &str, ev: Option<&Event>) -> Wide {
        let w = Wide::default();
        w.set("unit", unit);
        for (k, v) in &self.baseline {
            w.set(k, v.clone());
        }
        w.set("process.rss_kb", self.rss.get());
        w.set("process.uptime_ms", self.started.elapsed().as_millis() as u64);
        if let Some(e) = ev {
            w.set("event.id", e.id.0.as_str());
            w.set("event.stream", e.stream.0.as_str());
            w.set("event.kind", e.kind.tag.as_str());
            w.set("event.at", e.at);
            w.set("wake", self.wake_of(&e.id).0.as_str());
            if let Some(c) = &e.cause {
                w.set("event.cause", c.0.as_str());
            }
            w.set("correlation.id", e.correlation.0.as_str());
        }
        w.set("dispatch.queued", self.queue.len() as u64);
        if let Some(c) = self.journal.shipped() {
            w.set("ship.dropped", c.dropped());
            w.set("ship.failed", c.failed());
        }
        w
    }

    fn finish(&self, w: &Wide) {
        let fields: Fields = w.take();
        self.sink.emit(&fields);
    }

    fn track_effect(&mut self, ev: &Event) -> Result<()> {
        if ev.kind.tag == EFFECT_REQUESTED {
            let b: RequestBody = serde_json::from_str(&ev.kind.body).map_err(|e| RuntimeError::new(format!("bad effect request {}: {e}", ev.id.0)))?;
            let class = self.class_of(&b.spec);
            self.refusals.entry(class).or_insert(0);
            let task = b.task.unwrap_or_default();
            self.pending.insert(
                ev.id.0.clone(),
                PendingEffect { key: ev.id.0.clone(), task, stream: ev.stream.clone(), spec: b.spec, deadline_at: b.deadline_at, requested_at: ev.at, correlation: ev.correlation.clone() },
            );
        } else if is_outcome(&ev.kind.tag) {
            if let Some(c) = &ev.cause {
                self.pending.remove(&c.0);
                self.queued.remove(&c.0);
            }
        }
        Ok(())
    }

    /// Request `spec` for `task`, once per `(task, key)` at a time: a second request while one is
    /// in flight is refused and reported, not requested. `None` when refused.
    fn request(&mut self, task: &str, stream: &StreamKey, id: String, spec: EffectSpec, cause: &Event, at: u64) -> Result<Option<Event>> {
        let flying = self.pending.values().find(|p| p.task == task && p.spec.key == spec.key && p.key != id && !self.memory_only.contains(&p.key)).map(|p| p.key.clone());
        if let Some(first) = flying {
            let w = self.unit("effect.refused", Some(cause));
            w.set("handler", task);
            w.set("effect.kind", spec.name.as_str());
            w.set("effect.key", spec.key.as_str());
            w.set("effect.in_flight", first.as_str());
            w.set("outcome", "refused");
            self.finish(&w);
            return Ok(None);
        }
        let deadline_at = at.saturating_add(spec.deadline_ms);
        let row = EffectRow { task: task.into(), ekey: crate::rows::bounded_key(&spec.key), request_id: id.clone(), stream: stream.clone(), spec: spec.clone(), requested_at: at, deadline_at };
        let body = serde_json::to_string(&RequestBody { spec, deadline_at, task: Some(task.into()) }).map_err(|e| RuntimeError::new(e.to_string()))?;
        let req = self.append_derived(id, stream, EventKind::new(EFFECT_REQUESTED, &body), cause, at)?;
        self.owners.insert(req.id.0.clone(), task.into());
        self.requesters.insert(req.id.0.clone(), task.into());
        self.track_effect(&req)?;
        self.turn.effects.retain(|e| e.request_id != row.request_id);
        self.turn.effects.push(row);
        Ok(Some(req))
    }

    /// Requests a held write: it leaves the held list as it is requested. The request is placed
    /// with the resume, so no event queued ahead of it sees the write neither held nor requested.
    fn release(&mut self, held: &Event, at: u64) -> Result<()> {
        let b: HeldBody = serde_json::from_str(&held.kind.body).map_err(|e| RuntimeError::new(format!("bad held effect {}: {e}", held.id.0)))?;
        if let Some(req) = self.request(&b.task, &b.stream, derive_id(format!("{}/released", held.id.0)), b.spec, held, at)? {
            self.wrote(true, req, false);
        }
        Ok(())
    }

    /// A halt holds every `Act` write on the `Work` plane from then on; a resume requests what
    /// was held, in the order it was held.
    fn gate(&mut self, ev: &Event) -> Result<()> {
        match ev.kind.tag.as_str() {
            SCHEDULER_HALTED => {
                self.halted = true;
                self.dirty_halt = true;
            }
            SCHEDULER_RESUMED => {
                self.halted = false;
                self.dirty_halt = true;
                for h in std::mem::take(&mut self.held) {
                    self.release(&h, ev.at)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub fn is_halted(&self) -> bool {
        self.halted
    }

    /// Halts the scheduler before its first effect when it is not already halted: a boot under
    /// an outside halt must not run a Work effect in the gap before the halt is dispatched.
    pub fn halt_at_boot(&mut self, why: &str) -> Result<()> {
        if self.halted {
            return Ok(());
        }
        let at = self.clock.now_ms();
        self.submit(&format!("boot-halt/{at}"), HALT_STREAM, EventKind::new(SCHEDULER_HALTED, why))?;
        self.halted = true;
        self.dirty_halt = true;
        Ok(())
    }

    /// Replaces the handler of task `name`, keeping its mode, plane, effects and carry. `false` when no task has that name.
    pub fn rebind(&mut self, name: &str, handler: Box<dyn Handler>) -> bool {
        match self.regs.iter_mut().find(|r| r.name == name) {
            Some(r) => {
                r.handler = handler;
                true
            }
            None => false,
        }
    }

    /// Arms the tick of task `name` as a change to what it reads would, for a cause no store
    /// change reports. A task that polls, or an engine with no sweep, keeps its own timer.
    pub fn wake(&mut self, name: &str) {
        if self.sweep.is_none() {
            return;
        }
        let now = self.clock.now_ms();
        if let Some(t) = self.ticking.get_mut(name).filter(|t| t.stream.is_some() && !t.wake.polls && t.signal_at.is_none()) {
            t.signal_at = Some(now.max(t.last_tick_at.saturating_add(t.wake.min_gap_ms)));
        }
    }

    /// The body of the newest carry of `task` under `tag`, as the engine holds it now.
    pub fn carry_body(&self, task: &str, tag: &str) -> Option<String> {
        self.carries.get(task)?.iter().filter(|((_, t), _)| t == tag).map(|(_, e)| e).max_by_key(|e| e.at).map(|e| e.kind.body.clone())
    }

    pub fn into_registrations(mut self) -> Vec<Registration> {
        std::mem::take(&mut self.regs)
    }

    pub fn projection(&self) -> &Projection {
        &self.projection
    }

    pub fn next_timer_at(&self) -> Option<u64> {
        let deadline = self.running_deadlines().map(|(d, _)| d).min();
        let wakes = self.sweep.as_ref().map(|w| w.at).into_iter().chain(self.ticking.values().filter(|t| t.stream.is_some()).flat_map(|t| t.signal_at.into_iter().chain(t.due_at)));
        let timer = self.timers.values().map(|r| r.at).chain(wakes).min();
        match (self.scheduler.next_at().into_iter().chain(timer).min(), deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn local_event(&mut self, stream: &StreamKey, kind: EventKind, at: u64, cause: Option<&Event>) -> Event {
        self.local_seq += 1;
        let id = EventId(derive_id(format!("local/{}/{}/{at}/{}", stream.0, kind.tag, self.local_seq)));
        let correlation = cause.map_or_else(|| CorrelationId(id.0.clone()), |c| c.correlation.clone());
        if let Some(c) = cause {
            self.inherit_wake(&id, &c.id);
        }
        Event { id, stream: stream.clone(), kind, at, cause: cause.map(|c| c.id.clone()), correlation }
    }

    fn set_wake(&mut self, id: &EventId, wake: Wake, root: bool) {
        if self.wakes.insert(id.clone(), (wake, root)).is_none() {
            self.wake_order.push_back(id.clone());
            if self.wake_order.len() > WAKES_KEEP {
                if let Some(old) = self.wake_order.pop_front() {
                    self.wakes.remove(&old);
                    self.sweep_run.remove(&old);
                    self.sweep_counted.remove(&old);
                }
            }
        }
    }

    fn inherit_wake(&mut self, id: &EventId, cause: &EventId) {
        let (wake, _) = self.wake_of(cause);
        self.set_wake(id, wake, false);
        if let Some(run) = self.sweep_run.get(cause).cloned() {
            self.sweep_run.insert(id.clone(), run);
        }
    }

    /// An event the memory forgot is a signal that began no run of its own.
    fn wake_of(&self, id: &EventId) -> (Wake, bool) {
        self.wakes.get(id).map(|&(w, root)| (w, root)).unwrap_or((Wake::Signal, false))
    }

    /// Hand `kind` to the handlers of `stream` now, without shipping it: the way a process
    /// re-arms its memory-only timers each time it starts.
    pub fn start(&mut self, stream: &str, kind: EventKind) {
        self.boot_tags.insert(stream.into(), kind.tag.clone());
        let ev = self.local_event(&StreamKey(stream.into()), kind, self.clock.now_ms(), None);
        self.set_wake(&ev.id, Wake::Sweep, true);
        let id = format!("{stream}/{}", ev.kind.tag);
        self.tick_of.insert(ev.id.clone(), ScheduleRow { id, at: ev.at, stream: ev.stream.clone(), kind: ev.kind.clone(), cause: None, correlation: ev.correlation.clone() });
        self.queue.push_back((ev, true));
    }

    /// The engine's clock now.
    pub fn now_ms(&self) -> u64 {
        self.clock.now_ms()
    }

    /// [`start`](Self::start), but handed over at `at` rather than now: a memory-only timer, so
    /// a process that boots many tasks can spread their first ticks instead of firing them all
    /// at one instant. A task's own timer for the same tick replaces it, as any re-arm does.
    pub fn start_at(&mut self, stream: &str, kind: EventKind, at: u64) {
        if at <= self.clock.now_ms() {
            return self.start(stream, kind);
        }
        self.boot_tags.insert(stream.into(), kind.tag.clone());
        let id = format!("{stream}/{}", kind.tag);
        let correlation = CorrelationId(derive_id(format!("start/{id}/{at}")));
        self.timers.insert(id.clone(), ScheduleRow { id, at, stream: StreamKey(stream.into()), kind, cause: None, correlation });
    }

    /// Ship `ev` and queue it for its handlers.
    fn enqueue(&mut self, ev: Event) {
        if !self.queued_ids.insert(ev.id.clone()) {
            return;
        }
        self.journal.record(&ev);
        self.queue.push_back((ev, false));
    }

    fn enqueue_local(&mut self, ev: Event) {
        self.queued_ids.insert(ev.id.clone());
        self.queue.push_back((ev, true));
    }

    fn append_derived(&mut self, id: String, stream: &StreamKey, kind: EventKind, cause: &Event, at: u64) -> Result<Event> {
        let ev = Event { id: EventId(id), stream: stream.clone(), kind, at, cause: Some(cause.id.clone()), correlation: cause.correlation.clone() };
        self.inherit_wake(&ev.id, &cause.id);
        self.enqueue(ev.clone());
        Ok(ev)
    }

    /// Run a handler's commands for `ev`. What lands on `ev`'s own stream is also recorded in
    /// `written`, so the projection holds it before `ev`'s stream is dispatched again.
    fn apply(&mut self, cmds: Vec<Command>, ev: &Event, reg: &Registration, admitted: &mut usize) -> Result<()> {
        let (handler, mode) = (reg.name.as_str(), reg.mode);
        let own = ev.stream.0 != HALT_STREAM;
        for (i, cmd) in cmds.into_iter().enumerate() {
            let id = derive_id(format!("{}/{handler}/{i}", ev.id.0));
            *admitted += 1;
            match cmd {
                Command::Emit(kind) => {
                    self.carry(handler, &ev.stream, &kind);
                    let e = self.append_derived(id, &ev.stream, kind, ev, ev.at)?;
                    if own {
                        self.carried(handler, &e);
                    }
                    self.wrote(own, e, false);
                }
                Command::EmitTo { stream, kind } => {
                    let id = derive_id(format!("{}/{handler}/{i}>{}", ev.id.0, stream.0));
                    let e = if mode == Mode::Act {
                        let e = self.append_derived(id, &stream, kind, ev, ev.at)?;
                        self.set_wake(&e.id, Wake::Signal, true);
                        self.found_work(ev, handler);
                        e
                    } else {
                        let body = serde_json::to_string(&ShadowedEmit { stream, kind }).map_err(|e| RuntimeError::new(e.to_string()))?;
                        self.append_derived(id, &ev.stream, EventKind::new(EMIT_SHADOWED, &body), ev, ev.at)?
                    };
                    let mine = own && e.stream == ev.stream;
                    self.wrote(mine, e, false);
                }
                Command::Effect(spec) if mode == Mode::Act && self.halted && reg.plane_of(&spec.name) == Plane::Work => {
                    let body = serde_json::to_string(&HeldBody { stream: ev.stream.clone(), task: handler.into(), spec }).map_err(|e| RuntimeError::new(e.to_string()))?;
                    let held = self.append_derived(id, &StreamKey(HALT_STREAM.into()), EventKind::new(EFFECT_HELD, &body), ev, ev.at)?;
                    *admitted -= 1;
                    if !self.held.iter().any(|h| h.id == held.id) {
                        self.held.push(held);
                        self.dirty_halt = true;
                    }
                }
                Command::Effect(spec) if mode == Mode::Act => {
                    match self.request(handler, &ev.stream, id, spec, ev, ev.at)? {
                        Some(req) => {
                            self.found_work(ev, handler);
                            self.wrote(own, req, false)
                        }
                        None => *admitted -= 1,
                    }
                }
                Command::Effect(spec) => {
                    let deadline_at = ev.at.saturating_add(spec.deadline_ms);
                    let body = serde_json::to_string(&RequestBody { spec, deadline_at, task: Some(handler.into()) }).map_err(|e| RuntimeError::new(e.to_string()))?;
                    let req = self.append_derived(id, &ev.stream, EventKind::new(EFFECT_SHADOWED, &body), ev, ev.at)?;
                    self.found_work(ev, handler);
                    self.wrote(own, req, false);
                }
                Command::Schedule { at, kind } => {
                    let row = ScheduleRow { id, at, stream: ev.stream.clone(), kind, cause: Some(ev.id.clone()), correlation: ev.correlation.clone() };
                    self.turn.schedules.retain(|r| r.id != row.id);
                    self.turn.schedules.push(row.clone());
                    self.scheduler.insert(row);
                    self.found_work(ev, handler);
                }
                Command::Note(kind) => {
                    self.carry(handler, &ev.stream, &kind);
                    let note = self.local_event(&ev.stream, kind, ev.at, Some(ev));
                    self.enqueue_local(note.clone());
                    self.wrote(own, note, true);
                }
                Command::Probe(spec) => {
                    let deadline_at = ev.at.saturating_add(spec.deadline_ms);
                    let body = serde_json::to_string(&RequestBody { spec, deadline_at, task: Some(handler.into()) }).map_err(|e| RuntimeError::new(e.to_string()))?;
                    let req = self.local_event(&ev.stream, EventKind::new(EFFECT_REQUESTED, &body), ev.at, Some(ev));
                    self.projection.push_local(&req, LOCAL_KEEP, |id| self.pending.contains_key(&id.0) || self.queue.iter().any(|(q, _)| q.cause.as_ref() == Some(id)));
                    self.memory_only.insert(req.id.0.clone());
                    self.owners.insert(req.id.0.clone(), handler.into());
                    self.requesters.insert(req.id.0.clone(), handler.into());
                    self.track_effect(&req)?;
                }
                Command::Timer { at, kind } => {
                    if self.sweep.is_some() {
                        if let Some(t) = self.ticking.get_mut(handler).filter(|t| t.stream.as_ref() == Some(&ev.stream) && t.tag == kind.tag) {
                            t.body = kind.body.clone();
                            if !t.wake.polls {
                                continue;
                            }
                        }
                    }
                    let id = format!("{}/{}", ev.stream.0, kind.tag);
                    self.timers.insert(id.clone(), ScheduleRow { id, at, stream: ev.stream.clone(), kind, cause: Some(ev.id.clone()), correlation: ev.correlation.clone() });
                }
                Command::Due { at } => {
                    if self.sweep.is_some() {
                        if let Some(t) = self.ticking.get_mut(handler).filter(|t| t.stream.as_ref() == Some(&ev.stream)) {
                            let at = at.max(self.clock.now_ms().saturating_add(1));
                            t.due_at = Some(t.due_at.map_or(at, |d| d.min(at)));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Counts the run `ev` belongs to once, when the engine's sweep began it, no change woke its row since, and it has now admitted an act.
    fn found_work(&mut self, ev: &Event, handler: &str) {
        if let Some(run) = self.sweep_run.get(&ev.id).cloned() {
            if self.ticking.get(handler).is_some_and(|t| t.sweep_run.as_ref() == Some(&run)) && self.sweep_counted.insert(run) {
                let rev = self.ticking.get(handler).and_then(|t| t.sweep_at_rev).or_else(|| self.sweep.as_ref().and_then(|w| w.changes.rev())).unwrap_or(0);
                if let Some(t) = self.ticking.get_mut(handler) {
                    t.held = Some(Held { ev: ev.clone(), rev, from: t.sweep_from });
                }
            }
        }
    }

    /// A run found work on the sweep alone: a wide record for the metric, an event for the page.
    fn report_sweep_found(&mut self, handler: &str) {
        let Some(held) = self.ticking.get_mut(handler).and_then(|t| t.held.take()) else {
            return;
        };
        let Held { ev, rev, from } = held;
        let ev = &ev;
        let count = {
            let t = self.tracks.entry(handler.into()).or_default();
            t.sweep_found += 1;
            t.sweep_found
        };
        let lost = from.is_some() && self.heard_rev.is_none_or(|h| h < rev);
        let (reads, changed): (Vec<String>, Vec<String>) = self.ticking.get(handler).map(|t| (t.wake.reads.iter().map(|r| r.to_string()).collect(), t.sweep_heard.union(&t.heard).cloned().collect())).unwrap_or_default();
        let wide = self.unit("wake.sweep_found", Some(ev));
        wide.set("task", handler);
        wide.set("wake.sweep_found_total", count);
        wide.set("store.rev", rev);
        wide.set("wake.lost", lost);
        wide.set("wake.reads", reads.join(","));
        self.finish(&wide);
        let body = serde_json::to_string(&SweepFound { task: handler.into(), count, rev, reads, lost, changed }).unwrap_or_default();
        let (stream, at) = (StreamKey(WAKE_AUDIT_STREAM.into()), self.clock.now_ms());
        let audit = self.local_event(&stream, EventKind::new(WAKE_SWEEP_FOUND, &body), at, None);
        self.enqueue_local(audit);
    }

    /// The tick of a task that declared its reads, when `ev` is it: what the engine remembers of the run.
    fn note_tick(&mut self, name: &str, ev: &Event) {
        if self.sweep.is_none() || self.boot_tags.get(&ev.stream.0) != Some(&ev.kind.tag) {
            return;
        }
        let (now, rev) = (self.clock.now_ms(), self.sweep.as_ref().and_then(|w| w.changes.rev()));
        if let Some(t) = self.ticking.get_mut(name) {
            if t.stream.is_none() {
                t.body = ev.kind.body.clone();
            }
            t.stream = Some(ev.stream.clone());
            t.tag = ev.kind.tag.clone();
            (t.last_tick_at, t.last_rev, t.signal_at, t.due_at) = (now, rev, None, None);
            t.heard.clear();
        }
    }

    /// A `store.changed` arms the tick of each task that reads a key it names.
    fn wake_on_change(&mut self, ev: &Event) {
        if self.sweep.is_none() {
            return;
        }
        let Some(change) = StoreChanged::parse(&ev.kind.body) else { return };
        self.heard_rev = self.heard_rev.max(Some(change.rev));
        let now = self.clock.now_ms();
        let mut settled = vec![];
        for (name, t) in self.ticking.iter_mut() {
            let hit = change.reload || change.keys.iter().any(|k| t.wake.reads.iter().any(|p| k.starts_with(p)));
            t.heard.extend(change.keys.iter().map(|k| key_prefix(k)));
            if hit {
                t.sweep_run = None;
            }
            if t.held.as_ref().is_some_and(|h| change.rev > h.rev) {
                if hit {
                    t.held = None;
                } else {
                    settled.push(name.clone());
                }
            }
            if t.stream.is_some() && hit && t.signal_at.is_none() {
                t.signal_at = Some(now.max(t.last_tick_at.saturating_add(t.wake.min_gap_ms)));
            }
        }
        for name in settled {
            self.report_sweep_found(&name);
        }
    }

    /// Queues the tick of `name` as a run begun by `wake`, at `at`.
    fn wake_tick(&mut self, name: &str, wake: Wake, at: u64) {
        let Some(t) = self.ticking.get_mut(name) else { return };
        let Some(stream) = t.stream.clone() else { return };
        let (tag, body) = (t.tag.clone(), t.body.clone());
        (t.signal_at, t.due_at) = (None, None);
        t.sweep_run = None;
        t.sweep_heard = if wake == Wake::Sweep { std::mem::take(&mut t.heard) } else { BTreeSet::new() };
        t.heard.clear();
        t.sweep_from = t.last_rev;
        t.sweep_at_rev = self.sweep.as_ref().and_then(|w| w.changes.rev());
        let id = format!("{}/{tag}", stream.0);
        let correlation = CorrelationId(derive_id(format!("wake/{id}/{at}")));
        let row = ScheduleRow { id, at, stream: stream.clone(), kind: EventKind::new(&tag, &body), cause: None, correlation: correlation.clone() };
        let ev = Event { correlation, ..self.local_event(&stream, row.kind.clone(), at, None) };
        self.set_wake(&ev.id, wake, true);
        if wake == Wake::Sweep {
            self.sweep_run.insert(ev.id.clone(), ev.id.clone());
            if let Some(t) = self.ticking.get_mut(name) {
                t.sweep_run = Some(ev.id.clone());
            }
        }
        self.tick_of.insert(ev.id.clone(), row);
        self.enqueue_local(ev);
    }

    /// Runs the tick of each declared task whose wake or due time has come, then the sweep.
    fn fire_wakes(&mut self, now: u64) -> usize {
        let mut fired = 0;
        let names: Vec<String> = self.ticking.keys().cloned().collect();
        let mut woken = BTreeSet::new();
        if self.sweep.is_some() {
            for name in &names {
                let t = &self.ticking[name];
                let (signal, due) = (t.signal_at.filter(|a| *a <= now), t.due_at.filter(|a| *a <= now));
                if t.stream.is_some() && (signal.is_some() || due.is_some()) {
                    let (wake, at) = if let Some(a) = signal { (Wake::Signal, a) } else { (Wake::Timer, due.unwrap_or(now)) };
                    self.wake_tick(name, wake, at);
                    woken.insert(name.clone());
                    fired += 1;
                }
            }
        }
        let Some(sweep) = self.sweep.as_mut().filter(|w| w.at <= now) else { return fired };
        sweep.at = now.saturating_add(sweep.every_ms);
        let rev = sweep.changes.rev();
        for name in names {
            self.report_sweep_found(&name);
            let t = &self.ticking[&name];
            let moved = rev.is_some() && rev != t.last_rev;
            let carried = self.pending.values().any(|p| p.task == name);
            if !woken.contains(&name) && !t.wake.polls && t.stream.is_some() && t.signal_at.is_none() && t.due_at.is_none() && moved && !carried && now >= t.last_tick_at.saturating_add(t.wake.min_gap_ms) {
                self.wake_tick(&name, Wake::Sweep, now);
                fired += 1;
            }
        }
        fired
    }

    fn wrote(&mut self, own: bool, e: Event, local: bool) {
        if own {
            self.written.push((e, local));
        }
    }

    fn carry(&mut self, handler: &str, stream: &StreamKey, kind: &EventKind) {
        self.tracks.entry(handler.into()).or_default().state = Some(CarriedState { stream: stream.0.clone(), tag: kind.tag.clone(), body_len: kind.body.len(), body: kind.body.clone() });
    }

    /// The newest own-stream emit of each kind is the task's carry: what a restart hands back.
    fn carried(&mut self, task: &str, e: &Event) {
        self.carries.entry(task.into()).or_default().insert((e.stream.0.clone(), e.kind.tag.clone()), e.clone());
        self.dirty_carry.insert(task.into());
    }

    /// Push `ev` into the projection unless it is there already, offer it, then place what its
    /// handlers wrote on its stream right after it: per stream, a handler sees every event its
    /// stream's handlers wrote for an earlier event before it sees a later one, whatever the
    /// journal offsets. Nothing reorders appends; only the view is ordered by cause.
    fn dispatch(&mut self, ev: &Event, local: bool) -> Result<()> {
        self.seq += 1;
        if !self.early.remove(&ev.id) {
            if local {
                self.push_local(ev);
            } else {
                self.projection.push(ev);
            }
        }
        if let Some(row) = self.fire_of.remove(&ev.id) {
            self.turn.fired.push(row);
        }
        if let Some(request) = self.resolves.remove(&ev.id) {
            self.turn.resolved.push(request);
        }
        if !local && ev.stream.0 == HALT_STREAM {
            self.gate(ev)?;
        }
        if ev.kind.tag == STORE_CHANGED {
            self.wake_on_change(ev);
        }
        let offered = if self.is_poisoned(&ev.stream)? {
            self.park(ev);
            Ok(())
        } else {
            let offered = self.offer(ev);
            if offered.is_ok() {
                if self.poison_seen.get(&ev.stream) == Some(&true) {
                    self.park(ev);
                } else {
                    self.tick_of.remove(&ev.id);
                }
            }
            offered
        };
        self.place();
        offered
    }

    /// A tick on a quarantined stream is re-armed for the next poison recheck, not dropped.
    fn park(&mut self, ev: &Event) {
        if let Some(mut row) = self.tick_of.remove(&ev.id) {
            row.at = self.clock.now_ms() + POISON_RECHECK_MS;
            self.timers.entry(row.id.clone()).or_insert(row);
        }
    }

    fn push_local(&mut self, ev: &Event) {
        let (pending, queue) = (&self.pending, &self.queue);
        self.projection.push_local(ev, LOCAL_KEEP, |id| ev.cause.as_ref() == Some(id) || pending.contains_key(&id.0) || queue.iter().any(|(q, _)| q.cause.as_ref() == Some(id)));
    }

    /// What `ev`'s handlers wrote on its stream, into the projection in the order it was written.
    fn place(&mut self) {
        for (e, local) in std::mem::take(&mut self.written) {
            if !self.early.insert(e.id.clone()) {
                continue;
            }
            if local {
                self.push_local(&e);
            } else {
                self.projection.push(&e);
            }
        }
    }

    /// The task that requested the effect `ev` is the outcome of, when it is one and the
    /// request named its task.
    fn requester(&mut self, ev: &Event) -> Option<String> {
        if !is_outcome(&ev.kind.tag) {
            return None;
        }
        let cause = ev.cause.as_ref()?;
        self.requesters.remove(&cause.0)
    }

    /// Offer `ev` to every handler interested in it, an outcome only to the task that asked
    /// for it (and those that watch every outcome).
    fn offer(&mut self, ev: &Event) -> Result<()> {
        let regs = std::mem::take(&mut self.regs);
        let r = self.offer_to(&regs, ev);
        self.regs = regs;
        r
    }

    fn offer_to(&mut self, regs: &[Registration], ev: &Event) -> Result<()> {
        let requester = self.requester(ev);
        for reg in regs {
            let (name, mode, handler) = (reg.name.as_str(), reg.mode, &reg.handler);
            if !handler.interests().contains(&ev.kind.tag.as_str()) {
                continue;
            }
            if requester.as_deref().is_some_and(|r| r != name) && !handler.watches_foreign_outcomes() {
                continue;
            }
            let stalls = if self.suspects.is_empty() { 0 } else { self.suspects.get(&(ev.id.clone(), name.to_string())).copied().unwrap_or(0) };
            if stalls >= self.stall_limit {
                let message = format!("stalled {stalls} times in {name}");
                self.quarantine(ev, name, &message)?;
                let wide = self.dispatch_unit(ev, name, mode, stalls);
                wide.set("outcome", "quarantined");
                self.finish(&wide);
                return Ok(());
            }
            let step = self.steps.enter("marker.set", name);
            self.marker.set(&Mark { event: ev.id.clone(), handler: name.to_string(), stream: ev.stream.clone(), stalls })?;
            drop(step);
            let now = self.clock.now_ms();
            self.note_tick(name, ev);
            let (wake, began_run) = self.wake_of(&ev.id);
            let t = self.tracks.entry(name.to_string()).or_default();
            (t.fired_at, t.due_at) = (Some(now), Some(ev.at));
            if began_run {
                *t.runs_by_wake.entry(wake.as_str().into()).or_default() += 1;
            }
            self.busy.enter();
            let ctx = Ctx::with(ev.at, mix(self.seed, &[&ev.id.0, name]), self.seq, Wide::default());
            let began = Instant::now();
            let view = &self.projection;
            let step = self.steps.enter("handler", name);
            let called = catch_unwind(AssertUnwindSafe(|| handler.handle(&ctx, view, ev)));
            drop(step);
            self.busy.exit();
            let step = self.steps.enter("marker.clear", name);
            self.marker.clear()?;
            drop(step);
            let elapsed = began.elapsed();
            let set = ctx.into_event();
            if stalls == 0 && set.is_empty() && matches!(&called, Ok(c) if c.is_empty()) {
                let kept = matches!(ev.kind.tag.as_str(), EFFECT_FAILED | EFFECT_TIMED_OUT);
                if let Some(t) = self.tracks.get_mut(name) {
                    if !kept {
                        t.outcome = Some(LastOutcome::Quiet);
                    }
                    t.commands = Some(FireCommands { returned: 0, admitted: 0 });
                }
                continue;
            }
            let wide = self.dispatch_unit(ev, name, mode, stalls);
            wide.merge(set.take());
            wide.set("duration_us", elapsed.as_micros() as u64);
            match called {
                Ok(cmds) => {
                    let count = |f: fn(&Command) -> bool| cmds.iter().filter(|c| f(c)).count();
                    wide.set("commands.emit", count(|c| matches!(c, Command::Emit(_))));
                    wide.set("commands.emit_to", count(|c| matches!(c, Command::EmitTo { .. })));
                    wide.set("commands.effect", count(|c| matches!(c, Command::Effect(_))));
                    wide.set("commands.schedule", count(|c| matches!(c, Command::Schedule { .. })));
                    let returned = cmds.len();
                    let mut admitted = 0;
                    let done = self.apply(cmds, ev, reg, &mut admitted);
                    if let Some(t) = self.tracks.get_mut(name) {
                        t.commands = Some(FireCommands { returned, admitted });
                    }
                    if matches!(&done, Err(e) if e.is_refused()) {
                        let message = done.unwrap_err().0;
                        wide.set("outcome", "refused");
                        self.finish(&wide);
                        self.quarantine(ev, name, &message)?;
                        return Ok(());
                    }
                    wide.set("outcome", if done.is_ok() { "ok" } else { "error" });
                    self.finish(&wide);
                    done?;
                    if stalls > 0 {
                        self.suspects.remove(&(ev.id.clone(), name.to_string()));
                    }
                }
                Err(p) => {
                    let message = panic_message(&*p);
                    wide.set("outcome", "panic");
                    wide.set("panic.message", message.as_str());
                    self.finish(&wide);
                    self.quarantine(ev, name, &message)?;
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// The wide event of one handler call that did something.
    fn dispatch_unit(&self, ev: &Event, name: &str, mode: Mode, stalls: u32) -> Wide {
        let wide = self.unit("dispatch", Some(ev));
        wide.set("handler", name);
        wide.set("task.mode", mode.as_str());
        if stalls > 0 {
            wide.set("stall.count", stalls);
        }
        wide
    }

    fn is_poisoned(&mut self, stream: &StreamKey) -> Result<bool> {
        if let Some(p) = self.poison_seen.get(stream) {
            return Ok(*p);
        }
        let step = self.steps.enter("journal.poisoned", &stream.0);
        let found = self.journal.poisoned(stream);
        drop(step);
        let p = match found? {
            None => {
                self.quarantined_here.remove(stream);
                false
            }
            Some(row) if row.handler == SCHEDULER_TASK && !self.quarantined_here.contains(stream) => {
                if self.set_aside.insert(row.event.clone()) {
                    let w = self.unit("poison.set_aside", None);
                    w.set("stream", stream.0.as_str());
                    w.set("poison.event", row.event.0.as_str());
                    w.set("poison.message", row.message.as_str());
                    self.finish(&w);
                }
                false
            }
            Some(_) => true,
        };
        self.poison_seen.insert(stream.clone(), p);
        Ok(p)
    }

    fn quarantine(&mut self, ev: &Event, handler: &str, message: &str) -> Result<()> {
        let step = self.steps.enter("journal.poison", &ev.stream.0);
        self.journal.poison(&Poison { stream: ev.stream.clone(), event: ev.id.clone(), handler: handler.into(), message: message.into() })?;
        drop(step);
        self.poison_seen.insert(ev.stream.clone(), true);
        self.quarantined_here.insert(ev.stream.clone());
        let w = self.unit("quarantine", Some(ev));
        w.set("handler", handler);
        w.set("quarantine.reason", message);
        w.set("outcome", "quarantined");
        self.finish(&w);
        Ok(())
    }

    /// Dispatch every queued event, committing the store once after each: the number dispatched.
    /// A turn that could not commit is sent again before the next event is taken.
    pub fn pump(&mut self) -> Result<usize> {
        self.pump_until(None)
    }

    /// One tick, then events until the queue is empty or the engine clock passes `budget_ms` from
    /// now, checked after each committed event. True when events are still queued.
    pub fn run_slice(&mut self, budget_ms: u64) -> Result<bool> {
        self.rss.reset();
        let end = self.clock.now_ms().saturating_add(budget_ms);
        let run = self.tick().and_then(|_| self.pump_until(Some(end)));
        self.publish();
        run?;
        Ok(!self.queue.is_empty())
    }

    fn pump_until(&mut self, end: Option<u64>) -> Result<usize> {
        self.commit_turn()?;
        self.answer_committed(Ok(()));
        let mut n = 0;
        let now = self.clock.now_ms();
        if now.saturating_sub(self.poison_cleared_at) >= POISON_RECHECK_MS {
            self.poison_seen.retain(|_, p| !*p);
            self.poison_cleared_at = now;
        }
        while let Some((ev, local)) = self.queue.pop_front() {
            if let Err(e) = self.dispatch(&ev, local) {
                self.queue.push_front((ev, local));
                return Err(e);
            }
            self.queued_ids.remove(&ev.id);
            n += 1;
            if self.replies.contains_key(&ev.id.0) {
                self.uncommitted.push(ev.id.0.clone());
            }
            match self.commit_turn() {
                Err(e) if e.is_refused() => {
                    self.answer_committed(Err(e.0.clone()));
                    self.discard_turn(&ev, &e.0)?
                }
                other => {
                    other?;
                    self.answer_committed(Ok(()));
                }
            }
            if end.is_some_and(|e| self.clock.now_ms() >= e) {
                break;
            }
        }
        Ok(n)
    }

    /// The changed state keys, staged into the turn: each task's carry and the scheduler's halt.
    fn stage_state(&mut self) {
        let at = self.clock.now_ms();
        let mut changed: Vec<(String, String, String, String)> = vec![];
        let mut dropped: Vec<(String, String)> = vec![];
        for task in std::mem::take(&mut self.dirty_carry) {
            if let Some(codec) = self.codecs.get(&task).copied() {
                let body = std::iter::once(codec.stream).chain(codec.also.iter().copied()).filter_map(|s| self.carries.get(&task)?.get(&(s.to_string(), codec.tag.to_string()))).max_by_key(|e| e.at).map(|e| e.kind.body.clone()).unwrap_or_default();
                let mut rows = (codec.split)(&body);
                let held = self.kept.iter().filter(|(_, sig)| !sig.is_empty() && *sig != LEGACY_SPENT).map(|(key, _)| key);
                dropped.extend(held.filter(|(t, k)| t == &task && k != CARRY_KEY && !rows.contains_key(k)).cloned());
                if self.kept.get(&(task.clone(), CARRY_KEY.into())).is_some_and(|s| s != LEGACY_SPENT) {
                    rows.insert(CARRY_KEY.into(), LEGACY_SPENT.into());
                }
                changed.extend(rows.into_iter().map(|(k, v)| (task.clone(), k, v.clone(), v)));
                continue;
            }
            let events: Vec<&Event> = self.carries.get(&task).map(|c| c.values().collect()).unwrap_or_default();
            let sig = carry_signature(events.iter().copied());
            changed.push((task, CARRY_KEY.into(), serde_json::to_string(&events).unwrap_or_default(), sig));
        }
        if std::mem::take(&mut self.dirty_halt) {
            let halted = if self.halted { "1" } else { "0" }.to_string();
            let held = serde_json::to_string(&self.held).unwrap_or_default();
            changed.push((SCHEDULER_TASK.into(), HALTED_KEY.into(), halted.clone(), halted));
            changed.push((SCHEDULER_TASK.into(), HELD_KEY.into(), held.clone(), held));
        }
        for key in dropped {
            self.turn.state.retain(|r| !(r.task == key.0 && r.skey == key.1));
            self.staged.remove(&key);
            if !self.turn.cleared.contains(&key) {
                self.turn.cleared.push(key);
            }
        }
        for (task, skey, value, sig) in changed {
            let key = (task.clone(), skey.clone());
            self.turn.cleared.retain(|c| c != &key);
            self.turn.state.retain(|r| !(r.task == task && r.skey == skey));
            self.staged.remove(&key);
            if self.kept.get(&key) != Some(&sig) {
                self.turn.state.push(StateRow { task, skey, value, at });
                self.staged.insert(key, sig);
            }
        }
    }

    /// Answers `reply` once the turn handling submission `id` has committed, or refused: a
    /// submission already past its turn is answered now.
    pub fn answer_on_commit(&mut self, id: &str, reply: Reply) {
        if self.queued_ids.contains(&EventId(id.into())) || self.uncommitted.iter().any(|u| u == id) {
            self.replies.insert(id.into(), reply);
        } else {
            let _ = reply.send(Ok(()));
        }
    }

    fn answer_committed(&mut self, result: std::result::Result<(), String>) {
        for id in std::mem::take(&mut self.uncommitted) {
            if let Some(reply) = self.replies.remove(&id) {
                let _ = reply.send(result.clone());
            }
        }
    }

    fn commit_turn(&mut self) -> Result<()> {
        self.stage_state();
        if self.turn.is_empty() {
            return Ok(());
        }
        let step = self.steps.enter("journal.commit", "");
        self.journal.commit(&self.turn)?;
        drop(step);
        let turn = std::mem::take(&mut self.turn);
        self.committed.extend(turn.effects.iter().map(|e| e.request_id.clone()));
        for key in turn.cleared {
            self.kept.remove(&key);
            self.staged.remove(&key);
        }
        for s in turn.state {
            let key = (s.task, s.skey);
            if let Some(sig) = self.staged.remove(&key) {
                self.kept.insert(key, sig);
            }
        }
        Ok(())
    }

    /// The store refused the turn: its effects are never requested, and the stream that made it is quarantined.
    fn discard_turn(&mut self, ev: &Event, message: &str) -> Result<()> {
        self.staged.clear();
        for e in std::mem::take(&mut self.turn).effects {
            self.pending.remove(&e.request_id);
            self.owners.remove(&e.request_id);
            self.requesters.remove(&e.request_id);
        }
        self.quarantine(ev, "scheduler", message)
    }

    fn fire(&mut self, row: &ScheduleRow) {
        let ev = Event {
            id: EventId(derive_id(format!("sched/{}", row.id))),
            stream: row.stream.clone(),
            kind: row.kind.clone(),
            at: row.at,
            cause: row.cause.clone(),
            correlation: row.correlation.clone(),
        };
        self.fire_of.insert(ev.id.clone(), row.id.clone());
        self.set_wake(&ev.id, Wake::Timer, true);
        self.enqueue(ev);
    }

    /// Fire the schedule rows now due and time out the effects past their deadline.
    pub fn tick(&mut self) -> Result<usize> {
        let mut n = 0;
        for row in self.scheduler.pop_due(self.clock.as_ref()) {
            self.fire(&row);
            n += 1;
        }
        let now = self.clock.now_ms();
        let mut due: Vec<_> = self.timers.values().filter(|r| r.at <= now).map(|r| (r.at, r.id.clone())).collect();
        due.sort();
        for (_, id) in due {
            if let Some(row) = self.timers.remove(&id) {
                if let Some(t) = self.ticking.values_mut().find(|t| t.stream.as_ref() == Some(&row.stream) && t.tag == row.kind.tag) {
                    (t.signal_at, t.due_at) = (None, None);
                    t.sweep_run = None;
                }
                let ev = Event { cause: row.cause.clone(), correlation: row.correlation.clone(), ..self.local_event(&row.stream, row.kind.clone(), row.at, None) };
                let wake = if self.boot_tags.get(&row.stream.0) == Some(&row.kind.tag) { Wake::Sweep } else { Wake::Timer };
                self.set_wake(&ev.id, wake, true);
                self.tick_of.insert(ev.id.clone(), row);
                self.enqueue_local(ev);
                n += 1;
            }
        }
        n += self.fire_wakes(now);
        let mut late: Vec<_> = self.running_deadlines().filter(|(d, _)| *d <= now).map(|(d, k)| (d, k.clone())).collect();
        late.sort();
        for (_, key) in late {
            if self.complete(&key, Outcome::TimedOut)? {
                n += 1;
            }
        }
        Ok(n)
    }
}

impl Executor for Engine {
    fn submit(&mut self, id: &str, stream: &str, kind: EventKind) -> Result<()> {
        if !self.submitted_ids.insert(id.into()) {
            return Ok(());
        }
        self.submitted.push_back(id.into());
        if self.submitted.len() > SUBMITTED_KEEP {
            if let Some(old) = self.submitted.pop_front() {
                self.submitted_ids.remove(&old);
            }
        }
        let ev = Event {
            id: EventId(id.into()),
            stream: StreamKey(stream.into()),
            kind,
            at: self.clock.now_ms(),
            cause: None,
            correlation: CorrelationId(id.into()),
        };
        self.set_wake(&ev.id, Wake::Signal, true);
        if memory_only_tag(&ev.kind.tag) {
            self.enqueue_local(ev);
        } else {
            self.enqueue(ev);
        }
        Ok(())
    }

    fn run_until_idle(&mut self) -> Result<()> {
        self.rss.reset();
        let run = (|| loop {
            let fired = self.tick()?;
            let dispatched = self.pump()?;
            if (fired == 0 && dispatched == 0) || self.next_timer_at().is_none_or(|t| t > self.clock.now_ms()) {
                return Ok(());
            }
        })();
        self.publish();
        run
    }

    fn pending_effects(&self) -> Vec<PendingEffect> {
        self.pending.values().filter(|p| self.committed.contains(&p.key) || self.memory_only.contains(&p.key)).cloned().collect()
    }

    fn complete(&mut self, key: &str, outcome: Outcome) -> Result<bool> {
        let Some(p) = self.pending.get(key).cloned() else { return Ok(false) };
        let ev = Event {
            id: EventId(derive_id(format!("{key}/outcome"))),
            stream: p.stream.clone(),
            kind: EventKind::new(outcome.tag(), &outcome.detail()),
            at: self.clock.now_ms(),
            cause: Some(EventId(key.into())),
            correlation: p.correlation.clone(),
        };
        self.inherit_wake(&ev.id, &EventId(key.into()));
        if self.memory_only.remove(key) {
            self.enqueue_local(ev.clone());
        } else {
            self.resolves.insert(ev.id.clone(), key.into());
            self.enqueue(ev.clone());
        }
        self.pending.remove(key);
        self.committed.remove(key);
        self.queued.remove(key);
        if let Some(owner) = self.owners.remove(key) {
            self.tracks.entry(owner).or_default().outcome = Some(match outcome {
                Outcome::Succeeded(_) => LastOutcome::Succeeded,
                Outcome::Failed(_) => LastOutcome::Failed,
                Outcome::TimedOut | Outcome::Lost => LastOutcome::TimedOut,
            });
        }
        let w = self.unit("effect", Some(&ev));
        w.set("effect.kind", p.spec.name.as_str());
        w.set("effect.idempotency_key", key);
        w.set("effect.deadline_at", p.deadline_at);
        w.set("effect.outcome", ev.kind.tag.trim_start_matches("effect."));
        if matches!(outcome, Outcome::Failed(_) | Outcome::Lost) {
            w.set("effect.detail", clip(&outcome.detail(), EFFECT_DETAIL_MAX));
        }
        w.set("effect.attempt", self.attempts.remove(key).unwrap_or(1));
        let children = self.children.remove(key).unwrap_or_default();
        if !children.is_empty() {
            w.set("task", p.task.as_str());
        }
        for (program, n) in children {
            w.set(&format!("effect.children.{program}"), n);
        }
        w.set("duration_ms", ev.at.saturating_sub(p.requested_at));
        self.finish(&w);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{Boot, MemJournal, Poison, Turn};
    use crate::ports::VirtualClock;

    struct SlowToAnswer {
        clock: VirtualClock,
        ask_ms: u64,
        asked: std::sync::atomic::AtomicU32,
    }

    impl Journal for SlowToAnswer {
        fn record(&self, _: &Event) {}
        fn boot(&self) -> Result<Boot> {
            self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.clock.advance(self.ask_ms);
            Err(RuntimeError::cannot_tell("journal open-effects: no reply from the store"))
        }
        fn commit(&self, t: &Turn) -> Result<()> {
            MemJournal::new().commit(t)
        }
        fn poison(&self, _: &Poison) -> Result<()> {
            Ok(())
        }
        fn poisoned(&self, _: &StreamKey) -> Result<Option<Poison>> {
            Ok(None)
        }
    }

    #[test_id(tjh7mzx)]
    #[test]
    fn the_time_each_ask_takes_counts_toward_the_boot_bound() {
        let clock = VirtualClock::new(0);
        let j = Arc::new(SlowToAnswer { clock: clock.clone(), ask_ms: 5_000, asked: Default::default() });
        let sleeper = clock.clone();
        let retry = BootRetry { first_ms: 250, ceiling_ms: 1_000, bound_ms: 10_000, store: "the store".into(), sleep: Arc::new(move |ms| sleeper.advance(ms)) };
        let err = Engine::open_with(j.clone(), Arc::new(clock.clone()), vec![], 0, retry).err().expect("must fail");
        assert!(err.0.contains("journal open-effects: no reply from the store"), "{err}");
        assert!(err.0.contains("2 attempt(s)"), "{err}");
        assert_eq!(j.asked.load(std::sync::atomic::Ordering::SeqCst), 2, "no ask begins once 10 s are spent");
    }

    struct Waker(std::sync::atomic::AtomicBool);

    impl Handler for Waker {
        fn interests(&self) -> &[EventKindTag] {
            &["boot", "other", "sched", "ask", EFFECT_SUCCEEDED]
        }
        fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
            match ev.kind.tag.as_str() {
                "boot" if !self.0.swap(true, std::sync::atomic::Ordering::SeqCst) => vec![
                    Command::Timer { at: ev.at + 1_000, kind: EventKind::new("boot", "") },
                    Command::Timer { at: ev.at + 500, kind: EventKind::new("other", "") },
                    Command::Schedule { at: ev.at + 300, kind: EventKind::new("sched", "") },
                    Command::Probe(EffectSpec::probe("look", "", 10_000)),
                ],
                "boot" => vec![Command::Timer { at: ev.at + 1_000, kind: EventKind::new("boot", "") }],
                "ask" | "other" | "sched" | "effect.succeeded" => vec![Command::Emit(EventKind::new("answered", ""))],
                _ => vec![],
            }
        }
    }

    fn wake_of_dispatch(sink: &crate::wide::MemSink, kind: &str) -> Vec<String> {
        let want: Value = kind.into();
        sink.records().iter().filter(|r| r.get("unit") == Some(&"dispatch".into()) && r.get("event.kind") == Some(&want)).filter_map(|r| match r.get("wake") { Some(Value::Str(w)) => Some(w.clone()), _ => None }).collect()
    }

    #[test_id(tf3lykx)]
    #[test]
    fn every_dispatch_names_what_woke_it_and_the_status_counts_runs_by_wake() {
        let clock = VirtualClock::new(0);
        let sink = crate::wide::MemSink::new();
        let regs = vec![Registration::new("waker", Waker(Default::default()))];
        let mut e = Engine::open(MemJournal::new(), Arc::new(clock.clone()), regs, 1).unwrap().with_sink(sink.clone());
        e.start("s", EventKind::new("boot", ""));
        e.run_until_idle().unwrap();
        let probe = e.pending_effects().into_iter().find(|p| p.spec.name == "look").expect("the boot tick asked a probe").key;
        assert!(e.complete(&probe, Outcome::Succeeded(String::new())).unwrap());
        e.run_until_idle().unwrap();
        clock.advance(300);
        e.run_until_idle().unwrap();
        clock.advance(200);
        e.run_until_idle().unwrap();
        e.submit("ask-1", "s", EventKind::new("ask", "")).unwrap();
        e.run_until_idle().unwrap();

        let sweep = |k| wake_of_dispatch(&sink, k);
        assert_eq!(sweep("boot"), vec!["sweep"]);
        assert_eq!(sweep(EFFECT_SUCCEEDED), vec!["sweep"], "an answer carries the wake of the run that asked");
        assert_eq!(sweep("other"), vec!["timer"]);
        assert_eq!(sweep("sched"), vec!["timer"]);
        assert_eq!(sweep("ask"), vec!["signal"]);
        let runs = |e: &Engine| e.status().tasks[0].runs_by_wake.clone();
        assert_eq!(runs(&e), BTreeMap::from([("sweep".into(), 1), ("timer".into(), 2), ("signal".into(), 1)]));

        clock.advance(500);
        e.run_until_idle().unwrap();
        assert_eq!(sweep("boot"), vec!["sweep", "sweep"], "the re-armed boot tick is a sweep");
        assert_eq!(runs(&e)["sweep"], 2);
    }

    struct Asker;

    impl Handler for Asker {
        fn interests(&self) -> &[EventKindTag] {
            &["boot"]
        }
        fn handle(&self, _: &Ctx, _: &Projection, _: &Event) -> Vec<Command> {
            ["a", "b", "c", "d"].iter().map(|n| Command::Probe(EffectSpec::probe(*n, "", 10_000))).collect()
        }
    }

    #[test_id(t7dho38)]
    #[test]
    fn a_failed_effect_names_why_on_its_wide_event() {
        let sink = crate::wide::MemSink::new();
        let regs = vec![Registration::new("asker", Asker)];
        let mut e = Engine::open(MemJournal::new(), Arc::new(VirtualClock::new(0)), regs, 1).unwrap().with_sink(sink.clone());
        e.start("s", EventKind::new("boot", ""));
        e.run_until_idle().unwrap();
        let key = |e: &Engine, n: &str| e.pending_effects().into_iter().find(|p| p.spec.name == n).expect("asked").key;
        let why = "AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY are not in the environment";
        let long = format!("a{}", "é".repeat(2_500));
        assert!(e.complete(&key(&e, "a"), Outcome::Failed(why.into())).unwrap());
        assert!(e.complete(&key(&e, "b"), Outcome::Failed(long.clone())).unwrap());
        assert!(e.complete(&key(&e, "c"), Outcome::Succeeded("fine".into())).unwrap());
        assert!(e.complete(&key(&e, "d"), Outcome::Lost).unwrap());
        let details: Vec<Option<Value>> = sink
            .records()
            .iter()
            .filter(|r| r.get("unit") == Some(&"effect".into()))
            .map(|r| r.get("effect.detail").cloned())
            .collect();
        assert_eq!(details.len(), 4);
        assert_eq!(details[0], Some(why.into()));
        let Some(Value::Str(clipped)) = &details[1] else { panic!("a long failure keeps a detail") };
        assert_eq!(clipped.len(), EFFECT_DETAIL_MAX - 1, "the odd offset forces the boundary back-off");
        assert!(long.starts_with(clipped.as_str()));
        assert_eq!(details[2], None);
        assert_eq!(details[3], Some(LOST_AT_RESTART.into()));
    }

    /// A tick that re-arms itself every 30 s, and optionally asks a due time and an act.
    struct Poller {
        tag: &'static str,
        act: bool,
        dues: Vec<u64>,
    }

    impl Handler for Poller {
        fn interests(&self) -> &[EventKindTag] {
            std::slice::from_ref(&self.tag)
        }
        fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
            let mut cmds = vec![Command::Timer { at: ev.at + 30_000, kind: EventKind::new(self.tag, "again") }];
            cmds.extend(self.dues.iter().filter(|_| ev.at == 0).map(|at| Command::Due { at: *at }));
            if self.act && ev.at > 0 {
                cmds.push(Command::Effect(EffectSpec::keyed("write", "w", "", 1_000)));
            }
            cmds
        }
    }

    fn woken(name: &'static str, reads: &[&'static str], polls: bool, min_gap_ms: u64, dues: &[u64], act: bool) -> Registration {
        Registration::new(name, Poller { tag: name, act, dues: dues.to_vec() }).with_wake(crate::event::Wake { reads: reads.to_vec(), min_gap_ms, polls })
    }

    fn started(regs: Vec<Registration>, clock: &VirtualClock, changes: &Arc<crate::changes::ScriptedChanges>) -> Engine {
        let streams: Vec<String> = regs.iter().map(|r| r.name.clone()).collect();
        let mut e = Engine::open(MemJournal::new(), Arc::new(clock.clone()), regs, 1).unwrap().with_sweep(60_000, changes.clone());
        for s in &streams {
            e.start(s, EventKind::new(s, ""));
        }
        e.run_until_idle().unwrap();
        e
    }

    fn changed(e: &mut Engine, id: &str, keys: &[&str], reload: bool) {
        changed_at(e, id, 1, keys, reload);
    }

    fn changed_at(e: &mut Engine, id: &str, rev: u64, keys: &[&str], reload: bool) {
        let c = StoreChanged { rev, keys: keys.iter().map(|k| k.to_string()).collect(), reload };
        e.submit(id, STORE_STREAM, c.kind()).unwrap();
    }

    fn runs(e: &Engine, task: &str) -> u64 {
        e.status().tasks.iter().find(|t| t.name == task).unwrap().runs_by_wake.values().sum()
    }

    fn wake_runs(e: &Engine, task: &str, wake: &str) -> u64 {
        e.status().tasks.iter().find(|t| t.name == task).unwrap().runs_by_wake.get(wake).copied().unwrap_or(0)
    }

    fn quiet() -> (VirtualClock, Arc<crate::changes::ScriptedChanges>) {
        let changes = Arc::new(crate::changes::ScriptedChanges::default());
        changes.set(1);
        (VirtualClock::new(0), changes)
    }

    #[test_id(tsk31ni)]
    #[test]
    fn a_write_wakes_exactly_the_tasks_that_declare_it() {
        let (clock, changes) = quiet();
        let regs = vec![woken("a", &["a/"], false, 0, &[], false), woken("b", &["b/"], false, 0, &[], false), woken("c", &["a/x"], false, 0, &[], false)];
        let mut e = started(regs, &clock, &changes);
        changed(&mut e, "c1", &["a/x/1"], false);
        e.run_until_idle().unwrap();
        assert_eq!((wake_runs(&e, "a", "signal"), wake_runs(&e, "b", "signal"), wake_runs(&e, "c", "signal")), (1, 0, 1));
    }

    #[test_id(th6rul1)]
    #[test]
    fn a_lifecycle_write_wakes_a_row_that_reads_only_lifecycle_and_an_audit_key_does_not() {
        let (clock, changes) = quiet();
        let mut e = started(vec![woken("only", &["lifecycle/"], false, 0, &[], false)], &clock, &changes);
        changed(&mut e, "c1", &["events/00000000000000000042", "runtime/state/t/k"], false);
        e.run_until_idle().unwrap();
        assert_eq!(wake_runs(&e, "only", "signal"), 0);
        changed(&mut e, "c2", &["lifecycle/sp-x"], false);
        e.run_until_idle().unwrap();
        assert_eq!(wake_runs(&e, "only", "signal"), 1);
    }

    #[test_id(t71q1wh)]
    #[test]
    fn ten_writes_in_one_turn_wake_a_declaring_task_once() {
        let (clock, changes) = quiet();
        let mut e = started(vec![woken("a", &["a/"], false, 0, &[], false)], &clock, &changes);
        for i in 0..10 {
            changed(&mut e, &format!("c{i}"), &["a/x"], false);
        }
        e.run_until_idle().unwrap();
        assert_eq!(wake_runs(&e, "a", "signal"), 1);
    }

    #[test_id(tsdcvip)]
    #[test]
    fn a_reload_wakes_every_task_that_declared_reads_once() {
        let (clock, changes) = quiet();
        let regs = vec![woken("a", &["a/"], false, 0, &[], false), woken("b", &["b/"], true, 0, &[], false)];
        let mut e = started(regs, &clock, &changes);
        changed(&mut e, "r1", &[], true);
        e.run_until_idle().unwrap();
        assert_eq!((wake_runs(&e, "a", "signal"), wake_runs(&e, "b", "signal")), (1, 1));
    }

    #[test_id(txd3zav)]
    #[test]
    fn a_swallowed_wake_up_is_found_by_the_next_sweep_and_the_one_after_runs_nothing() {
        let (clock, changes) = quiet();
        let mut e = started(vec![woken("a", &["a/"], false, 0, &[], true)], &clock, &changes);
        changes.set(7);
        clock.advance(60_000);
        e.run_until_idle().unwrap();
        assert_eq!(wake_runs(&e, "a", "sweep"), 2, "the boot tick and the sweep's");
        assert_eq!(e.status().tasks[0].sweep_found, 0, "held until a change or the next sweep");
        clock.advance(60_000);
        e.run_until_idle().unwrap();
        assert_eq!(wake_runs(&e, "a", "sweep"), 2);
        assert_eq!(e.status().tasks[0].sweep_found, 1);
        clock.advance(60_000);
        e.run_until_idle().unwrap();
        assert_eq!(e.status().tasks[0].sweep_found, 1);
    }

    fn sweep_found_records(sink: &crate::wide::MemSink) -> Vec<i64> {
        sink.records().iter().filter(|r| r.get("unit") == Some(&"wake.sweep_found".into())).filter_map(|r| match r.get("wake.sweep_found_total") { Some(Value::Int(n)) => Some(*n), _ => None }).collect()
    }

    struct Audit(std::sync::Mutex<Vec<SweepFound>>);

    impl Handler for Audit {
        fn interests(&self) -> &[EventKindTag] {
            &[WAKE_SWEEP_FOUND]
        }
        fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
            assert_eq!(ev.stream.0, WAKE_AUDIT_STREAM);
            self.0.lock().unwrap().push(serde_json::from_str(&ev.kind.body).unwrap());
            vec![]
        }
    }

    struct Shared(Arc<Audit>);

    impl Handler for Shared {
        fn interests(&self) -> &[EventKindTag] {
            self.0.interests()
        }
        fn handle(&self, c: &Ctx, v: &Projection, ev: &Event) -> Vec<Command> {
            self.0.handle(c, v, ev)
        }
    }

    fn audited(clock: &VirtualClock, changes: &Arc<crate::changes::ScriptedChanges>) -> (Engine, Arc<Audit>) {
        let audit = Arc::new(Audit(Default::default()));
        let regs = vec![woken("a", &["a/"], false, 0, &[], true), Registration::new("audit", Shared(audit.clone()))];
        let mut e = Engine::open(MemJournal::new(), Arc::new(clock.clone()), regs, 1).unwrap().with_sweep(60_000, changes.clone());
        e.start("a", EventKind::new("a", ""));
        e.run_until_idle().unwrap();
        (e, audit)
    }

    fn sweep_once(e: &mut Engine, clock: &VirtualClock) {
        clock.advance(60_000);
        e.run_until_idle().unwrap();
    }

    #[test_id(tzh6kfp)]
    #[test]
    fn a_swallowed_wake_up_emits_a_counted_record_and_a_wake_audit_event_each_time() {
        let (clock, changes) = quiet();
        let sink = crate::wide::MemSink::new();
        let audit = Arc::new(Audit(Default::default()));
        let regs = vec![woken("a", &["a/"], false, 0, &[], true), Registration::new("audit", Shared(audit.clone()))];
        let mut e = Engine::open(MemJournal::new(), Arc::new(clock.clone()), regs, 1).unwrap().with_sweep(60_000, changes.clone()).with_sink(sink.clone());
        e.start("a", EventKind::new("a", ""));
        e.run_until_idle().unwrap();
        assert!(sweep_found_records(&sink).is_empty());
        changes.set(7);
        clock.advance(60_000);
        e.run_until_idle().unwrap();
        assert!(sweep_found_records(&sink).is_empty(), "held");
        clock.advance(60_000);
        e.run_until_idle().unwrap();
        assert_eq!(sweep_found_records(&sink), vec![1_i64]);
        let first = audit.0.lock().unwrap().clone();
        assert_eq!(first, vec![SweepFound { task: "a".into(), count: 1, rev: 7, reads: vec!["a/".into()], lost: true, changed: vec![] }]);
        changes.set(9);
        clock.advance(60_000);
        e.run_until_idle().unwrap();
        assert_eq!(sweep_found_records(&sink), vec![1_i64], "the second find is held");
        clock.advance(60_000);
        e.run_until_idle().unwrap();
        assert_eq!(sweep_found_records(&sink), vec![1_i64, 2]);
        assert_eq!(audit.0.lock().unwrap().len(), 2);
    }

    fn lost_flags(sink: &crate::wide::MemSink) -> Vec<bool> {
        sink.records().iter().filter(|r| r.get("unit") == Some(&"wake.sweep_found".into())).filter_map(|r| match r.get("wake.lost") { Some(Value::Bool(b)) => Some(*b), _ => None }).collect()
    }

    fn sweep_finding(boot_rev: Option<u64>, heard: Option<u64>) -> Vec<bool> {
        let (clock, changes) = if boot_rev.is_some() { quiet() } else { (VirtualClock::new(0), Arc::new(crate::changes::ScriptedChanges::default())) };
        let sink = crate::wide::MemSink::new();
        let regs = vec![woken("a", &["a/"], false, 0, &[], true)];
        let mut e = Engine::open(MemJournal::new(), Arc::new(clock.clone()), regs, 1).unwrap().with_sweep(60_000, changes.clone()).with_sink(sink.clone());
        e.start("a", EventKind::new("a", ""));
        e.run_until_idle().unwrap();
        changes.set(if boot_rev.is_none() { 5 } else { 7 });
        if let Some(rev) = heard {
            changed_at(&mut e, "c1", rev, &["z/1"], false);
        }
        clock.advance(60_000);
        e.run_until_idle().unwrap();
        assert!(lost_flags(&sink).is_empty(), "held");
        clock.advance(60_000);
        e.run_until_idle().unwrap();
        lost_flags(&sink)
    }

    #[test_id(tzgigsj)]
    #[test]
    fn a_change_the_source_never_announced_is_found_as_lost() {
        assert_eq!(sweep_finding(Some(1), None), vec![true]);
    }

    #[test_id(t71iarw)]
    #[test]
    fn a_change_the_engine_heard_and_no_read_named_is_not_lost() {
        assert_eq!(sweep_finding(Some(1), Some(7)), vec![false]);
    }

    #[test_id(te5jp9a)]
    #[test]
    fn a_row_booted_before_the_table_had_a_revision_is_not_lost() {
        assert_eq!(sweep_finding(None, None), vec![false]);
    }

    #[test_id(te35jta)]
    #[test]
    fn an_idle_hour_with_the_sweep_on_dispatches_nothing_and_commits_nothing() {
        let (clock, changes) = quiet();
        let mut e = started(vec![woken("a", &["a/"], false, 0, &[], true)], &clock, &changes);
        let before = (runs(&e, "a"), e.projection().total());
        for _ in 0..60 {
            clock.advance(60_000);
            e.run_until_idle().unwrap();
        }
        assert_eq!((runs(&e, "a"), e.projection().total()), before);
        assert_eq!(e.next_timer_at(), Some(clock.now_ms() + 60_000));
        assert_eq!(e.status().tasks[0].next_due_at, e.next_timer_at());
    }

    #[test_id(tkutig1)]
    #[test]
    fn a_due_time_arms_the_tick_with_a_timer_wake_and_is_the_next_timer() {
        let (clock, changes) = quiet();
        let mut e = started(vec![woken("a", &["a/"], false, 0, &[45_000], false)], &clock, &changes);
        assert_eq!(e.next_timer_at(), Some(45_000));
        clock.advance(44_999);
        e.run_until_idle().unwrap();
        assert_eq!(wake_runs(&e, "a", "timer"), 0);
        clock.advance(1);
        e.run_until_idle().unwrap();
        assert_eq!(wake_runs(&e, "a", "timer"), 1);
    }

    #[test_id(tohbnms)]
    #[test]
    fn a_wake_waits_out_the_min_gap_and_a_polling_task_keeps_its_own_timer() {
        let (clock, changes) = quiet();
        let mut e = started(vec![woken("a", &["a/"], true, 30_000, &[], false)], &clock, &changes);
        clock.advance(5_000);
        changed(&mut e, "c1", &["a/x"], false);
        e.run_until_idle().unwrap();
        assert_eq!(runs(&e, "a"), 1, "not before the gap is out");
        assert_eq!(e.next_timer_at(), Some(30_000));
        clock.advance(25_000);
        e.run_until_idle().unwrap();
        assert_eq!(runs(&e, "a"), 2, "one run at 30 s, not a second for the poll timer");
        clock.advance(30_000);
        e.run_until_idle().unwrap();
        assert_eq!(runs(&e, "a"), 3, "its own cadence goes on");
    }

    #[test_id(t4jaf3v)]
    #[test]
    fn without_a_sweep_the_wake_is_ignored_and_the_task_polls_as_before() {
        let clock = VirtualClock::new(0);
        let regs = vec![woken("a", &["a/"], false, 0, &[10], false)];
        let mut e = Engine::open(MemJournal::new(), Arc::new(clock.clone()), regs, 1).unwrap();
        e.start("a", EventKind::new("a", ""));
        e.run_until_idle().unwrap();
        changed(&mut e, "c1", &["a/x"], false);
        e.run_until_idle().unwrap();
        assert_eq!(e.next_timer_at(), Some(30_000));
        assert_eq!(runs(&e, "a"), 1);
    }

    #[test_id(tetkger)]
    #[test]
    fn the_earliest_due_of_a_stream_wins_and_is_gone_once_the_tick_has_run() {
        let (clock, changes) = quiet();
        let mut e = started(vec![woken("a", &["a/"], false, 0, &[50_000, 40_000], false)], &clock, &changes);
        assert_eq!(e.next_timer_at(), Some(40_000));
        clock.advance(40_000);
        e.run_until_idle().unwrap();
        assert_eq!(wake_runs(&e, "a", "timer"), 1);
        assert_eq!(e.next_timer_at(), Some(60_000), "the later due did not outlive the tick it was beaten by");
        clock.advance(10_000);
        e.run_until_idle().unwrap();
        assert_eq!(wake_runs(&e, "a", "timer"), 1);
    }

    #[test_id(tfyyyyw)]
    #[test]
    fn a_polling_task_given_a_due_time_runs_at_it_and_keeps_its_cadence_after() {
        let (clock, changes) = quiet();
        let mut e = started(vec![woken("a", &["a/"], true, 30_000, &[10_000], false)], &clock, &changes);
        assert_eq!(e.next_timer_at(), Some(10_000));
        clock.advance(10_000);
        e.run_until_idle().unwrap();
        assert_eq!(wake_runs(&e, "a", "timer"), 1);
        assert_eq!(e.next_timer_at(), Some(40_000), "its own cadence re-armed from the due tick");
    }

    /// A tick that re-arms every 30 s and acts on every tick, the boot tick included; with `probe`, it acts on the probe's answer, twice.
    struct Actor {
        tags: [&'static str; 2],
        tag: &'static str,
        probe: bool,
    }

    impl Handler for Actor {
        fn interests(&self) -> &[EventKindTag] {
            &self.tags
        }
        fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
            let write = |k: &str| Command::Effect(EffectSpec::keyed("write", k, "", 1_000));
            if ev.kind.tag == EFFECT_SUCCEEDED {
                return vec![write("w1"), write("w2")];
            }
            let mut cmds = vec![Command::Timer { at: ev.at + 30_000, kind: EventKind::new(self.tag, "again") }];
            if !self.probe {
                cmds.push(write("w"));
            } else if ev.at > 0 {
                cmds.push(Command::Probe(EffectSpec::probe("look", "", 10_000)));
            }
            cmds
        }
    }

    fn acting(name: &'static str, reads: &[&'static str], polls: bool, probe: bool) -> Registration {
        Registration::new(name, Actor { tags: [name, EFFECT_SUCCEEDED], tag: name, probe }).with_wake(crate::event::Wake { reads: reads.to_vec(), min_gap_ms: 0, polls })
    }

    fn found(e: &Engine, sink: &Arc<crate::wide::MemSink>) -> (u64, usize) {
        (e.status().tasks.iter().map(|t| t.sweep_found).sum(), sweep_found_records(sink).len())
    }

    fn booted(regs: Vec<Registration>, clock: &VirtualClock, changes: &Arc<crate::changes::ScriptedChanges>, at: Option<u64>) -> (Engine, Arc<crate::wide::MemSink>) {
        let sink = crate::wide::MemSink::new();
        let streams: Vec<String> = regs.iter().map(|r| r.name.clone()).collect();
        let mut e = Engine::open(MemJournal::new(), Arc::new(clock.clone()), regs, 1).unwrap().with_sweep(60_000, changes.clone()).with_sink(sink.clone());
        for s in &streams {
            match at {
                Some(at) => e.start_at(s, EventKind::new(s, ""), at),
                None => e.start(s, EventKind::new(s, "")),
            }
        }
        clock.advance(at.unwrap_or(0));
        e.run_until_idle().unwrap();
        (e, sink)
    }

    #[test_id(to8fhp6)]
    #[test]
    fn a_boot_tick_is_never_found_on_the_sweep_however_it_was_handed_over() {
        for at in [None, Some(5_000)] {
            let (clock, changes) = quiet();
            let regs = vec![acting("only", &["a/"], false, false), acting("polled", &["a/"], true, false), Registration::new("bare", Actor { tags: ["bare", EFFECT_SUCCEEDED], tag: "bare", probe: false })];
            let (e, sink) = booted(regs, &clock, &changes, at);
            assert!(e.status().tasks.iter().all(|t| t.runs_by_wake.get("sweep") == Some(&1)), "{at:?}: every row booted");
            assert_eq!(found(&e, &sink), (0, 0), "{at:?}");
        }
    }

    #[test_id(tzyg3wt)]
    #[test]
    fn a_rows_own_cadence_tick_is_never_found_on_the_sweep_when_the_store_moved() {
        let (clock, changes) = quiet();
        let regs = vec![Registration::new("bare", Actor { tags: ["bare", EFFECT_SUCCEEDED], tag: "bare", probe: false }), acting("polled", &["a/"], true, false)];
        let (mut e, sink) = booted(regs, &clock, &changes, None);
        changes.set(7);
        for _ in 0..3 {
            clock.advance(30_000);
            e.run_until_idle().unwrap();
        }
        assert_eq!(wake_runs(&e, "polled", "sweep"), 4);
        assert_eq!(found(&e, &sink), (0, 0));
    }

    #[test_id(ty76yxc)]
    #[test]
    fn a_polled_row_a_change_woke_between_cadence_ticks_is_not_found_on_the_sweep() {
        let (clock, changes) = quiet();
        let (mut e, sink) = booted(vec![acting("polled", &["a/"], true, false)], &clock, &changes, None);
        changes.set(7);
        clock.advance(10_000);
        changed(&mut e, "c1", &["a/x"], false);
        e.run_until_idle().unwrap();
        clock.advance(20_000);
        e.run_until_idle().unwrap();
        assert_eq!(found(&e, &sink), (0, 0));
    }

    #[test_id(tnegw96)]
    #[test]
    fn a_change_queued_when_the_sweep_falls_due_wakes_the_row_and_is_not_a_miss() {
        let (clock, changes) = quiet();
        let (mut e, sink) = booted(vec![woken("a", &["a/"], false, 0, &[], true)], &clock, &changes, None);
        changes.set(7);
        changed(&mut e, "c1", &["a/x"], false);
        clock.advance(60_000);
        e.run_until_idle().unwrap();
        assert!(runs(&e, "a") >= 2, "the row ran");
        assert_eq!(found(&e, &sink), (0, 0));
    }

    #[test_id(tdue0wk)]
    #[test]
    fn a_due_time_and_the_sweep_falling_due_together_run_the_row_once_as_woken() {
        let (clock, changes) = quiet();
        let (mut e, sink) = booted(vec![woken("a", &["a/"], false, 0, &[60_000], true)], &clock, &changes, None);
        changes.set(7);
        clock.advance(60_000);
        e.run_until_idle().unwrap();
        assert_eq!((wake_runs(&e, "a", "timer"), wake_runs(&e, "a", "sweep")), (1, 1), "one run as woken; the sweep's is the boot tick");
        changed_at(&mut e, "c1", 8, &["a/x"], false);
        e.run_until_idle().unwrap();
        clock.advance(60_000);
        e.run_until_idle().unwrap();
        assert_eq!(found(&e, &sink), (0, 0));
    }

    #[test_id(tlim41q)]
    #[test]
    fn a_change_naming_the_reads_before_the_probe_answers_clears_the_sweep_find() {
        for (named, want) in [(true, (0, 0)), (false, (1, 1))] {
            let (clock, changes) = quiet();
            let (mut e, sink) = booted(vec![acting("only", &["a/"], false, true)], &clock, &changes, None);
            changes.set(7);
            clock.advance(60_000);
            e.run_until_idle().unwrap();
            let probe = e.pending_effects().into_iter().find(|p| p.spec.name == "look").expect("the sweep's tick probed").key;
            if named {
                changed(&mut e, "c1", &["a/x"], false);
            }
            assert!(e.complete(&probe, Outcome::Succeeded(String::new())).unwrap());
            e.run_until_idle().unwrap();
            clock.advance(60_000);
            e.run_until_idle().unwrap();
            assert_eq!(found(&e, &sink), want, "named: {named}; two effects admitted by the answer count once");
        }
    }

    #[test_id(tp3nd1w)]
    #[test]
    fn a_sweep_during_a_pending_effect_starts_no_run_and_the_next_after_it_ends_does() {
        let (clock, changes) = quiet();
        let regs = vec![acting("only", &["a/"], false, true)];
        let mut e = Engine::open(MemJournal::new(), Arc::new(clock.clone()), regs, 1).unwrap().with_sweep(4_000, changes.clone());
        e.start("only", EventKind::new("only", ""));
        e.run_until_idle().unwrap();
        changes.set(7);
        clock.advance(4_000);
        e.run_until_idle().unwrap();
        assert_eq!(wake_runs(&e, "only", "sweep"), 2, "the boot tick and the sweep's, whose probe is now in flight");
        changes.set(8);
        clock.advance(4_000);
        e.run_until_idle().unwrap();
        assert_eq!(wake_runs(&e, "only", "sweep"), 2, "a wake is already carrying the row");
        let look = e.pending_effects().into_iter().find(|p| p.spec.name == "look").unwrap().key;
        assert!(e.complete(&look, Outcome::Succeeded(String::new())).unwrap());
        e.run_until_idle().unwrap();
        for write in e.pending_effects() {
            assert!(e.complete(&write.key, Outcome::Failed("scripted".into())).unwrap());
        }
        e.run_until_idle().unwrap();
        assert!(e.pending_effects().is_empty());
        clock.advance(4_000);
        e.run_until_idle().unwrap();
        assert_eq!(wake_runs(&e, "only", "sweep"), 3, "the row's last_rev was left, so it runs");
    }

    #[test_id(t7kq2ve)]
    #[test]
    fn a_sweep_find_followed_by_a_change_naming_its_reads_reports_nothing() {
        let (clock, changes) = quiet();
        let (mut e, audit) = audited(&clock, &changes);
        changes.set(7);
        sweep_once(&mut e, &clock);
        assert!(audit.0.lock().unwrap().is_empty(), "held");
        changed_at(&mut e, "c1", 8, &["a/x"], false);
        e.run_until_idle().unwrap();
        sweep_once(&mut e, &clock);
        assert!(audit.0.lock().unwrap().is_empty());
    }

    #[test_id(t0m6xcr)]
    #[test]
    fn a_sweep_find_followed_by_a_change_naming_other_keys_reports_once_with_their_prefixes() {
        let (clock, changes) = quiet();
        let (mut e, audit) = audited(&clock, &changes);
        changes.set(7);
        changed_at(&mut e, "c0", 5, &["z/1"], false);
        e.run_until_idle().unwrap();
        sweep_once(&mut e, &clock);
        changed_at(&mut e, "c1", 8, &["edges/x/1", "edges/y"], false);
        e.run_until_idle().unwrap();
        sweep_once(&mut e, &clock);
        let found = audit.0.lock().unwrap().clone();
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].lost, found[0].changed.clone()), (false, vec!["edges/".to_string(), "z/".to_string()]));
    }

    #[test_id(t5w8hjn)]
    #[test]
    fn a_sweep_find_followed_by_silence_reports_lost_at_the_next_sweep() {
        let (clock, changes) = quiet();
        let (mut e, audit) = audited(&clock, &changes);
        changes.set(7);
        sweep_once(&mut e, &clock);
        assert!(audit.0.lock().unwrap().is_empty());
        sweep_once(&mut e, &clock);
        let found = audit.0.lock().unwrap().clone();
        assert_eq!(found.len(), 1);
        assert!(found[0].lost);
    }

    #[test_id(tm2r7ey)]
    #[test]
    fn a_reload_after_a_sweep_find_discards_it() {
        let (clock, changes) = quiet();
        let (mut e, audit) = audited(&clock, &changes);
        changes.set(7);
        sweep_once(&mut e, &clock);
        changed_at(&mut e, "c1", 8, &[], true);
        e.run_until_idle().unwrap();
        sweep_once(&mut e, &clock);
        assert!(audit.0.lock().unwrap().is_empty());
    }

    #[test_id(tb6v0kd)]
    #[test]
    fn a_change_at_or_below_the_found_revision_leaves_the_find_held_for_the_next_sweep() {
        let (clock, changes) = quiet();
        let (mut e, audit) = audited(&clock, &changes);
        changes.set(7);
        sweep_once(&mut e, &clock);
        changed_at(&mut e, "c1", 6, &["z/1"], false);
        e.run_until_idle().unwrap();
        assert!(audit.0.lock().unwrap().is_empty());
        sweep_once(&mut e, &clock);
        let found = audit.0.lock().unwrap().clone();
        assert_eq!(found.len(), 1);
        assert!(found[0].lost, "the source never announced rev 7");
    }

    #[test_id(tn4y9xs)]
    #[test]
    fn prefixes_heard_before_a_tick_that_a_change_woke_are_not_reported_as_changed() {
        let (clock, changes) = quiet();
        let (mut e, audit) = audited(&clock, &changes);
        changed_at(&mut e, "c0", 5, &["z/1"], false);
        changed_at(&mut e, "c1", 6, &["a/x"], false);
        e.run_until_idle().unwrap();
        changes.set(7);
        sweep_once(&mut e, &clock);
        sweep_once(&mut e, &clock);
        let found = audit.0.lock().unwrap().clone();
        assert_eq!(found.len(), 1);
        assert!(found[0].changed.is_empty(), "{:?}", found[0].changed);
    }
}
