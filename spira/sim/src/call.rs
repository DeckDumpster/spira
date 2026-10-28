//! `sim-call`: the world's `SPIRA_CALL`, the seam the real sentinel calls a dwarf through.
//! It stands where `systemd-run ... dwarf --home H <rune>` stands: ranks the rune's claimable
//! beads with `spira-claim select`, claims the first one the scenario scripts through the
//! lifecycle machine (`lc_claim_bead`), gives it a worktree on `spira/<bead>`, and runs the stub
//! agent there as the claimed holder. A scenario that carries [`REAL_DWARF`] runs the real `dwarf`
//! the argv names instead, with the stub agent as its `SPIRA_AGENT`.

use crate::agent::{SCENARIO_VAR, STATE_VAR};
use crate::world::{run, run_capture, LANDING_BASE};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

const CALL_DEADLINE: Duration = Duration::from_secs(120); // batch-job: spira-claim, spira-lc, git and the stub agent against a world
pub const HOLDER: &str = "dwarf-sim";
const LEASE_SECS: u64 = 3600;
const CLAIM_REFUSED: i32 = 3;
/// The scenario line that turns the call into the real `dwarf`.
const DWARF_HOME: &str = "dwarf-home";
pub const REAL_DWARF: &str = "@real-dwarf";
const DWARF_DEADLINE: Duration = Duration::from_secs(300); // batch-job: one real dwarf session around the stub agent
pub const CLAIM_ORDER: &str = "claim-order";
const LC_CLAIM: &str = r#". "$SPIRA_HOME/lib.sh" || exit 2; lc_claim_bead "$1" "$2" "$3" "" 0 0 "$4""#;

/// The rune a `systemd-run` call argv names: the argument after the `dwarf` binary's path.
pub fn rune_of(argv: &[String]) -> Result<&str, String> {
    let at = argv.iter().position(|a| Path::new(a).file_name().is_some_and(|n| n == "dwarf")).ok_or("sim-call: no dwarf in the call argv")?;
    argv.last().filter(|_| argv.len() > at + 1).map(String::as_str).ok_or_else(|| "sim-call: the call argv names no rune".to_string())
}

/// Whether the scenario runs the real dwarf.
pub fn real_dwarf(scenario: &str) -> bool {
    scenario.lines().any(|l| l.trim() == REAL_DWARF)
}

