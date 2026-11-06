//! idea-prompt pass --state FILE [--idle-secs N] [--wake-from H] [--wake-until H] [--tz ZONE] [--min-gap-secs N]
//!
//! Exit 0 swept, 3 the world could not be read or the mail could not be sent (never a green pass).

use idea_prompt::real::Real;
use idea_prompt::{parse_state, pass, render_state, Config, Outcome};
use std::path::PathBuf;
use std::process::ExitCode;

const COMPONENT: &str = "idea-prompt";
const USAGE: &str = "usage: idea-prompt pass --state FILE [--idle-secs N] [--wake-from H] [--wake-until H] [--tz ZONE] [--min-gap-secs N]";

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
    let result = pass(&Real::new(tz), &cfg, &mut state);
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
