// intent-report — the IO seam: reads the run dir's rows and prints the report (DESIGN.md).
//
// usage: intent-report [--run <dir>] [--since <dur|ISO>] [--until <dur|ISO>] [--no-backfill] [--round-vm-timing <file>]

use intent_report::{parse_when, render, Inputs, Window};
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

const USAGE: &str =
    "usage: intent-report [--run <dir>] [--since <dur|ISO>] [--until <dur|ISO>] [--no-backfill] [--round-vm-timing <file>]";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("intent-report: {e}");
            ExitCode::from(2)
        }
    }
}

fn run(args: Vec<String>) -> Result<String, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut run: Option<PathBuf> = std::env::var_os("SPIRA_RUN").map(PathBuf::from);
    let mut since = "24h".to_string();
    let mut until: Option<String> = None;
    let mut backfill = true;
    let mut vm_file: Option<PathBuf> = None;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().ok_or_else(|| USAGE.to_string());
        match a.as_str() {
            "--run" => run = Some(PathBuf::from(val()?)),
            "--since" => since = val()?,
            "--until" => until = Some(val()?),
            "--round-vm-timing" => vm_file = Some(PathBuf::from(val()?)),
            "--no-backfill" => backfill = false,
            "-h" | "--help" => return Ok(format!("{USAGE}\n")),
            other => return Err(format!("unknown argument {other:?}\n{USAGE}")),
        }
    }
    let run = run.ok_or("no --run and SPIRA_RUN is unset")?;
    if !run.is_dir() {
        return Err(format!("{} is not a directory", run.display()));
    }
    let from = parse_when(&since, now)
        .ok_or_else(|| format!("--since {since:?}: not a duration or ISO time"))?;
    let to = match &until {
        Some(u) => parse_when(u, now)
            .ok_or_else(|| format!("--until {u:?}: not a duration or ISO time"))?,
        None => now + 1,
    };
    let read = |rel: &str| fs::read_to_string(run.join(rel)).unwrap_or_default();
    let gate_log = backfill.then(|| read("gate.log"));
    let vm = match &vm_file {
        Some(f) => fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?,
        None => String::new(),
    };
    let (gate_run, ra, st, le) = (
        read("tsd/gate-run.jsonl"),
        read("tsd/round-attribution.jsonl"),
        read("tsd/suite-timing.jsonl"),
        read("tsd/landing-event.jsonl"),
    );
    Ok(render(
        &Inputs {
            gate_run: &gate_run,
            gate_log: gate_log.as_deref(),
            round_attribution: &ra,
            suite_timing: &st,
            landing_event: &le,
            round_vm_timing: &vm,
        },
        Window { from, to },
    ))
}
