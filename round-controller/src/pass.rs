//! `pass`: one visit to every registered repo, running each [`Step`] in order. Each piece of the
//! controller lands as a step.

use crate::outcome::Outcome;
use crate::ports::Ports;

#[derive(Clone, Copy)]
pub struct Step {
    pub name: &'static str,
    pub run: fn(&Ports, &str),
}

/// Supersede runs first, so the members it releases are in the pool for the cut the schedule
/// step makes later in the same pass with a slot it holds (round-controller/pass). Land runs
/// after both, so a round it lands moves the base the next cut is made on. The intents are
/// applied next and the section is built last, so it shows what every other step did.
pub const STEPS: &[Step] = &[
    Step {
        name: crate::supersede::STEP,
        run: crate::supersede::step,
    },
    Step {
        name: "orphans",
        run: crate::orphans::step,
    },
    Step {
        name: crate::land::STEP,
        run: crate::land::step,
    },
    Step {
        name: crate::intents::STEP,
        run: crate::intents::step,
    },
    Step {
        name: crate::section::STEP,
        run: crate::section::step,
    },
];

pub fn pass(p: &Ports, repos: &[String]) {
    pass_with(p, repos, STEPS)
}

pub fn pass_with(p: &Ports, repos: &[String], steps: &[Step]) {
    for repo in repos {
        for s in steps {
            (s.run)(p, repo);
        }
        p.log.write(&Outcome::visited(p.clock.now_ms(), repo));
    }
}

/// Every registered step but those named in `without`.
pub fn steps_without(without: &[String]) -> Vec<Step> {
    STEPS.iter().filter(|s| !without.iter().any(|w| w == s.name)).copied().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_section_is_built_last_after_the_intents_it_shows() {
        let names: Vec<&str> = STEPS.iter().map(|s| s.name).collect();
        assert_eq!(names, ["supersede", "orphans", "land", "intents", "section"]);
        let kept: Vec<&str> = steps_without(&["intents".into(), "section".into()]).iter().map(|s| s.name).collect();
        assert_eq!(kept, ["supersede", "orphans", "land"]);
    }
}
