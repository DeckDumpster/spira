use spira_sim::agent::{brief_row, play, record_brief, RESULT, SCENARIO_VAR, STATE_VAR};
use std::io::Read;
use std::path::PathBuf;

fn main() {
    let mut prompt = String::new();
    let _ = std::io::stdin().read_to_string(&mut prompt);
    spira_otel::oneshot::begin(spira_config::process::metrics_endpoint().as_deref(), "spira-sim-agent", "play");
    match run(&prompt) {
        Ok(()) => {
            println!("{RESULT}");
            spira_otel::oneshot::end(0);
        }
        Err(e) => {
            eprintln!("sim-agent: {e}");
            spira_otel::oneshot::exit(1);
        }
    }
}

fn run(prompt: &str) -> Result<(), String> {
    let need = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty()).ok_or_else(|| format!("{k} is not set"));
    let bead = match std::env::var("BEAD_ID") {
        Ok(b) if !b.is_empty() => b,
        _ => return Ok(()),
    };
    let path = need(SCENARIO_VAR)?;
    let scenario = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let state = PathBuf::from(need(STATE_VAR)?);
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let env: Vec<(String, String)> = std::env::vars().collect();
    record_brief(&state, &brief_row(&state, &bead, &argv, prompt, &cwd, &env)?)?;
    play(&scenario, &state, &bead, &cwd)
}
