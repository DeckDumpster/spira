//! Sources: what submits events to the host from outside it (a file watch, a bus listener, a
//! mail listener). Each is a future on the scheduler's runtime and owns no thread: what waits is
//! an fd or a timer on the loop, what blocks goes through [`on_pool`]. It submits through the
//! host's bounded channel, so a full channel slows the source, not the loop. Under virtual time a source runs only if it is scripted: the driver asks it when it
//! next has something and takes what is due, so one seed gives one order.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::unix::AsyncFd;
use tokio::io::Interest;
use tokio::sync::mpsc;

use crate::changes::ScriptedChanges;
use crate::event::StoreChanged;
use crate::host::Submit;
use crate::offload::{self, OffloadError};
use crate::ports::Clock;
use crate::units::{RanOutput, ScriptedUnits, UnitCompletions, UnitRan, Wake, UNIT_RAN};
use crate::{Result, RuntimeError};

/// What a source is while it runs: a future on the scheduler's runtime.
pub type Running = Pin<Box<dyn Future<Output = ()> + Send>>;

/// How long a refused pool submission waits before it is offered again.
const SPILL_RETRY: Duration = Duration::from_millis(50);

/// `f` on the offload pool, offered again after each refusal; a source never blocks the loop and
/// never drops work because the queue was full.
pub async fn on_pool<T: Send + 'static>(what: &'static str, f: Arc<dyn Fn() -> T + Send + Sync>) -> std::result::Result<T, OffloadError> {
    loop {
        let job = f.clone();
        match offload::offload(what, None, move || job()) {
            Ok(done) => return done.await,
            Err(_) => tokio::time::sleep(SPILL_RETRY).await, // clock-safe: paces a retry of the pool on the source's own task
        }
    }
}

pub trait Source: Send {
    fn name(&self) -> &str;

    /// Submits until the source ends or `tx` closes.
    fn run(self: Box<Self>, tx: mpsc::Sender<Submit>) -> Running;

    /// The virtual-time face of this source; `None` for one that reads the real world.
    fn scripted(&mut self) -> Option<&mut dyn Scripted> {
        None
    }
}

pub trait Scripted {
    /// The earliest virtual time this source has a submission for.
    fn next_at(&self) -> Option<u64>;
    /// Every submission due at or before `now_ms`, in script order.
    fn take_due(&mut self, now_ms: u64) -> Vec<Submit>;
}

/// The host's sources, by unique name.
#[derive(Default)]
pub struct Sources {
    list: Vec<Box<dyn Source>>,
}

impl Sources {
    pub fn register(&mut self, s: Box<dyn Source>) -> Result<()> {
        if self.list.iter().any(|x| x.name() == s.name()) {
            return Err(RuntimeError::refused(format!("source {} is already registered", s.name())));
        }
        self.list.push(s);
        Ok(())
    }

    pub fn names(&self) -> Vec<String> {
        self.list.iter().map(|s| s.name().to_string()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// One task per source on the current runtime, each submitting on `tx`. Panics outside a runtime.
    pub fn start(self, tx: mpsc::Sender<Submit>) -> Vec<tokio::task::JoinHandle<()>> {
        self.list.into_iter().map(|s| tokio::spawn(s.run(tx.clone()))).collect()
    }

    /// Every source, for a virtual-time driver; refuses the set if any reads the real world.
    pub fn into_scripted(mut self) -> Result<Vec<Box<dyn Source>>> {
        let real: Vec<String> = self.list.iter_mut().filter_map(|s| if s.scripted().is_none() { Some(s.name().to_string()) } else { None }).collect();
        if !real.is_empty() {
            return Err(RuntimeError::refused(format!("sources cannot run under virtual time: {}", real.join(", "))));
        }
        Ok(self.list)
    }
}

/// The earliest `next_at` over scripted sources.
pub fn next_at(sources: &mut [Box<dyn Source>]) -> Option<u64> {
    sources.iter_mut().filter_map(|s| s.scripted().and_then(|x| x.next_at())).min()
}

/// Submissions at fixed times on its clock. Under a virtual driver they are taken when due;
/// on the runtime they are sent when the clock reaches them.
pub struct ScriptedSource {
    name: String,
    clock: Arc<dyn Clock>,
    script: Vec<(u64, Submit)>,
}

impl ScriptedSource {
    /// Equal times keep the order given.
    pub fn new(name: &str, clock: Arc<dyn Clock>, mut script: Vec<(u64, Submit)>) -> Self {
        script.sort_by_key(|(at, _)| *at);
        let mut seen = BTreeSet::new();
        script.retain(|(_, s)| seen.insert(s.id.clone()));
        ScriptedSource { name: name.into(), clock, script }
    }
}

const WAIT_STEP_MS: u64 = 20;

impl Source for ScriptedSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn run(self: Box<Self>, tx: mpsc::Sender<Submit>) -> Running {
        Box::pin(async move {
            for (at, s) in self.script {
                loop {
                    let now = self.clock.now_ms();
                    if now >= at {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis((at - now).min(WAIT_STEP_MS))).await; // clock-safe: waits on the source's own task, rechecking its Clock port
                }
                if tx.send(s).await.is_err() {
                    return;
                }
            }
        })
    }

    fn scripted(&mut self) -> Option<&mut dyn Scripted> {
        Some(self)
    }
}

impl Scripted for ScriptedSource {
    fn next_at(&self) -> Option<u64> {
        self.script.first().map(|(at, _)| *at)
    }

