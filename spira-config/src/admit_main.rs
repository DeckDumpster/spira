//! spira-admit — the admission pools' agent-facing entry (sp-f4ig1; gate/DESIGN-admission.md).
//!
//! ```text
//! spira-admit <rustc> <args…>                              RUSTC_WRAPPER mode (§3.3)
//! spira-admit status [--json]                              every pool: size, held, waiting, holders
//! spira-admit run --pool compile|test [--who W] [--weight N] -- <cmd…>  hold a lease around a command
//! ```

use spira_config::admission::{self, Pool, RealProcs};
use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

fn var(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.trim().is_empty())
}

/// `spira.run`, resolved through config; a resolution failure is named, not read as unset.
fn run_dir() -> Option<PathBuf> {
    match spira_config::process::cfg("SPIRA_RUN") {
        Ok(r) if !r.is_empty() => Some(PathBuf::from(r)),
        Ok(_) => None,
        Err(e) => {
            say(&e);
            None
        }
    }
}

fn who() -> String {
    var(admission::WHO_ENV)
        .or_else(|| var("SPIRA_WORK_BEAD_ID"))
        .or_else(|| var("BEAD_ID"))
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|d| d.file_name().map(|f| f.to_string_lossy().into_owned()))
        })
        .unwrap_or_else(|| "-".into())
}

fn say(line: &str) {
    eprintln!("spira-admit: {line}");
}

fn usage() -> ExitCode {
    eprintln!("usage: spira-admit <rustc> <args…>   (as RUSTC_WRAPPER)");
    eprintln!("       spira-admit status [--json]");
    eprintln!("       spira-admit run --pool compile|test [--who W] [--weight N] -- <cmd…>");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match args.first().and_then(|a| a.to_str()) {
        None | Some("-h") | Some("--help") | Some("help") => usage(),
        Some("status") => status(&args[1..]),
        Some("run") => run(&args[1..]),
        Some(_) => wrapper(args),
    }
}

/// RUSTC_WRAPPER mode: admit the cargo (our parent) to a compile slot, then exec the inner
/// compiler. Only a crate compile waits; probes and inherited jobs exec at once.
fn wrapper(args: Vec<OsString>) -> ExitCode {
    let rest: Vec<String> = args[1..].iter().map(|a| a.to_string_lossy().into_owned()).collect();
    match admission::wrapper_action(var(admission::INHERIT_ENV).as_deref(), &rest) {
        admission::WrapperAction::Exec => {}
        admission::WrapperAction::Wait => {
            if let Some(run) = run_dir() {
                let who = who();
                let q = admission::Request { run: &run, pool: Pool::Compile, holder_pid: std::os::unix::process::parent_id(), who: &who, inherit: None, weight: admission::build_weight(&rest) };
                let g = admission::acquire_real(&q, &mut |l: &str| say(l));
                // The lease is the cargo's, not ours: it ends when the cargo does.
                std::mem::forget(g);
            }
        }
        admission::WrapperAction::TakeNow => {
            // A gate's build (D11): a lease for the cargo at once, oversubscribing a full pool.
            if let Some(run) = run_dir() {
                let who = who();
                let q = admission::Request { run: &run, pool: Pool::Compile, holder_pid: std::os::unix::process::parent_id(), who: &who, inherit: None, weight: admission::build_weight(&rest) };
                let g = admission::take_now_guard(&q, admission::size_from_env(Pool::Compile), &RealProcs, &mut |l: &str| say(l));
                std::mem::forget(g);
            }
        }
    }
    let (prog, argv): (OsString, &[OsString]) = match var(admission::INNER_ENV) {
        Some(inner) => (OsString::from(inner), &args[..]),
        None => (args[0].clone(), &args[1..]),
    };
    // batch-job: child is spawned or exec-replaced, not awaited under a deadline
    let e = Command::new(&prog).args(argv).exec();
    say(&format!("cannot exec {}: {e}", prog.to_string_lossy()));
    ExitCode::from(127)
}

fn status(args: &[OsString]) -> ExitCode {
    let json = args.iter().any(|a| a == "--json");
    let Some(run) = run_dir() else {
        say("no run directory: set SPIRA_RUN or configure spira.run");
        return ExitCode::from(2);
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

fn run(args: &[OsString]) -> ExitCode {
    let mut pool = None;
    let mut who_arg = None;
    let mut weight: u64 = 1;
    let mut i = 0;
    while i < args.len() {
        match args[i].to_str() {
            Some("--pool") => {
                pool = args.get(i + 1).and_then(|a| a.to_str()).and_then(Pool::parse);
                i += 2;
            }
            Some("--weight") => {
                weight = args.get(i + 1).and_then(|a| a.to_str()).and_then(|w| w.parse().ok()).unwrap_or(1);
                i += 2;
            }
            Some("--who") => {
                who_arg = args.get(i + 1).map(|a| a.to_string_lossy().into_owned());
                i += 2;
            }
            Some("--") => {
                i += 1;
                break;
            }
            _ => return usage(),
        }
    }
    let cmd = &args[i.min(args.len())..];
    let (Some(pool), Some(prog)) = (pool.filter(|p| *p != Pool::Gate), cmd.first()) else {
        return usage();
    };
    let Some(run) = run_dir() else {
        say("no run directory: set SPIRA_RUN or configure spira.run");
        return ExitCode::from(2);
    };
    let who = who_arg.unwrap_or_else(who);
    let inherit = var(admission::INHERIT_ENV);
    let q = admission::Request { run: &run, pool, holder_pid: std::process::id(), who: &who, inherit: inherit.as_deref(), weight };
    let g = admission::acquire_real(&q, &mut |l: &str| say(l));
    // batch-job: the admitted command is whatever the caller asked to run under admission
    let status = Command::new(prog).args(&cmd[1..]).env(admission::INHERIT_ENV, &g.token).status();
    drop(g);
    match status {
        Ok(s) => ExitCode::from(s.code().unwrap_or(128).clamp(0, 255) as u8),
        Err(e) => {
            say(&format!("cannot run {}: {e}", prog.to_string_lossy()));
            ExitCode::from(127)
        }
    }
}
