//! A build phase is ended for making no progress, never for taking long: progress is a
//! compiler output line or CPU time in the phase's process group.

/// The status a watched phase returns when it was ended for a stall.
pub const RC: i32 = 122;

pub struct Watch {
    bound: u64,
    since: u64,
    cpu: u64,
    last: String,
}

impl Watch {
    pub fn new(bound: u64, now: u64) -> Self {
        Watch { bound, since: now, cpu: 0, last: String::new() }
    }

    pub fn line(&mut self, now: u64, line: &str) {
        self.since = now;
        if !line.trim().is_empty() {
            self.last = line.trim().to_string();
        }
    }

    /// `ticks` is the group's cumulative CPU time; only growth is progress.
    pub fn cpu(&mut self, now: u64, ticks: u64) {
        if ticks > self.cpu {
            self.cpu = ticks;
            self.since = now;
        }
    }

    pub fn stalled(&self, now: u64) -> bool {
        now.saturating_sub(self.since) >= self.bound
    }

    pub fn last(&self) -> &str {
        &self.last
    }

    pub fn message(&self) -> String {
        format!(
            "gate: build-stall — no compiler output and no CPU time in the phase's process group for {}s; last compiler output: {}",
            self.bound,
            if self.last.is_empty() { "(none)" } else { &self.last }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_every_interval_under_the_bound_never_stalls() {
        let mut w = Watch::new(300, 0);
        for t in (60..=900).step_by(60) {
            w.line(t, "Compiling x");
            assert!(!w.stalled(t));
        }
    }

    #[test]
    fn cpu_growth_is_progress_and_a_flat_counter_is_not() {
        let mut w = Watch::new(300, 0);
        w.cpu(200, 50);
        assert!(!w.stalled(450));
        w.cpu(400, 50);
        assert!(w.stalled(500));
    }
}
