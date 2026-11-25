//! The drain loop: every [`INTERVAL_MS`] take what the kernel counted, fold it, push the totals,
//! and leave for the supervisor to restart when the active release is no longer the one running.

use std::time::{Duration, SystemTime};

use ports::Clock;
use spira_otel::metrics::Series;

use crate::totals::{Drain, Totals};

pub const INTERVAL: Duration = Duration::from_secs(20);

/// Everything the loop touches outside itself.
pub trait Host {
    fn sleep(&mut self, d: Duration);
    /// `readlink` of the release root; `None` when it cannot be read.
    fn release(&mut self) -> Option<String>;
    fn drain(&mut self) -> Result<Drain, String>;
    fn resolve(&mut self, cgroup: u64) -> Option<String>;
    fn self_cpu_seconds(&mut self) -> f64;
    fn push(&mut self, series: &[Series]) -> Result<(), String>;
}

/// Runs until the release link names a different release than `started_on`, after one last push.
pub fn run(host: &mut dyn Host, clock: &dyn Clock, started_on: Option<String>) {
    let mut totals = Totals::default();
    loop {
        host.sleep(INTERVAL);
        match host.drain() {
            Ok(d) => {
                let mut resolve = |id| host.resolve(id);
                totals.fold(d, &mut resolve);
            }
            Err(e) => eprintln!("proc-starts: drain failed: {e}"),
        }
        let at = SystemTime::UNIX_EPOCH + Duration::from_millis(clock.now_ms());
        let cpu = host.self_cpu_seconds();
        if let Err(e) = host.push(&totals.series(cpu, at)) {
            eprintln!("proc-starts: push failed: {e}");
        }
        if let Some(now) = host.release().filter(|now| started_on.as_ref() != Some(now)) {
            if started_on.is_some() {
                eprintln!("proc-starts: release is now {now}; exiting for the supervisor to restart");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::totals::StartCount;
    use ports::VirtualClock;

    struct Fake {
        clock: VirtualClock,
        releases: Vec<Option<String>>,
        pushes: Vec<Vec<Series>>,
        push_fails: bool,
        drains: u32,
    }

    impl Host for Fake {
        fn sleep(&mut self, d: Duration) {
            self.clock.advance(d.as_millis() as u64);
        }
        fn release(&mut self) -> Option<String> {
            self.releases.remove(0)
        }
        fn drain(&mut self) -> Result<Drain, String> {
            self.drains += 1;
            Ok(Drain { starts: vec![StartCount { cgroup: 1, program: "git".into(), starter: "bash".into(), n: 2 }], ..Drain::default() })
        }
        fn resolve(&mut self, _: u64) -> Option<String> {
            Some("/system.slice/a.service".into()) // not a path: a cgroup path
        }
        fn self_cpu_seconds(&mut self) -> f64 {
            0.5
        }
        fn push(&mut self, series: &[Series]) -> Result<(), String> {
            self.pushes.push(series.to_vec());
            if self.push_fails { Err("down".into()) } else { Ok(()) }
        }
    }

    fn fake(releases: &[Option<&str>], push_fails: bool) -> Fake {
        Fake { clock: VirtualClock::new(0), releases: releases.iter().map(|r| r.map(String::from)).collect(), pushes: vec![], push_fails, drains: 0 }
    }

    fn total(f: &Fake, at: usize) -> f64 {
        f.pushes[at].iter().find(|s| s.name == crate::totals::STARTS).unwrap().value
    }

    #[test_id(t7i0ghr)]
    #[test]
    fn a_changed_release_link_pushes_once_more_and_returns() {
        let mut f = fake(&[Some("r1"), Some("r1"), Some("r2")], false);
        let clock = f.clock.clone();
        run(&mut f, &clock, Some("r1".into()));
        assert_eq!(f.pushes.len(), 3);
        assert_eq!(clock.now_ms(), 60_000);
    }

    #[test_id(tx6kys5)]
    #[test]
    fn an_unreadable_release_link_is_not_a_change() {
        let mut f = fake(&[None, Some("r1"), Some("r2")], false);
        let clock = f.clock.clone();
        run(&mut f, &clock, Some("r1".into()));
        assert_eq!(f.drains, 3);
    }

    #[test_id(ts9flyd)]
    #[test]
    fn a_failed_push_keeps_the_totals_for_the_next_one() {
        let mut f = fake(&[Some("r1"), Some("r2")], true);
        let clock = f.clock.clone();
        run(&mut f, &clock, Some("r1".into()));
        assert_eq!((total(&f, 0), total(&f, 1)), (2.0, 4.0));
    }
}
