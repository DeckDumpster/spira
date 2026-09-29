//! strand — the stranded-work detector (DESIGN.md). Replaces spira/strand.sh and
//! spira/strand-classify.py with the same subcommands and output contract.
//!
//!   strand report [--json]      the human view; changes nothing
//!   strand check                the timer path: act where the fix is mechanical, escalate once
//!                               where it is not
//!   strand check --dry-run      classify and print, change nothing
//!   strand check --from <f>     classify a saved TSV instead of the live graph
//!   strand throttle-state       print "open|shut|unreadable<TAB>detail"

mod check;
mod classify;
mod config;
mod model;
mod probe;
mod state;
mod timefmt;

use std::io::Read;

const HELP: &str = "strand report [--json]   the human view; changes nothing   (what `gt convoy stranded` was)
strand check             the timer path: act where the fix is mechanical, escalate once
                         where it is not
strand check --dry-run   classify and print, change nothing
strand check --from <f>  classify a saved TSV instead of the live graph
strand throttle-state    print \"open|shut|unreadable<TAB>detail\" — what classify_one
                         reads before calling withheld aeons a strand
";

#[derive(Debug, PartialEq, Eq)]
enum Mode {
    Report,
    Check,
    ThrottleState,
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    mode: Mode,
    json: bool,
    dry: bool,
    from: Option<String>,
    help: bool,
}

/// As strand.sh: a first argument that is not a subcommand means `report`, and is then
/// read as a flag.
fn parse(argv: &[String]) -> Result<Args, String> {
    let mut a = Args { mode: Mode::Report, json: false, dry: false, from: None, help: false };
    let mut rest = argv;
    match argv.first().map(String::as_str) {
        Some("report") => rest = &argv[1..],
        Some("check") => {
            a.mode = Mode::Check;
            rest = &argv[1..];
        }
        Some("throttle-state") => {
            a.mode = Mode::ThrottleState;
            rest = &argv[1..];
        }
        _ => {}
    }
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--json" => a.json = true,
            "--dry-run" => a.dry = true,
            "--from" => {
                i += 1;
                a.from = Some(rest.get(i).cloned().ok_or("--from needs a file, or - for stdin")?);
            }
            "-h" | "--help" => a.help = true,
            other => return Err(format!("unknown argument '{other}' — try: report [--json] | check [--dry-run]")),
        }
        i += 1;
    }
    Ok(a)
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse(&argv) {
        Ok(a) => a,
        Err(e) => {
            check::warn(&format!("FATAL {e}"));
            std::process::exit(1);
        }
    };
    if args.help {
        print!("{HELP}");
        return;
    }
    let cfg = config::Config::resolve(&config::Live::load());
    if args.mode == Mode::ThrottleState {
        println!("{}", probe::throttle_line(&probe::throttle(&cfg)));
        return;
    }
    let classified = match &args.from {
        Some(f) => {
            let mut text = String::new();
            let read = if f == "-" {
                std::io::stdin().read_to_string(&mut text).map(|_| ())
            } else {
                std::fs::read_to_string(f).map(|t| text = t)
            };
            if let Err(e) = read {
                check::warn(&format!("FATAL cannot read {f}: {e}"));
                std::process::exit(1);
            }
            check::from_tsv(&cfg, &text)
        }
        None => match check::classify_live(&cfg) {
            Ok(c) => c,
            Err(e) => {
                // R7: an unreadable input is not an empty graph — say so, write nothing.
                check::warn(&format!("strand: cannot classify: {e}"));
                std::process::exit(2);
            }
        },
    };
    let rc = match args.mode {
        Mode::Report => {
            check::report(&cfg, &classified, args.json);
            0
        }
        Mode::Check => check::check(&cfg, classified, args.dry),
        Mode::ThrottleState => 0,
    };
    std::process::exit(rc);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(v: &[&str]) -> Result<Args, String> {
        parse(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn modes_and_flags() {
        assert_eq!(p(&[]).unwrap().mode, Mode::Report);
        let a = p(&["--json"]).unwrap();
        assert!(a.json && a.mode == Mode::Report, "a bare flag means report");
        let a = p(&["check", "--dry-run", "--from", "-"]).unwrap();
        assert_eq!((a.mode, a.dry, a.from.as_deref()), (Mode::Check, true, Some("-")));
        assert_eq!(p(&["throttle-state"]).unwrap().mode, Mode::ThrottleState);
        assert!(p(&["check", "--from"]).is_err());
        assert_eq!(
            p(&["check", "--bogus"]).unwrap_err(),
            "unknown argument '--bogus' — try: report [--json] | check [--dry-run]"
        );
        assert!(p(&["bogus"]).is_err(), "an unknown first word is an unknown flag of report");
    }
}
