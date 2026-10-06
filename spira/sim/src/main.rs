use spira_sim::gh;
use spira_sim::world::{self, ProcessSteps};
use std::path::PathBuf;

const USAGE: &str = "usage: sim world up <dir> [--tree <rev>]\n       sim world down <dir>\n       sim gh <gh arguments...>\n       sim ghctl <state-dir> <verb> ...";

fn main() {
    let mut args: Vec<String> = std::env::args().collect();
    let invoked_as_gh = args.first().and_then(|a| std::path::Path::new(a).file_name()).is_some_and(|n| n == "gh");
    args.remove(0);
    if invoked_as_gh {
        args.insert(0, "gh".to_string());
    }
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
    let state_dir = || std::env::var_os(gh::STATE_ENV).filter(|v| !v.is_empty()).map(PathBuf::from).ok_or(format!("{} is not set: sim gh answers only inside a world", gh::STATE_ENV));
    match args.first().map(String::as_str) {
        Some("gh") => return gh_main(&state_dir()?, &cwd, &args[1..]),
        Some("ghctl") => {
            let (dir, rest) = args[1..].split_first().ok_or(USAGE.to_string())?;
            print!("{}", gh::ctl(&PathBuf::from(dir), rest)?);
            return Ok(());
        }
        _ => {}
    }
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["world", "up", dir] => world::up(&cwd, &PathBuf::from(dir), "HEAD", &env, &ProcessSteps::default()),
        ["world", "up", dir, "--tree", rev] => world::up(&cwd, &PathBuf::from(dir), rev, &env, &ProcessSteps::default()),
        ["world", "down", dir] => world::down(&PathBuf::from(dir), &ProcessSteps::default()),
        _ => Err(USAGE.to_string()),
    }
}

fn gh_main(state: &std::path::Path, cwd: &std::path::Path, args: &[String]) -> Result<(), String> {
    use std::io::Write;
    let out = gh::run(state, cwd, args)?;
    let _ = std::io::stdout().write_all(&out.stdout);
    eprint!("{}", out.stderr);
    if out.code != 0 {
        std::process::exit(out.code);
    }
    Ok(())
}