    fn take_due(&mut self, now_ms: u64) -> Vec<Submit> {
        let n = self.script.iter().take_while(|(at, _)| *at <= now_ms).count();
        self.script.drain(..n).map(|(_, s)| s).collect()
    }
}

/// The stream a `unit.ran` is submitted on when its unit names no task the source's resolver
/// knows (or the source has none). A run of a known task goes to that task's own stream
/// ([`UnitRanSource::routed`]), so the commands its handler answers with land there too; the
/// task still recognises its own runs by the unit's name ([`UnitRan::key_for`]).
pub const UNITS_STREAM: &str = "units";

/// Task name to the stream that task's handler owns; `None` for a task it does not host.
pub type StreamOf = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

enum Face {
    /// A backend that reads the world: drained on its wake, or every `every` when it has none.
    Polled { every: Duration },
    /// The simulator's units: drained when the virtual clock reaches the next end.
    Scripted(Arc<ScriptedUnits>),
}

/// Completions as events (S6, S9): every run a [`UnitCompletions`] backend reports is submitted
/// once as `unit.ran`, so a task that started a unit hears its end through the journal. A scope
/// run's stdout and stderr tails ride the same event ([`RanOutput::from_event_kind`]).
pub struct UnitRanSource {
    name: String,
    completions: Arc<dyn UnitCompletions>,
    clock: Arc<dyn Clock>,
    face: Face,
    stream_of: Option<StreamOf>,
}

impl UnitRanSource {
    /// A real backend (systemd, child processes), drained every `every`.
    pub fn polled(name: &str, completions: Arc<dyn UnitCompletions>, clock: Arc<dyn Clock>, every: Duration) -> Self {
        UnitRanSource { name: name.into(), completions, clock, face: Face::Polled { every }, stream_of: None }
    }

    /// The simulator's backend: runs under a virtual-time driver, delivering each end when due.
    pub fn scripted(name: &str, units: Arc<ScriptedUnits>, clock: Arc<dyn Clock>) -> Self {
        UnitRanSource { name: name.into(), completions: units.clone(), clock, face: Face::Scripted(units), stream_of: None }
    }

    /// Submit each run on the stream `stream_of` gives the task its unit name parses to
    /// ([`crate::units::parse_unit_name`]); a name that parses to no task, or to one `stream_of`
    /// does not know, stays on [`UNITS_STREAM`]. Either face.
    pub fn routed(mut self, stream_of: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> Self {
        self.stream_of = Some(Arc::new(stream_of));
        self
    }

    /// The stream `r` is submitted on.
    pub fn stream_for(&self, r: &UnitRan) -> String {
        let task = r.handle.owner().map(|(task, _)| task);
        task.and_then(|t| self.stream_of.as_ref().and_then(|f| f(&t))).unwrap_or_else(|| UNITS_STREAM.to_string())
    }

    /// The real face: a drain at start, then one per wake (or per `every` for a backend with
    /// nothing to wait on), each submission awaited so a full channel holds the rest in the
    /// socket and the backend's pending signals.
    async fn run_polled(self, every: Duration, tx: mpsc::Sender<Submit>) {
        let wake = match self.completions.wake() {
            Ok(w) => w,
            Err(e) => {
                let line = format!("runtime: source {}: no wake, draining every {every:?}: {e}", self.name);
                let _ = on_pool("source", Arc::new(move || eprintln!("{line}"))).await;
                None
            }
        };
        let (afd, pending) = match wake {
            Some(Wake { fd, pending }) => match AsyncFd::with_interest(fd, Interest::READABLE) {
                Ok(afd) => (Some(afd), Some(pending)),
                Err(e) => {
                    let line = format!("runtime: source {}: cannot wait on the bus, draining every {every:?}: {e}", self.name);
                    let _ = on_pool("source", Arc::new(move || eprintln!("{line}"))).await;
                    (None, None)
                }
            },
            None => (None, None),
        };
        loop {
            let done = self.completions.clone();
            let drained = on_pool("unit.ran", Arc::new(move || done.ended())).await;
            let now = self.clock.now_ms();
            let healthy = matches!(drained, Ok(Ok(_)));
            for (r, out) in drained.ok().and_then(|d| d.ok()).unwrap_or_default() {
                if tx.send(self.submit_of(&r, &out, now)).await.is_err() {
                    return;
                }
            }
            let retry = tokio::time::sleep(every); // clock-safe: paces the retry and the poll of a backend with no wake on the source's own task
            tokio::pin!(retry);
            match (&afd, &pending) {
                (Some(afd), Some(pending)) if healthy => tokio::select! {
                    ready = afd.readable() => match ready {
                        Ok(mut g) => g.clear_ready(),
                        Err(_) => retry.as_mut().await,
                    },
                    _ = pending.notified() => {}
                    _ = tx.closed() => return,
                },
                _ => tokio::select! {
                    _ = &mut retry => {}
                    _ = tx.closed() => return,
                },
            }
        }
    }

    fn submit_of(&self, r: &UnitRan, out: &RanOutput, now_ms: u64) -> Submit {
        Submit::new(&format!("{UNIT_RAN}/{}/{now_ms}", r.handle.name), &self.stream_for(r), r.event_kind_with(out))
    }

    /// Every run ended since the last drain, as submissions; a backend fault drains nothing and
    /// is retried on the next drain.
    fn drain(&self) -> Vec<Submit> {
        let now = self.clock.now_ms();
        self.completions.ended().unwrap_or_default().iter().map(|(r, out)| self.submit_of(r, out, now)).collect()
    }
}

impl Source for UnitRanSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn run(self: Box<Self>, tx: mpsc::Sender<Submit>) -> Running {
        match self.face {
            Face::Polled { every } => Box::pin(self.run_polled(every, tx)),
            Face::Scripted(_) => Box::pin(async move {
                while !tx.is_closed() {
                    for s in self.drain() {
                        if tx.send(s).await.is_err() {
                            return;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(WAIT_STEP_MS)).await; // clock-safe: paces the drain on the source's own task; the backend stamps each run
                }
            }),
        }
    }

