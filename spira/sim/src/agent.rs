use crate::world::run;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const CALL_DEADLINE: Duration = Duration::from_secs(120); // batch-job: git commit and work verbs

pub const SCENARIO_VAR: &str = "SPIRA_SIM_SCENARIO";
pub const STATE_VAR: &str = "SPIRA_SIM_STATE";

#[derive(Debug, PartialEq)]
pub enum Step {
    Commit(String),
    Submit,
    NoProgress,
    Ask { question: String, default: String },
}

/// One bead's script: a list of summons, each an ordered list of steps.
/// Lines are `<bead> <verb> [args]`; `<bead> summon` starts the next summon.
pub fn parse(text: &str, bead: &str) -> Result<Vec<Vec<Step>>, String> {
    let mut summons: Vec<Vec<Step>> = vec![vec![]];
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(3, char::is_whitespace);
        let (who, verb, rest) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or("").trim());
        if who != bead {
            continue;
        }
        let at = |m: &str| format!("scenario line {}: {m}", n + 1);
        let step = match (verb, rest) {
            ("summon", "") => {
                summons.push(vec![]);
                continue;
            }
            ("commit", f) if !f.is_empty() => Step::Commit(f.to_string()),
            ("submit", "") => Step::Submit,
            ("no-progress", "") => Step::NoProgress,
            ("ask", r) if !r.is_empty() => match r.split_once(" | ") {
                Some((q, d)) => Step::Ask { question: q.trim().to_string(), default: d.trim().to_string() },
                None => return Err(at("ask needs `<question> | <default>`")),
            },
            _ => return Err(at(&format!("unknown or malformed step `{verb} {rest}`"))),
        };
        summons.last_mut().ok_or("no summon")?.push(step);
    }
    if summons.iter().all(Vec::is_empty) {
        return Err(format!("scenario has no steps for {bead}"));
    }
    Ok(summons)
}

/// Which summon of this bead this is, counted in `state` so it survives the process.
fn next_summon(state: &Path, bead: &str) -> Result<usize, String> {
    std::fs::create_dir_all(state).map_err(|e| e.to_string())?;
    let f = state.join(format!("{bead}.summons"));
    let n = match std::fs::read_to_string(&f) {
        Ok(s) => s.trim().parse::<usize>().map_err(|e| format!("{}: {e}", f.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => return Err(e.to_string()),
    };
    std::fs::write(&f, (n + 1).to_string()).map_err(|e| e.to_string())?;
    Ok(n)
}

fn git(worktree: &Path, args: &[&str]) -> Result<String, String> {
    run(
        Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(args)
            .env("GIT_AUTHOR_NAME", "sim-agent")
            .env("GIT_AUTHOR_EMAIL", "sim@sim.invalid")
            .env("GIT_COMMITTER_NAME", "sim-agent")
            .env("GIT_COMMITTER_EMAIL", "sim@sim.invalid"),
        CALL_DEADLINE,
    )
}

fn work(worktree: &Path, args: &[&str]) -> Result<String, String> {
    run(Command::new("work").args(args).current_dir(worktree), CALL_DEADLINE)
}

fn exec(worktree: &Path, bead: &str, step: &Step, ordinal: usize) -> Result<(), String> {
    match step {
        Step::Commit(file) => {
            let body = format!("{bead} {ordinal}\n");
            let path: PathBuf = worktree.join(file);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            let mut prior = std::fs::read_to_string(&path).unwrap_or_default();
            prior.push_str(&body);
            std::fs::write(&path, prior).map_err(|e| e.to_string())?;
            git(worktree, &["add", "--", file])?;
            git(worktree, &["commit", "-q", "-m", &format!("{bead}: sim commit {file}")]).map(|_| ())
        }
        Step::Submit => work(worktree, &["submit"]).map(|_| ()),
        Step::NoProgress => Ok(()),
        Step::Ask { question, default } => work(worktree, &["blocked", question, "--default", default]).map(|_| ()),
    }
}

/// Play this summon of `bead` against `worktree`. A summon past the end of the script is an error.
pub fn play(scenario: &str, state: &Path, bead: &str, worktree: &Path) -> Result<(), String> {
    let summons = parse(scenario, bead)?;
    let n = next_summon(state, bead)?;
    let steps = summons.get(n).ok_or_else(|| format!("{bead}: summon {} but the scenario scripts {}", n + 1, summons.len()))?;
    for (i, step) in steps.iter().enumerate() {
        exec(worktree, bead, step, i)?;
    }
    Ok(())
}

pub const RESULT: &str = r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":100,"num_turns":1,"total_cost_usd":0}"#;
