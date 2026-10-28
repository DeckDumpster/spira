//! `work review`: one read-only reviewer session over the bound work item and the branch diff.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const REVIEWER_PROMPT: &str = include_str!("../../spira/runes/reviewer.md");
// batch-job: a reviewer session reads a whole diff and codebase; the caller may lower it with --timeout
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);
pub const TIMED_OUT: i32 = 124;

pub const HELP: &str = "work review [--timeout SECONDS] [--base REF]
  A second reader before you submit. Launches one read-only reviewer session over this bead's
  text (as `work show` gives it) and your branch's diff against REF (default local/main), and
  prints its findings, one `- file:line — what to change` per line. It changes nothing.
  Answer each finding in your submission. Exits 124 if the reviewer outlives the timeout
  (default 600 s) and says so; 3 if it cannot run.";

pub struct Args {
    pub timeout: Duration,
    pub base: String,
}

pub fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args { timeout: DEFAULT_TIMEOUT, base: "local/main".to_string() };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--timeout" => {
                let v = it.next().ok_or("--timeout wants a number of seconds")?;
                let n: u64 = v.parse().map_err(|_| format!("--timeout {v:?} is not a number of seconds"))?;
                if n == 0 {
                    return Err("--timeout must be at least 1 second".to_string());
                }
                out.timeout = Duration::from_secs(n);
            }
            "--base" => out.base = it.next().ok_or("--base wants a ref")?.clone(),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(out)
}

pub fn input(item: &str, diff: &str) -> String {
    format!("## Work item\n\n{}\n\n## Diff\n\n{}\n", item.trim(), if diff.trim().is_empty() { "(the branch has no diff against the base)" } else { diff.trim_end() })
}

pub fn agent_argv(model: &str) -> Vec<String> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let mut a = s(&["-p", "--output-format", "stream-json", "--verbose", "--model", model, "--system-prompt", REVIEWER_PROMPT]);
    a.extend(s(&["--tools", "Read,Grep,Glob,Bash", "--allowedTools", "Read,Grep,Glob,Bash(work law:*)"]));
    a
}

pub fn persona_model(persona: &str, spec: &str) -> Result<String, String> {
    if spec.is_empty() {
        return Err("SPIRA_TOML is not set — it names the one source of config".into());
    }
    spira_config::load(Path::new(spec))?
        .persona
        .get(persona)
        .map(|p| p.model.clone())
        .filter(|m| !m.is_empty())
        .ok_or_else(|| format!("persona.{persona}.model is not declared in {spec}"))
}

pub enum Outcome {
    Findings(String),
    TimedOut,
    Failed(String),
}

/// What the reviewer did, read from its stream-json transcript.
#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    pub tool_calls: std::collections::BTreeMap<String, u64>,
    pub law_calls: Vec<String>,
    pub findings: usize,
}

/// The reviewer's final text: the `result` event of a stream-json transcript.
pub fn verdict_text(transcript: &str) -> Option<String> {
    transcript.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()).filter(|v| v["type"] == "result").filter_map(|v| v["result"].as_str().map(str::to_string)).next_back()
}

/// `work law <arg>` when a Bash command is a `work law` call; the arg is empty for the table of contents.
fn law_arg(command: &str) -> Option<String> {
    let rest = command.trim().strip_prefix("work law")?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim().to_string())
}

pub fn summarize(transcript: &str, verdict: &str) -> Summary {
    let mut out = Summary { findings: verdict.lines().filter(|l| l.starts_with("- ")).count(), ..Summary::default() };
    for v in transcript.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()) {
        if v["type"] != "assistant" {
            continue;
        }
        for block in v["message"]["content"].as_array().into_iter().flatten().filter(|b| b["type"] == "tool_use") {
            let name = block["name"].as_str().unwrap_or("?");
            *out.tool_calls.entry(name.to_string()).or_default() += 1;
            if name == "Bash" {
                out.law_calls.extend(block["input"]["command"].as_str().and_then(law_arg));
            }
        }
    }
    out
}

