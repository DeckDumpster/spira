//! spira-admit — the admission pools' agent-facing entry (sp-f4ig1; gate/DESIGN-admission.md).
//!
//! ```text
//! spira-admit status [--json]                              every pool: size, held, waiting, holders
//! spira-admit <rustc> <args…>                              retired RUSTC_WRAPPER: exec, touch no pool
//! ```

use spira_config::admission::{self, Pool, RealProcs};
use std::ffi::OsString;
use std::path::PathBuf;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

/// `spira.run` alone, never the whole registry: a tree that adds a config key must still be
/// readable under a spira.toml that does not declare it yet.
fn run_dir() -> Result<PathBuf, String> {
    spira_config::resolve::declared_run_dir_for_process()
}

fn say(line: &str) {
    eprintln!("spira-admit: {line}");
}

fn usage() -> ExitCode {
    eprintln!("usage: spira-admit status [--json]");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match args.first().and_then(|a| a.to_str()) {
        Some("status") => {
            spira_otel::oneshot::begin(spira_config::process::metrics_endpoint().as_deref(), "spira-admit", "status");
            spira_otel::oneshot::finish(status(&args[1..]))
        }
        None | Some("-h" | "--help" | "help") => usage(),
        Some(_) => wrapper(&args),
    }
}

/// Sessions started before compile admission was deleted still carry this binary as their
/// RUSTC_WRAPPER: it execs the compiler and reads or writes no pool file.
fn wrapper(args: &[OsString]) -> ExitCode {
    let inner = std::env::var(admission::INNER_ENV).ok();
    let (prog, argv) = admission::passthrough(inner.as_deref(), args);
    // batch-job: the compiler is exec-replaced, not awaited under a deadline
    let e = Command::new(&prog).args(argv).exec();
    say(&format!("cannot exec {}: {e}", prog.to_string_lossy()));
    ExitCode::from(127)
}

fn status(args: &[OsString]) -> ExitCode {
    let json = args.iter().any(|a| a == "--json");
    let run = match run_dir() {
        Ok(run) => run,
        Err(e) => {
            say(&e);
            return ExitCode::from(2);
        }
    };
    let occ: Vec<admission::Occupancy> = Pool::ALL
        .into_iter()
        .map(|p| {
            let size = admission::size_configured(p);
            match p {
                Pool::Gate => admission::gate_occupancy(&run, size),
                _ => admission::occupancy(&run, p, size, &RealProcs),
            }
        })
        .collect();
    let now = admission::now_epoch();
    if json {
        let v: Vec<serde_json::Value> = occ
            .iter()
            .map(|o| {
                serde_json::json!({
                    "pool": o.pool.name(),
                    "size": o.size,
                    "held": o.holders.len(),
                    "waiting": o.waiting,
                    "holders": o.holders.iter().map(|l| serde_json::json!({
                        "slot": l.slot, "pid": l.pid, "who": l.who,
                        "secs": if l.since > 0 { now.saturating_sub(l.since) } else { 0 },
                    })).collect::<Vec<_>>(),
                    "waiters": o.waiters.iter().map(|w| serde_json::json!({
                        "pid": w.pid, "who": w.who, "weight": w.weight,
                        "secs": now.saturating_sub(w.since),
                    })).collect::<Vec<_>>(),
                    "head_blocked": o.head_blocked(),
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(v));
    } else {
        for o in &occ {
            let names: Vec<String> = o
                .holders
                .iter()
                .map(|l| {
                    if l.since > 0 {
                        format!("{} (pid {}, {}s)", l.who, l.pid, now.saturating_sub(l.since))
                    } else {
                        l.who.clone()
                    }
                })
                .collect();
            println!(
                "{:<8} {} of {} held, {} waiting{}{}",
                o.pool.name(),
                o.holders.len(),
                o.size,
                o.waiting,
                if names.is_empty() { "" } else { ": " },
                names.join(", ")
            );
            for w in &o.waiters {
                let wt = if w.weight > 1 { format!(" ×{}", w.weight) } else { String::new() };
                println!("           waiting: {}{wt} (pid {}, {}s)", w.who, w.pid, now.saturating_sub(w.since));
            }
            if let Some(line) = o.head_blocked() {
                println!("           {line}");
            }
        }
    }
    ExitCode::SUCCESS
}
