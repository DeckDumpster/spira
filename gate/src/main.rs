//! `gate [--home <spira-dir>] <branch> [repo-name]` — see DESIGN.md.
//! `gate [--home <spira-dir>] --definition [repo-name]` — the landing ref's gate command.

use gate::engine::{Args, Trial};
use gate::real::{install_signal_handlers, Real};
use std::path::PathBuf;

fn default_home() -> PathBuf {
    if let Some(h) = std::env::var_os("SPIRA_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(h);
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().and_then(|d| d.parent()).map(|r| r.join("spira")))
        .unwrap_or_else(|| PathBuf::from("spira"))
}

fn main() {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    let mut home = None;
    if argv.first().map(String::as_str) == Some("--home") {
        if argv.len() < 2 {
            eprintln!("gate: --home needs a directory");
            std::process::exit(1);
        }
        home = Some(PathBuf::from(argv.remove(1)));
        argv.remove(0);
    }
    if argv.first().map(String::as_str) == Some("--definition") {
        let repo = argv.get(1).filter(|r| !r.is_empty()).cloned();
        let world = Real::new(home.clone().unwrap_or_else(default_home));
        match gate::engine::definition(&world, repo.as_deref()) {
            Ok(cmd) => {
                println!("{cmd}");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("gate: --definition: {e}");
                std::process::exit(1);
            }
        }
    }
    let Some(branch) = argv.first().filter(|b| !b.is_empty()).cloned() else {
        eprintln!("gate.sh: 1: usage: gate.sh <branch> [repo-name]");
        std::process::exit(1);
    };
    let repo = argv.get(1).filter(|r| !r.is_empty()).cloned();
    let home = home.unwrap_or_else(default_home);
    install_signal_handlers();
    let world = Real::new(home.clone());
    let code = Trial::new(&world, Args { home, branch, repo }).run();
    std::process::exit(code);
}
