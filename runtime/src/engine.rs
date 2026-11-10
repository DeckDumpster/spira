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
use crate::offload::Busy;
use crate::ports::Clock;
use crate::scheduler::Scheduler;
use crate::wide::{Fields, NullSink, TurnRss, Value, Wide, WideSink};
use crate::{Result, RuntimeError};

/// A stream's poison state is asked again after this long, or when events were dispatched.
const POISON_RECHECK_MS: u64 = 300_000;

/// Memory-only events a stream keeps in the projection; older ones are dropped.
pub const LOCAL_KEEP: usize = 64;

const LOST_AT_RESTART: &str = "lost at restart";

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum LastOutcome {
    Succeeded,
    Failed,
    TimedOut,
    /// Its effector class was full; the effect is still waiting to run.
    Deferred,
}

/// The newest event a task emitted or noted: what it carries forward.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CarriedState {
    pub stream: String,
    pub tag: String,
    pub body_len: usize,
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
    /// Idempotency keys of its effects and probes with no outcome yet.
    pub in_flight: Vec<String>,
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
    stall_limit: u32,
    suspects: BTreeMap<(EventId, String), u32>,
    attempts: BTreeMap<String, u32>,
    baseline: Vec<(String, Value)>,
    started: Instant,
    poison_seen: BTreeMap<StreamKey, bool>,
    poison_cleared_at: u64,
    /// Every event waiting for its handlers, in the order it was made; `true` is memory-only.
    queue: VecDeque<(Event, bool)>,
    queued_ids: BTreeSet<EventId>,
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
    resolves: BTreeMap<EventId, String>,
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
/// until the waits sum to exactly `bound_ms` (the last wait is cut short to fit), then fail naming `store`.
/// The bound counts waits only; each ask is separately bounded by the socket's own deadline.
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

    fn read<T>(&self, mut ask: impl FnMut() -> Result<T>) -> Result<T> {
        let (mut wait, mut waited) = (self.first_ms, 0);
        loop {
            match ask() {
                Err(e) if e.is_cannot_tell() => {
                    if wait == 0 || waited >= self.bound_ms {
                        return Err(RuntimeError::new(format!("{}: still cannot tell after {waited} ms of retries: {}", self.store, e.0)));
                    }
                    let wait_now = wait.min(self.bound_ms - waited);
                    (self.sleep)(wait_now);
                    waited += wait_now;
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
            stall_limit: 2,
            suspects: BTreeMap::new(),
            attempts: BTreeMap::new(),
            baseline: vec![],
            started: Instant::now(),
            poison_seen: BTreeMap::new(),
            poison_cleared_at: 0,
            queue: VecDeque::new(),
            queued_ids: BTreeSet::new(),
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
            resolves: BTreeMap::new(),
        };
        let boot = retry.read(|| e.journal.boot())?;
        let mut legacy: BTreeMap<String, StateRow> = BTreeMap::new();
        let mut keyed: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        for row in boot.state {
            if e.codecs.contains_key(&row.task) {
                e.kept.insert((row.task.clone(), row.skey.clone()), row.value.clone());
                if row.skey == CARRY_KEY {
                    legacy.insert(row.task.clone(), row);
                } else {
                    keyed.entry(row.task.clone()).or_default().insert(row.skey, row.value);
                }
                continue;
            }
            e.restore(&row)?;
        }
        e.restore_keyed(legacy, keyed)?;
        let lost: Vec<String> = boot.effects.iter().map(|r| r.request_id.clone()).collect();
        for row in boot.effects {
            e.restore_effect(row);
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
            (SCHEDULER_TASK, HELD_KEY) => self.held = serde_json::from_str(&row.value).map_err(|e| bad(HELD_KEY, e))?,
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
    /// store is read only while no row of the task exists.
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
            let id = derive_id(format!("boot/{task}/{}/{}", codec.stream, codec.tag));
            let kind = EventKind::new(codec.tag, &(codec.join)(&rows));
            let ev = Event { id: EventId(id.clone()), stream: StreamKey(codec.stream.into()), kind, at: self.clock.now_ms(), cause: None, correlation: CorrelationId(id) };
            self.projection.push(&ev);
            self.carry(&task, &ev.stream, &ev.kind);
            self.carries.entry(task).or_default().insert((ev.stream.0.clone(), ev.kind.tag.clone()), ev);
        }
        Ok(())
    }

    fn restore_effect(&mut self, row: EffectRow) {
        let class = self.class_of(&row.spec);
        self.refusals.entry(class).or_insert(0);
        self.owners.insert(row.request_id.clone(), row.task.clone());
        self.requesters.insert(row.request_id.clone(), row.task.clone());
        self.committed.insert(row.request_id.clone());
        let correlation = CorrelationId(row.request_id.clone());
        self.pending.insert(row.request_id.clone(), PendingEffect { key: row.request_id, task: row.task, stream: row.stream, spec: row.spec, deadline_at: row.deadline_at, requested_at: row.requested_at, correlation });
    }

    /// Names the effector class of an effect, for `status`.
    pub fn with_classifier(mut self, c: Classifier) -> Self {
        self.classifier = Some(c);
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
                    next_due_at: rows.iter().filter(|row| interests.contains(&row.kind.tag.as_str())).map(|row| row.at).min(),
                    last_outcome: t.and_then(|t| t.outcome),
                    in_flight: self.pending.keys().filter(|k| self.owners.get(*k) == Some(&r.name)).cloned().collect(),
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
        let row = EffectRow { task: task.into(), ekey: spec.key.clone(), request_id: id.clone(), stream: stream.clone(), spec: spec.clone(), requested_at: at, deadline_at };
        let body = serde_json::to_string(&RequestBody { spec, deadline_at, task: Some(task.into()) }).map_err(|e| RuntimeError::new(e.to_string()))?;
        let req = self.append_derived(id, stream, EventKind::new(EFFECT_REQUESTED, &body), cause, at)?;
        self.owners.insert(req.id.0.clone(), task.into());
        self.requesters.insert(req.id.0.clone(), task.into());
        self.track_effect(&req)?;
        self.turn.effects.retain(|e| e.request_id != row.request_id);
        self.turn.effects.push(row);
        Ok(Some(req))
    }

    /// Requests a held write: it leaves the held list as it is requested.
    fn release(&mut self, held: &Event, at: u64) -> Result<()> {
        let b: HeldBody = serde_json::from_str(&held.kind.body).map_err(|e| RuntimeError::new(format!("bad held effect {}: {e}", held.id.0)))?;
        self.request(&b.task, &b.stream, derive_id(format!("{}/released", held.id.0)), b.spec, held, at)?;
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

    pub fn into_registrations(mut self) -> Vec<Registration> {
        std::mem::take(&mut self.regs)
    }

    pub fn projection(&self) -> &Projection {
        &self.projection
    }

    pub fn next_timer_at(&self) -> Option<u64> {
        let deadline = self.running_deadlines().map(|(d, _)| d).min();
        let timer = self.timers.values().map(|r| r.at).min();
        match (self.scheduler.next_at().into_iter().chain(timer).min(), deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn local_event(&mut self, stream: &StreamKey, kind: EventKind, at: u64, cause: Option<&Event>) -> Event {
        self.local_seq += 1;
        let id = EventId(derive_id(format!("local/{}/{}/{at}/{}", stream.0, kind.tag, self.local_seq)));
        let correlation = cause.map_or_else(|| CorrelationId(id.0.clone()), |c| c.correlation.clone());
        Event { id, stream: stream.clone(), kind, at, cause: cause.map(|c| c.id.clone()), correlation }
    }

    /// Hand `kind` to the handlers of `stream` now, without shipping it: the way a process
    /// re-arms its memory-only timers each time it starts.
    pub fn start(&mut self, stream: &str, kind: EventKind) {
        let ev = self.local_event(&StreamKey(stream.into()), kind, self.clock.now_ms(), None);
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
        self.enqueue(ev.clone());
        Ok(ev)
    }

    /// Run a handler's commands for `ev`. What lands on `ev`'s own stream is also recorded in
    /// `written`, so the projection holds it before `ev`'s stream is dispatched again.
    fn apply(&mut self, cmds: Vec<Command>, ev: &Event, reg: &Registration) -> Result<()> {
        let (handler, mode) = (reg.name.as_str(), reg.mode);
        let own = ev.stream.0 != HALT_STREAM;
        for (i, cmd) in cmds.into_iter().enumerate() {
            let id = derive_id(format!("{}/{handler}/{i}", ev.id.0));
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
                        self.append_derived(id, &stream, kind, ev, ev.at)?
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
                    if !self.held.iter().any(|h| h.id == held.id) {
                        self.held.push(held);
                        self.dirty_halt = true;
                    }
                }
                Command::Effect(spec) if mode == Mode::Act => {
                    if let Some(req) = self.request(handler, &ev.stream, id, spec, ev, ev.at)? {
                        self.wrote(own, req, false);
                    }
                }
                Command::Effect(spec) => {
                    let deadline_at = ev.at.saturating_add(spec.deadline_ms);
                    let body = serde_json::to_string(&RequestBody { spec, deadline_at, task: Some(handler.into()) }).map_err(|e| RuntimeError::new(e.to_string()))?;
                    let req = self.append_derived(id, &ev.stream, EventKind::new(EFFECT_SHADOWED, &body), ev, ev.at)?;
                    self.wrote(own, req, false);
                }
                Command::Schedule { at, kind } => {
                    let row = ScheduleRow { id, at, stream: ev.stream.clone(), kind, cause: Some(ev.id.clone()), correlation: ev.correlation.clone() };
                    self.turn.schedules.retain(|r| r.id != row.id);
                    self.turn.schedules.push(row.clone());
                    self.scheduler.insert(row);
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
                    let id = format!("{}/{}", ev.stream.0, kind.tag);
                    self.timers.insert(id.clone(), ScheduleRow { id, at, stream: ev.stream.clone(), kind, cause: Some(ev.id.clone()), correlation: ev.correlation.clone() });
                }
            }
        }
        Ok(())
    }

    fn wrote(&mut self, own: bool, e: Event, local: bool) {
        if own {
            self.written.push((e, local));
        }
    }

    fn carry(&mut self, handler: &str, stream: &StreamKey, kind: &EventKind) {
        self.tracks.entry(handler.into()).or_default().state = Some(CarriedState { stream: stream.0.clone(), tag: kind.tag.clone(), body_len: kind.body.len() });
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
        let offered = if self.is_poisoned(&ev.stream)? { Ok(()) } else { self.offer(ev) };
        self.place();
        offered
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
            self.marker.set(&Mark { event: ev.id.clone(), handler: name.to_string(), stream: ev.stream.clone(), stalls })?;
            let now = self.clock.now_ms();
            match self.tracks.get_mut(name) {
                Some(t) => (t.fired_at, t.due_at) = (Some(now), Some(ev.at)),
                None => {
                    self.tracks.insert(name.to_string(), Track { fired_at: Some(now), due_at: Some(ev.at), ..Track::default() });
                }
            }
            self.busy.enter();
            let ctx = Ctx::with(ev.at, mix(self.seed, &[&ev.id.0, name]), self.seq, Wide::default());
            let began = Instant::now();
            let view = &self.projection;
            let called = catch_unwind(AssertUnwindSafe(|| handler.handle(&ctx, view, ev)));
            self.busy.exit();
            self.marker.clear()?;
            let elapsed = began.elapsed();
            let set = ctx.into_event();
            if stalls == 0 && set.is_empty() && matches!(&called, Ok(c) if c.is_empty()) {
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
                    let done = self.apply(cmds, ev, reg);
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
        let p = self.journal.poisoned(stream)?.is_some();
        self.poison_seen.insert(stream.clone(), p);
        Ok(p)
    }

    fn quarantine(&mut self, ev: &Event, handler: &str, message: &str) -> Result<()> {
        self.poison_seen.insert(ev.stream.clone(), true);
        self.journal.poison(&Poison { stream: ev.stream.clone(), event: ev.id.clone(), handler: handler.into(), message: message.into() })?;
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
        self.commit_turn()?;
        let mut n = 0;
        let now = self.clock.now_ms();
        if now.saturating_sub(self.poison_cleared_at) >= POISON_RECHECK_MS {
            self.poison_seen.clear();
            self.poison_cleared_at = now;
        }
        while let Some((ev, local)) = self.queue.pop_front() {
            if let Err(e) = self.dispatch(&ev, local) {
                self.queue.push_front((ev, local));
                return Err(e);
            }
            self.queued_ids.remove(&ev.id);
            n += 1;
            match self.commit_turn() {
                Err(e) if e.is_refused() => self.discard_turn(&ev, &e.0)?,
                other => other?,
            }
        }
        Ok(n)
    }

    /// The changed state keys, staged into the turn: each task's carry and the scheduler's halt.
    fn stage_state(&mut self) {
        let at = self.clock.now_ms();
        let mut changed: Vec<(String, String, String, String)> = vec![];
        for task in std::mem::take(&mut self.dirty_carry) {
            if let Some(codec) = self.codecs.get(&task).copied() {
                let body = std::iter::once(codec.stream).chain(codec.also.iter().copied()).filter_map(|s| self.carries.get(&task)?.get(&(s.to_string(), codec.tag.to_string()))).max_by_key(|e| e.at).map(|e| e.kind.body.clone()).unwrap_or_default();
                let mut rows = (codec.split)(&body);
                for ((t, k), sig) in &self.kept {
                    if t == &task && !sig.is_empty() && sig != LEGACY_SPENT && k != CARRY_KEY && !rows.contains_key(k) {
                        rows.insert(k.clone(), String::new());
                    }
                }
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
        for (task, skey, value, sig) in changed {
            let key = (task.clone(), skey.clone());
            self.turn.state.retain(|r| !(r.task == task && r.skey == skey));
            self.staged.remove(&key);
            if self.kept.get(&key) != Some(&sig) {
                self.turn.state.push(StateRow { task, skey, value, at });
                self.staged.insert(key, sig);
            }
        }
    }

    fn commit_turn(&mut self) -> Result<()> {
        self.stage_state();
        if self.turn.is_empty() {
            return Ok(());
        }
        self.journal.commit(&self.turn)?;
        let turn = std::mem::take(&mut self.turn);
        self.committed.extend(turn.effects.iter().map(|e| e.request_id.clone()));
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
                let ev = Event { cause: row.cause.clone(), correlation: row.correlation.clone(), ..self.local_event(&row.stream, row.kind.clone(), row.at, None) };
                self.enqueue_local(ev);
                n += 1;
            }
        }
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
        self.enqueue(ev);
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
        w.set("effect.attempt", self.attempts.remove(key).unwrap_or(1));
        w.set("duration_ms", ev.at.saturating_sub(p.requested_at));
        self.finish(&w);
        Ok(true)
    }
}
