use crate::drive::{parse_scenario, seed_steps, ProcessExec};
use crate::world::{self, Steps};
use std::path::Path;
use std::process::{Command, Output};

pub const FIXTURE: &str = include_str!("../fixtures/pilgrim-world.toml");
const SHELL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120); // batch-job: a bead filing in a world

/// A command under the world's own environment and PATH — its config, socket and release — so a
/// tool run through it reads the world's store and never production's.
pub fn bound_command(world: &Path, shell: &str) -> Result<Command, String> {
    ProcessExec { world: world.to_path_buf(), epoch: parse_scenario(FIXTURE)?.epoch }.command_with(shell, 0, false)
}

pub fn exec(world: &Path, shell: &str) -> Result<Output, String> {
    let mut cmd = bound_command(world, shell)?;
    let (status, stdout, stderr) = world::run_capture(&mut cmd, SHELL_DEADLINE)?;
    Ok(Output { status, stdout: stdout.into_bytes(), stderr: stderr.into_bytes() })
}

/// Files the fixture's beads in the world's own store.
pub fn seed(world: &Path, fixture: &str) -> Result<(), String> {
    let scenario = parse_scenario(fixture)?;
    for (_, shell) in seed_steps(&scenario.seeds)? {
        let out = exec(world, &shell)?;
        if !out.status.success() {
            return Err(format!("seeding the pilgrim world: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
    }
    Ok(())
}

/// A loopback address nothing listens on, for the world's own Loom.
fn free_addr() -> Result<String, String> {
    let l = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    l.local_addr().map(|a| a.to_string()).map_err(|e| e.to_string())
}

/// Marks `world` a pilgrim world, seeds its store from `fixture`, and gives its Loom an address
/// of its own, so `lc-view` and `loom` run through [`exec`] read this world's socket.
pub fn up(world: &Path, fixture: &str, steps: &dyn Steps) -> Result<(), String> {
    world::make_pilgrim_world(world)?;
    let release = world.join("release");
    steps.config_set(&release, &world.join("config/sim.toml"), "spira.loom_addr", &free_addr()?)?;
    seed(world, fixture)
}
