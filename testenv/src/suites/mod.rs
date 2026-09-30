//! `testenv suites` — the suite population, the gate/timed partition, the watchtower's
//! status block, flake reports, quarantine hygiene and the spira/suite-state transitions.
//! Replaces spira/suites.sh. Contract: DESIGN-suites.md.

pub mod cmd;
pub mod model;
pub mod ports;
pub mod real;

use cmd::Transition;
use ports::World;

pub const USAGE: &str =
    "usage: testenv suites [list|names|corpus|status|hygiene|lint|observe-flake|quarantine|disable|activate]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    Arg(String),
    /// `--reason-file <path>` (`-` = stdin).
    File(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    List,
    Names,
    Corpus,
    Status,
    Hygiene,
    Lint,
    ObserveFlake { suite: String, run_id: String },
    Quarantine { suite: String, bead: String, reason: Option<Reason>, base: Option<String> },
    Disable { suite: String, reason: Option<Reason>, base: Option<String> },
    Activate { suite: String, base: Option<String> },
}

/// A usage error: the line for stderr; exit 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage(pub String);

/// (positionals, `--reason-file`, `--base`).
type TransitionArgs = (Vec<String>, Option<String>, Option<String>);

/// Positionals plus `--reason-file` / `--base` (`--k v` or `--k=v`).
fn transition_args(cmd: &str, args: &[String]) -> Result<TransitionArgs, Usage> {
    let (mut pos, mut rfile, mut base) = (Vec::new(), None, None);
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let (k, inline) = match a.split_once('=') {
            Some((k, v)) if k.starts_with("--") => (k, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        match k {
            "--reason-file" | "--base" => {
                let v = match inline {
                    Some(v) => v,
                    None => {
                        i += 1;
                        args.get(i)
                            .cloned()
                            .ok_or_else(|| Usage(format!("suites {cmd}: {k} requires an argument")))?
                    }
                };
                if k == "--base" {
                    base = Some(v);
                } else {
                    rfile = Some(v);
                }
            }
            "--" => {
                pos.extend(args[i + 1..].iter().cloned());
                break;
            }
            _ if k.starts_with("--") => return Err(Usage(format!("suites {cmd}: unknown option: {a}"))),
            _ => pos.push(a.clone()),
        }
        i += 1;
    }
    Ok((pos, rfile, base))
}

pub fn parse(args: &[String]) -> Result<Cmd, Usage> {
    let Some(sub) = args.first() else { return Ok(Cmd::List) };
    let rest = &args[1..];
    let pos = |i: usize| rest.get(i).cloned().unwrap_or_default();
    let reason = |pos: &[String], at: usize, file: Option<String>| -> Option<Reason> {
        match file {
            Some(f) => Some(Reason::File(f)),
            None => pos.get(at).filter(|r| !r.is_empty()).cloned().map(Reason::Arg),
        }
    };
    Ok(match sub.as_str() {
        // suites.sh ignored anything after these
        "list" => Cmd::List,
        "names" => Cmd::Names,
        "corpus" => Cmd::Corpus,
        "status" => Cmd::Status,
        "hygiene" => Cmd::Hygiene,
        "lint" => Cmd::Lint,
        "observe-flake" => Cmd::ObserveFlake { suite: pos(0), run_id: pos(1) },
        "quarantine" => {
            let (p, f, base) = transition_args("quarantine", rest)?;
            Cmd::Quarantine {
                suite: p.first().cloned().unwrap_or_default(),
                bead: p.get(1).cloned().unwrap_or_default(),
                reason: reason(&p, 2, f),
                base,
            }
        }
        "disable" => {
            let (p, f, base) = transition_args("disable", rest)?;
            Cmd::Disable { suite: p.first().cloned().unwrap_or_default(), reason: reason(&p, 1, f), base }
        }
        "activate" => {
            let (p, _, base) = transition_args("activate", rest)?;
            Cmd::Activate { suite: p.first().cloned().unwrap_or_default(), base }
        }
        _ => return Err(Usage(USAGE.into())),
    })
}

fn reason_text(w: &World, r: &Option<Reason>) -> Result<String, String> {
    match r {
        None => Ok(String::new()),
        Some(Reason::Arg(s)) => Ok(s.clone()),
        Some(Reason::File(f)) => (w.read_input)(f).map(|t| t.trim_end_matches(['\n', '\r']).to_string()),
    }
}

/// Run one parsed command; the exit code.
pub fn dispatch(w: &World, c: &Cmd) -> i32 {
    let with_reason = |label: &str, r: &Option<Reason>, f: &dyn Fn(String) -> i32| match reason_text(w, r) {
        Ok(t) => f(t),
        Err(e) => {
            w.err(format!("suites {label}: cannot read the reason: {e}"));
            cmd::USAGE
        }
    };
    match c {
        Cmd::List => cmd::list(w),
        Cmd::Names => cmd::names(w),
        Cmd::Corpus => cmd::corpus(w),
        Cmd::Status => cmd::status(w),
        Cmd::Hygiene => cmd::hygiene(w),
        Cmd::Lint => cmd::lint(w),
        Cmd::ObserveFlake { suite, run_id } => cmd::observe_flake(w, suite, run_id),
        Cmd::Quarantine { suite, bead, reason, base } => with_reason("quarantined", reason, &|r| {
            let t = Transition::Quarantine { bead: bead.clone(), reason: r };
            cmd::run_transition(w, &t, suite, base.as_deref())
        }),
        Cmd::Disable { suite, reason, base } => with_reason("disabled", reason, &|r| {
            cmd::run_transition(w, &Transition::Disable { reason: r }, suite, base.as_deref())
        }),
        Cmd::Activate { suite, base } => cmd::run_transition(w, &Transition::Activate, suite, base.as_deref()),
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
    let config = spira_config::discover(None).and_then(|p| spira_config::load(&p).ok());
    let src = crate::settings::Source { env: &env, config: config.as_ref() };
    let s = ports::Settings::load(&src, &harness.root);
    let r = real::Real::new(&s, &env);
    let read_input = |p: &str| real::read_input(p);
    let w = World {
        s: &s,
        clock: &r,
        lib: &r,
        intake: &r,
        mail: &r,
        host: &r,
        queue: &r,
        git: &r,
        io: &r,
        read_input: &read_input,
    };
    dispatch(&w, &c)
}

#[cfg(test)]
mod tests;
