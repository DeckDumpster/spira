//! The two-process test executor: runtime processes share one journal and own streams through
//! leases. A node dispatches only the streams it holds; a lease whose holder stops renewing is
//! stolen at the next epoch, and the journal refuses the old holder's writes from then on.
//! Test-only: a node rescans the journal from its start whenever its set of streams changes.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::conformance::Rig;
use crate::engine::{Engine, Executor, Outcome, PendingEffect};
use crate::event::*;
use crate::journal::{Journal, LeaseTable, Offset, Poison, ScheduleRow, LEASE_LOST};
use crate::ports::{Clock, VirtualClock};
use crate::{Result, RuntimeError};

const SCAN: usize = 512;

#[derive(Clone, Debug)]
pub struct LeaseConfig {
    pub node: String,
    pub ttl_ms: u64,
}

#[derive(Default)]
struct NodeState {
    owned: BTreeMap<StreamKey, u64>,
    known: BTreeSet<StreamKey>,
    scanned: Offset,
    renewed_at: u64,
    changed: bool,
}

/// One node's view of the shared journal: reads show only the streams it holds, writes are
/// fenced by its lease epochs, and its cursor lives in this process.
pub struct NodeJournal {
    inner: Arc<dyn Journal>,
    clock: Arc<dyn Clock>,
    cfg: LeaseConfig,
    cursor: AtomicU64,
    st: Mutex<NodeState>,
}

impl NodeJournal {
    pub fn new(inner: Arc<dyn Journal>, clock: Arc<dyn Clock>, cfg: LeaseConfig) -> Result<Arc<Self>> {
        if inner.leases().is_none() {
            return Err(RuntimeError::new("journal has no leases"));
        }
        Ok(Arc::new(NodeJournal { inner, clock, cfg, cursor: AtomicU64::new(0), st: Mutex::new(NodeState::default()) }))
    }

    fn table(&self) -> &dyn LeaseTable {
        self.inner.leases().expect("checked at construction")
    }

    fn lock(&self) -> MutexGuard<'_, NodeState> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn claim(&self, st: &mut NodeState, stream: &StreamKey) -> Result<()> {
        let got = self.table().acquire(stream, &self.cfg.node, self.cfg.ttl_ms, self.clock.now_ms())?;
        match (got, st.owned.get(stream)) {
            (Some(l), Some(&e)) if e == l.epoch => {}
            (Some(l), _) => {
                st.owned.insert(stream.clone(), l.epoch);
                st.changed = true;
            }
            (None, Some(_)) => {
                st.owned.remove(stream);
                st.changed = true;
            }
            (None, None) => {}
        }
        Ok(())
    }

    fn renew_owned(&self, st: &mut NodeState) -> Result<()> {
        st.renewed_at = self.clock.now_ms();
        for s in st.owned.keys().cloned().collect::<Vec<_>>() {
            self.claim(st, &s)?;
        }
        Ok(())
    }

    /// Renew what this node holds, learn the streams the journal has, take any that are free or
    /// expired. `true` when the held set changed since the last call.
    pub fn refresh(&self) -> Result<bool> {
        let mut st = self.lock();
        self.renew_owned(&mut st)?;
        loop {
            let batch = self.inner.read_from(st.scanned, SCAN)?;
            let Some((last, _)) = batch.last() else { break };
            st.scanned = last + 1;
            st.known.extend(batch.into_iter().map(|(_, e)| e.stream));
        }
        for s in st.known.clone() {
            self.claim(&mut st, &s)?;
        }
        if st.changed {
            self.cursor.store(0, Ordering::SeqCst);
        }
        Ok(std::mem::take(&mut st.changed))
    }

    /// Take `stream` for a first event the caller is about to submit.
    pub fn claim_stream(&self, stream: &StreamKey) -> Result<()> {
        let mut st = self.lock();
        st.known.insert(stream.clone());
        self.claim(&mut st, stream)?;
        if !st.owned.contains_key(stream) {
            return Err(RuntimeError::new(format!("stream {} is held by another node", stream.0)));
        }
        Ok(())
    }

    pub fn release_all(&self) {
        let mut st = self.lock();
        for s in std::mem::take(&mut st.owned).into_keys() {
            let _ = self.table().release(&s, &self.cfg.node);
        }
    }

    pub fn owned(&self) -> Vec<StreamKey> {
        self.lock().owned.keys().cloned().collect()
    }
}

impl Journal for NodeJournal {
    fn append(&self, ev: &Event) -> Result<Offset> {
        let epoch = self.lock().owned.get(&ev.stream).copied();
        let epoch = epoch.ok_or_else(|| RuntimeError::new(format!("{LEASE_LOST}: {}", ev.stream.0)))?;
        self.table().append_fenced(ev, &self.cfg.node, epoch)
    }

    fn read_from(&self, from: Offset, limit: usize) -> Result<Vec<(Offset, Event)>> {
        let owned: BTreeSet<StreamKey> = self.lock().owned.keys().cloned().collect();
        let mut out = Vec::new();
        let mut pos = from;
        while out.len() < limit {
            let batch = self.inner.read_from(pos, SCAN)?;
            let Some((last, _)) = batch.last() else { break };
            pos = last + 1;
            out.extend(batch.into_iter().filter(|(_, e)| owned.contains(&e.stream)));
        }
        out.truncate(limit);
        Ok(out)
    }

