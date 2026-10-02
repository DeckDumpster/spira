//! `queue` — see DESIGN.md §2.

use std::process::ExitCode;

use queue::cli;
use queue::ports::World;
use queue::real::*;

use queue::conf::harness_home;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cmd = match cli::parse(&argv) {
        Ok(c) => c,
        Err(cli::Usage(m)) => {
            eprintln!("{m}");
            return ExitCode::from(2);
        }
    };
    if cmd == cli::Cmd::Help {
        println!("{}", cli::USAGE);
        return ExitCode::SUCCESS;
    }
    let Some(home) = harness_home() else {
        eprintln!("queue.sh: cannot find the harness (lib.sh): set SPIRA_HOME or spira.prod");
        return ExitCode::from(1);
    };
    let lib = RealLib { home: home.clone() };
    // bd, spira-lc and the forge are configured by conf.sh; resolve once for them.
    let (bd, lc) = match queue::ports::Lib::context(&lib, None) {
        Ok((s, _)) => (RealBd { bd: s.bd.clone(), db: s.db.clone() }, RealLc { bin: s.lc_bin.clone() }),
        Err(e) => {
            eprintln!("queue.sh: cannot resolve the harness configuration: {e}");
            return ExitCode::from(1);
        }
    };
    let (git, scripts, forge, config, clock, env, io) = (RealGit, RealScripts { home }, RealForge, RealConfig, SysClock, SysEnv, StdEmit::new());
    let w = World { git: &git, bd: &bd, lib: &lib, scripts: &scripts, forge: &forge, lc: &lc, config: &config, clock: &clock, env: &env, io: &io };
    let rc = queue::dispatch(&w, &cmd);
    ExitCode::from(u8::try_from(rc.clamp(0, 255)).unwrap_or(1))
}
