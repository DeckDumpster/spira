//! `pass`: one visit to every registered repo. The off switches come first; then each [`Step`]
//! runs in the fixed order of [`STEPS`]. Each step is a few store and verb calls and none waits
//! on a VM, so one repo's visit never delays another's, and no lock is held across a step.

use crate::outcome::Outcome;
use crate::ports::Ports;
use crate::switches;

#[derive(Clone, Copy)]
pub struct Step {
    pub name: &'static str,
    pub run: fn(&Ports, &str),
}

/// The forge sweep, which reconciles `queue batch open`'s crashes, until
/// round-controller/forge-controller registers it.
pub const FORGE_SWEEP: &str = "forge-sweep";

/// A step whose piece of the controller has not landed: it does nothing.
fn unregistered(_: &Ports, _: &str) {}

/// The order is fixed (round-controller/pass). Orphans are re-driven before anything is cut, so
/// a re-drive takes its slot first. Supersede releases the members of a round that can no longer
/// land, so they are in the pool for the cut the schedule step makes later in the same pass. Land
/// runs before the schedule step, so a round it lands moves the base the next cut is made on. The
/// intents are applied next and the section is built last, so it shows what every other step did.
pub const STEPS: &[Step] = &[
    Step {
        name: FORGE_SWEEP,
        run: unregistered,
    },
    Step {
        name: "orphans",
        run: crate::orphans::step,
    },
    Step {
        name: crate::supersede::STEP,
        run: crate::supersede::step,
    },
    Step {
        name: crate::land::STEP,
        run: crate::land::step,
    },
    Step {
        name: crate::schedule::STEP,
        run: crate::schedule::step,
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

/// The steps a stopped repo still runs: publishing its section, which names the switch. It
/// writes only the regenerable section, through the section door.
const UNDER_A_STOP: &[&str] = &[crate::section::STEP];

pub fn pass(p: &Ports, repos: &[String]) {
    pass_with(p, repos, STEPS)
}

/// Each repo's visit ends in one outcome: `Visited` when its steps ran, `Stopped` naming the off
/// switch when one is set, in which case only the section is published for it.
pub fn pass_with(p: &Ports, repos: &[String], steps: &[Step]) {
    for repo in repos {
        if let Some(stop) = switches::check(p, repo) {
            for s in steps.iter().filter(|s| UNDER_A_STOP.contains(&s.name)) {
                (s.run)(p, repo);
            }
            p.log.write(&Outcome::stopped(p.clock.now_ms(), repo, &stop));
            continue;
        }
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
    use crate::outcome::Kind;
    use crate::sim::bench::Bench;

    #[test]
    fn the_steps_run_in_the_fixed_order_with_the_section_last() {
        let names: Vec<&str> = STEPS.iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            ["forge-sweep", "orphans", "supersede", "land", "schedule", "intents", "section"]
        );
        let kept: Vec<&str> = steps_without(&["intents".into(), "section".into()])
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(kept, ["forge-sweep", "orphans", "supersede", "land", "schedule"]);
    }

    #[test]
    fn a_stopped_repo_runs_only_the_section_and_the_others_run_every_step() {
        let b = Bench::new();
        b.stops.stop("held", "ops", "drill", "manual");
        thread_local!(static RAN: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) });
        fn note(_: &Ports, repo: &str) {
            RAN.with(|r| r.borrow_mut().push(repo.to_string()));
        }
        fn publish(_: &Ports, repo: &str) {
            RAN.with(|r| r.borrow_mut().push(format!("section {repo}")));
        }
        let steps = [
            Step {
                name: "note",
                run: note,
            },
            Step {
                name: crate::section::STEP,
                run: publish,
            },
        ];
        pass_with(&b.ports(), &["held".into(), "free".into()], &steps);
        assert_eq!(
            RAN.with(|r| r.borrow().clone()),
            ["section held", "free", "section free"]
        );
        let kinds: Vec<(String, Kind)> = b
            .log
            .outcomes()
            .into_iter()
            .map(|o| (o.repo, o.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("held".to_string(), Kind::Stopped),
                ("free".to_string(), Kind::Visited)
            ]
        );
        unregistered(&b.ports(), "free");
    }
}
