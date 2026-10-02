//! `gate [--home <spira-dir>] [--release-bins] <branch> [repo-name]` — see DESIGN.md.
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
    // `gate wait <out>` (sp-tcarr): block on a backgrounded run's own pid, not its
    // `gate: VERDICT=` line — a run SIGKILLed mid-flight never writes one. A branch
    // literally named `wait` is `gate -- wait` (same tradeoff `testenv container` takes).
    if argv.first().map(String::as_str) == Some("wait") {
        let rc = gate::wait::main(&argv[1..]);
        std::process::exit(rc);
    }
    let mut home = None;
    if argv.first().map(String::as_str) == Some("--home") {
        if argv.len() < 2 {
            eprintln!("gate: --home needs a directory");
            std::process::exit(1);
        }
        home = Some(PathBuf::from(argv.remove(1)));
        argv.remove(0);
    }
    // `--release-bins` (sp-z61hj): on a PASS, build the judged tree's release binaries in the
    // gate tree for a hand landing (engine.rs `release_bins`).
    let mut release_bins = false;
    if argv.first().map(String::as_str) == Some("--release-bins") {
        release_bins = true;
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
        eprintln!("gate.sh: 1: usage: gate.sh [--release-bins] <branch> [repo-name]");
        std::process::exit(1);
    };
    let repo = argv.get(1).filter(|r| !r.is_empty()).cloned();
    let home = home.unwrap_or_else(default_home);
    install_signal_handlers();
    // A panic unwinding out of `Trial::run` (a bug, not a signal) would otherwise exit via
    // Rust's default panic runtime with no `gate: VERDICT=` line at all — the same hang for
    // a waiter as a SIGKILL, just self-inflicted (sp-tcarr).
    {
        let br = branch.clone();
        let rp = repo.clone().unwrap_or_else(|| "?".to_string());
        std::panic::set_hook(Box::new(move |info| {
            eprintln!(
                "gate: VERDICT=NO_VERDICT reason=exit-101 branch={br} repo={rp} suite=-"
            );
            eprintln!("gate: panic: {info}");
        }));
    }
    let world = Real::new(home.clone());
    let code = Trial::new(&world, Args { home, branch, repo, release_bins }).run();
    std::process::exit(code);
}
