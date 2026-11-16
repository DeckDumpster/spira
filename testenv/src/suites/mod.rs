//! `testenv suites` — the suite population, the gate/timed partition, the watchtower's
//! status block and flake reports.
//! Replaces spira/suites.sh. Contract: DESIGN-suites.md.

pub mod cmd;
pub mod model;
pub mod ports;
pub mod real;

use ports::World;

pub const USAGE: &str =
    "usage: testenv suites [list|names|corpus|status|observe-flake]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    List,
    Names,
    Corpus,
    Status,
    ObserveFlake { suite: String, run_id: String },
}

/// A usage error: the line for stderr; exit 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage(pub String);

pub fn parse(args: &[String]) -> Result<Cmd, Usage> {
    let Some(sub) = args.first() else { return Ok(Cmd::List) };
    let rest = &args[1..];
    let pos = |i: usize| rest.get(i).cloned().unwrap_or_default();
    Ok(match sub.as_str() {
        // suites.sh ignored anything after these
        "list" => Cmd::List,
        "names" => Cmd::Names,
        "corpus" => Cmd::Corpus,
        "status" => Cmd::Status,
        "observe-flake" => Cmd::ObserveFlake { suite: pos(0), run_id: pos(1) },
        _ => return Err(Usage(USAGE.into())),
    })
}

/// Run one parsed command; the exit code.
pub fn dispatch(w: &World, c: &Cmd) -> i32 {
    match c {
        Cmd::List => cmd::list(w),
        Cmd::Names => cmd::names(w),
        Cmd::Corpus => cmd::corpus(w),
        Cmd::Status => cmd::status(w),
        Cmd::ObserveFlake { suite, run_id } => cmd::observe_flake(w, suite, run_id),
    }
}

/// `testenv suites <args>` against the host.
pub fn main(args: &[String]) -> i32 {
    let c = match parse(args) {
        Ok(c) => c,
        Err(Usage(m)) => {
            eprintln!("{m}");
            return cmd::USAGE;
        }
    };
    let env = |k: &str| std::env::var(k).ok();
    let Some(harness) = crate::run::Harness::locate(&env) else {
        eprintln!("suites: cannot find the harness (spira/testenv/Containerfile) above the testenv binary; set SPIRA_TESTENV_HARNESS");
        return cmd::FAIL;
    };
    let s = match ports::Settings::load(&harness.root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("suites: {e}");
            return cmd::FAIL;
        }
    };
    let r = real::Real::new(&s, &env);
    let w = World {
        s: &s,
        clock: &r,
        lib: &r,
        intake: &r,
        host: &r,
        io: &r,
    };
    dispatch(&w, &c)
}

#[cfg(test)]
mod tests;
