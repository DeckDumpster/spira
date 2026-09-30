//! `sop <subcommand> ...` — see DESIGN.md. Ported from `spira/sop.sh` (deleted, sp-8fsql);
//! same subcommands, same environment variables, called by bare name on the release PATH.

use sop::logic::{self, AppliedArgs, Env};
use sop::real::{resolve_out_path, RealBd, RealClock, RealProc};
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "sop — write, match, recall and synthesise a Standard Operating Procedure.

  sop write <slug> [-|<file>]   write or amend sop-<slug>; text on stdin or from a file
  sop show <slug>               one SOP's full text
  sop list                      what is on the shelf
  sop match [-|<file>]          which SOPs match an incident payload
  sop validate <slug>           the write-time validator, body on stdin — no shelf, no bd
  sop applied <slug> --bead <id>|--pass <id> --check pass|fail --held yes|no|unknown [--why -|<file>]
                                 record that a runbook was consulted, and what came of it
  sop log [--bead <id>] [--pass <id>] [--sop <slug>] [--check pass|fail] [--since <epoch>]
                                 the applications ledger, oldest first
  sop digest                    one `<key> <hash>` line per SOP — what the shelf holds now
  sop ledger-init               create an empty applications ledger if there is none
  sop retire <slug>             remove it
  sop synth                     regenerate wiki/notes/standard-operating-procedures.md
  sop lint                      validate every sop- key against the write validator
";

fn slurp(src: Option<&str>) -> std::io::Result<String> {
    match src {
        None | Some("-") => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            Ok(s)
        }
        Some(path) => std::fs::read_to_string(path),
    }
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn build_env() -> Env {
    let word_cap: usize = env_or("SOP_WORD_CAP", "250").parse().unwrap_or(250);
    let why_cap: usize = env_or("SOP_WHY_CAP", "400").parse().unwrap_or(400);
    let actor = std::env::var("BEADS_ACTOR")
        .ok()
        .or_else(|| std::env::var("SPIRA_AEON").ok().map(|a| format!("aeon-{a}")))
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "unknown".to_string());
    let cockpit_override = std::env::var("SOP_METRIC_COCKPIT").ok().filter(|v| !v.is_empty());
    let cockpit_bash_prefix = cockpit_override.is_some();
    let cockpit_bin = cockpit_override.unwrap_or_else(|| "cockpit.sh".to_string());
    Env { word_cap, why_cap, actor, cockpit_bin, cockpit_bash_prefix }
}

fn ledger_path() -> PathBuf {
    if let Ok(p) = std::env::var("SPIRA_SOP_LEDGER") {
        return PathBuf::from(p);
    }
    let run = env_or("SPIRA_RUN", "/tmp/spira");
    PathBuf::from(run).join("sop").join("applied.jsonl")
}

