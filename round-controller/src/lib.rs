//! round-controller — a round is a resource with one owner and one store. Every effect goes
//! through [`ports::Ports`], so [`pass::pass`] runs the same code against the real queue and
//! against the scenario harness in [`sim`]. Only [`sifts`] is installed (DESIGN.md).

pub mod clear;
pub mod conditions;
pub mod cutover_check;
pub mod cut;
pub mod death;
pub mod intents;
pub mod judge;
pub mod knobs;
pub mod land;
pub mod lane;
pub mod lease;
pub mod ledger;
pub mod ledger_verbs;
pub mod liveness;
pub mod orphans;
pub mod outcome;
pub mod pass;
pub mod ports;
pub mod probe;
pub mod real;
pub mod refusal;
pub mod resources;
pub mod run;
pub mod schedule;
pub mod section;
pub mod sifts;
pub mod sim;
pub mod slots;
pub mod stack;
pub mod supersede;
pub mod switches;
pub mod telemetry;
pub mod trace;

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
}

#[cfg(test)]
mod fork_fence {
    #[test]
    fn round_controller_starts_no_spira_child_that_is_not_a_named_fork_edge() {
        let got = forge::fork_scan::scan(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
        assert!(got.is_empty(), "unmarked children:\n{}", got.join("\n"));
    }
}
