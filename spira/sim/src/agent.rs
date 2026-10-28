use crate::world::run;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const CALL_DEADLINE: Duration = Duration::from_secs(120); // batch-job: git commit and work verbs

pub const SCENARIO_VAR: &str = "SPIRA_SIM_SCENARIO";
pub const STATE_VAR: &str = "SPIRA_SIM_STATE";
pub const BRIEFS: &str = "briefs.jsonl";

#[derive(Debug, PartialEq)]
pub enum Step {
    Commit(String),
    Submit,
    /// `work submit` run from the world's primary checkout, not the bead's worktree.
    SubmitFromMain,
    /// Remove a file the branch added, in a commit of its own.
    Drop(String),
    NoProgress,
    /// Record the `slain` disposition on the bead, as an operator's slay does mid-session.
    Slain,
    Ask { question: String, default: String },
}

/// One bead's script: a list of calls, each an ordered list of steps.
/// Lines are `<bead> <verb> [args]`; `<bead> call` starts the next call.
pub fn parse(text: &str, bead: &str) -> Result<Vec<Vec<Step>>, String> {
    let mut calls: Vec<Vec<Step>> = vec![vec![]];
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
            ("repo", r) if !r.is_empty() => continue,
            ("call", "") => {
                calls.push(vec![]);
                continue;
            }
            ("commit", f) if !f.is_empty() => Step::Commit(f.to_string()),
            ("submit", "") => Step::Submit,
            ("submit-from-main", "") => Step::SubmitFromMain,
            ("drop", f) if !f.is_empty() => Step::Drop(f.to_string()),
            ("no-progress", "") => Step::NoProgress,
            ("slain", "") => Step::Slain,
            ("ask", r) if !r.is_empty() => match r.split_once(" | ") {
                Some((q, d)) => Step::Ask { question: q.trim().to_string(), default: d.trim().to_string() },
                None => return Err(at("ask needs `<question> | <default>`")),
            },
            _ => return Err(at(&format!("unknown or malformed step `{verb} {rest}`"))),
        };
        calls.last_mut().ok_or("no call")?.push(step);
    }
    if calls.iter().all(Vec::is_empty) {
        return Err(format!("scenario has no steps for {bead}"));
    }
    Ok(calls)
}

/// Which call of this bead this is, counted in `state` so it survives the process.
fn next_call(state: &Path, bead: &str) -> Result<usize, String> {
    std::fs::create_dir_all(state).map_err(|e| e.to_string())?;
    let f = state.join(format!("{bead}.calls"));
    let n = match std::fs::read_to_string(&f) {
        Ok(s) => s.trim().parse::<usize>().map_err(|e| format!("{}: {e}", f.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => return Err(e.to_string()),
    };
    std::fs::write(&f, (n + 1).to_string()).map_err(|e| e.to_string())?;
    Ok(n)
}

/// The model's restricted PATH, with the release's tools after it: the commit hook needs them and a
/// release whose binaries are links has no `spira/hooks/../../bin` to find them by.
fn hook_path() -> Option<String> {
    let path = std::env::var("PATH").ok();
    spira_config::release_env::ensured_path(std::env::var(spira_config::RELEASE_ENV).ok().as_deref(), None, path.as_deref()).map(|(_, p)| p)
}

fn git(worktree: &Path, args: &[&str]) -> Result<String, String> {
    run(
        Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(args)
            .envs(hook_path().map(|p| ("PATH", p)))
            .env("GIT_AUTHOR_NAME", "sim-agent")
            .env("GIT_AUTHOR_EMAIL", "sim@sim.invalid")
            .env("GIT_COMMITTER_NAME", "sim-agent")
            .env("GIT_COMMITTER_EMAIL", "sim@sim.invalid"),
        CALL_DEADLINE,
    )
}

fn work(worktree: &Path, args: &[&str]) -> Result<String, String> {
    run(Command::new(spira_config::release_env::tool("work")).args(args).current_dir(worktree), CALL_DEADLINE)
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
        Step::SubmitFromMain => {
            let world = std::env::var(crate::roundvm::WORLD_ENV).ok().filter(|v| !v.is_empty()).ok_or_else(|| format!("{} is not set", crate::roundvm::WORLD_ENV))?;
            work(&Path::new(&world).join("work"), &["submit"]).map(|_| ())
        }
        Step::Drop(file) => {
            git(worktree, &["rm", "-q", "--", file])?;
            git(worktree, &["commit", "-q", "-m", &format!("{bead}: sim drop {file}")]).map(|_| ())
        }
        Step::NoProgress => Ok(()),
        Step::Slain => run(Command::new(spira_config::release_env::tool("spira-lc")).args(["disposition", bead, "slain", "slain by operator", "slay"]).current_dir(worktree), CALL_DEADLINE).map(|_| ()),
        Step::Ask { question, default } => work(worktree, &["blocked", question, "--default", default]).map(|_| ()),
    }
}

/// Play this call of `bead` against `worktree`. A call past the end of the script is an error.
pub fn play(scenario: &str, state: &Path, bead: &str, worktree: &Path) -> Result<(), String> {
    let calls = parse(scenario, bead)?;
    let n = next_call(state, bead)?;
    let steps = calls.get(n).ok_or_else(|| format!("{bead}: call {} but the scenario scripts {}", n + 1, calls.len()))?;
    for (i, step) in steps.iter().enumerate() {
        exec(worktree, bead, step, i)?;
    }
    Ok(())
}

pub const RESULT: &str = r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":100,"num_turns":1,"total_cost_usd":0}"#;

/// The `--system-prompt-file` the session was launched with, read now: the dwarf removes it at teardown.
fn system_prompt(argv: &[String]) -> String {
    argv.iter().position(|a| a == "--system-prompt-file" || a == "--append-system-prompt-file").and_then(|i| argv.get(i + 1)).and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default()
}

/// One row of the `briefs` trace table: what the dwarf handed the agent for `bead` — its argv, the
/// system prompt file, the prompt on stdin, the cwd and the restricted environment. `n` counts this
/// bead's sessions from 1.
pub fn brief_row(state: &Path, bead: &str, argv: &[String], prompt: &str, cwd: &Path, env: &[(String, String)]) -> Result<serde_json::Value, String> {
    let seen = std::fs::read_to_string(state.join(BRIEFS)).unwrap_or_default().lines().filter(|l| serde_json::from_str::<serde_json::Value>(l).is_ok_and(|v| v["bead"] == bead)).count();
    let env: std::collections::BTreeMap<&str, &str> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    Ok(serde_json::json!({"bead": bead, "n": seen + 1, "argv": argv.join("\n"), "system": system_prompt(argv), "prompt": prompt, "cwd": cwd.display().to_string(), "env": serde_json::to_string(&env).map_err(|e| e.to_string())?}))
}

/// Appends the brief the agent was launched with to the world's `briefs.jsonl`.
pub fn record_brief(state: &Path, row: &serde_json::Value) -> Result<(), String> {
    use std::io::Write;
    std::fs::create_dir_all(state).map_err(|e| e.to_string())?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(state.join(BRIEFS)).map_err(|e| e.to_string())?;
    writeln!(f, "{row}").map_err(|e| e.to_string())
}
