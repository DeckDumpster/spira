//! `scheduler status|halt|resume` on the runtime's control socket. Status is the engine's own
//! picture as of its last turn, one JSON document; halt and resume are journaled events.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use runtime::engine::{LastOutcome, SchedulerStatus, StatusBoard};
use runtime::ports::Clock;
use runtime::{EventKind, RuntimeError, HALT_STREAM, SCHEDULER_HALTED, SCHEDULER_RESUMED};

/// Bumped when a field changes meaning or goes away.
pub const SCHEMA: u64 = 1;

pub type SubmitFn = Arc<dyn Fn(&str, &str, EventKind) -> Result<(), String> + Send + Sync>;

pub fn document(s: &SchedulerStatus) -> serde_json::Value {
    let tasks: Vec<_> = s
        .tasks
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "mode": t.mode.as_str(),
                "plane": t.plane.as_str(),
                "effects": t.effects.iter().map(|(name, plane)| serde_json::json!({ "name": name, "plane": plane.as_str() })).collect::<Vec<_>>(),
                "state": t.state.as_ref().map(|c| serde_json::json!({ "stream": c.stream, "tag": c.tag, "body_len": c.body_len })),
                "last_fired_at": t.last_fired_at,
                "last_due_at": t.last_due_at,
                "lag_ms": t.lag_ms,
                "next_due_at": t.next_due_at,
                "last_outcome": t.last_outcome.map(|o| match o {
                    LastOutcome::Succeeded => "succeeded",
                    LastOutcome::Failed => "failed",
                    LastOutcome::TimedOut => "timed_out",
                    LastOutcome::Deferred => "deferred",
                    LastOutcome::Quiet => "quiet",
                }),
                "in_flight": t.in_flight,
            })
        })
        .collect();
    let classes: Vec<_> = s.classes.iter().map(|c| serde_json::json!({ "class": c.class, "depth": c.depth, "refusals": c.refusals })).collect();
    serde_json::json!({ "schema": SCHEMA, "at": s.at, "halted": s.halted, "held": s.held, "tasks": tasks, "classes": classes })
}

pub struct Control {
    pub board: Arc<StatusBoard>,
    /// Journals one event; the host's submission channel.
    pub submit: SubmitFn,
    pub clock: Arc<dyn Clock>,
    seq: AtomicU64,
}

impl Control {
    pub fn new(board: Arc<StatusBoard>, submit: SubmitFn, clock: Arc<dyn Clock>) -> Self {
        Control { board, submit, clock, seq: AtomicU64::new(0) }
    }

    pub fn respond(&self, argv: &[String]) -> runtime::Result<String> {
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        match argv.as_slice() {
            ["scheduler", "status"] => {
                let s = self.board.get().ok_or_else(|| RuntimeError::new("cannot tell: no turn has run yet"))?;
                Ok(document(&s).to_string())
            }
            ["scheduler", "halt", reason @ ..] => self.gate(SCHEDULER_HALTED, &reason.join(" ")),
            ["scheduler", "resume"] => self.gate(SCHEDULER_RESUMED, ""),
            _ => Err(RuntimeError::refused(format!("unknown request {argv:?} (scheduler status|halt [reason]|resume)"))),
        }
    }

    /// The reply says the request is journaled; the engine acts on it when it reads it.
    fn gate(&self, tag: &str, reason: &str) -> runtime::Result<String> {
        let id = format!("{tag}/{}/{}", self.clock.now_ms(), self.seq.fetch_add(1, Ordering::SeqCst));
        (self.submit)(&id, HALT_STREAM, EventKind::new(tag, reason)).map_err(RuntimeError::new)?;
        Ok(format!("{tag} {id}"))
    }
}
