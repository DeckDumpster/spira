//! `scheduler status|halt|resume` on the runtime's control socket. Status is the engine's own
//! picture as of its last turn, one JSON document; halt and resume are journaled events.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use runtime::engine::{LastOutcome, SchedulerStatus, StatusBoard};
use runtime::ports::Clock;
use runtime::{EventKind, RuntimeError, HALT_STREAM, SCHEDULER_HALTED, SCHEDULER_RESUMED};

/// Bumped when a field changes meaning or goes away.
pub const SCHEMA: u64 = 1;

pub(crate) const NO_PASS: &str = "cannot tell: no call pass has run yet";

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
                "last_commands": t.last_commands,
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
    repin: Option<Arc<crate::repin::Repin>>,
    seq: AtomicU64,
}

impl Control {
    pub fn new(board: Arc<StatusBoard>, submit: SubmitFn, clock: Arc<dyn Clock>) -> Self {
        Control { board, submit, clock, repin: None, seq: AtomicU64::new(0) }
    }

    pub fn with_repin(mut self, repin: Arc<crate::repin::Repin>) -> Self {
        self.repin = Some(repin);
        self
    }

    pub fn respond(&self, argv: &[String]) -> runtime::Result<String> {
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        match argv.as_slice() {
            ["scheduler", "status"] => {
                let s = self.board.get().ok_or_else(|| RuntimeError::new("cannot tell: no turn has run yet"))?;
                let mut doc = document(&s);
                if let Some(r) = &self.repin {
                    doc["repin"] = r.document();
                }
                Ok(doc.to_string())
            }
            ["scheduler", "repin", key] => {
                let r = self.repin.as_ref().ok_or_else(|| RuntimeError::refused("this scheduler re-pins nothing"))?;
                let answer = r.request(key).map_err(RuntimeError::refused)?;
                // A pinned key told unchanged needs its turn too: the tasks that read it are woken.
                if answer["state"] == "pending" || (answer["state"] == "unchanged" && r.held(key).is_some()) {
                    let id = format!("{}/{}/{}/{key}", crate::repin::WANTED, self.clock.now_ms(), self.seq.fetch_add(1, Ordering::SeqCst));
                    (self.submit)(&id, crate::repin::STREAM, EventKind::new(crate::repin::WANTED, key)).map_err(RuntimeError::new)?;
                }
                Ok(answer.to_string())
            }
            ["scheduler", "running", key] => {
                let r = self.repin.as_ref().ok_or_else(|| RuntimeError::refused("this scheduler re-pins nothing"))?;
                r.held(key).ok_or_else(|| RuntimeError::refused(format!("{key} is not a key the scheduler pins per task")))
            }
            ["scheduler", "bound"] => {
                let bound = self.call_carry()?.bound.ok_or_else(|| RuntimeError::new(NO_PASS))?;
                serde_json::to_string(&bound).map_err(|e| RuntimeError::new(e.to_string()))
            }
            ["scheduler", "uncalled"] => {
                let uncalled = self.call_carry()?.uncalled.ok_or_else(|| RuntimeError::new(NO_PASS))?;
                serde_json::to_string(&uncalled).map_err(|e| RuntimeError::new(e.to_string()))
            }
            ["scheduler", "halt", reason @ ..] => self.gate(SCHEDULER_HALTED, &reason.join(" ")),
            ["scheduler", "resume"] => self.gate(SCHEDULER_RESUMED, ""),
            _ => Err(RuntimeError::refused(format!("unknown request {argv:?} (scheduler status|halt [reason]|resume|repin <key>|running <key>|bound|uncalled)"))),
        }
    }

    fn call_carry(&self) -> runtime::Result<crate::tasks::a::sentinel_call::Carry> {
        sentinel_carry(&self.board)
    }

    fn gate(&self, tag: &str, reason: &str) -> runtime::Result<String> {
        let id = format!("{tag}/{}/{}", self.clock.now_ms(), self.seq.fetch_add(1, Ordering::SeqCst));
        (self.submit)(&id, HALT_STREAM, EventKind::new(tag, reason)).map_err(RuntimeError::new)?;
        Ok(format!("{tag} {id}"))
    }
}

/// The sentinel_call carry as the status board last saw it; the one read `scheduler bound|uncalled` and the reconciler's fleet check share.
pub fn sentinel_carry(board: &runtime::engine::StatusBoard) -> runtime::Result<crate::tasks::a::sentinel_call::Carry> {
    let s = board.get().ok_or_else(|| RuntimeError::new(NO_PASS))?;
    let carried = s.tasks.iter().find(|t| t.name == crate::tasks::a::sentinel_call::NAME).and_then(|t| t.state.as_ref());
    match carried {
        Some(c) => serde_json::from_str(&c.body).map_err(|e| RuntimeError::new(format!("the carried call state does not decode: {e}"))),
        None => Ok(Default::default()),
    }
}
