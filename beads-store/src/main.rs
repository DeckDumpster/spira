//! `beads-store commit --db <path> --message <text>` — DESIGN.md §3.

use std::path::PathBuf;

fn usage() -> ! {
    eprintln!(
        "usage: beads-store commit --db <path> --message <text> | push --db <path> --remote <name>"
    );
    std::process::exit(1);
}

fn parse_flags(args: &[String]) -> Result<(PathBuf, String), ()> {
    let mut db: Option<PathBuf> = None;
    let mut message: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--db" => {
                i += 1;
                db = Some(PathBuf::from(args.get(i).ok_or(())?));
            }
            "--message" => {
                i += 1;
                message = Some(args.get(i).ok_or(())?.clone());
            }
            _ => return Err(()),
        }
        i += 1;
    }
    Ok((db.ok_or(())?, message.ok_or(())?))
}

fn cmd_commit(args: &[String]) -> i32 {
    let (db, message) = match parse_flags(args) {
        Ok(v) => v,
        Err(()) => usage(),
    };
    let spira_dolt_data = match spira_config::process::cfg("SPIRA_DOLT_DATA") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let dolt_bin = std::env::var("BEADS_STORE_DOLT_BIN").unwrap_or_else(|_| "dolt".to_string());

    let engine = match beads_store::resolve(&db, Some(&spira_dolt_data)) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    match beads_store::run_commit(&engine, &dolt_bin, &message) {
        Ok(committed) => {
            println!("{committed}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    }
}

fn cmd_push(args: &[String]) -> i32 {
    let (mut db, mut remote) = (None, None);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--db" => {
                i += 1;
                db = args.get(i).map(PathBuf::from);
            }
            "--remote" => {
                i += 1;
                remote = args.get(i).cloned();
            }
            _ => usage(),
        }
        i += 1;
    }
    let (Some(db), Some(remote)) = (db, remote) else {
        usage()
    };
    let spira_dolt_data = match spira_config::process::cfg("SPIRA_DOLT_DATA") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let dolt_bin = std::env::var("BEADS_STORE_DOLT_BIN").unwrap_or_else(|_| "dolt".to_string());
    let deadline = std::env::var("BEADS_STORE_PUSH_DEADLINE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600);
    let engine = match beads_store::resolve(&db, Some(&spira_dolt_data)) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    match beads_store::run_push(&engine, &dolt_bin, &remote, deadline) {
        Ok(head) => {
            println!("{head}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(String::as_str);
    let run: fn(&[String]) -> i32 = match sub {
        Some("commit") => cmd_commit,
        Some("push") => cmd_push,
        _ => usage(),
    };
    let sink = spira_otel::metrics::MetricsSink::for_component(spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref(), "spira-beads-store");
    let code = spira_otel::metrics::instrument(&sink, "beads-store", sub.unwrap_or("none"), || run(&args[2..]));
    std::process::exit(code);
}
