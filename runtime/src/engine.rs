//! The in-process executor's core, synchronous and clock-agnostic: append first, then
//! dispatch, then advance the cursor. Every id it writes is derived from the event that
//! caused it, so a replay after a crash at any point appends nothing twice.

use std::collections::{BTreeMap, BTreeSet};
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
use crate::wide::{rss_kb, Fields, NullSink, Value, Wide, WideSink};
use crate::{Result, RuntimeError};

const BATCH: usize = 256;

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
    /// outcome, the schedule rows not yet fired, and the end of the journal. A stream's past is
    /// loaded when its first event past the cursor is dispatched.
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
        };
        let cursor = e.journal.cursor()?;
        e.cursor = cursor;
        e.saved_cursor = cursor;
        for (_, ev) in e.journal.unresolved_effects()? {
            e.track_effect(&ev)?;
        }
        let mut from = cursor;
        loop {
            let batch = e.journal.read_from(from, BATCH)?;
            let Some((last, _)) = batch.last() else { break };
            from = last + 1;
        }
        e.check_applied_below = from;
        for row in e.journal.pending_schedules()? {
            e.scheduler.insert(row);
        }
        Ok(e)
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

    /// The executor started the runner for effect `key`.
    pub fn note_attempt(&mut self, key: &str) {
        *self.attempts.entry(key.into()).or_insert(0) += 1;
    }

    fn unit(&self, unit: &str, ev: Option<&Event>, offset: Option<(Offset, Offset)>) -> Wide {
        let w = Wide::default();
        w.set("unit", unit);
        for (k, v) in &self.baseline {
            w.set(k, v.clone());
        }
        w.set("process.rss_kb", rss_kb());
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
            self.pending.insert(
                ev.id.0.clone(),
                PendingEffect { key: ev.id.0.clone(), stream: ev.stream.clone(), spec: b.spec, deadline_at: b.deadline_at, requested_at: ev.at, correlation: ev.correlation.clone() },
            );
        } else if matches!(ev.kind.tag.as_str(), EFFECT_SUCCEEDED | EFFECT_FAILED | EFFECT_TIMED_OUT) {
            if let Some(c) = &ev.cause {
                self.pending.remove(&c.0);
            }
        }
        Ok(())
    }

    pub fn into_registrations(self) -> Vec<Registration> {
        self.regs
    }

    pub fn projection(&self) -> &Projection {
        &self.projection
    }

    pub fn next_timer_at(&self) -> Option<u64> {
        let deadline = self.pending.values().map(|p| p.deadline_at).min();
        match (self.scheduler.next_at(), deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn append_derived(&mut self, id: String, stream: &StreamKey, kind: EventKind, cause: &Event, at: u64) -> Result<Event> {
        let ev = Event { id: EventId(id), stream: stream.clone(), kind, at, cause: Some(cause.id.clone()), correlation: cause.correlation.clone() };
        self.journal.append(&ev)?;
        self.appended = true;
        Ok(ev)
    }

    fn apply(&mut self, cmds: Vec<Command>, ev: &Event, handler: &str) -> Result<()> {
        for (i, cmd) in cmds.into_iter().enumerate() {
            let id = derive_id(format!("{}/{handler}/{i}", ev.id.0));
            match cmd {
                Command::Emit(kind) => {
                    self.append_derived(id, &ev.stream, kind, ev, ev.at)?;
                }
                Command::Effect(spec) => {
                    let deadline_at = ev.at.saturating_add(spec.deadline_ms);
                    let body = serde_json::to_string(&RequestBody { spec, deadline_at }).map_err(|e| RuntimeError::new(e.to_string()))?;
                    let req = self.append_derived(id, &ev.stream, EventKind::new(EFFECT_REQUESTED, &body), ev, ev.at)?;
                    self.track_effect(&req)?;
                }
                Command::Schedule { at, kind } => {
                    let row = ScheduleRow { id, at, stream: ev.stream.clone(), kind, cause: Some(ev.id.clone()), correlation: ev.correlation.clone() };
                    self.journal.schedule(&row)?;
                    self.scheduler.insert(row);
                }
            }
        }
        Ok(())
    }

    fn dispatch(&mut self, ev: &Event, at: (Offset, Offset)) -> Result<()> {
        if !self.loaded.contains(&ev.stream) {
            for (_, past) in self.journal.read_stream(&ev.stream, at.0)? {
                self.projection.push(&past);
            }
            self.loaded.insert(ev.stream.clone());
        }
        self.projection.push(ev);
        if self.is_poisoned(&ev.stream)? {
            return Ok(());
        }
        for i in 0..self.regs.len() {
            let name = self.regs[i].name.clone();
            if !self.regs[i].handler.interests().contains(&ev.kind.tag.as_str()) || (at.0 < self.check_applied_below && self.journal.applied(&name, &ev.id)?) {
                continue;
            }
            let key = (ev.id.clone(), name.clone());
            let stalls = self.suspects.get(&key).copied().unwrap_or(0);
            let wide = self.unit("dispatch", Some(ev), Some(at));
            wide.set("handler", name.as_str());
            if stalls >= self.stall_limit {
                let message = format!("stalled {stalls} times in {name}");
                self.quarantine(ev, &name, &message)?;
                wide.set("stall.count", stalls);
                wide.set("outcome", "quarantined");
                self.finish(&wide);
                return Ok(());
            }
            if stalls > 0 {
                wide.set("stall.count", stalls);
            }
            self.marker.set(&Mark { event: ev.id.clone(), handler: name.clone(), stream: ev.stream.clone(), stalls })?;
            self.busy.enter();
            let ctx = Ctx::with(ev.at, mix(self.seed, &[&ev.id.0, &name]), at.0, wide.clone());
            let began = Instant::now();
            let handler = &self.regs[i].handler;
            let view = &self.projection;
            let called = catch_unwind(AssertUnwindSafe(|| handler.handle(&ctx, view, ev)));
            self.busy.exit();
            self.marker.clear()?;
            wide.set("duration_us", began.elapsed().as_micros() as u64);
            match called {
                Ok(cmds) => {
                    let count = |f: fn(&Command) -> bool| cmds.iter().filter(|c| f(c)).count();
                    wide.set("commands.emit", count(|c| matches!(c, Command::Emit(_))));
                    wide.set("commands.effect", count(|c| matches!(c, Command::Effect(_))));
                    wide.set("commands.schedule", count(|c| matches!(c, Command::Schedule { .. })));
                    let done = self.apply(cmds, ev, &name).and_then(|()| self.journal.mark_applied(&name, &ev.id));
                    if matches!(&done, Err(e) if e.is_refused()) {
                        let message = done.unwrap_err().0;
                        wide.set("outcome", "refused");
                        self.finish(&wide);
                        self.quarantine(ev, &name, &message)?;
                        return Ok(());
                    }
                    wide.set("outcome", if done.is_ok() { "ok" } else { "error" });
                    self.finish(&wide);
                    done?;
                    self.suspects.remove(&key);
                }
                Err(p) => {
                    let message = panic_message(&*p);
                    wide.set("outcome", "panic");
                    wide.set("panic.message", message.as_str());
                    self.finish(&wide);
                    self.quarantine(ev, &name, &message)?;
                    return Ok(());
                }
            }
        }
        Ok(())
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
        self.poison_seen.clear();
        let mut n = 0;
        let run = self.pump_batches(&mut n);
        let saved = self.save_cursor();
        run.and(saved)?;
        Ok(n)
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
                if let Err(e) = self.dispatch(&ev, (off, last)) {
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

    /// Fire the schedule rows now due and time out the effects past their deadline.
    pub fn tick(&mut self) -> Result<usize> {
        let mut n = 0;
        for row in self.scheduler.pop_due(self.clock.as_ref()) {
            let ev = Event {
                id: EventId(derive_id(format!("sched/{}", row.id))),
                stream: row.stream.clone(),
                kind: row.kind.clone(),
                at: row.at,
                cause: row.cause.clone(),
                correlation: row.correlation.clone(),
            };
            match self.journal.append(&ev) {
                Ok(_) => self.appended = true,
                Err(e) if e.is_refused() => {
                    self.quarantine(&ev, "scheduler", &e.0)?;
                }
                Err(e) => return Err(e),
            }
            self.journal.mark_fired(&row.id)?;
            n += 1;
        }
        let now = self.clock.now_ms();
        let mut late: Vec<_> = self.pending.values().filter(|p| p.deadline_at <= now).map(|p| (p.deadline_at, p.key.clone())).collect();
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
        loop {
            let fired = self.tick()?;
            let dispatched = self.pump()?;
            if (fired == 0 && dispatched == 0) || self.next_timer_at().is_none_or(|t| t > self.clock.now_ms()) {
                return Ok(());
            }
        }
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
        self.journal.append(&ev)?;
        self.appended = true;
        self.pending.remove(key);
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
