//! `sim-summon`: the world's `SPIRA_SUMMON`, the seam the real sentinel summons an aeon through.
//! It stands where `systemd-run ... aeon --home H <fayth>` stands: ranks the fayth's claimable
//! beads with `spira-claim select`, claims the first one the scenario scripts through the
//! lifecycle machine (`lc_claim_bead`), gives it a worktree on `spira/<bead>`, and runs the stub
//! agent there as the claimed holder.

use crate::agent::{SCENARIO_VAR, STATE_VAR};
use crate::world::{run, LANDING_BASE};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

const CALL_DEADLINE: Duration = Duration::from_secs(120); // batch-job: spira-claim, spira-lc, git and the stub agent against a world
pub const HOLDER: &str = "aeon-sim";
const LEASE_SECS: u64 = 3600;
const CLAIM_REFUSED: i32 = 3;
pub const CLAIM_ORDER: &str = "claim-order";
const LC_CLAIM: &str = r#". "$SPIRA_HOME/lib.sh" || exit 2; lc_claim_bead "$1" "$2" "$3" "" 0 0 "$4""#;

/// The fayth a `systemd-run` summon argv names: the argument after the `aeon` binary's path.
pub fn fayth_of(argv: &[String]) -> Result<&str, String> {
    let at = argv.iter().position(|a| Path::new(a).file_name().is_some_and(|n| n == "aeon")).ok_or("sim-summon: no aeon in the summon argv")?;
    argv.last().filter(|_| argv.len() > at + 1).map(String::as_str).ok_or_else(|| "sim-summon: the summon argv names no fayth".to_string())
}

/// The bead ids of `spira-claim select`'s ranked rows, best first (the id is the sixth field).
pub fn ranked_ids(select_out: &str) -> Vec<String> {
    select_out.lines().filter_map(|l| l.split('\t').nth(5)).filter(|s| !s.is_empty()).map(String::from).collect()
}

/// Whether `scenario` scripts the stub agent for `bead`.
pub fn scripted(scenario: &str, bead: &str) -> bool {
    scenario.lines().any(|l| l.split_whitespace().next() == Some(bead))
}

/// The repository the scenario routes `bead` to (`<bead> repo <name>`); `None` is the world's own.
pub fn routed_repo(scenario: &str, bead: &str) -> Option<String> {
    scenario.lines().find_map(|l| {
        let mut w = l.split_whitespace();
        (w.next() == Some(bead) && w.next() == Some("repo")).then(|| w.next().map(str::to_string)).flatten()
    })
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

fn git(work: &Path, args: &[&str]) -> Result<String, String> {
    run(Command::new("git").arg("-C").arg(work).args(args), CALL_DEADLINE)
}

fn claim_tool(args: &[&str], stdin: &str) -> Result<String, String> {
    let mut child = Command::new(spira_config::release_env::tool("spira-claim")).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("spira-claim: {e}"))?;
    child.stdin.take().ok_or("spira-claim: no stdin")?.write_all(stdin.as_bytes()).map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("spira-claim {}: {}: {}", args.join(" "), out.status, String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// The fayth's claimable beads in the order an aeon would take them.
fn ranked(fayth: &str, scratch: &Path) -> Result<Vec<String>, String> {
    let ready = claim_tool(&["fayth-ready", fayth, "--json"], "")?;
    let epics = scratch.join("epics");
    let resumable = scratch.join("resumable");
    std::fs::write(&epics, claim_tool(&["epics"], &ready)?).map_err(|e| e.to_string())?;
    std::fs::write(&resumable, "").map_err(|e| e.to_string())?;
    let out = claim_tool(
        &["select", "--fayth", fayth, "--epics", &epics.display().to_string(), "--resumable", &resumable.display().to_string(), "--blockers", "machine"],
        &ready,
    )?;
    Ok(ranked_ids(&out))
}

/// The lifecycle Claim, by the function an aeon claims with: 0 applied, 3 refused. An applied
/// claim is appended to the world's `claim-order` as `<bead> <holder> <lease_until> <fayth>`.
fn claim(world: &Path, id: &str, fayth: &str) -> Result<bool, String> {
    let lease = spira_config::vtime::now_epoch() + LEASE_SECS;
    let status = Command::new("bash").args(["-c", LC_CLAIM, "lc-claim", id, HOLDER, &lease.to_string(), fayth]).stdin(Stdio::null()).status().map_err(|e| format!("lc_claim_bead: {e}"))?;
    match status.code() {
        Some(0) => {
            let mut log = std::fs::OpenOptions::new().create(true).append(true).open(world.join(CLAIM_ORDER)).map_err(|e| e.to_string())?;
            writeln!(log, "{id} {HOLDER} {lease} {fayth}").map_err(|e| e.to_string())?;
            Ok(true)
        }
        Some(CLAIM_REFUSED) => Ok(false),
        other => Err(format!("lc_claim_bead {id}: exit {other:?}")),
    }
}

/// One summon: returns the bead the agent ran for, or none when nothing ranked is scripted.
pub fn summon(world: &Path, scenario_file: &Path, state: &Path, fayth: &str) -> Result<Option<String>, String> {
    let scenario = std::fs::read_to_string(scenario_file).unwrap_or_default();
    std::fs::create_dir_all(state).map_err(|e| e.to_string())?;
    let mut won = None;
    for id in ranked(fayth, state)? {
        if scripted(&scenario, &id) && claim(world, &id, fayth)? {
            won = Some(id);
            break;
        }
    }
    let Some(id) = won else { return Ok(None) };
    let repo = routed_repo(&scenario, &id);
    let work = match &repo {
        Some(name) => world.join("repos").join(name).join("work"),
        None => world.join("work"),
    };
    let wt = world.join("wt").join(&id);
    let branch = format!("spira/{id}");
    let wt_arg = wt.display().to_string();
    let has_branch = git(&work, &["rev-parse", "--verify", "-q", &format!("refs/heads/{branch}")]).is_ok();
    if !wt.join(".git").exists() {
        if has_branch {
            git(&work, &["worktree", "add", &wt_arg, &branch])?;
        } else {
            git(&work, &["worktree", "add", "-b", &branch, &wt_arg, if repo.is_some() { "HEAD" } else { LANDING_BASE }])?;
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
    Ok(Some(id))
}
