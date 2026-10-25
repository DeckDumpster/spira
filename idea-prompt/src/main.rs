//! idea-prompt pass --state FILE [--idle-secs N] [--wake-from H] [--wake-until H] [--tz ZONE] [--min-gap-secs N]
//!
//! Exit 0 swept, 3 the world could not be read or the mail could not be sent (never a green pass).

use idea_prompt::{parse_state, pass, render_state, Config, Idea, Outcome, Ports};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

const COMPONENT: &str = "idea-prompt";
const USAGE: &str = "usage: idea-prompt pass --state FILE [--idle-secs N] [--wake-from H] [--wake-until H] [--tz ZONE] [--min-gap-secs N]";
const BUSY_STATES: &str = "READY,REWORK,WORKING,SUBMITTED,CERTIFIED,IN_DELIVERY";

struct Real {
    tz: String,
}

fn env_tool(var: &str, default: &str) -> String {
    std::env::var(var).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| default.into())
}

impl Ports for Real {
    fn now(&self) -> u64 {
        if let Some(n) = std::env::var("SPIRA_NOW").ok().and_then(|v| v.parse().ok()) {
            return n;
        }
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
    }

    fn local_hour(&self, now: u64) -> Result<u32, String> {
        let out = spira_config::bounded::bounded("date")
            .env("TZ", &self.tz)
            .args(["-d", &format!("@{now}"), "+%H"])
            .output()
            .map_err(|e| e.to_string())?;
        String::from_utf8_lossy(&out.stdout).trim().parse().map_err(|_| format!("date gave no hour for TZ={}", self.tz))
    }

    fn busy(&self) -> Result<bool, String> {
        let out = spira_config::bounded::bounded(spira_config::lifecycle_row::lc_bin())
            .args(["list", "--state", BUSY_STATES])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!("spira-lc list exited {}", out.status));
        }
        let rows: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("spira-lc list is not JSON: {e}"))?;
        let rows = rows.as_array().ok_or("spira-lc list is not an array")?;
        Ok(rows.iter().any(|r| !has_holds(r.get("holds"))))
    }

    fn ideas(&self) -> Result<Vec<Idea>, String> {
        let out = spira_config::bounded::bounded(env_tool("IDEA_PROMPT_BEAD", "bead.sh"))
            .args(["ideas", "--json"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!("bead.sh ideas exited {}", out.status));
        }
        let rows: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("bead.sh ideas is not JSON: {e}"))?;
        let text = |r: &serde_json::Value, k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
        Ok(rows.as_array().ok_or("bead.sh ideas is not an array")?.iter().map(|r| Idea { id: text(r, "id"), title: text(r, "title") }).collect())
    }

    fn send(&self, subject: &str, default: &str, body: &str) -> Result<(), String> {
        // batch-job: mail send delivers to the operator mailbox and runs for as long as that takes
        let mut child = Command::new(env_tool("IDEA_PROMPT_MAIL", "mail"))
            .args(["send", "operator", "--from", "Spira Ideas <ideas@spira>", "--subject", subject, "--kind", "question", "--class", "design", "--default", default])
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("mail send operator: {e}"))?;
        child.stdin.take().ok_or("mail send operator: no stdin")?.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
        let st = child.wait().map_err(|e| e.to_string())?;
        if st.success() {
            Ok(())
        } else {
            Err(format!("mail send operator exited {st}"))
        }
    }
}

fn has_holds(v: Option<&serde_json::Value>) -> bool {
    match v {
        Some(serde_json::Value::String(s)) => !matches!(s.trim(), "" | "[]" | "null"),
        Some(serde_json::Value::Array(a)) => !a.is_empty(),
        _ => false,
    }
}

fn args(argv: &[String]) -> Result<(Config, String, PathBuf), String> {
    let mut cfg = Config { idle_secs: 1800, wake_from: 7, wake_until: 22, min_gap_secs: 86400 };
    let mut tz = "America/Los_Angeles".to_string();
    let mut state = None;
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        let v = it.next().ok_or_else(|| format!("{a} needs a value"))?;
        let n = || v.parse::<u64>().map_err(|_| format!("{a} {v:?} is not a number"));
        match a.as_str() {
            "--state" => state = Some(PathBuf::from(v)),
            "--idle-secs" => cfg.idle_secs = n()?,
            "--min-gap-secs" => cfg.min_gap_secs = n()?,
            "--wake-from" => cfg.wake_from = n()? as u32,
            "--wake-until" => cfg.wake_until = n()? as u32,
            "--tz" => tz = v.clone(),
            _ => return Err(format!("unknown argument {a}")),
        }
    }
    Ok((cfg, tz, state.ok_or("--state is required")?))
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) != Some("pass") {
        eprintln!("idea-prompt: unknown command\n{USAGE}");
        return ExitCode::from(2);
    }
    let (cfg, tz, state_path) = match args(&argv[1..]) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("idea-prompt: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let endpoint = spira_config::process::metrics_endpoint();
    spira_otel::oneshot::begin(endpoint.as_deref(), COMPONENT, "pass");
    let mut state = parse_state(&std::fs::read_to_string(&state_path).unwrap_or_default());
    let result = pass(&Real { tz }, &cfg, &mut state);
    if let Err(e) = std::fs::write(&state_path, render_state(&state)) {
        eprintln!("idea-prompt: cannot record {}: {e}", state_path.display());
        spira_otel::oneshot::end(3);
        return ExitCode::from(3);
    }
    match result {
        Ok(outcome) => {
            if let Outcome::Prompted(n) = outcome {
                spira_otel::oneshot::event("spira_idea_prompts_sent", 1, &[]);
                println!("idea-prompt: prompted with {n} idea(s)");
            } else {
                println!("idea-prompt: {outcome:?}");
            }
            spira_otel::oneshot::end(0);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("idea-prompt: {e}");
            spira_otel::oneshot::end(3);
            ExitCode::from(3)
        }
    }
}
