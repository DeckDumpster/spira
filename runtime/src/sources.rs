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
