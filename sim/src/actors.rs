//! Actors from `actors.toml`, scheduled by the real timer units and run for fitted durations.

use crate::fit::Durations;
use crate::units::{parse_timer, require_oneshot, timer_target};
use crate::{Outcome, Sim};
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
}

#[derive(Debug, Deserialize)]
struct Actors {
    actor: Vec<ActorSpec>,
}

pub fn parse_specs(text: &str) -> Result<Vec<ActorSpec>, String> {
    let a: Actors = toml::from_str(text).map_err(|e| format!("actors: {e}"))?;
    if a.actor.is_empty() {
        return Err("actors: no [[actor]] entries".into());
    }
    Ok(a.actor)
}

fn read_unit(dir: &Path, name: &str) -> Result<String, String> {
    std::fs::read_to_string(dir.join(name)).map_err(|e| format!("{}: {e}", dir.join(name).display()))
}

/// Registers each actor and schedules its Due events from its timer unit over `horizon` ms.
pub fn install(
    sim: &mut Sim,
    specs: &[ActorSpec],
    units_dir: &Path,
    durations: &Durations,
    epoch_unix_s: u64,
    horizon: u64,
) -> Result<(), String> {
    for spec in specs {
        let timer = read_unit(units_dir, &spec.timer)?;
        let service = timer_target(&spec.timer, &timer);
        require_oneshot(&service, &read_unit(units_dir, &service)?)?;
        let schedule = parse_timer(&timer).map_err(|e| format!("{}: {e}", spec.timer))?;
        let model = durations
            .series
            .get(&spec.duration)
            .ok_or_else(|| format!("{}: no fitted series {:?}", spec.name, spec.duration))?
            .clone();
        sim.add_actor(
            &spec.name,
            spec.staged,
            Box::new(move |_, rng| Outcome { writes: Vec::new(), duration: model.sample_ms(rng) }),
        );
        for at in schedule.fire_times(epoch_unix_s, horizon, &mut sim.rng) {
            sim.schedule_due(at, &spec.name);
        }
    }
    Ok(())
}