    fn scripted(&mut self) -> Option<&mut dyn Scripted> {
        match self.face {
            Face::Scripted(_) => Some(self),
            Face::Polled { .. } => None,
        }
    }
}

impl Scripted for UnitRanSource {
    fn next_at(&self) -> Option<u64> {
        match &self.face {
            Face::Scripted(u) => u.next_end_at(),
            Face::Polled { .. } => None,
        }
    }

    /// Drained every turn the driver takes: whatever ended by now, each once.
    fn take_due(&mut self, _now_ms: u64) -> Vec<Submit> {
        self.drain()
    }
}

/// The `store.changed` source's virtual-time stand-in: at each entry's time it advances `changes`
/// to the entry's revision and submits the event, except the swallowed entry, which only advances
/// `changes`.
pub struct ScriptedStoreChanges {
    clock: Arc<dyn Clock>,
    changes: Arc<ScriptedChanges>,
    script: Vec<(u64, StoreChanged, bool)>,
}

impl ScriptedStoreChanges {
    pub const NAME: &'static str = "store.changed";

    /// Equal times keep the order given.
    pub fn new(clock: Arc<dyn Clock>, changes: Arc<ScriptedChanges>, mut script: Vec<(u64, StoreChanged)>) -> Self {
        script.sort_by_key(|(at, _)| *at);
        ScriptedStoreChanges { clock, changes, script: script.into_iter().map(|(at, c)| (at, c, false)).collect() }
    }

    /// The entry at `index` (in time order) advances the changes but is never submitted.
    pub fn swallowing(mut self, index: usize) -> Self {
        if let Some(e) = self.script.get_mut(index) {
            e.2 = true;
        }
        self
    }

    fn deliver(&self, c: &StoreChanged, swallowed: bool) -> Option<Submit> {
        self.changes.set(c.rev);
        (!swallowed).then(|| c.submit())
    }
}

impl Source for ScriptedStoreChanges {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn run(self: Box<Self>, tx: mpsc::Sender<Submit>) -> Running {
        Box::pin(async move {
            for (at, c, swallowed) in &self.script {
                loop {
                    let now = self.clock.now_ms();
                    if now >= *at {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis((*at - now).min(WAIT_STEP_MS))).await; // clock-safe: waits on the source's own task, rechecking its Clock port
                }
                if let Some(s) = self.deliver(c, *swallowed) {
                    if tx.send(s).await.is_err() {
                        return;
                    }
                }
            }
        })
    }

    fn scripted(&mut self) -> Option<&mut dyn Scripted> {
        Some(self)
    }
}

impl Scripted for ScriptedStoreChanges {
    fn next_at(&self) -> Option<u64> {
        self.script.first().map(|(at, _, _)| *at)
    }

    fn take_due(&mut self, now_ms: u64) -> Vec<Submit> {
        let n = self.script.iter().take_while(|(at, _, _)| *at <= now_ms).count();
        let due: Vec<_> = self.script.drain(..n).collect();
        due.iter().filter_map(|(_, c, swallowed)| self.deliver(c, *swallowed)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tv6hrjv)]
    #[tokio::test]
    async fn on_pool_returns_what_its_work_returned() {
        let got = on_pool("probe", Arc::new(|| 7)).await.unwrap();
        assert_eq!(got, 7);
    }
}
