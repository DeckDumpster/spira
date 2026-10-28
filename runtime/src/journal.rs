//! The journal behind its trait: append, read from an offset, schedule rows.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use crate::event::{Event, EventId, EventKind, StreamKey};
use crate::{Result, RuntimeError};

pub type Offset = u64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleRow {
    pub id: String,
    pub at: u64,
    pub stream: StreamKey,
    pub kind: EventKind,
    pub cause: Option<EventId>,
    pub correlation: crate::event::CorrelationId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Poison {
    pub stream: StreamKey,
    pub event: EventId,
    pub handler: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub stream: StreamKey,
    pub holder: String,
    pub epoch: u64,
    pub expires_at: u64,
}

pub const LEASE_LOST: &str = "lease lost";

/// The lease a caller would hold after asking for `stream`: its own renewed, a free or expired
/// one taken at the next epoch, or nothing when a live holder has it. A released lease keeps
/// its epoch (holder empty, expired) so the next holder still fences the last.
pub fn grant(cur: Option<&Lease>, stream: &StreamKey, holder: &str, ttl_ms: u64, now: u64) -> Option<Lease> {
    let expires_at = now.saturating_add(ttl_ms);
    let lease = |epoch| Some(Lease { stream: stream.clone(), holder: holder.into(), epoch, expires_at });
    match cur {
        None => lease(1),
        Some(c) if c.holder == holder => lease(c.epoch),
        Some(c) if c.expires_at <= now => lease(c.epoch + 1),
        Some(_) => None,
    }
}

pub fn fence(cur: Option<&Lease>, stream: &StreamKey, holder: &str, epoch: u64) -> Result<()> {
    match cur {
        Some(c) if c.holder == holder && c.epoch == epoch => Ok(()),
        _ => Err(RuntimeError::new(format!("{LEASE_LOST}: {}", stream.0))),
    }
}

pub fn release(cur: Option<&Lease>, holder: &str) -> Option<Lease> {
    cur.filter(|c| c.holder == holder).map(|c| Lease { holder: String::new(), expires_at: 0, ..c.clone() })
}

/// Stream ownership. `append_fenced` is the one write that checks the lease and appends in a
/// single step, so a holder that was stolen from cannot land an event after the steal.
pub trait LeaseTable: Send + Sync {
    fn acquire(&self, stream: &StreamKey, holder: &str, ttl_ms: u64, now: u64) -> Result<Option<Lease>>;
    fn release(&self, stream: &StreamKey, holder: &str) -> Result<()>;
    fn current(&self, stream: &StreamKey) -> Result<Option<Lease>>;
    fn append_fenced(&self, ev: &Event, holder: &str, epoch: u64) -> Result<Offset>;
}

/// Every method is idempotent on its key: `append` on an id already present returns the
/// existing offset, so a replayed handler writes nothing twice.
pub trait Journal: Send + Sync {
    fn append(&self, ev: &Event) -> Result<Offset>;
    fn read_from(&self, from: Offset, limit: usize) -> Result<Vec<(Offset, Event)>>;
    fn applied(&self, handler: &str, id: &EventId) -> Result<bool>;
    fn mark_applied(&self, handler: &str, id: &EventId) -> Result<()>;
    fn cursor(&self) -> Result<Offset>;
    fn set_cursor(&self, next: Offset) -> Result<()>;
    fn schedule(&self, row: &ScheduleRow) -> Result<()>;
    fn pending_schedules(&self) -> Result<Vec<ScheduleRow>>;
    fn mark_fired(&self, row_id: &str) -> Result<()>;
    fn poison(&self, p: &Poison) -> Result<()>;
    fn poisoned(&self, stream: &StreamKey) -> Result<Option<Poison>>;
    /// `None` when the journal cannot arbitrate stream ownership between processes.
    fn leases(&self) -> Option<&dyn LeaseTable> {
        None
    }
}

#[derive(Default)]
struct Mem {
    events: Vec<Event>,
    by_id: BTreeMap<EventId, Offset>,
    applied: BTreeSet<(String, EventId)>,
    cursor: Offset,
    schedules: BTreeMap<String, (ScheduleRow, bool)>,
    poison: BTreeMap<StreamKey, Poison>,
    leases: BTreeMap<StreamKey, Lease>,
}

impl Mem {
    fn append(&mut self, ev: &Event) -> Offset {
        if let Some(o) = self.by_id.get(&ev.id) {
            return *o;
        }
        let o = self.events.len() as Offset;
        self.events.push(ev.clone());
        self.by_id.insert(ev.id.clone(), o);
        o
    }
}

#[derive(Default)]
pub struct MemJournal(Mutex<Mem>);

impl MemJournal {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> MutexGuard<'_, Mem> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Journal for MemJournal {
    fn append(&self, ev: &Event) -> Result<Offset> {
        Ok(self.lock().append(ev))
    }

    fn read_from(&self, from: Offset, limit: usize) -> Result<Vec<(Offset, Event)>> {
        let m = self.lock();
        Ok(m.events
            .iter()
            .enumerate()
            .skip(from as usize)
            .take(limit)
            .map(|(i, e)| (i as Offset, e.clone()))
            .collect())
    }

    fn applied(&self, handler: &str, id: &EventId) -> Result<bool> {
        Ok(self.lock().applied.contains(&(handler.to_string(), id.clone())))
    }

    fn mark_applied(&self, handler: &str, id: &EventId) -> Result<()> {
        self.lock().applied.insert((handler.to_string(), id.clone()));
        Ok(())
    }

    fn cursor(&self) -> Result<Offset> {
        Ok(self.lock().cursor)
    }

    fn set_cursor(&self, next: Offset) -> Result<()> {
        self.lock().cursor = next;
        Ok(())
    }

    fn schedule(&self, row: &ScheduleRow) -> Result<()> {
        self.lock().schedules.entry(row.id.clone()).or_insert((row.clone(), false));
        Ok(())
    }

    fn pending_schedules(&self) -> Result<Vec<ScheduleRow>> {
        Ok(self.lock().schedules.values().filter(|(_, fired)| !fired).map(|(r, _)| r.clone()).collect())
    }

    fn mark_fired(&self, row_id: &str) -> Result<()> {
        if let Some(s) = self.lock().schedules.get_mut(row_id) {
            s.1 = true;
        }
        Ok(())
    }

    fn poison(&self, p: &Poison) -> Result<()> {
        self.lock().poison.entry(p.stream.clone()).or_insert_with(|| p.clone());
        Ok(())
    }

    fn poisoned(&self, stream: &StreamKey) -> Result<Option<Poison>> {
        Ok(self.lock().poison.get(stream).cloned())
    }

    fn leases(&self) -> Option<&dyn LeaseTable> {
        Some(self)
    }
}

impl LeaseTable for MemJournal {
    fn acquire(&self, stream: &StreamKey, holder: &str, ttl_ms: u64, now: u64) -> Result<Option<Lease>> {
        let mut m = self.lock();
        let g = grant(m.leases.get(stream), stream, holder, ttl_ms, now);
        if let Some(l) = &g {
            m.leases.insert(stream.clone(), l.clone());
        }
        Ok(g)
    }

    fn release(&self, stream: &StreamKey, holder: &str) -> Result<()> {
        let mut m = self.lock();
        if let Some(l) = release(m.leases.get(stream), holder) {
            m.leases.insert(stream.clone(), l);
        }
        Ok(())
    }

    fn current(&self, stream: &StreamKey) -> Result<Option<Lease>> {
        Ok(self.lock().leases.get(stream).cloned())
    }

    fn append_fenced(&self, ev: &Event, holder: &str, epoch: u64) -> Result<Offset> {
        let mut m = self.lock();
        fence(m.leases.get(&ev.stream), &ev.stream, holder, epoch)?;
        Ok(m.append(ev))
    }
}

/// A journal that refuses one named operation once, to stand in for a crash at that point.
pub struct FaultyJournal {
    inner: Arc<dyn Journal>,
    fail: Mutex<Option<&'static str>>,
}

impl FaultyJournal {
    pub fn new(inner: Arc<dyn Journal>) -> Arc<Self> {
        Arc::new(FaultyJournal { inner, fail: Mutex::new(None) })
    }

    /// The next call of `op` (a `Journal` method name) fails, once.
    pub fn fail_next(&self, op: &'static str) {
        *self.fail.lock().unwrap_or_else(|e| e.into_inner()) = Some(op);
    }

    fn gate(&self, op: &'static str) -> Result<()> {
        let mut f = self.fail.lock().unwrap_or_else(|e| e.into_inner());
        if *f == Some(op) {
            *f = None;
            return Err(RuntimeError::new(format!("injected fault in {op}")));
        }
        Ok(())
    }
}

impl Journal for FaultyJournal {
    fn append(&self, ev: &Event) -> Result<Offset> {
        self.gate("append")?;
        self.inner.append(ev)
    }
    fn read_from(&self, from: Offset, limit: usize) -> Result<Vec<(Offset, Event)>> {
        self.inner.read_from(from, limit)
    }
    fn applied(&self, handler: &str, id: &EventId) -> Result<bool> {
        self.inner.applied(handler, id)
    }
    fn mark_applied(&self, handler: &str, id: &EventId) -> Result<()> {
        self.gate("mark_applied")?;
        self.inner.mark_applied(handler, id)
    }
    fn cursor(&self) -> Result<Offset> {
        self.inner.cursor()
    }
    fn set_cursor(&self, next: Offset) -> Result<()> {
        self.gate("set_cursor")?;
        self.inner.set_cursor(next)
    }
    fn schedule(&self, row: &ScheduleRow) -> Result<()> {
        self.gate("schedule")?;
        self.inner.schedule(row)
    }
    fn pending_schedules(&self) -> Result<Vec<ScheduleRow>> {
        self.inner.pending_schedules()
    }
    fn mark_fired(&self, row_id: &str) -> Result<()> {
        self.gate("mark_fired")?;
        self.inner.mark_fired(row_id)
    }
    fn poison(&self, p: &Poison) -> Result<()> {
        self.inner.poison(p)
    }
    fn poisoned(&self, stream: &StreamKey) -> Result<Option<Poison>> {
        self.inner.poisoned(stream)
    }
    fn leases(&self) -> Option<&dyn LeaseTable> {
        self.inner.leases().map(|_| self as &dyn LeaseTable)
    }
}

impl FaultyJournal {
    fn table(&self) -> Result<&dyn LeaseTable> {
        self.inner.leases().ok_or_else(|| RuntimeError::new("journal has no leases"))
    }
}

impl LeaseTable for FaultyJournal {
    fn acquire(&self, stream: &StreamKey, holder: &str, ttl_ms: u64, now: u64) -> Result<Option<Lease>> {
        self.table()?.acquire(stream, holder, ttl_ms, now)
    }
    fn release(&self, stream: &StreamKey, holder: &str) -> Result<()> {
        self.table()?.release(stream, holder)
    }
    fn current(&self, stream: &StreamKey) -> Result<Option<Lease>> {
        self.table()?.current(stream)
    }
    fn append_fenced(&self, ev: &Event, holder: &str, epoch: u64) -> Result<Offset> {
        self.gate("append")?;
        self.table()?.append_fenced(ev, holder, epoch)
    }
}
