//! The store behind its trait: what must outlive a process, committed once per turn, and the
//! event history shipped beside it. Dispatch never reads either back.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use spira_otel::vlogs::ShipCounts;

use crate::event::{EffectSpec, Event, EventId, EventKind, StreamKey, EFFECT_FAILED, EFFECT_SUCCEEDED, EFFECT_TIMED_OUT};
use crate::{Class, Result, RuntimeError};

pub fn is_outcome(tag: &str) -> bool {
    matches!(tag, EFFECT_SUCCEEDED | EFFECT_FAILED | EFFECT_TIMED_OUT)
}

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

/// The task whose keys hold the scheduler's own state, and the keys.
pub const SCHEDULER_TASK: &str = "scheduler";
pub const HALTED_KEY: &str = "halted";
pub const HELD_KEY: &str = "held";
/// The key a task's whole carry is stored under until the per-family split.
pub const CARRY_KEY: &str = "carry";

/// One key of a task's state, overwritten in place.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateRow {
    pub task: String,
    pub skey: String,
    pub value: String,
    pub at: u64,
}

/// An effect requested and not yet answered, keyed by the act's idempotency key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectRow {
    pub task: String,
    pub ekey: String,
    pub request_id: String,
    pub stream: StreamKey,
    pub spec: EffectSpec,
    pub requested_at: u64,
    pub deadline_at: u64,
}

/// Everything one turn of the engine changes in the store, committed together.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    /// Only the keys whose value changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state: Vec<StateRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<EffectRow>,
    /// Request ids whose outcome this turn handled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolved: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schedules: Vec<ScheduleRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fired: Vec<String>,
}

impl Turn {
    pub fn is_empty(&self) -> bool {
        self.state.is_empty() && self.effects.is_empty() && self.resolved.is_empty() && self.schedules.is_empty() && self.fired.is_empty()
    }
}

/// What a booting process reads: every state key, the unresolved effects and the unfired schedule rows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Boot {
    pub state: Vec<StateRow>,
    pub effects: Vec<EffectRow>,
    pub schedules: Vec<ScheduleRow>,
}

/// `record` ships history and is read by nobody; `commit` is the one write that must hold, and is
/// idempotent on its keys, so a turn that failed may be sent again.
pub trait Journal: Send + Sync {
    /// Ship `ev` to the event history, best effort, ahead of the turn's commit.
    fn record(&self, ev: &Event);
    fn boot(&self) -> Result<Boot>;
    fn commit(&self, turn: &Turn) -> Result<()>;
    fn poison(&self, p: &Poison) -> Result<()>;
    fn poisoned(&self, stream: &StreamKey) -> Result<Option<Poison>>;
    /// The events recorded so far, for a journal that keeps them: tests read history here, and
    /// nothing in the engine does.
    fn history(&self) -> Vec<Event> {
        vec![]
    }
    /// What the shipper has dropped and failed, for a journal that ships.
    fn shipped(&self) -> Option<ShipCounts> {
        None
    }
}

#[derive(Default)]
struct Mem {
    log: Vec<Event>,
    commits: Vec<Turn>,
    state: BTreeMap<(String, String), StateRow>,
    effects: BTreeMap<(String, String), (EffectRow, bool)>,
    schedules: BTreeMap<String, (ScheduleRow, bool)>,
    poison: BTreeMap<StreamKey, Poison>,
}

/// The store in memory. Booting another engine on the same journal is a restart: it finds what
/// was committed and nothing else; `log` is the history a shipper would have been handed.
#[derive(Default)]
pub struct MemJournal(Mutex<Mem>);

impl MemJournal {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> MutexGuard<'_, Mem> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Every event recorded, in order.
    pub fn log(&self) -> Vec<Event> {
        self.lock().log.clone()
    }

    /// Every turn committed, in order.
    pub fn commits(&self) -> Vec<Turn> {
        self.lock().commits.clone()
    }

    /// Every state key as stored.
    pub fn state(&self) -> BTreeMap<(String, String), String> {
        self.lock().state.iter().map(|(k, r)| (k.clone(), r.value.clone())).collect()
    }
}

impl Journal for MemJournal {
    fn record(&self, ev: &Event) {
        self.lock().log.push(ev.clone());
    }

