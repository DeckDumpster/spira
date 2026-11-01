//! The server's monotonic clock for lock and key TTLs; tests inject `runtime::ports::VirtualClock`.

use std::time::Instant;

use runtime::ports::Clock;

pub struct MonotonicClock(Instant);

impl MonotonicClock {
    pub fn new() -> Self {
        MonotonicClock(Instant::now())
    }
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for MonotonicClock {
    fn now_ms(&self) -> u64 {
        self.0.elapsed().as_millis() as u64
    }
}
