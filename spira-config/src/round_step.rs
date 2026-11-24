//! The one list of steps a round can report, whether as a phase marker or as a red.

/// Every name the round script marks with `phase` or `phase_failed`, and its initial `start`.
pub const PHASES: [&str; 15] = [
    "start",
    "preflight",
    "checkout",
    "spira-env",
    "toolchain",
    "build",
    "unit-build",
    "release",
    "image",
    "lint",
    "fences",
    "unit-tests",
    "suites",
    "integration-tests",
    "post",
];

/// The reds a round attributes to a member; an older installed runner can still report any of them.
pub const REDS: [&str; 6] = ["workspace-build", "spira-lint", "unit-tests", "bare-suites", "config-delta", "release-verify"];

const FENCE_PREFIX: &str = "fence-";

/// A round step: a listed name, or `fence-` and a fence's name.
pub fn is_step(name: &str) -> bool {
    REDS.contains(&name) || PHASES.contains(&name) || name.strip_prefix(FENCE_PREFIX).is_some_and(|f| !f.is_empty())
}

/// Every listed step once, reds first.
pub fn names() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = REDS.to_vec();
    out.extend(PHASES.iter().filter(|p| !REDS.contains(p)));
    out
}

/// What a runner can run at a tip in place of a round step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Counterpart {
    Lint,
    UnitTests,
}

const COUNTERPARTS: [(&str, Counterpart); 3] = [
    ("workspace-build", Counterpart::Lint),
    ("spira-lint", Counterpart::Lint),
    ("unit-tests", Counterpart::UnitTests),
];

/// The runner check standing in for a listed step, if any; a fence is not in the table.
pub fn counterpart(name: &str) -> Option<Counterpart> {
    COUNTERPARTS.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

/// The fence a `fence-<name>` step names.
pub fn fence_of(name: &str) -> Option<&str> {
    name.strip_prefix(FENCE_PREFIX).filter(|f| !f.is_empty())
}

/// A step no host runner can run for a tip: only a round proves it.
pub fn proven_by_round(name: &str) -> bool {
    is_step(name) && fence_of(name).is_none() && counterpart(name).is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(t6zmjml)]
    #[test]
    fn names_lists_each_step_once_with_reds_first() {
        let n = names();
        assert_eq!(&n[..6], &REDS);
        let mut sorted = n.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), n.len());
    }

    #[test_id(tddtblu)]
    #[test]
    fn a_fence_is_a_step_and_an_unknown_name_is_not() {
        assert!(is_step("fence-round-image") && is_step("image") && is_step("bare-suites"));
        assert!(!is_step("round-vm") && !is_step("testenv") && !is_step("fence-"));
    }

    #[test_id(tyucs8j)]
    #[test]
    fn only_steps_without_a_host_runner_are_proven_by_the_round() {
        for n in ["bare-suites", "image", "config-delta", "release-verify"] {
            assert!(proven_by_round(n), "{n}");
        }
        for n in ["unit-tests", "spira-lint", "workspace-build", "fence-x", "test-a.sh", "testenv"] {
            assert!(!proven_by_round(n), "{n}");
        }
    }
}
