use spira_sim::agent::{play, RESULT, SCENARIO_VAR, STATE_VAR};
use std::io::Read;
use std::path::PathBuf;

fn main() {
    let mut prompt = String::new();
    let _ = std::io::stdin().read_to_string(&mut prompt);
    match run() {
        Ok(()) => println!("{RESULT}"),
        Err(e) => {
            eprintln!("sim-agent: {e}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<(), String> {
    let need = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty()).ok_or_else(|| format!("{k} is not set"));
    let bead = match std::env::var("BEAD_ID") {
        Ok(b) if !b.is_empty() => b,
        _ => return Ok(()),
    };
    let path = need(SCENARIO_VAR)?;
    let scenario = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    play(&scenario, &PathBuf::from(need(STATE_VAR)?), &bead, &cwd)
}
