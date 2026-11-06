//! Actors from `actors.toml`: processes a world scenario runs on their own timer units (under
//! [`SIM_UNITS`]) for fitted durations. Only sim-only actors remain: every unit the loop ran on a
//! shipped timer is a task of `spira-runtime serve`, which the simulator drives in-process
//! (ruling D6), so a shipped actor is refused.

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct ActorSpec {
    pub name: String,
    pub command: String,
    pub timer: String,
    pub duration: String,
    #[serde(default)]
    pub staged: bool,
    /// Its units live under [`SIM_UNITS`], which nothing installs: an actor only a world
    /// scenario runs. Every actor must be one.
    #[serde(default)]
    pub sim_only: bool,
}

/// Where an actor's timer and service live, relative to the tree.
pub const SIM_UNITS: &str = "sim/units";

impl ActorSpec {
    pub fn units_in(&self, tree: &Path) -> std::path::PathBuf {
        tree.join(SIM_UNITS)
    }
}

#[derive(Debug, Deserialize)]
struct Actors {
    actor: Vec<ActorSpec>,
}

/// Every actor in `text`. One that is not `sim_only` is refused: its units would be shipped
/// ones, and a shipped unit of the loop is a task of the service now (ruling D6).
pub fn parse_specs(text: &str) -> Result<Vec<ActorSpec>, String> {
    let a: Actors = toml::from_str(text).map_err(|e| format!("actors: {e}"))?;
    if a.actor.is_empty() {
        return Err("actors: no [[actor]] entries".into());
    }
    if let Some(s) = a.actor.iter().find(|s| !s.sim_only) {
        return Err(format!("actors: {} is not sim_only — a unit the loop ran on a shipped timer is a task of spira-runtime serve, driven in-process by the simulator (ruling D6), never an actor process", s.name));
    }
    Ok(a.actor)
}