    fn applied(&self, handler: &str, id: &EventId) -> Result<bool> {
        self.inner.applied(handler, id)
    }

    fn mark_applied(&self, handler: &str, id: &EventId) -> Result<()> {
        self.inner.mark_applied(handler, id)
    }

    fn cursor(&self) -> Result<Offset> {
        Ok(self.cursor.load(Ordering::SeqCst))
    }

    fn set_cursor(&self, next: Offset) -> Result<()> {
        self.cursor.store(next, Ordering::SeqCst);
        let mut st = self.lock();
        if self.clock.now_ms().saturating_sub(st.renewed_at) >= self.cfg.ttl_ms / 3 {
            self.renew_owned(&mut st)?;
        }
        Ok(())
    }

    fn schedule(&self, row: &ScheduleRow) -> Result<()> {
        self.inner.schedule(row)
    }

    fn pending_schedules(&self) -> Result<Vec<ScheduleRow>> {
        let st = self.lock();
        Ok(self.inner.pending_schedules()?.into_iter().filter(|r| st.owned.contains_key(&r.stream)).collect())
    }

    fn mark_fired(&self, row_id: &str) -> Result<()> {
        self.inner.mark_fired(row_id)
    }

    fn poison(&self, p: &Poison) -> Result<()> {
        self.inner.poison(p)
    }

    fn poisoned(&self, stream: &StreamKey) -> Result<Option<Poison>> {
        self.inner.poisoned(stream)
    }
}

pub struct LeaseExecutor {
    view: Arc<NodeJournal>,
    clock: Arc<dyn Clock>,
    seed: u64,
    engine: Option<Engine>,
}

impl LeaseExecutor {
    pub fn open(journal: Arc<dyn Journal>, clock: Arc<dyn Clock>, regs: Vec<Registration>, seed: u64, cfg: LeaseConfig) -> Result<Self> {
        let view = NodeJournal::new(journal, clock.clone(), cfg)?;
        view.refresh()?;
        let engine = Engine::open(view.clone(), clock.clone(), regs, seed)?;
        Ok(LeaseExecutor { view, clock, seed, engine: Some(engine) })
    }

    pub fn owned(&self) -> Vec<StreamKey> {
        self.view.owned()
    }

    fn engine(&mut self) -> Result<&mut Engine> {
        self.engine.as_mut().ok_or_else(|| RuntimeError::new("executor closed by an earlier failure"))
    }

    fn reopen(&mut self) -> Result<()> {
        let regs = self.engine.take().ok_or_else(|| RuntimeError::new("executor closed by an earlier failure"))?.into_registrations();
        self.engine = Some(Engine::open(self.view.clone(), self.clock.clone(), regs, self.seed)?);
        Ok(())
    }
}

impl Drop for LeaseExecutor {
    fn drop(&mut self) {
        self.view.release_all();
    }
}

impl Executor for LeaseExecutor {
    fn submit(&mut self, id: &str, stream: &str, kind: EventKind) -> Result<()> {
        self.view.claim_stream(&StreamKey(stream.into()))?;
        self.engine()?.submit(id, stream, kind)
    }

    fn run_until_idle(&mut self) -> Result<()> {
        let mut reopen = false;
        loop {
            if reopen | self.view.refresh()? {
                self.reopen()?;
            }
            match self.engine()?.run_until_idle() {
                Ok(()) => {
                    reopen = self.view.refresh()?;
                    if !reopen {
                        return Ok(());
                    }
                }
                Err(e) if e.0.starts_with(LEASE_LOST) => {
                    reopen = self.view.refresh()?;
                    if !reopen {
                        return Err(e);
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn pending_effects(&self) -> Vec<PendingEffect> {
        self.engine.as_ref().map(|e| e.pending_effects()).unwrap_or_default()
    }

    fn complete(&mut self, key: &str, outcome: Outcome) -> Result<bool> {
        self.engine()?.complete(key, outcome)
    }
}

/// The conformance rig for the lease executor: every boot is a new node of one process, and a
/// dropped executor releases its leases, as a node that exits cleanly does.
pub struct LeaseRig<F: Fn() -> Arc<dyn Journal>> {
    pub journal: F,
    pub ttl_ms: u64,
    boots: AtomicU64,
}

impl<F: Fn() -> Arc<dyn Journal>> LeaseRig<F> {
    pub fn new(journal: F) -> Self {
        LeaseRig { journal, ttl_ms: 60_000, boots: AtomicU64::new(0) }
    }
}

impl<F: Fn() -> Arc<dyn Journal>> Rig for LeaseRig<F> {
    fn journal(&self) -> Arc<dyn Journal> {
        (self.journal)()
    }

    fn boot(&self, journal: Arc<dyn Journal>, clock: &VirtualClock, seed: u64, regs: Vec<Registration>) -> Result<Box<dyn Executor>> {
        let node = format!("node-{}", self.boots.fetch_add(1, Ordering::SeqCst));
        Ok(Box::new(LeaseExecutor::open(journal, Arc::new(clock.clone()), regs, seed, LeaseConfig { node, ttl_ms: self.ttl_ms })?))
    }
}
