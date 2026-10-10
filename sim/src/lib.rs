//! Single-threaded event loop over virtual time. Every decision between actor runs
//! (tie order, run durations, jitter) comes from one seeded PRNG.

pub mod actors;
pub mod fit;
pub mod units;

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};

pub fn actor_command(prog: &str, now: u64) -> std::process::Command {
    let mut cmd = std::process::Command::new(prog);
    cmd.envs(spira_config::vtime::actor_env(now));
    cmd
}

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Standard normal (Box-Muller; always consumes two draws).
    pub fn normal(&mut self) -> f64 {
        let u1 = 1.0 - self.unit();
        let u2 = self.unit();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    /// Uniform in `lo..=hi`.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        assert!(lo <= hi, "empty range");
        let span = hi - lo + 1;
        if span == 0 {
            return self.next_u64();
        }
        lo + self.next_u64() % span
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EventKind {
    Due(String),
    Complete(String),
    Step(String),
    Fault,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Traced {
    pub time: u64,
    pub kind: EventKind,
    pub coalesced: bool,
}

pub type World = BTreeMap<String, String>;

#[derive(Default)]
pub struct Outcome {
    pub writes: Vec<(String, String)>,
    pub duration: u64,
}

pub type ActorFn = Box<dyn FnMut(&World, &mut Rng) -> Outcome>;
pub type ScenarioFn = Box<dyn FnMut(&mut Sim)>;

struct Actor {
    run: ActorFn,
    staged: bool,
    running: bool,
    pending: Vec<(String, String)>,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Queued {
    time: u64,
    tie: u64,
    seq: u64,
    kind: EventKind,
}

pub struct Sim {
    pub seed: u64,
    pub now: u64,
    pub world: World,
    pub trace: Vec<Traced>,
    pub rng: Rng,
    queue: BinaryHeap<Reverse<Queued>>,
    seq: u64,
    actors: BTreeMap<String, Actor>,
    scenarios: BTreeMap<String, ScenarioFn>,
}

impl Sim {
    /// Prints the seed first, before any event can occur.
    pub fn new(seed: u64) -> Self {
        println!("sim seed: {seed}");
        Sim {
            seed,
            now: 0,
            world: World::new(),
            trace: Vec::new(),
            rng: Rng::new(seed),
            queue: BinaryHeap::new(),
            seq: 0,
            actors: BTreeMap::new(),
            scenarios: BTreeMap::new(),
        }
    }

    pub fn add_actor(&mut self, name: &str, staged: bool, run: ActorFn) {
        let actor = Actor { run, staged, running: false, pending: Vec::new() };
        self.actors.insert(name.to_string(), actor);
    }

    pub fn add_scenario(&mut self, name: &str, f: ScenarioFn) {
        self.scenarios.insert(name.to_string(), f);
    }

    pub fn is_running(&self, actor: &str) -> bool {
        self.actors.get(actor).is_some_and(|a| a.running)
    }

    /// The world as a concurrent reader sees it: staged writes of a running
    /// actor are not in it.
    pub fn read(&self, key: &str) -> Option<&str> {
        self.world.get(key).map(String::as_str)
    }

    pub fn schedule(&mut self, at: u64, kind: EventKind) {
        assert!(at >= self.now, "event scheduled in the past");
        let tie = self.rng.next_u64();
        self.seq += 1;
        let seq = self.seq;
        self.queue.push(Reverse(Queued { time: at, tie, seq, kind }));
    }

    pub fn schedule_due(&mut self, at: u64, actor: &str) {
        self.schedule(at, EventKind::Due(actor.to_string()));
    }

    pub fn schedule_step(&mut self, at: u64, scenario: &str) {
        self.schedule(at, EventKind::Step(scenario.to_string()));
    }

    pub fn next_time(&self) -> Option<u64> {
        self.queue.peek().map(|Reverse(q)| q.time)
    }

    /// Runs the next event; false when the queue is empty. Virtual time jumps
    /// straight to the event, so empty time costs nothing.
    pub fn step(&mut self) -> bool {
        let Some(Reverse(ev)) = self.queue.pop() else {
            return false;
        };
        self.now = ev.time;
        let mut coalesced = false;
        match &ev.kind {
            EventKind::Due(name) => coalesced = self.due(name),
            EventKind::Complete(name) => self.complete(name),
            EventKind::Step(name) => {
                let mut f = self
                    .scenarios
                    .remove(name)
                    .unwrap_or_else(|| panic!("unknown scenario {name}"));
                f(self);
                self.scenarios.insert(name.clone(), f);
            }
            EventKind::Fault => {}
        }
        self.trace.push(Traced { time: ev.time, kind: ev.kind, coalesced });
        true
    }

    pub fn run(&mut self) {
        while self.step() {}
    }

    pub fn run_until(&mut self, limit: u64) {
        while self.queue.peek().is_some_and(|Reverse(q)| q.time <= limit) {
            self.step();
        }
    }

    fn due(&mut self, name: &str) -> bool {
        let actor = self
            .actors
            .get_mut(name)
            .unwrap_or_else(|| panic!("unknown actor {name}"));
        if actor.running {
            return true;
        }
        actor.running = true;
        let out = (actor.run)(&self.world, &mut self.rng);
        if actor.staged {
            actor.pending = out.writes;
        } else {
            self.world.extend(out.writes);
        }
        let at = self.now + out.duration;
        self.schedule(at, EventKind::Complete(name.to_string()));
        false
    }

    fn complete(&mut self, name: &str) {
        let actor = self.actors.get_mut(name).expect("completing unknown actor");
        actor.running = false;
        self.world.extend(std::mem::take(&mut actor.pending));
    }

    pub fn trace_signature(&self) -> Vec<(u64, EventKind, bool)> {
        self.trace.iter().map(|t| (t.time, t.kind.clone(), t.coalesced)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn jittery(sim: &mut Sim, name: &str, staged: bool) {
        sim.add_actor(
            name,
            staged,
            Box::new(|_, rng| Outcome {
                writes: vec![("k".into(), format!("v{}", rng.next_u64() % 100))],
                duration: rng.range(1, 50),
            }),
        );
    }

    fn scripted(seed: u64) -> Vec<(u64, EventKind, bool)> {
        let mut sim = Sim::new(seed);
        for n in ["a", "b", "c"] {
            jittery(&mut sim, n, false);
            for i in 0..5 {
                let at = sim.rng.range(0, 20) + i * 30;
                sim.schedule_due(at, n);
            }
        }
        sim.run();
        sim.trace_signature()
    }

    #[test]
    fn same_seed_same_sequence_and_timestamps() {
        assert_eq!(scripted(42), scripted(42));
        assert_ne!(scripted(42), scripted(43));
    }

    #[test]
    fn simultaneous_ties_are_seeded() {
        let order = |seed| {
            let mut sim = Sim::new(seed);
            for n in ["a", "b", "c", "d"] {
                jittery(&mut sim, n, false);
                sim.schedule_due(10, n);
            }
            sim.run();
            sim.trace_signature()
        };
        assert_eq!(order(7), order(7));
        assert!((0..20).any(|s| order(s) != order(7)));
    }

    #[test]
    fn due_while_running_is_coalesced() {
        let mut sim = Sim::new(1);
        sim.add_actor("a", false, Box::new(|_, _| Outcome { writes: vec![], duration: 100 }));
        sim.schedule_due(0, "a");
        sim.schedule_due(10, "a");
        sim.schedule_due(50, "a");
        sim.schedule_due(100, "a");
        sim.run();
        let runs = sim
            .trace
            .iter()
            .filter(|t| matches!(t.kind, EventKind::Due(_)) && !t.coalesced)
            .count();
        let merged = sim.trace.iter().filter(|t| t.coalesced).count();
        assert!(merged >= 2, "due at 10 and 50 must coalesce");
        assert_eq!(runs + merged, 4);
        assert!(!sim.is_running("a"));
    }

    #[test]
    fn staged_writes_invisible_until_complete() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let mut sim = Sim::new(3);
        sim.add_actor(
            "w",
            true,
            Box::new(|_, _| Outcome { writes: vec![("x".into(), "new".into())], duration: 100 }),
        );
        sim.add_actor(
            "u",
            false,
            Box::new(|_, _| Outcome { writes: vec![("y".into(), "now".into())], duration: 100 }),
        );
        let s = seen.clone();
        sim.add_scenario(
            "read",
            Box::new(move |sim| {
                s.borrow_mut().push((sim.now, sim.read("x").map(String::from), sim.read("y").map(String::from)))
            }),
        );
        sim.schedule_due(0, "w");
        sim.schedule_due(0, "u");
        sim.schedule_step(50, "read");
        sim.schedule_step(100, "read");
        sim.schedule_step(101, "read");
        sim.run();
        let seen = seen.borrow();
        assert_eq!(seen[0], (50, None, Some("now".into())));
        assert_eq!(seen[2], (101, Some("new".into()), Some("now".into())));
    }

    #[test]
    fn empty_time_is_skipped() {
        let mut sim = Sim::new(1);
        sim.add_scenario("s", Box::new(|_| {}));
        sim.schedule_step(u64::MAX / 2, "s");
        assert!(sim.step());
        assert_eq!(sim.now, u64::MAX / 2);
        assert!(!sim.step());
    }
}

#[cfg(test)]
mod vtime_tests {
    use super::*;

    const VIRTUAL: u64 = 1_800_000_000;

    #[test]
    fn a_commit_made_in_an_actor_run_carries_the_virtual_date() {
        let dir = testkit::TempDir::new("sim-vtime-commit");
        let git = |args: &[&str]| {
            let out = actor_command("git", VIRTUAL)
                .args(["-C", dir.to_str().unwrap()])
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "x"]);
        assert_eq!(git(&["log", "-1", "--format=%at %ct"]), format!("{VIRTUAL} {VIRTUAL}"));
    }

    #[test]
    fn actor_env_sets_spira_now() {
        let env = spira_config::vtime::actor_env(VIRTUAL);
        assert!(env.contains(&("SPIRA_NOW", VIRTUAL.to_string())));
    }
}
