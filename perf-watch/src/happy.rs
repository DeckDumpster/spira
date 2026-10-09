//! The sim happy path measured against its design goal, outside any round. The goal is a
//! target the watcher reports against, never an assertion in the corpus: the clock and the sim
//! are traits so a test scripts both.

use crate::Clock;

pub trait Sim {
    /// Whether a round VM pass is certifying right now.
    fn round_open(&mut self) -> Result<bool, String>;
    /// Run the scenario to its end and return the world's `exec.log`.
    fn run(&mut self) -> Result<String, String>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Skipped,
    Measured { millis: u64, slowest: Option<(String, u64)> },
    Failed(String),
}

const PHASE_WIDTH: usize = 44;

/// The costliest command in an `exec.log` (`=== t=<n> <command> exit status: <rc> in <ms>ms`).
pub fn slowest_phase(exec_log: &str) -> Option<(String, u64)> {
    exec_log
        .lines()
        .filter_map(|l| {
            let body = l.strip_prefix("=== ")?.strip_suffix("ms")?;
            let (head, ms) = body.rsplit_once(" in ")?;
            let head = head.rsplit_once(" exit status: ")?.0;
            let cmd = head.strip_prefix("t=")?.split_once(' ')?.1;
            Some((cmd.chars().take(PHASE_WIDTH).collect::<String>(), ms.parse::<u64>().ok()?))
        })
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
}

pub fn measure(clock: &dyn Clock, sim: &mut dyn Sim) -> Outcome {
    match sim.round_open() {
        Ok(false) => {}
        Ok(true) => return Outcome::Skipped,
        Err(e) => return Outcome::Failed(format!("round state unknown, run withheld: {e}")),
    }
    let t0 = clock.now_ms();
    let r = sim.run();
    let millis = clock.now_ms().saturating_sub(t0);
    match r {
        Ok(log) => Outcome::Measured { millis, slowest: slowest_phase(&log) },
        Err(e) => Outcome::Failed(e),
    }
}

/// `<epoch>\t<ms>` lines, oldest first; the last `n` readings.
pub fn trend(history: &str, n: usize) -> Vec<u64> {
    let all: Vec<u64> = history.lines().filter_map(|l| l.split_once('\t')?.1.trim().parse().ok()).collect();
    all[all.len().saturating_sub(n)..].to_vec()
}

pub fn history_line(epoch: u64, millis: u64) -> String {
    format!("{epoch}\t{millis}\n")
}

/// The alarm for one pass, or nothing when it met the goal, was withheld, or the round was open.
pub fn alarm(outcome: &Outcome, goal_ms: u64, recent: &[u64]) -> Option<String> {
    match outcome {
        Outcome::Skipped => None,
        Outcome::Failed(e) => Some(format!("PERF UNRUNNABLE happy-path: {e}")),
        Outcome::Measured { millis, slowest } if *millis > goal_ms => {
            let phase = slowest.as_ref().map_or("unknown".to_string(), |(c, ms)| format!("{c} {ms}ms"));
            let tr = recent.iter().map(u64::to_string).collect::<Vec<_>>().join(" ");
            Some(format!("PERF SLOW happy-path {millis}ms (goal {goal_ms}ms) slowest phase: {phase}; recent ms: {tr}"))
        }
        Outcome::Measured { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Stub(Cell<u64>);
    impl Clock for Stub {
        fn now_ms(&self) -> u64 {
            self.0.get()
        }
    }

    struct FakeSim<'a> {
        clock: &'a Stub,
        round: Result<bool, String>,
        cost: u64,
        log: Result<String, String>,
        ran: bool,
    }
    impl Sim for FakeSim<'_> {
        fn round_open(&mut self) -> Result<bool, String> {
            self.round.clone()
        }
        fn run(&mut self) -> Result<String, String> {
            self.ran = true;
            self.clock.0.set(self.clock.0.get() + self.cost);
            self.log.clone()
        }
    }

    const LOG: &str = "=== t=1 file bead exit status: 0 in 900ms\n=== t=2 gate-worker pass --all exit status: 0 in 41000ms\n=== t=3 publish exit status: 0 in 2500ms\nnoise\n";

    fn sim<'a>(clock: &'a Stub, round: Result<bool, String>, cost: u64) -> FakeSim<'a> {
        FakeSim { clock, round, cost, log: Ok(LOG.to_string()), ran: false }
    }

    #[test]
    fn the_slowest_phase_is_named_with_its_time() {
        assert_eq!(slowest_phase(LOG), Some(("gate-worker pass --all".to_string(), 41000)));
        assert_eq!(slowest_phase("nothing here\n"), None);
    }

    #[test]
    fn a_run_over_the_goal_alarms_naming_the_slowest_phase_and_the_trend() {
        let clock = Stub(Cell::new(0));
        let mut s = sim(&clock, Ok(false), 73_400);
        let o = measure(&clock, &mut s);
        let line = alarm(&o, 60_000, &[52_000, 58_000, 73_400]).unwrap();
        assert_eq!(line, "PERF SLOW happy-path 73400ms (goal 60000ms) slowest phase: gate-worker pass --all 41000ms; recent ms: 52000 58000 73400");
    }

    #[test]
    fn a_run_at_or_under_the_goal_is_silent_but_still_measured() {
        let clock = Stub(Cell::new(0));
        for cost in [59_000, 60_000] {
            let o = measure(&clock, &mut sim(&clock, Ok(false), cost));
            assert!(matches!(o, Outcome::Measured { millis, .. } if millis == cost));
            assert_eq!(alarm(&o, 60_000, &[]), None);
        }
    }

    #[test]
    fn an_open_round_withholds_the_run_entirely() {
        let clock = Stub(Cell::new(0));
        let mut s = sim(&clock, Ok(true), 999_000);
        assert_eq!(measure(&clock, &mut s), Outcome::Skipped);
        assert!(!s.ran);
        assert_eq!(alarm(&Outcome::Skipped, 60_000, &[]), None);
    }

    #[test]
    fn an_unknowable_round_state_is_an_alarm_and_never_a_run() {
        let clock = Stub(Cell::new(0));
        let mut s = sim(&clock, Err("no answer".into()), 1);
        let o = measure(&clock, &mut s);
        assert!(!s.ran);
        assert!(alarm(&o, 60_000, &[]).unwrap().starts_with("PERF UNRUNNABLE happy-path: round state unknown"));
    }

    #[test]
    fn a_failed_sim_run_is_its_own_alarm_never_a_fast_pass() {
        let clock = Stub(Cell::new(0));
        let mut s = sim(&clock, Ok(false), 5);
        s.log = Err("sim exited 1".into());
        let o = measure(&clock, &mut s);
        assert_eq!(alarm(&o, 60_000, &[]), Some("PERF UNRUNNABLE happy-path: sim exited 1".to_string()));
    }

    #[test]
    fn trend_keeps_the_last_readings_and_skips_junk() {
        let h = format!("{}{}junk\n{}", history_line(1, 10), history_line(2, 20), history_line(3, 30));
        assert_eq!(trend(&h, 2), vec![20, 30]);
        assert_eq!(trend("", 5), Vec::<u64>::new());
    }
}
