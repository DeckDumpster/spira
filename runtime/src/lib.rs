//! runtime — the event/handler contract, the `Journal` trait with an in-memory and a
//! lifecycle-store implementation, the scheduler, the in-process executor and the conformance
//! suite every executor must pass.

pub mod bridge;
pub mod conformance;
pub mod engine;
pub mod event;
pub mod heartbeat;
pub mod host;
pub mod file_journal;
pub mod journal;
pub mod lease;
pub mod marker;
pub mod offload;
pub mod ports;
pub mod scheduler;
pub mod socket;
pub mod wide;

pub use engine::{Engine, Executor, Outcome, PendingEffect};
pub use event::*;
pub use journal::{FaultyJournal, Journal, Lease, LeaseTable, MemJournal, Offset, Poison, ScheduleRow};
pub use marker::{FileMarker, Mark, Marker, MemMarker, NullMarker};
pub use wide::{Fields, MemSink, NullSink, Value, Wide, WideSink};

/// `Refused` is deterministic (the journal looked and said no); `Fault` may clear on restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Fault,
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeError(pub String, pub Class);

impl RuntimeError {
    pub fn new(m: impl Into<String>) -> Self {
        RuntimeError(m.into(), Class::Fault)
    }

    pub fn refused(m: impl Into<String>) -> Self {
        RuntimeError(m.into(), Class::Refused)
    }

    pub fn is_refused(&self) -> bool {
        self.1 == Class::Refused
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RuntimeError {}

pub type Result<T> = std::result::Result<T, RuntimeError>;
