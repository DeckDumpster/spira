//! mender — what a mender dwarf is given and how it answers (see the library). Run through
//! `work mender <verb> <spec> ...`; `<spec>` is the failure spec the calls carries.

use std::io::Read;
use std::process::Command;

use mender::verbs::{self, World};

const USAGE: &str = "mender start <spec> | pack <spec> | diff <spec> <bead> | bead <id> | members <spec> | failures <spec> | passes <spec> | verdicts <spec> | landing <spec> | triaged <spec> | diagnose <spec> (text on stdin) | outcome <spec> <mended|environment|flaky|return|unsure> [--checks <text>] [--ask <id>] (diagnosis on stdin)";

struct Real;

impl World for Real {
    fn lc(&self, args: &[&str]) -> (i32, String) {
        let mut cmd = Command::new(spira_config::lc_call::lc_bin());
        cmd.args(args);
        spira_config::lc_call::run_bounded(cmd, spira_config::lc_call::LC_TIMEOUT)
    }

    fn git(&self, repo: &str, args: &[&str]) -> Result<String, String> {
        let out = spira_config::bounded::bounded("git").arg("-C").arg(repo).args(args).output().map_err(|e| format!("git: {e}"))?;
        match out.status.success() {
            true => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
            false => Err(format!("git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr).trim())),
        }
    }

    fn read(&self, path: &str) -> Result<String, String> {
        std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
    }

    fn deadline_secs(&self) -> Result<u64, String> {
        spira_config::process::cfg_parse("SPIRA_MENDER_DEADLINE_SECS")
    }

    fn wiki(&self) -> Result<String, String> {
        spira_config::process::cfg("SPIRA_WIKI").and_then(|w| if w.is_empty() { Err("SPIRA_WIKI is empty".into()) } else { Ok(w) })
    }
}

fn stdin() -> String {
    let mut s = String::new();
    let _ = std::io::stdin().read_to_string(&mut s);
    s
}

fn outcome_flags<'a>(flags: &[&'a str]) -> Result<(Option<&'a str>, Option<&'a str>), String> {
    let (mut checks, mut ask) = (None, None);
    let mut it = flags.iter();
    while let Some(flag) = it.next() {
        let slot = match *flag {
            "--checks" => &mut checks,
            "--ask" => &mut ask,
            other => return Err(format!("outcome: unknown flag {other:?}; usage: {USAGE}")),
        };
        *slot = Some(*it.next().ok_or_else(|| format!("outcome: {flag} needs a value"))?);
    }
    Ok((checks, ask))
}

fn run(args: &[String]) -> Result<String, String> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let w = Real;
    match a.as_slice() {
        ["start", spec] => verbs::start(&w, spec),
        ["pack", spec] => verbs::pack(&w, spec),
        ["diff", spec, bead] => verbs::diff(&w, spec, bead),
        ["bead", id] => verbs::bead(&w, id),
        ["members", spec] => verbs::round_members(&w, spec),
        ["failures", spec] => verbs::failures(&w, spec),
        ["passes", spec] => verbs::passes(&w, spec),
        ["verdicts", spec] => verbs::verdicts(&w, spec),
        ["landing", spec] => verbs::landing(&w, spec),
        ["triaged", spec] => verbs::triaged(&w, spec),
        ["diagnose", spec] => verbs::diagnose(&w, spec, &stdin()),
        ["outcome", spec, kind, flags @ ..] => {
            let (checks, ask) = outcome_flags(flags)?;
            verbs::finish(&w, spec, kind, &stdin(), checks, ask)
        }
        _ => Err(format!("usage: {USAGE}")),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = mender::telemetry::run(&mender::telemetry::sink(), args.first().map(String::as_str), || match run(&args) {
        Ok(out) => {
            println!("{out}");
            0
        }
        Err(e) => {
            eprintln!("mender: {e}");
            2
        }
    });
    if code != 0 {
        std::process::exit(code);
    }
}
