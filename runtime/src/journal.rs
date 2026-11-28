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
/// The key a whole carry is stored under, for a task registered without a row codec; a task with one has this row spent.
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
    /// (task, skey) of state rows to delete: keys a task's carry no longer holds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cleared: Vec<(String, String)>,
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
        self.state.is_empty() && self.cleared.is_empty() && self.effects.is_empty() && self.resolved.is_empty() && self.schedules.is_empty() && self.fired.is_empty()
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
    effects: BTreeMap<(String, String), EffectRow>,
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
            effects: m.effects.values().cloned().collect(),
            schedules: m.schedules.values().filter(|(_, fired)| !fired).map(|(r, _)| r.clone()).collect(),
        })
    }

    fn commit(&self, turn: &Turn) -> Result<()> {
        crate::socket::within_widths(turn)?;
        let mut m = self.lock();
        for s in &turn.state {
            m.state.insert((s.task.clone(), s.skey.clone()), s.clone());
        }
        for key in &turn.cleared {
            m.state.remove(key);
        }
        for e in &turn.effects {
            m.effects.insert((e.task.clone(), e.ekey.clone()), e.clone());
        }
        m.effects.retain(|_, row| !turn.resolved.contains(&row.request_id));
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
    fail: Mutex<Option<(&'static str, Class, u32)>>,
    gone: Mutex<bool>,
}

impl FaultyJournal {
    pub fn new(inner: Arc<dyn Journal>) -> Arc<Self> {
        Arc::new(FaultyJournal { inner, fail: Mutex::new(None), gone: Mutex::new(false) })
    }

    /// The next call of `op` fails, once; `append` is a crash: that event is dropped and every later call fails.
    pub fn fail_next(&self, op: &'static str) {
        *self.fail.lock().unwrap_or_else(|e| e.into_inner()) = Some((op, Class::Fault, 1));
    }

    /// The next call of `op` is refused, once: the journal looked and said no.
    pub fn refuse_next(&self, op: &'static str) {
        *self.fail.lock().unwrap_or_else(|e| e.into_inner()) = Some((op, Class::Refused, 1));
    }

    /// The next call of `op` cannot tell, once: the journal's reply was slow or lost.
    pub fn cannot_tell_next(&self, op: &'static str) {
        self.cannot_tell_times(op, 1);
    }

    /// The next `n` calls of `op` cannot tell, one after another.
    pub fn cannot_tell_times(&self, op: &'static str, n: u32) {
        *self.fail.lock().unwrap_or_else(|e| e.into_inner()) = Some((op, Class::CannotTell, n));
    }

    fn gate(&self, op: &'static str) -> Result<()> {
        if *self.gone.lock().unwrap_or_else(|e| e.into_inner()) {
            return Err(RuntimeError::new("injected fault in append"));
        }
        let mut f = self.fail.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((want, class, left)) = f.filter(|(want, _, _)| *want == op) {
            *f = (left > 1).then_some((want, class, left - 1));
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
        let mut gone = self.gone.lock().unwrap_or_else(|e| e.into_inner());
        let mut f = self.fail.lock().unwrap_or_else(|e| e.into_inner());
        if f.is_some_and(|(want, _, _)| want == "append") {
            *f = None;
            *gone = true;
        }
        if !*gone {
            self.inner.record(ev)
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(id: &str) -> Event {
        Event { id: EventId(id.into()), stream: StreamKey("s".into()), kind: EventKind::new("t", ""), at: 0, cause: None, correlation: crate::event::CorrelationId(id.into()) }
    }

    #[test_id(tcwvht2)]
    #[test]
    fn an_append_fault_keeps_nothing_from_that_event_on_and_fails_every_later_call() {
        let inner = MemJournal::new();
        let faulty = FaultyJournal::new(inner.clone());
        faulty.record(&ev("before"));
        faulty.fail_next("append");
        faulty.record(&ev("lost"));
        faulty.record(&ev("after"));
        let kept: Vec<String> = inner.history().into_iter().map(|e| e.id.0).collect();
        assert_eq!(kept, ["before"]);
        assert_eq!(faulty.commit(&Turn::default()).unwrap_err().to_string(), "injected fault in append");
        assert_eq!(faulty.boot().unwrap_err().to_string(), "injected fault in append");
        assert!(inner.boot().is_ok(), "the inner journal is what a restart opens");
    }
}
