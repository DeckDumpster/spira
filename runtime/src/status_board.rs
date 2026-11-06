//! The scheduler status as of the engine's last turn, for a reader on another thread. Kept out
//! of the engine (a handler module, law-an-event-loop-never-blocks): the lock is held only for a
//! clone or a store, never across I/O, so a reader delays the engine's publish by one clone.

use crate::engine::SchedulerStatus;

#[derive(Default)]
pub struct StatusBoard(std::sync::Mutex<Option<SchedulerStatus>>);

impl StatusBoard {
    pub fn get(&self) -> Option<SchedulerStatus> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(crate) fn put(&self, s: SchedulerStatus) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(s);
    }
}
