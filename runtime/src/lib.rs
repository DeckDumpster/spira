//! runtime — the event/handler contract, the `Journal` trait with an in-memory and a
//! lifecycle-store implementation, the scheduler, the in-process executor and the conformance
//! suite every executor must pass.

pub mod bridge;
pub mod conformance;
pub mod engine;
pub mod event;
pub mod host;
pub mod journal;
pub mod ports;
pub mod scheduler;
pub mod store;

pub use engine::{Engine, Executor, Outcome, PendingEffect};
pub use event::*;
pub use journal::{FaultyJournal, Journal, MemJournal, Offset, Poison, ScheduleRow};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeError(pub String);

impl RuntimeError {
    pub fn new(m: impl Into<String>) -> Self {
        RuntimeError(m.into())
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RuntimeError {}

pub type Result<T> = std::result::Result<T, RuntimeError>;
