//! round-controller — a round is a resource with one owner and one store. Every effect goes
//! through [`ports::Ports`], so [`pass::pass`] runs the same code against the real queue and
//! against the scenario harness in [`sim`]. Inert: nothing installs it (DESIGN.md).

pub mod outcome;
pub mod pass;
pub mod ports;
pub mod real;
#[cfg(feature = "sim")]
pub mod sim;
pub mod telemetry;

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
