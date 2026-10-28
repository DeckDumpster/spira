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
}

#[derive(Default)]
struct Mem {
    events: Vec<Event>,
    by_id: BTreeMap<EventId, Offset>,
    applied: BTreeSet<(String, EventId)>,
    cursor: Offset,
    schedules: BTreeMap<String, (ScheduleRow, bool)>,
    poison: BTreeMap<StreamKey, Poison>,
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
        let mut m = self.lock();
        if let Some(o) = m.by_id.get(&ev.id) {
            return Ok(*o);
        }
        let o = m.events.len() as Offset;
        m.events.push(ev.clone());
        m.by_id.insert(ev.id.clone(), o);
        Ok(o)
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
}