    fn boot(&self) -> Result<Boot> {
        let m = self.lock();
        Ok(Boot {
            state: m.state.values().cloned().collect(),
            effects: m.effects.values().filter(|(_, resolved)| !resolved).map(|(r, _)| r.clone()).collect(),
            schedules: m.schedules.values().filter(|(_, fired)| !fired).map(|(r, _)| r.clone()).collect(),
        })
    }

    fn commit(&self, turn: &Turn) -> Result<()> {
        let mut m = self.lock();
        for s in &turn.state {
            m.state.insert((s.task.clone(), s.skey.clone()), s.clone());
        }
        for e in &turn.effects {
            m.effects.insert((e.task.clone(), e.ekey.clone()), (e.clone(), false));
        }
        for id in &turn.resolved {
            for (row, resolved) in m.effects.values_mut() {
                if &row.request_id == id {
                    *resolved = true;
                }
            }
        }
        for r in &turn.schedules {
            m.schedules.entry(r.id.clone()).or_insert((r.clone(), false));
        }
        for id in &turn.fired {
            if let Some((_, fired)) = m.schedules.get_mut(id) {
                *fired = true;
            }
        }
        m.commits.push(turn.clone());
        Ok(())
    }

    fn poison(&self, p: &Poison) -> Result<()> {
        self.lock().poison.entry(p.stream.clone()).or_insert_with(|| p.clone());
        Ok(())
    }

    fn poisoned(&self, stream: &StreamKey) -> Result<Option<Poison>> {
        Ok(self.lock().poison.get(stream).cloned())
    }

    fn history(&self) -> Vec<Event> {
        self.log()
    }
}

/// A journal that refuses one named operation once, to stand in for a crash at that point.
pub struct FaultyJournal {
    inner: Arc<dyn Journal>,
    fail: Mutex<Option<(&'static str, Class)>>,
}

impl FaultyJournal {
    pub fn new(inner: Arc<dyn Journal>) -> Arc<Self> {
        Arc::new(FaultyJournal { inner, fail: Mutex::new(None) })
    }

    /// The next call of `op` (`boot`, `commit`, `poison`) fails, once.
    pub fn fail_next(&self, op: &'static str) {
        *self.fail.lock().unwrap_or_else(|e| e.into_inner()) = Some((op, Class::Fault));
    }

    /// The next call of `op` is refused, once: the journal looked and said no.
    pub fn refuse_next(&self, op: &'static str) {
        *self.fail.lock().unwrap_or_else(|e| e.into_inner()) = Some((op, Class::Refused));
    }

    /// The next call of `op` cannot tell, once: the journal's reply was slow or lost.
    pub fn cannot_tell_next(&self, op: &'static str) {
        *self.fail.lock().unwrap_or_else(|e| e.into_inner()) = Some((op, Class::CannotTell));
    }

    fn gate(&self, op: &'static str) -> Result<()> {
        let mut f = self.fail.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((want, class)) = f.filter(|(want, _)| *want == op) {
            *f = None;
            let m = format!("injected fault in {want}");
            return Err(match class {
                Class::Refused => RuntimeError::refused(format!("journal: {m}")),
                Class::CannotTell => RuntimeError::cannot_tell(format!("cannot tell: {m}")),
                Class::Fault => RuntimeError::new(m),
            });
        }
        Ok(())
    }
}

impl Journal for FaultyJournal {
    fn record(&self, ev: &Event) {
        self.inner.record(ev)
    }
    fn boot(&self) -> Result<Boot> {
        self.gate("boot")?;
        self.inner.boot()
    }
    fn commit(&self, turn: &Turn) -> Result<()> {
        self.gate("commit")?;
        self.inner.commit(turn)
    }
    fn poison(&self, p: &Poison) -> Result<()> {
        self.gate("poison")?;
        self.inner.poison(p)
    }
    fn poisoned(&self, stream: &StreamKey) -> Result<Option<Poison>> {
        self.inner.poisoned(stream)
    }
    fn history(&self) -> Vec<Event> {
        self.inner.history()
    }
    fn shipped(&self) -> Option<ShipCounts> {
        self.inner.shipped()
    }
}