fn print_report(r: &logic::Report) -> ExitCode {
    for l in &r.out {
        println!("{l}");
    }
    for l in &r.err {
        eprintln!("{l}");
    }
    ExitCode::from(r.code.clamp(0, 255) as u8)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let spira_home = env_or("SPIRA_HOME", ".");
    let bd = RealBd { spira_home };
    let proc = RealProc;
    let clock = RealClock;

    let Some(sub) = args.first().map(String::as_str) else {
        print!("{USAGE}");
        return ExitCode::from(1);
    };

    match sub {
        "write" => {
            let Some(key) = args.get(1) else {
                print!("{USAGE}");
                return ExitCode::from(1);
            };
            let text = match slurp(args.get(2).map(String::as_str)) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("sop: {e}");
                    return ExitCode::from(1);
                }
            };
            let env = build_env();
            let r = logic::write(&bd, &proc, &env, key, &text);
            let code = print_report(&r);
            if r.code == 0 {
                let out = resolve_out_path(std::env::var("SOP_PAGE").ok().as_deref(), std::env::var("SPIRA_WIKI").ok().as_deref());
                let _ = print_report(&logic::synth(&bd, &clock, out.as_deref()));
            }
            code
        }
        "show" => {
            let Some(key) = args.get(1) else {
                print!("{USAGE}");
                return ExitCode::from(1);
            };
            print_report(&logic::show(&bd, key))
        }
        "list" => print_report(&logic::list(&bd)),
        "match" => {
            let payload = match slurp(args.get(1).map(String::as_str)) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("sop: {e}");
                    return ExitCode::from(1);
                }
            };
            print_report(&logic::match_cmd(&bd, &payload))
        }
        "validate" => {
            let Some(key) = args.get(1) else {
                print!("{USAGE}");
                return ExitCode::from(1);
            };
            let mut text = String::new();
            let _ = std::io::stdin().read_to_string(&mut text);
            let env = build_env();
            print_report(&logic::validate_cmd(&proc, key, &text, env.word_cap))
        }
        "applied" => {
            let Some(key) = args.get(1) else {
                print!("{USAGE}");
                return ExitCode::from(1);
            };
            let mut bead = None;
            let mut pass = None;
            let mut check = "";
            let mut held = "";
            let mut why_src: Option<&str> = None;
            let mut have_why = false;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--bead" if i + 1 < args.len() => {
                        bead = Some(args[i + 1].as_str());
                        i += 2;
                    }
                    "--pass" if i + 1 < args.len() => {
                        pass = Some(args[i + 1].as_str());
                        i += 2;
                    }
                    "--check" if i + 1 < args.len() => {
                        check = args[i + 1].as_str();
                        i += 2;
                    }
                    "--held" if i + 1 < args.len() => {
                        held = args[i + 1].as_str();
                        i += 2;
                    }
                    "--why" if i + 1 < args.len() => {
                        why_src = Some(args[i + 1].as_str());
                        have_why = true;
                        i += 2;
                    }
                    other => {
                        eprintln!("sop: applied: unexpected argument: {other}");
                        return ExitCode::from(1);
                    }
                }
            }
            let why_text = if have_why {
                match slurp(why_src) {
                    Ok(t) => Some(t),
                    Err(e) => {
                        eprintln!("sop: {e}");
                        return ExitCode::from(1);
                    }
                }
            } else {
                None
            };
            let env = build_env();
            let lp = ledger_path();
            let a = AppliedArgs { key, bead, pass, check, held, why: why_text.as_deref() };
            print_report(&logic::applied(&bd, &proc, &clock, &env, &lp, &a))
        }
        "log" => {
            let mut f = sop::ledger::LogFilter { bead: None, pass: None, sop: None, check: None, since: None };
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--bead" if i + 1 < args.len() => {
                        f.bead = Some(args[i + 1].clone());
                        i += 2;
                    }
                    "--pass" if i + 1 < args.len() => {
                        f.pass = Some(args[i + 1].clone());
                        i += 2;
                    }
                    "--sop" if i + 1 < args.len() => {
                        f.sop = Some(sop::shelf::slugify(&args[i + 1]));
                        i += 2;
                    }
                    "--check" if i + 1 < args.len() => {
                        f.check = Some(args[i + 1].clone());
                        i += 2;
                    }
                    "--since" if i + 1 < args.len() => {
                        f.since = args[i + 1].parse().ok();
                        if f.since.is_none() {
                            eprintln!("sop: log: --since takes a unix epoch, not '{}'", args[i + 1]);
                            return ExitCode::from(1);
                        }
                        i += 2;
                    }
                    other => {
                        eprintln!("sop: log: unexpected argument: {other}");
                        return ExitCode::from(1);
                    }
                }
            }
            let lp = ledger_path();
            let text = std::fs::read_to_string(&lp).ok();
            print_report(&logic::log(text.as_deref(), &f))
        }
        "digest" => print_report(&logic::digest(&bd)),
        "ledger-init" => print_report(&logic::ledger_init(&ledger_path())),
        "retire" => {
            let Some(key) = args.get(1) else {
                print!("{USAGE}");
                return ExitCode::from(1);
            };
            let r = logic::retire(&bd, key);
            let code = print_report(&r);
            if r.code == 0 {
                let out = resolve_out_path(std::env::var("SOP_PAGE").ok().as_deref(), std::env::var("SPIRA_WIKI").ok().as_deref());
                let _ = print_report(&logic::synth(&bd, &clock, out.as_deref()));
            }
            code
        }
        "synth" => {
            let out = resolve_out_path(std::env::var("SOP_PAGE").ok().as_deref(), std::env::var("SPIRA_WIKI").ok().as_deref());
            print_report(&logic::synth(&bd, &clock, out.as_deref()))
        }
        "lint" => {
            let env = build_env();
            print_report(&logic::lint(&bd, env.word_cap))
        }
        _ => {
            print!("{USAGE}");
            ExitCode::from(1)
        }
    }
}