/// Keeps the transcript at `<run>/<work-id>.review-<n>.jsonl` (n counts this session's reviews) and
/// appends one summary line to `<run>/review-summary.jsonl`.
pub fn keep(run_dir: &Path, work_id: &str, transcript: &str, verdict: &str) -> Result<std::path::PathBuf, String> {
    let io = |e: std::io::Error| format!("cannot keep the review transcript under {}: {e}", run_dir.display());
    std::fs::create_dir_all(run_dir).map_err(io)?;
    let prefix = format!("{work_id}.review-");
    let taken = std::fs::read_dir(run_dir).map_err(io)?.flatten().filter(|e| e.file_name().to_string_lossy().starts_with(&prefix)).count();
    let path = run_dir.join(format!("{prefix}{}.jsonl", taken + 1));
    std::fs::write(&path, transcript).map_err(io)?;
    let s = summarize(transcript, verdict);
    let line = serde_json::json!({"work": work_id, "review": taken + 1, "transcript": path, "tool_calls": s.tool_calls, "law_calls": s.law_calls.len(), "law_args": s.law_calls, "findings": s.findings});
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(run_dir.join("review-summary.jsonl")).map_err(io)?;
    writeln!(f, "{line}").map_err(io)?;
    Ok(path)
}

pub fn run(agent: &str, argv: &[String], stdin: &str, cwd: &Path, timeout: Duration) -> Result<(Outcome, String), String> {
    let mut child = Command::new(agent)
        .args(argv)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot launch {agent}: {e}"))?;
    let mut sin = child.stdin.take().expect("piped");
    let body = stdin.to_string();
    let feeder = std::thread::spawn(move || {
        let _ = sin.write_all(body.as_bytes());
    });
    let mut so = child.stdout.take().expect("piped");
    let mut se = child.stderr.take().expect("piped");
    let out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = so.read_to_string(&mut s);
        s
    });
    let err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = se.read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(st) => break Some(st),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    let _ = feeder.join();
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    match status {
        None => Ok((Outcome::TimedOut, out)),
        Some(st) => match verdict_text(&out).filter(|v| !v.trim().is_empty()) {
            Some(v) if st.success() => Ok((Outcome::Findings(v), out)),
            _ => Ok((Outcome::Failed(format!("the reviewer session failed ({st}): {}", err.trim())), out)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reviewer_prompt_renders_unchanged() {
        assert_eq!(REVIEWER_PROMPT, include_str!("../tests/golden/reviewer.md"));
    }

    #[test]
    fn the_prompt_is_the_whole_system_prompt_and_the_tools_are_read_only() {
        let a = agent_argv("m");
        let at = |f: &str| a.iter().position(|x| x == f).map(|i| a[i + 1].clone());
        assert_eq!(at("--system-prompt").as_deref(), Some(REVIEWER_PROMPT));
        assert_eq!(at("--model").as_deref(), Some("m"));
        assert!(!a.iter().any(|x| x.contains("Edit") || x.contains("Write") || x == "--dangerously-skip-permissions"));
    }

    #[test]
    fn arguments_parse_and_refuse() {
        let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let a = parse_args(&v(&["--timeout", "7", "--base", "main"])).unwrap();
        assert_eq!((a.timeout, a.base.as_str()), (Duration::from_secs(7), "main"));
        assert!(parse_args(&v(&["--timeout", "0"])).is_err());
        assert!(parse_args(&v(&["--timeout"])).is_err());
        assert!(parse_args(&v(&["x"])).is_err());
    }

    #[test]
    fn a_transcript_summarises_tool_calls_law_calls_and_findings() {
        let t = concat!(
            "{\"type\":\"system\"}\n",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Read\",\"input\":{}},{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":{\"command\":\"work law 3.2\"}}]}}\n",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":{\"command\":\"git log\"}}]}}\n",
            "{\"type\":\"result\",\"result\":\"- a.rs:1 — x\\n- b.rs:2 — y\"}\n"
        );
        let v = verdict_text(t).unwrap();
        let s = summarize(t, &v);
        assert_eq!((s.tool_calls["Read"], s.tool_calls["Bash"], s.law_calls, s.findings), (1, 2, vec!["3.2".to_string()], 2));
    }

    #[test]
    fn a_command_that_merely_starts_like_work_law_is_not_a_law_call() {
        assert_eq!(law_arg("work lawful"), None);
        assert_eq!(law_arg("work law"), Some(String::new()));
    }

    #[test]
    fn an_empty_diff_says_so() {
        assert!(input("item", "").contains("no diff"));
    }
}
