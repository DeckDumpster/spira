//! `aeon capacity <verb> ...` — the plumbing lib.sh's K-family functions become one-line
//! shims over (wave 4.26). Every verb below mirrors a retired bash function's own rc/
//! stdout contract exactly, so `spira/capacity.sh` (the one remaining bash caller) and
//! the fixtures that stub `$SPIRA_AGENT` keep working unchanged against the shim.
//!
//! Streams: the plain numeric answer (or nothing) goes on stdout; any `log`-style chatter
//! goes on stderr — `capacity_paused() { SPIRA_CAPACITY_LEFT="$(aeon capacity paused)"; }`
//! then captures only the number, the same separation the old `_aeon_capacity_paused`
//! seam wrapper already relied on (`capacity_paused >&2; printf '%s' "$SPIRA_CAPACITY_LEFT"`).

use crate::capacity::{self, Paused};
use crate::conf::Conf;
use crate::ports::Exec;
use crate::util;

pub fn run(conf: &Conf, exec: &dyn Exec, now: i64, args: &[String]) -> i32 {
    let verb = args.first().map(String::as_str).unwrap_or("");
    match verb {
        "paused" => {
            let v = capacity::check_and_probe(conf, exec, now);
            for l in &v.log {
                eprintln!("{}", util::log_line(now, l));
            }
            if v.clear_file {
                let _ = std::fs::remove_file(conf.capacity_pause());
            }
            match v.state {
                Paused::Open => 1,
                Paused::Paused(n) => {
                    print!("{n}");
                    0
                }
                Paused::Unknown => {
                    print!("?");
                    0
                }
            }
        }
        "pause-set" => {
            let (Some(at), Some(why)) = (args.get(1), args.get(2)) else {
                eprintln!("usage: aeon capacity pause-set <epoch> <reason>");
                return 2;
            };
            let at: i64 = at.trim().parse().unwrap_or(0);
            if let Some(line) = capacity::pause_set(&conf.capacity_pause(), &conf.ledger(), now, conf.capacity_backoff(), at, why) {
                println!("{}", util::log_line(now, &line));
            }
            0
        }
        "pause-until" => {
            let at = match capacity::pause_state(&conf.capacity_pause()) {
                crate::capacity::PauseState::Paused { until, .. } => until,
                _ => 0,
            };
            print!("{at}");
            0
        }
        "pause-why" => match capacity::pause_why(&conf.capacity_pause()) {
            Some(w) => {
                print!("{w}");
                0
            }
            None => 1,
        },
        "reset-at" => {
            let Some(logf) = args.get(1) else {
                eprintln!("usage: aeon capacity reset-at <logfile>");
                return 2;
            };
            match capacity::reset_at(std::path::Path::new(logf), &conf.trace_mark()) {
                Some(at) => {
                    print!("{at}");
                    0
                }
                None => 1,
            }
        }
        "log-fingerprint" => {
            let Some(f) = args.get(1) else {
                eprintln!("usage: aeon capacity log-fingerprint <file>");
                return 2;
            };
            match capacity::log_fingerprint(std::path::Path::new(f)) {
                Some(fp) => {
                    print!("{fp}");
                    0
                }
                None => 1,
            }
        }
        "withdrawn-fp" => {
            let Some(id) = args.get(1) else {
                eprintln!("usage: aeon capacity withdrawn-fp <id>");
                return 2;
            };
            match capacity::withdrawn_fp(&conf.capacity_withdrawn(), id) {
                Some(fp) => {
                    print!("{fp}");
                    0
                }
                None => 1,
            }
        }
        "withdrawn-mark" => {
            let (Some(id), Some(fp), attempt) = (args.get(1), args.get(2), args.get(3)) else {
                eprintln!("usage: aeon capacity withdrawn-mark <id> <fp> <attempt>");
                return 2;
            };
            let attempt: i64 = attempt.and_then(|a| a.trim().parse().ok()).unwrap_or(0);
            match capacity::withdrawn_mark(&conf.capacity_withdrawn(), id, fp, attempt, now) {
                Ok(()) => 0,
                Err(_) => 1,
            }
        }
        other => {
            eprintln!("aeon capacity: unknown verb '{other}'");
            2
        }
    }
}
