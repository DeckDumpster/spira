//! Never start a gate this pass cannot finish (landing.sh's gate_fits), and wait for a branch's own gate tree lock
//! twice the gate timeout (one holder can run two trials) (gate_lock_wait).

/// 0 or negative maxsec means no limit (a hand-run pass is not held to a budget nobody
/// enforces on it).
pub fn gate_fits(maxsec: i64, pass_start: u64, reserve: i64, now: u64) -> bool {
    if maxsec <= 0 {
        return true;
    }
    let spent = now as i64 - pass_start as i64;
    maxsec - spent >= reserve
}

/// The wait handed to gate.sh as SPIRA_GATE_LOCK_WAIT: twice the gate timeout, which an explicit
/// setting may only raise (gate-worker's rule). Never shortened by the pass budget: a gate
/// admitted past it (a base fix) would get a zero wait and end NO_VERDICT lock-timeout.
pub fn gate_lock_wait(gate_timeout: i64, explicit: Option<&str>) -> String {
    let floor = gate_timeout * 2;
    let e = explicit.and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(floor);
    e.max(floor).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tvw0sp3)]
    #[test]
    fn fits_until_the_reserve() {
        assert!(gate_fits(0, 0, 2700, 99_999));
        assert!(gate_fits(3600, 1000, 2700, 1900));
        assert!(!gate_fits(3600, 1000, 2700, 1901));
    }

    #[test_id(tj6by91)]
    #[test]
    fn lock_wait_is_twice_the_gate_timeout_and_an_exhausted_pass_budget_does_not_shorten_it() {
        assert_eq!(gate_lock_wait(700, None), "1400");
        assert_eq!(gate_lock_wait(700, Some("7")), "1400");
        assert_eq!(gate_lock_wait(700, Some("9000")), "9000");
        assert_eq!(gate_lock_wait(700, Some("")), "1400");
    }
}
