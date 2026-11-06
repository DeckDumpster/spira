//! Sources: what submits events to the host from outside it (a file watch, a bus listener, a
//! mail listener). Each runs on its own thread, so a blocking read never touches the loop, and
//! submits through the host's bounded channel, so a full channel slows the source, not the
//! loop. Under virtual time a source runs only if it is scripted: the driver asks it when it
//! next has something and takes what is due, so one seed gives one order.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::host::Submit;
use crate::ports::Clock;
use crate::units::{RanOutput, ScriptedUnits, UnitCompletions, UnitRan, UNIT_RAN};
use crate::{Result, RuntimeError};

pub trait Source: Send {
    fn name(&self) -> &str;

    /// Submits until the source ends or `tx` closes. Runs on a thread of its own.
    fn run(self: Box<Self>, tx: mpsc::Sender<Submit>);

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

    /// One named thread per source, each submitting on `tx`.
    pub fn start(self, tx: mpsc::Sender<Submit>) -> Result<Vec<std::thread::JoinHandle<()>>> {
        self.list
            .into_iter()
            .map(|s| {
                let tx = tx.clone();
                std::thread::Builder::new().name(format!("source-{}", s.name())).spawn(move || s.run(tx)).map_err(|e| RuntimeError::new(format!("source thread: {e}")))
            })
            .collect()
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
/// on a thread they are sent when the clock reaches them.
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

    fn run(self: Box<Self>, tx: mpsc::Sender<Submit>) {
        for (at, s) in self.script {
            loop {
                let now = self.clock.now_ms();
                if now >= at {
                    break;
                }
                std::thread::sleep(Duration::from_millis((at - now).min(WAIT_STEP_MS))); // clock-safe: waits on the source's own thread, rechecking its Clock port
            }
            if tx.blocking_send(s).is_err() {
                return;
            }
        }
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
    /// A backend that reads the world: drained every `every` on the source's thread.
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

    fn run(self: Box<Self>, tx: mpsc::Sender<Submit>) {
        let every = match self.face {
            Face::Polled { every } => every,
            Face::Scripted(_) => Duration::from_millis(WAIT_STEP_MS),
        };
        while !tx.is_closed() {
            for s in self.drain() {
                if tx.blocking_send(s).is_err() {
                    return;
                }
            }
            std::thread::sleep(every); // clock-safe: paces the drain on the source's own thread; the backend stamps each run
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