/// The `systemd-run` argv as the command it launches: the `--setenv=K=V` pairs, and the program
/// and arguments from the `dwarf` binary on.
#[allow(clippy::type_complexity)]
pub fn dwarf_command(argv: &[String]) -> Result<(Vec<(String, String)>, Vec<String>), String> {
    let at = argv.iter().position(|a| Path::new(a).file_name().is_some_and(|n| n == "dwarf")).ok_or("sim-call: no dwarf in the call argv")?;
    let env = argv[..at].iter().filter_map(|a| a.strip_prefix("--setenv=")).filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect();
    Ok((env, argv[at..].to_vec()))
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
    let mut child = Command::new(spira_config::release_env::tool("spira-claim")).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() // batch-job: spira-claim against a world, stdin-fed
        .map_err(|e| format!("spira-claim: {e}"))?;
    child.stdin.take().ok_or("spira-claim: no stdin")?.write_all(stdin.as_bytes()).map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("spira-claim {}: {}: {}", args.join(" "), out.status, String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// The rune's claimable beads in the order a dwarf would take them.
fn ranked(rune: &str, scratch: &Path) -> Result<Vec<String>, String> {
    let ready = claim_tool(&["rune-ready", rune, "--json"], "")?;
    let epics = scratch.join("epics");
    let resumable = scratch.join("resumable");
    std::fs::write(&epics, claim_tool(&["epics"], &ready)?).map_err(|e| e.to_string())?;
    std::fs::write(&resumable, "").map_err(|e| e.to_string())?;
    let out = claim_tool(
        &["select", "--rune", rune, "--epics", &epics.display().to_string(), "--resumable", &resumable.display().to_string(), "--blockers", "machine"],
        &ready,
    )?;
    Ok(ranked_ids(&out))
}

/// The lifecycle Claim, by the function a dwarf claims with: 0 applied, 3 refused. An applied
/// claim is appended to the world's `claim-order` as `<bead> <holder> <lease_until> <rune>`.
fn claim(world: &Path, id: &str, rune: &str) -> Result<bool, String> {
    let lease = spira_config::vtime::now_epoch() + LEASE_SECS;
    let (status, _, _) = run_capture(
        Command::new("bash").args(["-c", LC_CLAIM, "lc-claim", id, HOLDER, &lease.to_string(), rune]).envs(spira_config::release_env::child_path_env_for_process()),
        CALL_DEADLINE,
    )?;
    match status.code() {
        Some(0) => {
            let mut log = std::fs::OpenOptions::new().create(true).append(true).open(world.join(CLAIM_ORDER)).map_err(|e| e.to_string())?;
            writeln!(log, "{id} {HOLDER} {lease} {rune}").map_err(|e| e.to_string())?;
            Ok(true)
        }
        Some(CLAIM_REFUSED) => Ok(false),
        other => Err(format!("lc_claim_bead {id}: exit {other:?}")),
    }
}

/// The home the real dwarf is given: a directory of links to every entry of the release's harness
/// tree, so the dwarf's guard against RUNE keys nothing consumes finds no source workspace above it
/// to judge against, as in a fixture home.
fn dwarf_home(world: &Path, harness: &Path) -> Result<std::path::PathBuf, String> {
    let home = world.join(DWARF_HOME);
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(harness).map_err(|e| format!("{}: {e}", harness.display()))? {
        let from = entry.map_err(|e| e.to_string())?.path();
        let at = home.join(from.file_name().ok_or("unnamed harness entry")?);
        if std::fs::symlink_metadata(&at).is_err() {
            std::os::unix::fs::symlink(&from, &at).map_err(|e| format!("{}: {e}", at.display()))?;
        }
    }
    Ok(home)
}

/// `argv` with the `--home` argument of the `dwarf` it launches replaced by `home`.
pub fn with_home(argv: &[String], home: &Path) -> Vec<String> {
    let mut out = argv.to_vec();
    if let Some(i) = out.iter().position(|a| a == "--home") {
        if let Some(h) = out.get_mut(i + 1) {
            *h = home.display().to_string();
        }
    }
    out
}

/// One real dwarf session: the dwarf ranks, claims, builds its worktree and launches `SPIRA_AGENT`
/// (the stub) itself. Its output goes to the call's stdout, and so to the world's `exec.log`.
fn run_dwarf(world: &Path, argv: &[String]) -> Result<(), String> {
    let harness = argv.iter().position(|a| a == "--home").and_then(|i| argv.get(i + 1)).ok_or("sim-call: the dwarf argv names no --home")?;
    let (mut env, command) = dwarf_command(&with_home(argv, &dwarf_home(world, Path::new(harness))?))?;
    for (_, path) in env.iter_mut().filter(|(k, _)| k == "PATH") {
        *path = format!("{}:{path}", world.join("bin").display());
    }
    let (prog, args) = command.split_first().ok_or("sim-call: empty dwarf command")?;
    let (status, out, err) = run_capture(Command::new(prog).args(args).envs(spira_config::release_env::child_path_env_for_process()).envs(env), DWARF_DEADLINE)?;
    print!("{out}{err}");
    println!("sim call: dwarf exited {status}");
    Ok(())
}

/// What one call did.
#[derive(Debug, PartialEq)]
pub enum Ran {
    /// The stub agent ran for this bead.
    Stub(String),
    /// The real dwarf ran, and chose its bead itself.
    Dwarf,
    Nothing,
}

/// One call: the stub agent for the first ranked bead the scenario scripts, or with
/// [`REAL_DWARF`] the real dwarf.
pub fn call(world: &Path, scenario_file: &Path, state: &Path, rune: &str, argv: &[String]) -> Result<Ran, String> {
    let scenario = std::fs::read_to_string(scenario_file).unwrap_or_default();
    if real_dwarf(&scenario) {
        run_dwarf(world, argv)?;
        return Ok(Ran::Dwarf);
    }
    std::fs::create_dir_all(state).map_err(|e| e.to_string())?;
    let mut won = None;
    for id in ranked(rune, state)? {
        if scripted(&scenario, &id) && claim(world, &id, rune)? {
            won = Some(id);
            break;
        }
    }
    let Some(id) = won else { return Ok(Ran::Nothing) };
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
    Ok(Ran::Stub(id))
}
