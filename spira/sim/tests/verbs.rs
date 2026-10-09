use serde_json::json;
use spira_sim::drive::{Exec, Probe};
use spira_sim::trace::{bead_row, Snapshot, Trace};
use spira_sim::verbs::{replay_world, run_scenario, step_world};
use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;

const SCENARIO: &str = r#"
epoch = 1800000000
horizon = 600
goal = "sp-x:LANDED"

[[actor]]
name = "a"
command = "tick a"
every = 100
duration_min = 10
duration_max = 60

[[actor]]
name = "b"
command = "tick b"
every = 100
duration_min = 10
duration_max = 60

[[step]]
at = 50
command = "poke"
"#;

struct Count(Rc<Cell<u32>>);

impl Exec for Count {
    fn run(&mut self, _: &str, _: u64) -> Result<i32, String> {
        self.0.set(self.0.get() + 1);
        Ok(0)
    }
}

struct Script {
    snaps: u32,
    landed_at: u32,
    broken: bool,
}

impl Probe for Script {
    fn snapshot(&mut self) -> Result<Snapshot, String> {
        self.snaps += 1;
        let landed = self.snaps >= self.landed_at;
        let (state, tip) = if self.broken { ("SUBMITTED", "bbb") } else if landed { ("LANDED", "aaa") } else { ("WORKING", "aaa") };
        let row = json!({"bead": "sp-x", "bd_status": "open", "lc_state": state, "lc_tip": "aaa", "branch_tip": tip, "on_local_main": landed});
        Ok(Snapshot { beads: vec![bead_row(&row)?], ..Default::default() })
    }
}

fn world(name: &str) -> testkit::TempDir {
    testkit::TempDir::new(name)
}

fn go(w: &Path, seed: u64, landed_at: u32, broken: bool) -> spira_sim::verbs::RunResult {
    run_scenario(w, SCENARIO, seed, Box::new(Count(Rc::default())), Box::new(Script { snaps: 0, landed_at, broken })).unwrap()
}

#[test]
fn a_run_that_reaches_its_goal_holds_every_invariant() {
    if !spira_sim::trace::duckdb_available() {
        return;
    }
    let w = world("sim-verbs-ok");
    let r = go(&w, 7, 3, false);
    assert!(r.goal_reached && r.failures().is_empty(), "{:?}", r.failures());
}

#[test]
fn a_seeded_violation_fails_naming_seed_and_seq() {
    if !spira_sim::trace::duckdb_available() {
        return;
    }
    let w = world("sim-verbs-bad");
    let r = go(&w, 7, 3, true);
    let f = r.failures();
    assert!(f.iter().any(|l| l.contains("inv_tip_matches_branch") && l.contains("seed 7 seq 1")), "{f:?}");
}

#[test]
fn an_unreached_goal_fails_naming_the_seed() {
    if !spira_sim::trace::duckdb_available() {
        return;
    }
    let w = world("sim-verbs-unreached");
    let r = go(&w, 9, 10_000, false);
    assert_eq!(r.failures(), vec!["goal sp-x:LANDED unreached: seed 9".to_string()]);
}

#[test]
fn replaying_an_identical_run_reports_no_divergence() {
    if !spira_sim::trace::duckdb_available() {
        return;
    }
    let w = world("sim-verbs-replay");
    go(&w, 7, 100_000, false);
    assert_eq!(replay_world(&w, 7).unwrap(), None);
}

#[test]
fn replaying_a_run_that_stopped_at_its_goal_reports_no_divergence() {
    if !spira_sim::trace::duckdb_available() {
        return;
    }
    let w = world("sim-verbs-replay-goal");
    let r = go(&w, 7, 3, false);
    assert!(r.goal_reached);
    assert_eq!(Trace::open(&w).unwrap().events().unwrap().len(), 3);
    assert_eq!(replay_world(&w, 7).unwrap(), None);
}

#[test]
fn replaying_a_perturbed_run_reports_the_first_differing_seq() {
    if !spira_sim::trace::duckdb_available() {
        return;
    }
    let w = world("sim-verbs-perturbed");
    go(&w, 7, 100_000, false);
    let path = Trace::dir_of(&w).join("events.jsonl");
    let text = std::fs::read_to_string(&path).unwrap();
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let mut row: serde_json::Value = serde_json::from_str(&lines[4]).unwrap();
    row["vtime"] = json!(row["vtime"].as_u64().unwrap() + 1);
    lines[4] = row.to_string();
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    assert_eq!(replay_world(&w, 7).unwrap().unwrap().seq, 5);
    let other = (8..40).find_map(|s| replay_world(&w, s).unwrap());
    assert!(other.is_some(), "a different seed must change something");
}

#[test]
fn step_resumes_without_re_executing_recorded_events() {
    if !spira_sim::trace::duckdb_available() {
        return;
    }
    let w = world("sim-verbs-step");
    let runs = Rc::new(Cell::new(0));
    run_scenario(&w, SCENARIO, 7, Box::new(Count(runs.clone())), Box::new(Script { snaps: 0, landed_at: 4, broken: false })).unwrap();
    let before = Trace::open(&w).unwrap().events().unwrap().len();
    assert_eq!(before, 4);
    let more = Rc::new(Cell::new(0));
    step_world(&w, None, Box::new(Count(more.clone())), Box::new(Script { snaps: 4, landed_at: 4, broken: false })).unwrap();
    assert_eq!(Trace::open(&w).unwrap().events().unwrap().len(), 5);
    assert!(more.get() <= 1, "only the new event may execute, ran {}", more.get());
}
