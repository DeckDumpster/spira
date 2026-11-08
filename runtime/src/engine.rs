//! The in-process executor's core, synchronous and clock-agnostic: append first, then
//! dispatch, then advance the cursor. Every id it writes is derived from the event that
//! caused it, so a replay after a crash at any point appends nothing twice.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::event::*;
use crate::journal::{Journal, Offset, Poison, ScheduleRow};
use crate::marker::{Mark, Marker, NullMarker};
use crate::offload::Busy;
use crate::ports::Clock;
use crate::scheduler::Scheduler;
use crate::wide::{Fields, NullSink, TurnRss, Value, Wide, WideSink};
use crate::{Result, RuntimeError};

const BATCH: usize = 256;

type Rows = Vec<(Offset, Event)>;

/// A stream's poison state is asked again after this long, or when the journal brings events.
const POISON_RECHECK_MS: u64 = 300_000;

/// Memory-only events a stream keeps in the projection; older ones are dropped.
pub const LOCAL_KEEP: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Succeeded(String),
    Failed(String),
    TimedOut,
}

impl Outcome {
    fn tag(&self) -> &'static str {
        match self {
            Outcome::Succeeded(_) => EFFECT_SUCCEEDED,
            Outcome::Failed(_) => EFFECT_FAILED,
            Outcome::TimedOut => EFFECT_TIMED_OUT,
        }
    }

    fn detail(&self) -> String {
        match self {
            Outcome::Succeeded(d) | Outcome::Failed(d) => d.clone(),
            Outcome::TimedOut => String::new(),
        }
    }
}

/// An effect requested and not yet answered. `key` is the idempotency key: the id of the
/// request event, derived from the causing event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingEffect {
    pub key: String,
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

fn is_outcome(tag: &str) -> bool {
    matches!(tag, EFFECT_SUCCEEDED | EFFECT_FAILED | EFFECT_TIMED_OUT)
}

#[derive(Serialize, Deserialize)]
struct HeldBody {
    stream: StreamKey,
    task: String,
    spec: EffectSpec,
}

