//! `sim-launch`: the world's `SPIRA_LAUNCH` and `SPIRA_LAUNCH_CTL`, standing where `systemd-run`
//! and `systemctl` stand for the round controller's launcher. A `systemd-run` call records its
//! unit under `run/sim-units/<unit>` and runs the command after the unit options inline in the
//! world, with the `--setenv` pairs; `systemctl --user is-active <unit>` answers from what it
//! recorded.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Where each launched unit's record lives, under the world's run directory.
pub const UNITS_DIR: &str = "sim-units";
const INLINE_DEADLINE: Duration = Duration::from_secs(300); // batch-job: one runner pass against a world's stubs
/// `systemctl is-active`'s exit for a unit that is not active.
pub const NOT_ACTIVE: i32 = 3;

#[derive(Debug, PartialEq, Eq)]
pub enum Call {
    IsActive(String),
    Run { unit: String, env: Vec<(String, String)>, command: Vec<String> },
}

/// A `systemd-run` argv (`--unit=`, `--setenv=K=V` and any other `--` option, then the command)
/// or a `systemctl --user is-active <unit>` argv.
pub fn parse(argv: &[String]) -> Result<Call, String> {
    let options = argv.iter().position(|a| !a.starts_with("--")).unwrap_or(argv.len());
    if argv.get(options).is_some_and(|a| a == "is-active") {
        return match &argv[options + 1..] {
            [unit] => Ok(Call::IsActive(unit.clone())),
            _ => Err(format!("sim-launch: is-active takes one unit: {argv:?}")),
        };
    }
    let (mut unit, mut env) = (None, Vec::new());
    let at = argv.iter().position(|a| !a.starts_with("--")).ok_or_else(|| format!("sim-launch: no command after the unit options: {argv:?}"))?;
    for opt in &argv[..at] {
        if let Some(u) = opt.strip_prefix("--unit=") {
            unit = Some(u.to_string());
        } else if let Some((k, v)) = opt.strip_prefix("--setenv=").and_then(|kv| kv.split_once('=')) {
            env.push((k.to_string(), v.to_string()));
        }
    }
    let unit = unit.ok_or_else(|| format!("sim-launch: no --unit= in {argv:?}"))?;
    Ok(Call::Run { unit, env, command: argv[at..].to_vec() })
}

fn record(units: &Path, unit: &str) -> PathBuf {
    units.join(unit)
}

/// The recorded unit's state: `inactive` when nothing was recorded, as systemd says of a unit it
/// does not know.
pub fn state(units: &Path, unit: &str) -> String {
    std::fs::read_to_string(record(units, unit)).ok().and_then(|t| t.lines().next().map(str::to_string)).unwrap_or_else(|| "inactive".into())
}

/// What a call answered: its exit code, stdout and stderr.
#[derive(Debug, PartialEq, Eq)]
pub struct Said {
    pub code: i32,
    pub out: String,
    pub err: String,
}

/// Answers `call` against the units recorded under `units`.
pub fn answer(units: &Path, call: &Call) -> Result<Said, String> {
    match call {
        Call::IsActive(unit) => {
            let s = state(units, unit);
            Ok(Said { code: if s == "active" { 0 } else { NOT_ACTIVE }, out: format!("{s}\n"), err: String::new() })
        }
        Call::Run { unit, env, command } => {
            if state(units, unit) == "active" {
                return Ok(Said { code: 1, out: String::new(), err: format!("Failed to start transient service unit: Unit {unit}.service already exists.\n") });
            }
            std::fs::create_dir_all(units).map_err(|e| format!("{}: {e}", units.display()))?;
            let write = |body: &str| std::fs::write(record(units, unit), body).map_err(|e| e.to_string());
            write("active\n")?;
            let (prog, args) = command.split_first().ok_or("sim-launch: an empty command")?;
            let ran = crate::world::run_capture(Command::new(prog).args(args).envs(env.iter().map(|(k, v)| (k, v))), INLINE_DEADLINE);
            let exit = match &ran {
                Ok((status, _, _)) => status.code().map_or("signal".to_string(), |c| c.to_string()),
                Err(e) => format!("unrun: {e}"),
            };
            write(&format!("inactive\nexit={exit}\n"))?;
            Ok(Said { code: 0, out: String::new(), err: String::new() })
        }
    }
}
