//! The checks an eject may record as forced for a member: ones a runner can run at its tip.

use spira_config::round_step::{counterpart, fence_of, is_step};

/// `named` (a csv of failing checks) less what no runner can run at a tip whose `gate.steps` is
/// `gate_steps`: a round-only step, and a `fence-<name>` that definition does not list. An absent or
/// unparseable definition lists no fences. Suites that are not round steps stay.
pub fn forced(named: &str, gate_steps: Option<&str>) -> String {
    let fences: Vec<String> = gate_steps
        .and_then(|t| crate::def::parse(t).ok())
        .and_then(|d| crate::fences::list(&d, None).ok())
        .map(|l| l.into_iter().map(|f| f.name).collect())
        .unwrap_or_default();
    named
        .split(',')
        .filter(|n| !n.is_empty())
        .filter(|n| match fence_of(n) {
            Some(f) => fences.iter().any(|x| x == f),
            None => !is_step(n) || counterpart(n).is_some(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree_steps() -> String {
        std::fs::read_to_string(spira_config::resolve::checkout_root_for_process().unwrap().join(crate::def::PATH)).unwrap()
    }

    #[test_id(t8m0aug)]
    #[test]
    fn this_trees_definition_keeps_a_listed_fence_and_drops_the_image_stem() {
        let steps = tree_steps();
        assert_eq!(forced("fence-round-image-preflight,fence-round-image", Some(&steps)), "fence-round-image-preflight");
    }

    #[test_id(tcio27q)]
    #[test]
    fn every_listed_step_is_forced_only_when_a_runner_has_it() {
        let steps = tree_steps();
        for n in spira_config::round_step::names() {
            let want = if counterpart(n).is_some() { n } else { "" };
            assert_eq!(forced(n, Some(&steps)), want, "{n}");
        }
    }

    #[test_id(txkpzyu)]
    #[test]
    fn suites_stay_and_a_missing_or_broken_definition_lists_no_fence() {
        assert_eq!(forced("test-a.sh,fence-x,release-verify", None), "test-a.sh");
        assert_eq!(forced("test-a.sh,fence-x", Some("bogus directive")), "test-a.sh");
    }
}
