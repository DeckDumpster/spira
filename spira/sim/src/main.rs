use spira_sim::world::{self, ProcessSteps};
use std::path::PathBuf;

const USAGE: &str = "usage: sim world up <dir> [--tree <rev>]\n       sim world down <dir>";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match run(&args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("sim: {e}");
            if e.starts_with("usage") { 2 } else { 1 }
        }
    };
    std::process::exit(code);
}

fn run(args: &[String]) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let env = |k: &str| std::env::var(k).ok();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["world", "up", dir] => world::up(&cwd, &PathBuf::from(dir), "HEAD", &env, &ProcessSteps::default()),
        ["world", "up", dir, "--tree", rev] => world::up(&cwd, &PathBuf::from(dir), rev, &env, &ProcessSteps::default()),
        ["world", "down", dir] => world::down(&PathBuf::from(dir), &ProcessSteps::default()),
        _ => Err(USAGE.to_string()),
    }
}
