use crate::drive::{drive, parse_bead_state, parse_scenario, Exec, NoProbe, Probe, Resumed, Scenario, Stop};
use crate::trace::{first_divergence, unmet, violations, Trace, Violation};
use std::path::Path;

const SCENARIO_FILE: &str = "scenario.toml";
const SEED_FILE: &str = "sim.seed";

pub struct RunResult {
    pub seed: u64,
    pub violations: Vec<Violation>,
    pub goal: Option<String>,
    pub goal_reached: bool,
    pub unmet: Vec<String>,
}

impl RunResult {
    pub fn failures(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .violations
            .iter()
            .map(|v| format!("invariant {} violated: seed {} seq {} row {}", v.view, self.seed, v.seq, v.row))
            .collect();
        if let (Some(g), false) = (&self.goal, self.goal_reached) {
            out.push(format!("goal {g} unreached: seed {}", self.seed));
        }
        out.extend(self.unmet.iter().map(|n| format!("expectation {n} unmet: seed {}", self.seed)));
        out
    }
}

fn load_world(world: &Path) -> Result<(Scenario, u64), String> {
    let read = |f: &str| std::fs::read_to_string(world.join(f)).map_err(|e| format!("{}: {e}", world.join(f).display()));
    let seed = read(SEED_FILE)?.trim().parse().map_err(|e| format!("{SEED_FILE}: {e}"))?;
    let mut sc = parse_scenario(&read(SCENARIO_FILE)?)?;
    sc.tree = Some(world.join("work"));
    Ok((sc, seed))
}

fn finish(world: &Path, seed: u64, sc: &Scenario, goal: Option<String>, goal_reached: bool, at_end: bool) -> Result<RunResult, String> {
    let db = Trace::open(world)?.load(world)?;
    let unmet = if at_end { unmet(&db, &sc.expects)? } else { Vec::new() };
    Ok(RunResult { seed, violations: violations(&db, &sc.invariants)?, goal, goal_reached, unmet })
}

pub fn run_scenario(world: &Path, text: &str, seed: u64, exec: Box<dyn Exec>, probe: Box<dyn Probe>) -> Result<RunResult, String> {
    let mut sc = parse_scenario(text)?;
    sc.tree = Some(world.join("work"));
    std::fs::write(world.join(SCENARIO_FILE), text).map_err(|e| e.to_string())?;
    std::fs::write(world.join(SEED_FILE), seed.to_string()).map_err(|e| e.to_string())?;
    let trace = Trace::create(world)?;
    let report = drive(&sc, seed, exec, probe, Some(&trace), &[], &Stop::Goal)?;
    finish(world, seed, &sc, sc.goal.clone(), report.goal_reached, true)
}

pub fn parse_until(spec: &str) -> Result<Stop, String> {
    if let Some(rest) = spec.strip_prefix("bead:") {
        let (b, s) = parse_bead_state(rest)?;
        return Ok(Stop::Bead(b, s));
    }
    spec.parse().map(Stop::VTime).map_err(|_| format!("--until {spec:?} is neither a vtime nor bead:<bead>:<STATE>"))
}

/// Advances a world's recorded run: one event, or up to `until`.
pub fn step_world(world: &Path, until: Option<Stop>, exec: Box<dyn Exec>, probe: Box<dyn Probe>) -> Result<RunResult, String> {
    let (sc, seed) = load_world(world)?;
    let trace = Trace::open(world)?;
    let resume = trace.events()?;
    let stop = until.unwrap_or(Stop::Events(1));
    let report = drive(&sc, seed, exec, probe, Some(&trace), &resume, &stop)?;
    finish(world, seed, &sc, sc.goal.clone().filter(|_| matches!(stop, Stop::Goal)), report.goal_reached, matches!(stop, Stop::Goal))
}

/// Re-derives the world's run from `seed` and reports the first event that differs from the recorded one.
pub fn replay_world(world: &Path, seed: u64) -> Result<Option<crate::trace::Divergence>, String> {
    let (sc, _) = load_world(world)?;
    let recorded = Trace::open(world)?.events()?;
    let exec = Box::new(Resumed { recorded: recorded.iter().filter(|e| matches!(e["kind"].as_str(), Some("start" | "step"))).map(|e| e["exit"].as_i64().unwrap_or(0) as i32).collect(), live: None });
    let report = match drive(&sc, seed, exec, Box::new(NoProbe), None, &[], &Stop::Events(recorded.len() as u64 + u64::from(sc.goal.is_none()))) {
        Ok(r) => r,
        Err(e) if e.contains("fewer executions") => return Ok(first_divergence(&recorded, &[])),
        Err(e) => return Err(e),
    };
    Ok(first_divergence(&recorded, &report.events))
}
