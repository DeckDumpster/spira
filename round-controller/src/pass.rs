//! `pass`: one visit to every registered repo, running each [`Step`] in order. Each piece of the
//! controller lands as a step.

use crate::outcome::Outcome;
use crate::ports::Ports;

pub struct Step {
    pub name: &'static str,
    pub run: fn(&Ports, &str),
}

pub const STEPS: &[Step] = &[Step {
    name: "orphans",
    run: crate::orphans::step,
}];

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
