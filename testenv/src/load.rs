//! Host-load admission for a capped (no `--deadline`) run: wait for the 1-minute load average
//! to fall under a per-core ceiling before starting, instead of starting and overrunning the cap.

use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_secs(5);
const SAY_EVERY: u64 = 30;

#[derive(Debug, PartialEq)]
pub enum Admit {
    Immediate,
    Waited(u64),
    GaveUp(u64, f64),
}

/// Blocks until `load()` <= `max_pct`% of `cores`, at most `wait` seconds. Never fails the run:
/// a host that stays loaded is reported `GaveUp` and the caller starts anyway.
pub fn admit(
    max_pct: u64,
    wait: u64,
    cores: u32,
    load: &dyn Fn() -> Option<f64>,
    sleep: &dyn Fn(Duration),
    cancelled: &dyn Fn() -> bool,
    say: &mut dyn FnMut(&str),
) -> Admit {
    let ceiling = cores.max(1) as f64 * max_pct as f64 / 100.0;
    let busy = || load().filter(|l| *l > ceiling);
    if max_pct == 0 || wait == 0 || busy().is_none() {
        return Admit::Immediate;
    }
    let start = Instant::now();
    let mut said = 0;
    loop {
        let Some(l) = busy() else {
            return Admit::Waited(start.elapsed().as_secs());
        };
        let waited = start.elapsed().as_secs();
        if waited >= wait || cancelled() {
            return Admit::GaveUp(waited, l);
        }
        if waited == 0 || waited / SAY_EVERY > said {
            said = waited / SAY_EVERY;
            say(&format!("load-wait: load1={l:.2} > {ceiling:.2} ({max_pct}% of {cores} cores), waited {waited}s of {wait}s"));
        }
        sleep(POLL.min(Duration::from_secs(wait - waited)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn run(loads: &[f64], wait: u64) -> (Admit, Vec<String>) {
        let i = Cell::new(0);
        let load = || {
            let v = loads[i.get().min(loads.len() - 1)];
            i.set(i.get() + 1);
            Some(v)
        };
        let mut said = Vec::new();
        let a = admit(100, wait, 4, &load, &|_| {}, &|| false, &mut |l| said.push(l.to_string()));
        (a, said)
    }

    #[test]
    fn an_idle_host_starts_at_once() {
        assert_eq!(run(&[3.9], 60).0, Admit::Immediate);
    }

    #[test]
    fn a_busy_host_is_waited_on_until_it_drops() {
        let (a, said) = run(&[4.6, 4.6, 2.0], 60);
        assert!(matches!(a, Admit::Waited(_)), "{a:?}");
        assert!(said[0].contains("load1=4.60 > 4.00"), "{said:?}");
    }

    #[test]
    fn a_host_that_stays_busy_is_given_up_on_not_failed() {
        let (a, _) = run(&[9.0], 0);
        assert_eq!(a, Admit::Immediate, "wait=0 disables");
        let i = Cell::new(0u64);
        let a = admit(100, 10, 4, &|| Some(9.0), &|_| i.set(i.get() + 1), &|| i.get() >= 1, &mut |_| {});
        assert!(matches!(a, Admit::GaveUp(_, l) if l == 9.0), "{a:?}");
    }

    #[test]
    fn an_unreadable_load_never_blocks() {
        let a = admit(100, 60, 4, &|| None, &|_| {}, &|| false, &mut |_| {});
        assert_eq!(a, Admit::Immediate);
    }

    #[test]
    fn a_zero_ceiling_disables_the_wait() {
        let a = admit(0, 60, 4, &|| Some(99.0), &|_| {}, &|| false, &mut |_| {});
        assert_eq!(a, Admit::Immediate);
    }
}
