//! The in-process executor's core, synchronous and clock-agnostic: append first, then
//! dispatch, then advance the cursor. Every id it writes is derived from the event that
//! caused it, so a replay after a crash at any point appends nothing twice.

use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::event::*;
use crate::journal::{Journal, Offset, Poison, ScheduleRow};
use crate::ports::Clock;
use crate::scheduler::Scheduler;
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
    /// Rebuild from the journal: the projection up to the cursor, the effects still waiting for
    /// an outcome, and the schedule rows not yet fired.
    pub fn open(journal: Arc<dyn Journal>, clock: Arc<dyn Clock>, regs: Vec<Registration>, seed: u64) -> Result<Self> {
        let mut e = Engine {
            journal,
            clock,
            regs,
            seed,
            projection: Projection::default(),
            scheduler: Scheduler::new(seed),
            pending: BTreeMap::new(),
        };
        let cursor = e.journal.cursor()?;
        let mut from = 0;
        loop {
            let batch = e.journal.read_from(from, BATCH)?;
            let Some((last, _)) = batch.last() else { break };
            from = last + 1;
            for (off, ev) in batch {
                if off < cursor {
                    e.projection.push(&ev);
                }
                e.track_effect(&ev)?;
            }
        }
        for row in e.journal.pending_schedules()? {
            e.scheduler.insert(row);
        }
        Ok(e)
    }

    fn track_effect(&mut self, ev: &Event) -> Result<()> {
        if ev.kind.tag == EFFECT_REQUESTED {
            let b: RequestBody = serde_json::from_str(&ev.kind.body).map_err(|e| RuntimeError::new(format!("bad effect request {}: {e}", ev.id.0)))?;
            self.pending.insert(
                ev.id.0.clone(),
                PendingEffect { key: ev.id.0.clone(), stream: ev.stream.clone(), spec: b.spec, deadline_at: b.deadline_at, correlation: ev.correlation.clone() },
            );
        } else if matches!(ev.kind.tag.as_str(), EFFECT_SUCCEEDED | EFFECT_FAILED | EFFECT_TIMED_OUT) {
            if let Some(c) = &ev.cause {
                self.pending.remove(&c.0);
            }
        }
        Ok(())
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

    fn append_derived(&self, id: String, stream: &StreamKey, kind: EventKind, cause: &Event, at: u64) -> Result<Event> {
        let ev = Event { id: EventId(id), stream: stream.clone(), kind, at, cause: Some(cause.id.clone()), correlation: cause.correlation.clone() };
        self.journal.append(&ev)?;
        Ok(ev)
    }

    fn apply(&mut self, cmds: Vec<Command>, ev: &Event, handler: &str) -> Result<()> {
        for (i, cmd) in cmds.into_iter().enumerate() {
            let id = format!("{}/{handler}/{i}", ev.id.0);
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

    fn dispatch(&mut self, ev: &Event) -> Result<()> {
        self.projection.push(ev);
        if self.journal.poisoned(&ev.stream)?.is_some() {
            return Ok(());
        }
        for i in 0..self.regs.len() {
            let name = self.regs[i].name.clone();
            if !self.regs[i].handler.interests().contains(&ev.kind.tag.as_str()) || self.journal.applied(&name, &ev.id)? {
                continue;
            }
            let ctx = Ctx::new(ev.at, mix(self.seed, &[&ev.id.0, &name]));
            let handler = &self.regs[i].handler;
            let view = &self.projection;
            match catch_unwind(AssertUnwindSafe(|| handler.handle(&ctx, view, ev))) {
                Ok(cmds) => {
                    self.apply(cmds, ev, &name)?;
                    self.journal.mark_applied(&name, &ev.id)?;
                }
                Err(p) => {
                    self.journal.poison(&Poison { stream: ev.stream.clone(), event: ev.id.clone(), handler: name, message: panic_message(&*p) })?;
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Dispatch every journal event past the cursor; the number dispatched.
    pub fn pump(&mut self) -> Result<usize> {
        let mut n = 0;
        loop {
            let from: Offset = self.journal.cursor()?;
            let batch = self.journal.read_from(from, BATCH)?;
            if batch.is_empty() {
                return Ok(n);
            }
            for (off, ev) in batch {
                self.dispatch(&ev)?;
                self.journal.set_cursor(off + 1)?;
                n += 1;
            }
        }
    }

    /// Fire the schedule rows now due and time out the effects past their deadline.
    pub fn tick(&mut self) -> Result<usize> {
        let mut n = 0;
        for row in self.scheduler.pop_due(self.clock.as_ref()) {
            let ev = Event {
                id: EventId(format!("sched/{}", row.id)),
                stream: row.stream.clone(),
                kind: row.kind.clone(),
                at: row.at,
                cause: row.cause.clone(),
                correlation: row.correlation.clone(),
            };
            self.journal.append(&ev)?;
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
        Ok(())
    }

    fn run_until_idle(&mut self) -> Result<()> {
        loop {
            let fired = self.tick()?;
            let dispatched = self.pump()?;
            if fired == 0 && dispatched == 0 {
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
            id: EventId(format!("{key}/outcome")),
            stream: p.stream.clone(),
            kind: EventKind::new(outcome.tag(), &outcome.detail()),
            at: self.clock.now_ms(),
            cause: Some(EventId(key.into())),
            correlation: p.correlation.clone(),
        };
        self.journal.append(&ev)?;
        self.pending.remove(key);
        Ok(true)
    }
}
