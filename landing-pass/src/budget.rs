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

/// The wait handed to gate.sh as SPIRA_GATE_LOCK_WAIT, and a log line when the pass budget
/// capped it. An explicit setting is honoured unchanged.
pub fn gate_lock_wait(gate_timeout: i64, maxsec: i64, pass_start: u64, explicit: Option<&str>, now: u64) -> (String, Option<String>) {
    if let Some(e) = explicit.filter(|e| !e.is_empty()) {
        return (e.to_string(), None);
    }
    let ideal: i64 = gate_timeout * 2;
    if maxsec > 0 {
        let remaining = maxsec - (now as i64 - pass_start as i64);
        if remaining < ideal {
            return (
                remaining.to_string(),
                Some(format!(
                    "landing: gate lock wait capped at {remaining}s by pass budget (ideal {ideal}s); rc=75 should be read as contention"
                )),
            );
        }
    }
    (ideal.to_string(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_until_the_reserve() {
        assert!(gate_fits(0, 0, 2700, 99_999));
        assert!(gate_fits(3600, 1000, 2700, 1900));
        assert!(!gate_fits(3600, 1000, 2700, 1901));
    }

    #[test]
    fn lock_wait_is_twice_the_gate_timeout_and_capped() {
        assert_eq!(gate_lock_wait(700, 0, 0, None, 5).0, "1400");
        assert_eq!(gate_lock_wait(700, 3600, 0, Some("7"), 3590), ("7".into(), None));
        let (w, note) = gate_lock_wait(700, 3600, 0, None, 3550);
        assert_eq!(w, "50");
        assert!(note.unwrap().contains("capped at 50s"));
    }
}
