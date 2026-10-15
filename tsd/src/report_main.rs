// cert-report — certification throughput per repo and instance type, from run/tsd/cert-pass.jsonl.
//
// usage: cert-report [--root <dir>] [--day YYYY-MM-DD | --since <iso> --until <iso>] [--line] [--record]
//                    [--prices type=usd_per_hour,...]
//
//   --root    defaults to $SPIRA_RUN.        --prices defaults to $SPIRA_CERT_PRICES.
//   (window)  defaults to the 24 hours ending now.
//   --line    prints the one status line the Concierge's briefing reads.
//   --record  also appends one cert-throughput row per cell.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};
use tsd::report;

fn main() -> ExitCode {
    match run(&env::args().skip(1).collect::<Vec<_>>()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cert-report: {e}");
            ExitCode::from(2)
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let (mut root, mut day, mut since, mut until, mut prices) = (None, None, None, None, None);
    let (mut line, mut record) = (false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--root" => root = Some(PathBuf::from(val("--root")?)),
            "--day" => day = Some(val("--day")?),
            "--since" => since = Some(val("--since")?),
            "--until" => until = Some(val("--until")?),
            "--prices" => prices = Some(val("--prices")?),
            "--line" => line = true,
            "--record" => record = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let root = root
        .or_else(|| env::var("SPIRA_RUN").ok().filter(|v| !v.is_empty()).map(PathBuf::from))
        .ok_or("no --root and SPIRA_RUN is unset")?;
    let prices = report::parse_prices(&prices.or_else(|| env::var("SPIRA_CERT_PRICES").ok()).unwrap_or_default())?;
    let ts = |s: &str| tsd::parse_ts(s).ok_or_else(|| format!("{s:?} is not YYYY-MM-DD or YYYY-MM-DDTHH:MM:SSZ"));
    let (from, to) = match (day, since, until) {
        (Some(d), None, None) => {
            let from = ts(&d)?;
            (from, from + 86_400)
        }
        (None, None, None) => {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e| e.to_string())?.as_secs();
            (now - 86_400, now)
        }
        (None, Some(s), Some(u)) => (ts(&s)?, ts(&u)?),
        _ => return Err("give --day, or --since with --until, or neither".into()),
    };
    if to <= from {
        return Err("the window is empty".into());
    }
    let path = tsd::family_path(&root, tsd::pass::FAMILY);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let (passes, unreadable) = report::parse_passes(&text);
    let r = report::compute(&passes, from, to);
    if line {
        println!("{}", r.line(&prices));
    } else {
        print!("{}", r.text(&prices));
    }
    if unreadable > 0 {
        eprintln!("cert-report: {unreadable} cert-pass rows could not be read as passes and were left out");
    }
    if record {
        let host = tsd::default_host();
        for fields in r.rows(&prices) {
            let row = tsd::build_row(&tsd::now_iso(), &host, report::SUMMARY_FAMILY, &fields)?;
            tsd::append_line(&tsd::family_path(&root, report::SUMMARY_FAMILY), &row)?;
        }
    }
    Ok(())
}
