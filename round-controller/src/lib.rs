//! round-controller — a round is a resource with one owner and one store. Every effect goes
//! through [`ports::Ports`], so [`pass::pass`] runs the same code against the real queue and
//! against the scenario harness in [`sim`]. Inert: nothing installs it (DESIGN.md).

pub mod death;
pub mod lease;
pub mod ledger;
pub mod liveness;
pub mod orphans;
pub mod outcome;
pub mod pass;
pub mod ports;
pub mod probe;
pub mod real;
pub mod resources;
#[cfg(feature = "sim")]
pub mod sim;
pub mod slots;
pub mod telemetry;

/// A mutex's guard, poisoned or not: every lock here guards data a panic cannot leave half-written.
pub fn lock<T: ?Sized>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `SPIRA_SEED` as the simulator sets it; `None` when unset or not a number.
pub fn seed_from(raw: Option<&str>) -> Option<u64> {
    raw.and_then(|s| s.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_numeric_seed_is_read_and_anything_else_is_none() {
        assert_eq!(seed_from(Some(" 7 ")), Some(7));
        assert_eq!(seed_from(Some("seven")), None);
        assert_eq!(seed_from(None), None);
    }

    #[test]
    fn a_poisoned_lock_still_hands_back_its_data() {
        let m = std::sync::Arc::new(std::sync::Mutex::new(1));
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("poison it");
        })
        .join();
        assert!(m.is_poisoned());
        assert_eq!(*lock(&m), 1);
    }
}
