//! filings-collect pass --state FILE
//!
//! Hosts come from SPIRA_FILINGS_HOSTS. Exit 0 swept, 1 a host or filing failed (named on stderr,
//! retried next pass; nothing unacknowledged is lost), 2 the configuration is unusable.

use filings_collect::{pass, parse_hosts, real::Real, State};
use std::process::ExitCode;

const USAGE: &str = "usage: filings-collect pass --state FILE";

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let state_path = match argv.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["pass", "--state", f] => std::path::PathBuf::from(f),
        _ => {
            eprintln!("filings-collect: {USAGE}");
            return ExitCode::from(2);
        }
    };
    let hosts = match spira_config::process::cfg("SPIRA_FILINGS_HOSTS").and_then(|s| parse_hosts(&s)) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("filings-collect: {e}");
            return ExitCode::from(2);
        }
    };
    if hosts.is_empty() {
        println!("filings-collect: unconfigured — SPIRA_FILINGS_HOSTS names no host");
        return ExitCode::SUCCESS;
    }
    let mut state = State::parse(&std::fs::read_to_string(&state_path).unwrap_or_default());
    let mut save = |s: &State| {
        std::fs::write(&state_path, s.render()).map_err(|e| format!("cannot record state in {}: {e}", state_path.display()))
    };
    let o = pass(&Real::default(), &hosts, &mut state, &mut save);
    println!(
        "filings-collect: {} filed, {} amended, {} acknowledged, {} failed",
        o.filed.len(),
        o.amended.len(),
        o.acked.len(),
        o.failed.len()
    );
    for f in &o.failed {
        eprintln!("filings-collect: ALARM {f}");
    }
    ExitCode::from(u8::from(!o.failed.is_empty()))
}
