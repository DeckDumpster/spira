//! `round-controller lease probe`: the renewal loop alone, against the store, measuring how late
//! each renewal lands. `SPIRA_ROUND_LEASE_TTL_SECS` must cover the renewal interval plus the
//! worst of these under IO pressure.

use crate::lease::{acquire, Resource};
use crate::ports::Ports;

pub trait Sleep {
    fn sleep_ms(&self, ms: u64);
}

pub struct ThreadSleep;

impl Sleep for ThreadSleep {
    fn sleep_ms(&self, ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lateness {
    /// Each renewal's lateness past its due time, in order.
    pub late_ms: Vec<u64>,
    /// A renewal found the lease already lapsed, which ended the probe.
    pub lost: bool,
}

impl Lateness {
    /// The nearest-rank `q`-quantile; 0 with no samples.
    pub fn quantile(&self, q: f64) -> u64 {
        let mut s = self.late_ms.clone();
        s.sort_unstable();
        let rank = ((q * s.len() as f64).ceil() as usize).clamp(1, s.len().max(1));
        s.get(rank - 1).copied().unwrap_or(0)
    }

    pub fn report(&self) -> String {
        let each: Vec<String> = self.late_ms.iter().map(u64::to_string).collect();
        format!(
            "renewals={}\nlost={}\np50_ms={}\np99_ms={}\nmax_ms={}\nlate_ms={}\n",
            self.late_ms.len(),
            self.lost,
            self.quantile(0.50),
            self.quantile(0.99),
            self.late_ms.iter().max().copied().unwrap_or(0),
            each.join(" ")
        )
    }
}

/// Holds a lease of its own for `secs`, renewing it one interval after each renewal returns, as
/// a runner does, and measures each renewal from its due time to its return.
pub fn probe(p: &Ports, sleep: &dyn Sleep, secs: u64) -> Result<Lateness, String> {
    let res = Resource {
        lock: format!("round/lease-probe/{:016x}", p.next_u64()),
        record: None,
    };
    let lease = acquire(p, &res, "lease-probe").map_err(|e| format!("lease probe: {e:?}"))?;
    let every = p.leases.renew_every_ms();
    let end = p.clock.now_ms().saturating_add(secs.saturating_mul(1000));
    let mut out = Lateness {
        late_ms: Vec::new(),
        lost: false,
    };
    let mut due = p.clock.now_ms() + every;
    while due <= end {
        let now = p.clock.now_ms();
        if now < due {
            sleep.sleep_ms(due - now);
        }
        let renewed = lease.renew(p);
        let done = p.clock.now_ms();
        out.late_ms.push(done.saturating_sub(due));
        if renewed.is_err() {
            out.lost = true;
            return Ok(out);
        }
        due = done + every;
    }
    let _ = lease.release(p);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::bench::Bench;
    use std::sync::Mutex;

    /// Advances the virtual clock by each sleep plus the next scripted oversleep.
    struct Oversleep<'a> {
        b: &'a Bench,
        extra: Mutex<Vec<u64>>,
    }

    impl Sleep for Oversleep<'_> {
        fn sleep_ms(&self, ms: u64) {
            let extra = crate::lock(&self.extra).pop().unwrap_or(0);
            self.b.clock.advance(ms + extra);
        }
    }

    #[test]
    fn each_renewal_is_measured_from_its_due_time_and_summarised() {
        let b = Bench::new();
        let p = b.ports();
        let every = p.leases.renew_every_ms();
        let s = Oversleep {
            b: &b,
            extra: Mutex::new(vec![0, 40, 0, 7_000, 10]),
        };
        let l = probe(&p, &s, (every * 5 + 8_000) / 1000).unwrap();
        assert_eq!(l.late_ms, [10, 7_000, 0, 40, 0]);
        assert!(!l.lost);
        assert_eq!(
            l.report(),
            "renewals=5\nlost=false\np50_ms=10\np99_ms=7000\nmax_ms=7000\nlate_ms=10 7000 0 40 0\n"
        );
        let leftover = b
            .store
            .list(
                &["locks/round/lease-probe/"],
                &state_store::Selector::Everything,
            )
            .unwrap()
            .0;
        assert!(leftover.is_empty(), "the probe gives its lease back");
    }

    #[test]
    fn a_stall_past_the_ttl_is_a_lost_lease_and_ends_the_probe() {
        let b = Bench::new();
        let p = b.ports();
        let s = Oversleep {
            b: &b,
            extra: Mutex::new(vec![p.leases.ttl_ms]),
        };
        let l = probe(&p, &s, 3_600).unwrap();
        assert_eq!(l.late_ms, [p.leases.ttl_ms]);
        assert!(l.lost);
        assert_eq!(
            Lateness {
                late_ms: vec![],
                lost: false
            }
            .quantile(0.99),
            0
        );
        assert!(probe(&Bench::unserved().ports(), &s, 1)
            .unwrap_err()
            .starts_with("lease probe: Store("));
        ThreadSleep.sleep_ms(0); // clock-safe: a zero sleep, so the real sleeper is reached
    }
}
