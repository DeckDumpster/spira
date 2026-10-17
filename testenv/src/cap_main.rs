//! testenv-cap {scan [--dry-run] [--grace SECS --exe PATH] | watch --interval SECS | health} — the backstop behind
//! testenv's own wall-clock cap (testenv::cap). A live testenv run past its cap plus the
//! grace is killed by pid, its cmdline re-verified first, and reported once on the concierge
//! inbox with its tier and wall.

use std::io::Write;
use std::process::ExitCode;
use std::time::Duration;
use testenv::cap::{self, Breach};

const USAGE: &str = "usage: testenv-cap scan [--dry-run] | watch --interval SECS | health";

fn caps() -> Result<(u64, u64), String> {
    Ok((
        spira_config::process::cfg_parse("SPIRA_TESTENV_SUITE_CAP")?,
        spira_config::process::cfg_parse("SPIRA_TESTENV_GATE_CAP")?,
    ))
}

fn report(inbox: &str, b: &Breach) {
    let line = format!(
        "testenv over-cap killed: pid {} tier={} wall={}s cap={}s cmd={}",
        b.proc.pid,
        b.tier,
        b.proc.age_secs,
        b.cap,
        b.proc.argv.join(" ")
    );
    println!("{line}");
    let stamp = testenv::util::iso_utc(testenv::batch::now_epoch());
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(inbox)
        .and_then(|mut f| writeln!(f, "{stamp} {line}"));
    if let Err(e) = written {
        eprintln!("testenv-cap: cannot write {inbox}: {e}");
    }
}

fn scan(dry: bool, grace: u64, exe: Option<&str>) -> Result<usize, String> {
    let (suite, gate) = caps()?;
    let inbox = spira_config::process::cfg("SPIRA_CONCIERGE_INBOX")?;
    let mut killed = 0;
    for b in cap::breaches(&cap::live(), suite, gate, grace, exe) {
        if dry {
            println!("would kill pid {} tier={} wall={}s cap={}s", b.proc.pid, b.tier, b.proc.age_secs, b.cap);
        } else if cap::kill_verified(&b.proc) {
            report(&inbox, &b);
            testenv::telemetry::push_over_cap(&testenv::telemetry::sink(), b.tier);
            killed += 1;
        }
    }
    Ok(killed)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["health"] => match caps() {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("testenv-cap: {e}");
                ExitCode::from(1)
            }
        },
        ["scan", rest @ ..] => {
            let (dry, grace, exe) = match rest {
                [] => (false, cap::GRACE, None),
                ["--dry-run"] => (true, cap::GRACE, None),
                ["--grace", g, "--exe", e] if g.parse::<u64>().is_ok() => (false, g.parse().unwrap(), Some(*e)),
                _ => {
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                }
            };
            match scan(dry, grace, exe) {
                Ok(_) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("testenv-cap: {e}");
                    ExitCode::from(1)
                }
            }
        }
        ["watch", "--interval", n] => {
            let Some(secs) = n.parse::<u64>().ok().filter(|s| *s > 0) else {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            };
            loop {
                if let Err(e) = scan(false, cap::GRACE, None) {
                    eprintln!("testenv-cap: {e}");
                    return ExitCode::from(1);
                }
                std::thread::sleep(Duration::from_secs(secs));
            }
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
