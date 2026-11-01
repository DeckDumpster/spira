//! `pass`: one visit to every registered repo, running each [`Step`] in order. Each piece of the
//! controller lands as a step.

use crate::outcome::Outcome;
use crate::ports::Ports;

pub struct Step {
    pub name: &'static str,
    pub run: fn(&Ports, &str),
}

/// Supersede runs first, so the members it releases are in the pool for the cut the schedule
/// step makes later in the same pass with a slot it holds (round-controller/pass). Land runs
/// after both, so a round it lands moves the base the next cut is made on.
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
