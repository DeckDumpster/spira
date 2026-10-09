//! `sim summon`: the world's stand-in for the sentinel's summon of an aeon. One pass claims
//! every READY bead the scenario has scripted for the stub agent, gives it a worktree on
//! `spira/<bead>`, and runs `sim-agent` there as the claimed holder.

use crate::agent::{SCENARIO_VAR, STATE_VAR};
use crate::world::{run, LANDING_BASE};
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const CALL_DEADLINE: Duration = Duration::from_secs(120); // batch-job: spira-lc, git and the stub agent against a world
pub const HOLDER: &str = "sim-aeon";
const LEASE_UNTIL: u64 = 4_102_444_800;

/// The READY or REWORK beads in `lc_list` that `scenario` scripts, in id order, with their versions.
pub fn claimable(lc_list: &str, scenario: &str) -> Result<Vec<(String, String, i64)>, String> {
    let rows: Value = serde_json::from_str(lc_list.trim()).map_err(|e| format!("spira-lc list: not JSON: {e}"))?;
    let rows = rows.as_array().ok_or("spira-lc list: not a JSON array")?;
    let mut out = Vec::new();
    for r in rows {
        if !matches!(r["state"].as_str(), Some("READY" | "REWORK")) {
            continue;
        }
        let id = r["bead_id"].as_str().ok_or_else(|| format!("spira-lc list: a row has no bead_id: {r}"))?;
        if !scenario.lines().any(|l| l.split_whitespace().next() == Some(id)) {
            continue;
        }
        let version = match &r["version"] {
            Value::String(s) => s.trim().parse::<i64>().ok(),
            v => v.as_i64(),
        };
        out.push((id.to_string(), r["state"].as_str().unwrap_or_default().to_string(), version.ok_or_else(|| format!("spira-lc list: {id} has no numeric version"))?));
    }
    out.sort();
    Ok(out)
}

/// `sim-agent` beside the running `sim`: both are built and released together.
fn agent_bin() -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let agent = exe.with_file_name("sim-agent");
    if agent.is_file() {
        Ok(agent)
    } else {
        Err(format!("no sim-agent beside {}", exe.display()))
    }
}

fn claim_event() -> String {
    serde_json::json!({"Claim": {"holder": HOLDER, "lease_until": LEASE_UNTIL}}).to_string()
}

fn git(work: &Path, args: &[&str]) -> Result<String, String> {
    run(Command::new("git").arg("-C").arg(work).args(args), CALL_DEADLINE)
}

/// One summon pass; returns the beads it ran the agent for.
pub fn summon(world: &Path, scenario_file: &Path, state: &Path) -> Result<Vec<String>, String> {
    let scenario = std::fs::read_to_string(scenario_file).unwrap_or_default();
    let list = run(Command::new("spira-lc").arg("list"), CALL_DEADLINE)?;
    let work = world.join("work");
    let mut ran = Vec::new();
    for (id, lc_state, version) in claimable(&list, &scenario)? {
        run(
            Command::new("spira-lc").args(["event", "bead", &id, "--expect", &lc_state, "--version", &version.to_string(), "--actor", HOLDER, "--kind", &claim_event()]),
            CALL_DEADLINE,
        )?;
        let wt = world.join("wt").join(&id);
        let branch = format!("spira/{id}");
        let wt_arg = wt.display().to_string();
        let has_branch = git(&work, &["rev-parse", "--verify", "-q", &format!("refs/heads/{branch}")]).is_ok();
        if !wt.join(".git").exists() {
            if has_branch {
                git(&work, &["worktree", "add", &wt_arg, &branch])?;
            } else {
                git(&work, &["worktree", "add", "-b", &branch, &wt_arg, LANDING_BASE])?;
            }
        }
        run(
            Command::new(agent_bin()?)
                .current_dir(&wt)
                .env("BEAD_ID", &id)
                .env("SPIRA_WORK_BEAD_ID", &id)
                .env(SCENARIO_VAR, scenario_file)
                .env(STATE_VAR, state),
            CALL_DEADLINE,
        )?;
        ran.push(id);
    }
    Ok(ran)
}