/// The `applied` key under which a held write's release is recorded.
const RELEASE: &str = "scheduler.release";

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SchedulerStatus {
    pub at: u64,
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
    cursor: Offset,
    saved_cursor: Offset,
    check_applied_below: Offset,
    appended: bool,
    poison_seen: BTreeMap<StreamKey, bool>,
    loaded: BTreeSet<StreamKey>,
    local: VecDeque<Event>,
    timers: BTreeMap<String, ScheduleRow>,
    memory_only: BTreeSet<String>,
    local_seq: u64,
    foreign_poll_ms: u64,
    next_read_at: u64,
    poison_cleared_at: u64,
    halted: bool,
    held: usize,
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
    /// On a replay, what each event still to be dispatched had written on its stream before.
    rewritten: BTreeMap<EventId, Vec<Event>>,
    /// The task that requested each effect not yet answered, as far as this run has seen.
    requesters: BTreeMap<String, String>,
    /// Effects an executor has taken and not yet started: waiting for a permit or a worker.
    /// Their deadline is not running, so `tick` never times them out.
    queued: BTreeSet<String>,
    rss: TurnRss,
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

impl Engine {
    /// Rebuild from the journal without reading its history: the effects still waiting for an
    /// outcome, the schedule rows not yet fired, and the end of the journal. A stream's newest
    /// `WINDOW` events are loaded when its first event is dispatched, memory-only or not.
    pub fn open(journal: Arc<dyn Journal>, clock: Arc<dyn Clock>, regs: Vec<Registration>, seed: u64) -> Result<Self> {
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
            cursor: 0,
            saved_cursor: 0,
            check_applied_below: 0,
            appended: false,
            poison_seen: BTreeMap::new(),
            loaded: BTreeSet::new(),
            local: VecDeque::new(),
            timers: BTreeMap::new(),
            memory_only: BTreeSet::new(),
            local_seq: 0,
            foreign_poll_ms: 0,
            next_read_at: 0,
            poison_cleared_at: 0,
            halted: false,
            held: 0,
            tracks: BTreeMap::new(),
            owners: BTreeMap::new(),
            refusals: BTreeMap::new(),
            classifier: None,
            board: None,
            early: BTreeSet::new(),
            written: vec![],
            rewritten: BTreeMap::new(),
            requesters: BTreeMap::new(),
            queued: BTreeSet::new(),
            rss: TurnRss::default(),
        };
        let cursor = e.journal.cursor()?;
        e.cursor = cursor;
        e.saved_cursor = cursor;
        let (halted, held) = e.halt_state(cursor)?;
        e.halted = halted;
        for h in held {
            if !e.journal.applied(RELEASE, &h.id)? {
                e.held += 1;
            }
        }
        for (_, ev) in e.journal.unresolved_effects()? {
            e.track_effect(&ev)?;
        }
        e.check_applied_below = e.journal.end()?;
        for row in e.journal.pending_schedules()? {
            e.scheduler.insert(row);
        }
        Ok(e)
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
        SchedulerStatus { at: self.clock.now_ms(), halted: self.halted, held: self.held, tasks, classes: classes.into_values().collect() }
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

    /// Read the journal for events other writers appended at most this often, when this engine
    /// appended nothing since the last read. 0, the default, reads on every pump.
    pub fn with_foreign_poll(mut self, ms: u64) -> Self {
        self.foreign_poll_ms = ms;
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
            let w = self.unit("watchdog.restart", None, None);
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

    fn unit(&self, unit: &str, ev: Option<&Event>, offset: Option<(Offset, Offset)>) -> Wide {
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
        if let Some((off, last)) = offset {
            w.set("journal.offset", off);
            w.set("journal.lag", last - off);
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
            self.pending.insert(
                ev.id.0.clone(),
                PendingEffect { key: ev.id.0.clone(), stream: ev.stream.clone(), spec: b.spec, deadline_at: b.deadline_at, requested_at: ev.at, correlation: ev.correlation.clone() },
            );
        } else if matches!(ev.kind.tag.as_str(), EFFECT_SUCCEEDED | EFFECT_FAILED | EFFECT_TIMED_OUT) {
            if let Some(c) = &ev.cause {
                self.pending.remove(&c.0);
                self.queued.remove(&c.0);
            }
        }
        Ok(())
    }

    /// Whether the scheduler stood halted at `below`, and the writes held since the last resume.
    fn halt_state(&self, mut below: Offset) -> Result<(bool, Vec<Event>)> {
        let stream = StreamKey(HALT_STREAM.into());
        let (mut halted, mut held) = (None, vec![]);
        loop {
            let page = self.journal.read_stream(&stream, below, WINDOW)?;
            let Some(&(first, _)) = page.first() else { break };
            let full = page.len() == WINDOW;
            for (_, e) in page.into_iter().rev() {
                match e.kind.tag.as_str() {
                    SCHEDULER_RESUMED => {
                        held.reverse();
                        return Ok((halted.unwrap_or(false), held));
                    }
                    SCHEDULER_HALTED => {
                        halted.get_or_insert(true);
                    }
                    EFFECT_HELD => held.push(e),
                    _ => {}
                }
            }
            if !full {
                break;
            }
            below = first;
        }
        held.reverse();
        Ok((halted.unwrap_or(false), held))
    }

    /// Requests a held write once: a replay finds its release applied.
    fn release(&mut self, held: &Event, at: u64) -> Result<()> {
        if self.journal.applied(RELEASE, &held.id)? {
            return Ok(());
        }
        let b: HeldBody = serde_json::from_str(&held.kind.body).map_err(|e| RuntimeError::new(format!("bad held effect {}: {e}", held.id.0)))?;
        let deadline_at = at.saturating_add(b.spec.deadline_ms);
        let body = serde_json::to_string(&RequestBody { spec: b.spec, deadline_at, task: Some(b.task.clone()) }).map_err(|e| RuntimeError::new(e.to_string()))?;
        let task = b.task;
        let req = self.append_derived(derive_id(format!("{}/released", held.id.0)), &b.stream, EventKind::new(EFFECT_REQUESTED, &body), held, at)?;
        self.requesters.insert(req.id.0.clone(), task.clone());
        self.owners.insert(req.id.0.clone(), task);
        self.track_effect(&req)?;
        self.journal.mark_applied(RELEASE, &held.id)?;
        self.held = self.held.saturating_sub(1);
        Ok(())
    }

    /// The halt stream is the engine's own: a halt holds every `Act` write from then on, a
    /// resume releases what was held in the order it was held, and a write held after the
    /// resume that ended its halt is released when it is read.
    fn gate(&mut self, ev: &Event, off: Offset) -> Result<()> {
        match ev.kind.tag.as_str() {
            SCHEDULER_HALTED => self.halted = true,
            SCHEDULER_RESUMED => {
                self.halted = false;
                for h in self.halt_state(off)?.1 {
                    self.release(&h, ev.at)?;
                }
                self.held = 0;
            }
            EFFECT_HELD if self.halted => self.held += 1,
            EFFECT_HELD => self.release(ev, ev.at)?,
            _ => {}
        }
        Ok(())
    }

    pub fn is_halted(&self) -> bool {
        self.halted
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

    /// Hand `kind` to the handlers of `stream` now, without journaling it: the way a process
    /// re-arms its memory-only timers each time it starts.
    pub fn start(&mut self, stream: &str, kind: EventKind) {
        let ev = self.local_event(&StreamKey(stream.into()), kind, self.clock.now_ms(), None);
        self.local.push_back(ev);
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

    fn append_derived(&mut self, id: String, stream: &StreamKey, kind: EventKind, cause: &Event, at: u64) -> Result<Event> {
        let ev = Event { id: EventId(id), stream: stream.clone(), kind, at, cause: Some(cause.id.clone()), correlation: cause.correlation.clone() };
        self.journal.record(&ev)?;
        self.appended = true;
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
                    self.append_derived(id, &StreamKey(HALT_STREAM.into()), EventKind::new(EFFECT_HELD, &body), ev, ev.at)?;
                }
                Command::Effect(spec) => {
                    let deadline_at = ev.at.saturating_add(spec.deadline_ms);
                    let body = serde_json::to_string(&RequestBody { spec, deadline_at, task: Some(handler.into()) }).map_err(|e| RuntimeError::new(e.to_string()))?;
                    let tag = if mode == Mode::Act { EFFECT_REQUESTED } else { EFFECT_SHADOWED };
                    let req = self.append_derived(id, &ev.stream, EventKind::new(tag, &body), ev, ev.at)?;
                    if mode == Mode::Act {
                        self.owners.insert(req.id.0.clone(), handler.into());
                        self.requesters.insert(req.id.0.clone(), handler.into());
                    }
                    self.track_effect(&req)?;
                    self.wrote(own, req, false);
                }
                Command::Schedule { at, kind } => {
                    let row = ScheduleRow { id, at, stream: ev.stream.clone(), kind, cause: Some(ev.id.clone()), correlation: ev.correlation.clone() };
                    self.journal.schedule(&row)?;
                    self.scheduler.insert(row);
                }
                Command::Note(kind) => {
                    self.carry(handler, &ev.stream, &kind);
                    let note = self.local_event(&ev.stream, kind, ev.at, Some(ev));
                    self.local.push_back(note.clone());
                    self.wrote(own, note, true);
                }
                Command::Probe(spec) => {
                    let deadline_at = ev.at.saturating_add(spec.deadline_ms);
                    let body = serde_json::to_string(&RequestBody { spec, deadline_at, task: Some(handler.into()) }).map_err(|e| RuntimeError::new(e.to_string()))?;
                    let req = self.local_event(&ev.stream, EventKind::new(EFFECT_REQUESTED, &body), ev.at, Some(ev));
                    self.projection.push_local(&req, LOCAL_KEEP, |id| self.pending.contains_key(&id.0) || self.local.iter().any(|q| q.cause.as_ref() == Some(id)));
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

    /// Push `ev` into the projection unless it is there already, offer it, then place what its
    /// handlers wrote on its stream right after it: per stream, a handler sees every event its
    /// stream's handlers wrote for an earlier event before it sees a later one, whatever the
    /// journal offsets. Nothing reorders appends; only the view is ordered by cause.
    fn dispatch(&mut self, ev: &Event, at: Option<(Offset, Offset)>) -> Result<()> {
        if !self.loaded.contains(&ev.stream) {
            self.load(&ev.stream, at.map_or(self.cursor, |a| a.0))?;
        }
        if !self.early.remove(&ev.id) {
            if at.is_some() {
                self.projection.push(ev);
            } else {
                self.push_local(ev);
            }
        }
        if let (Some((off, _)), true) = (at, ev.stream.0 == HALT_STREAM) {
            self.gate(ev, off)?;
        }
        let offered = if self.is_poisoned(&ev.stream)? { Ok(()) } else { self.offer(ev, at) };
        self.place(ev);
        offered
    }

    fn push_local(&mut self, ev: &Event) {
        let (pending, local) = (&self.pending, &self.local);
        self.projection.push_local(ev, LOCAL_KEEP, |id| ev.cause.as_ref() == Some(id) || pending.contains_key(&id.0) || local.iter().any(|q| q.cause.as_ref() == Some(id)));
    }

    /// What `ev`'s handlers wrote on its stream, this run or (on a replay) the run before, into
    /// the projection in the order it was written.
    fn place(&mut self, ev: &Event) {
        let mut writes: Vec<(Event, bool)> = self.rewritten.remove(&ev.id).unwrap_or_default().into_iter().map(|e| (e, false)).collect();
        for w in std::mem::take(&mut self.written) {
            if !writes.iter().any(|(e, _)| e.id == w.0.id) {
                writes.push(w);
            }
        }
        for (e, local) in writes {
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

    /// A stream's newest `WINDOW` events below `below`, in the order a run that dispatched them
    /// placed them: each event, then what its handlers wrote on the stream. On a replay the
    /// events still to be dispatched are read too, so a handler's writes for an event before
    /// the crash are placed where that run placed them.
    fn load(&mut self, stream: &StreamKey, below: Offset) -> Result<()> {
        self.loaded.insert(stream.clone());
        let (past, ahead) = self.read_window(stream, below)?;
        if stream.0 == HALT_STREAM {
            for (_, e) in &past {
                self.projection.push(e);
            }
            return Ok(());
        }
        let ids: BTreeSet<&EventId> = past.iter().chain(&ahead).map(|(_, e)| &e.id).collect();
        let mut writes: BTreeMap<EventId, Vec<(Offset, Event)>> = BTreeMap::new();
        for (o, e) in past.iter().chain(&ahead) {
            if let Some(c) = e.cause.as_ref().filter(|c| ids.contains(c) && written_by_handler_of(c, &e.id)) {
                writes.entry(c.clone()).or_default().push((*o, e.clone()));
            }
        }
        let mut placed: BTreeSet<EventId> = BTreeSet::new();
        for (_, e) in &past {
            if placed.insert(e.id.clone()) {
                self.projection.push(e);
            }
            for (o, w) in writes.remove(&e.id).unwrap_or_default() {
                if placed.insert(w.id.clone()) {
                    self.projection.push(&w);
                    if o >= below {
                        self.early.insert(w.id.clone());
                    }
                }
            }
        }
        for (c, ws) in writes {
            self.rewritten.insert(c, ws.into_iter().map(|(_, w)| w).collect());
        }
        Ok(())
    }

    /// `stream`'s newest `WINDOW` events below `from`, and on a replay its events from `from`
    /// to the end the journal had at open, in one read: that stretch holds at most as many of
    /// the stream's events as it has offsets.
    fn read_window(&self, stream: &StreamKey, from: Offset) -> Result<(Rows, Rows)> {
        if from >= self.check_applied_below {
            return Ok((self.journal.read_stream(stream, from, WINDOW)?, vec![]));
        }
        let stretch = usize::try_from(self.check_applied_below - from).unwrap_or(usize::MAX);
        let mut past = self.journal.read_stream(stream, self.check_applied_below, WINDOW.saturating_add(stretch))?;
        let ahead = past.split_off(past.partition_point(|(o, _)| *o < from));
        let skip = past.len().saturating_sub(WINDOW);
        Ok((past.split_off(skip), ahead))
    }

    /// The task that requested the effect `ev` is the outcome of, when it is one and the
    /// request named its task.
    fn requester(&mut self, ev: &Event) -> Option<String> {
        if !is_outcome(&ev.kind.tag) {
            return None;
        }
        let cause = ev.cause.as_ref()?;
        if let Some(t) = self.requesters.remove(&cause.0) {
            return Some(t);
        }
        let req = self.projection.events(&ev.stream).iter().rev().find(|e| &e.id == cause && e.kind.tag == EFFECT_REQUESTED)?;
        serde_json::from_str::<RequestBody>(&req.kind.body).ok()?.task
    }

    /// Offer `ev` to every handler interested in it, an outcome only to the task that asked
    /// for it (and those that watch every outcome).
    fn offer(&mut self, ev: &Event, at: Option<(Offset, Offset)>) -> Result<()> {
        let regs = std::mem::take(&mut self.regs);
        let r = self.offer_to(&regs, ev, at);
        self.regs = regs;
        r
    }

    fn offer_to(&mut self, regs: &[Registration], ev: &Event, at: Option<(Offset, Offset)>) -> Result<()> {
        let requester = self.requester(ev);
        for reg in regs {
            let (name, mode, handler) = (reg.name.as_str(), reg.mode, &reg.handler);
            if !handler.interests().contains(&ev.kind.tag.as_str()) {
                continue;
            }
            if requester.as_deref().is_some_and(|r| r != name) && !handler.watches_foreign_outcomes() {
                continue;
            }
            if at.is_some_and(|(off, _)| off < self.check_applied_below) && self.journal.applied(name, &ev.id)? {
                continue;
            }
            let stalls = if self.suspects.is_empty() { 0 } else { self.suspects.get(&(ev.id.clone(), name.to_string())).copied().unwrap_or(0) };
            if stalls >= self.stall_limit {
                let message = format!("stalled {stalls} times in {name}");
                self.quarantine(ev, name, &message)?;
                let wide = self.dispatch_unit(ev, at, name, mode, stalls);
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
            let ctx = Ctx::with(ev.at, mix(self.seed, &[&ev.id.0, name]), at.map_or(0, |(off, _)| off), Wide::default());
            let began = Instant::now();
            let view = &self.projection;
            let called = catch_unwind(AssertUnwindSafe(|| handler.handle(&ctx, view, ev)));
            self.busy.exit();
            self.marker.clear()?;
            let elapsed = began.elapsed();
            let set = ctx.into_event();
            if stalls == 0 && set.is_empty() && matches!(&called, Ok(c) if c.is_empty()) {
                if at.is_some() {
                    self.journal.mark_applied(name, &ev.id)?;
                }
                continue;
            }
            let wide = self.dispatch_unit(ev, at, name, mode, stalls);
            wide.merge(set.take());
            wide.set("duration_us", elapsed.as_micros() as u64);
            match called {
                Ok(cmds) => {
                    let count = |f: fn(&Command) -> bool| cmds.iter().filter(|c| f(c)).count();
                    wide.set("commands.emit", count(|c| matches!(c, Command::Emit(_))));
                    wide.set("commands.emit_to", count(|c| matches!(c, Command::EmitTo { .. })));
                    wide.set("commands.effect", count(|c| matches!(c, Command::Effect(_))));
                    wide.set("commands.schedule", count(|c| matches!(c, Command::Schedule { .. })));
                    let done = self.apply(cmds, ev, reg).and_then(|()| if at.is_some() { self.journal.mark_applied(name, &ev.id) } else { Ok(()) });
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
    fn dispatch_unit(&self, ev: &Event, at: Option<(Offset, Offset)>, name: &str, mode: Mode, stalls: u32) -> Wide {
        let wide = self.unit("dispatch", Some(ev), at);
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
        let w = self.unit("quarantine", Some(ev), None);
        w.set("handler", handler);
        w.set("quarantine.reason", message);
        w.set("outcome", "quarantined");
        self.finish(&w);
        Ok(())
    }

    /// Dispatch every journal event past the cursor; the number dispatched. The cursor is read
    /// once at open and written once per pump: a crash in between replays events whose
    /// `applied` mark makes the replay a no-op.
    pub fn pump(&mut self) -> Result<usize> {
        let mut n = 0;
        let run = self.pump_all(&mut n);
        let saved = self.save_cursor();
        run.and(saved)?;
        Ok(n)
    }

    fn pump_all(&mut self, n: &mut usize) -> Result<()> {
        loop {
            let now = self.clock.now_ms();
            if self.appended || now >= self.next_read_at {
                let before = *n;
                self.pump_batches(n)?;
                if *n > before || now.saturating_sub(self.poison_cleared_at) >= POISON_RECHECK_MS {
                    self.poison_seen.clear();
                    self.poison_cleared_at = now;
                }
                self.appended = false;
                self.next_read_at = now.saturating_add(self.foreign_poll_ms);
            }
            if self.local.is_empty() {
                return Ok(());
            }
            self.appended = false;
            while let Some(ev) = self.local.pop_front() {
                if let Err(e) = self.dispatch(&ev, None) {
                    self.local.push_front(ev);
                    return Err(e);
                }
                *n += 1;
            }
            if !self.appended {
                return Ok(());
            }
        }
    }

    fn pump_batches(&mut self, n: &mut usize) -> Result<()> {
        loop {
            let batch = self.journal.read_from(self.cursor, BATCH)?;
            if batch.is_empty() {
                return Ok(());
            }
            let full = batch.len() == BATCH;
            let last = batch.last().map_or(0, |(o, _)| *o);
            self.appended = false;
            for (off, ev) in batch {
                if let Err(e) = self.dispatch(&ev, Some((off, last))) {
                    self.check_applied_below = self.check_applied_below.max(last + 1);
                    return Err(e);
                }
                self.cursor = off + 1;
                *n += 1;
            }
            if !full && !self.appended {
                return Ok(());
            }
        }
    }

    fn save_cursor(&mut self) -> Result<()> {
        if self.cursor != self.saved_cursor {
            self.journal.set_cursor(self.cursor)?;
            self.saved_cursor = self.cursor;
        }
        Ok(())
    }

    fn fire(&mut self, row: &ScheduleRow) -> Result<()> {
        let ev = Event {
            id: EventId(derive_id(format!("sched/{}", row.id))),
            stream: row.stream.clone(),
            kind: row.kind.clone(),
            at: row.at,
            cause: row.cause.clone(),
            correlation: row.correlation.clone(),
        };
        match self.journal.record(&ev) {
            Ok(()) => self.appended = true,
            Err(e) if e.is_refused() => self.quarantine(&ev, "scheduler", &e.0)?,
            Err(e) => return Err(e),
        }
        self.journal.mark_fired(&row.id)
    }

    /// Fire the schedule rows now due and time out the effects past their deadline.
    pub fn tick(&mut self) -> Result<usize> {
        let mut n = 0;
        let mut rows = self.scheduler.pop_due(self.clock.as_ref()).into_iter();
        while let Some(row) = rows.next() {
            if let Err(e) = self.fire(&row) {
                self.scheduler.insert(row);
                rows.for_each(|r| self.scheduler.insert(r));
                return Err(e);
            }
            n += 1;
        }
        let now = self.clock.now_ms();
        let mut due: Vec<_> = self.timers.values().filter(|r| r.at <= now).map(|r| (r.at, r.id.clone())).collect();
        due.sort();
        for (_, id) in due {
            if let Some(row) = self.timers.remove(&id) {
                let ev = Event { cause: row.cause.clone(), correlation: row.correlation.clone(), ..self.local_event(&row.stream, row.kind.clone(), row.at, None) };
                self.local.push_back(ev);
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

/// What the journal staged for its next request is sent on the way out, so a clean stop leaves
/// the cursor where this engine had it; a crash leaves it behind, and the replay re-derives it.
impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.journal.flush();
    }
}

impl Executor for Engine {
    fn submit(&mut self, id: &str, stream: &str, kind: EventKind) -> Result<()> {
        let ev = Event {
            id: EventId(id.into()),
            stream: StreamKey(stream.into()),
            kind,
            at: self.clock.now_ms(),
            cause: None,
            correlation: CorrelationId(id.into()),
        };
        self.journal.append(&ev)?;
        self.appended = true;
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
        self.pending.values().cloned().collect()
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
            self.local.push_back(ev.clone());
        } else {
            self.journal.record(&ev)?;
            self.appended = true;
        }
        self.pending.remove(key);
        self.queued.remove(key);
        if let Some(owner) = self.owners.remove(key) {
            self.tracks.entry(owner).or_default().outcome = Some(match outcome {
                Outcome::Succeeded(_) => LastOutcome::Succeeded,
                Outcome::Failed(_) => LastOutcome::Failed,
                Outcome::TimedOut => LastOutcome::TimedOut,
            });
        }
        let w = self.unit("effect", Some(&ev), None);
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
