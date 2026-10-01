//! archivist — rescue a full session's unfinished business before it is cleared.
//! Replaces spira/archivist.sh (DESIGN.md).
//!
//!   archivist sweep            every live session; archive the ones that have drifted
//!   archivist list             what the sweep can see, and what it would do about each
//!   archivist now [<session>]  archive one session now, whatever its context
//!   archivist mark <s> <state> [<n>]   the running archivist's own progress writes
//!   archivist state [<s>]      print what the status line and the dashboard are reading
//!   archivist digest <t> [<from-turn>] [--full]
//!                              the transcript rendered small enough for an agent to read
//!   archivist record <line>    register one durably-filed finding for today's digest
//!   archivist digest-send      mail today's digest (at most once a day) and empty the queue

use std::path::{Path, PathBuf};

use archivist::config;
use archivist::digest;
use archivist::run;
use archivist::seam::{locate_home, RealSeam, Seam};

fn usage() -> ! {
    eprintln!("usage: archivist sweep|list|now|mark|state|digest|record|digest-send ...");
    std::process::exit(2);
}

fn die(msg: &str) -> ! {
    eprintln!("archivist: {msg}");
    std::process::exit(1);
}

fn real_seam() -> (RealSeam, PathBuf) {
    let exe = std::env::current_exe().unwrap_or_else(|_| "archivist".into());
    let home = locate_home(std::env::var("SPIRA_HOME").ok().as_deref(), &exe).unwrap_or_else(|| die("cannot find lib.sh (set SPIRA_HOME)"));
    let lib_sh = home.join("lib.sh");
    (RealSeam { lib_sh }, home)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `case "${1:-sweep}"` — no argument at all defaults to a sweep.
    let (cmd, rest): (String, Vec<String>) = match args.split_first() {
        Some((c, r)) => (c.clone(), r.to_vec()),
        None => ("sweep".to_string(), Vec::new()),
    };
    dispatch(&cmd, &rest);
}

fn dispatch(cmd: &str, rest: &[String]) {
    match cmd {
        "sweep" => {
            let (seam, home) = real_seam();
            let probed = seam.probe();
            let cfg = config::resolve(&probed);
            let arc = Path::new(&cfg.run).join("archivist");
            let _ = home;
            run::sweep(&seam, &cfg, &arc);
        }
        "list" => {
            let (seam, _home) = real_seam();
            let probed = seam.probe();
            let cfg = config::resolve(&probed);
            let arc = Path::new(&cfg.run).join("archivist");
            print!("{}", run::list(&seam, &cfg, &arc));
        }
        "now" => {
            let (seam, _home) = real_seam();
            let probed = seam.probe();
            let cfg = config::resolve(&probed);
            let arc = Path::new(&cfg.run).join("archivist");
            let rc = run::now_cmd(&seam, &cfg, &arc, rest.first().map(String::as_str));
            std::process::exit(rc);
        }
        "mark" => {
            let sid = rest.first().unwrap_or_else(|| die("mark needs a session"));
            let st = rest.get(1).unwrap_or_else(|| die("mark needs a state"));
            let arc = run_arc_dir();
            if let Err(e) = run::mark(&arc, sid, st, rest.get(2).map(String::as_str)) {
                die(&e);
            }
        }
        "state" => {
            let arc = run_arc_dir();
            print!("{}", run::state_cmd(&arc, rest.first().map(String::as_str)));
        }
        "digest" => {
            let tp = rest.first().unwrap_or_else(|| die("digest needs a transcript"));
            let from: u64 = rest.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            let full = rest.iter().any(|a| a == "--full");
            let text = std::fs::read_to_string(tp).unwrap_or_else(|e| die(&format!("digest: cannot read {tp}: {e}")));
            print!("{}", digest::render(&text, from, full));
        }
        "record" => {
            let line = rest.first().unwrap_or_else(|| die("record needs a line: <where the finding now lives>: <what it is>"));
            let arc = run_arc_dir();
            if let Err(e) = run::digest_record(&arc, line) {
                die(&e.to_string());
            }
        }
        "digest-send" => {
            let (seam, _home) = real_seam();
            let tz = seam.conf("SPIRA_TZ");
            let tz = if tz.is_empty() { "UTC".to_string() } else { tz };
            let arc = run_arc_dir_via(&seam);
            if run::digest_send(&seam, &arc, &tz).is_err() {
                std::process::exit(1);
            }
        }
        _ => usage(),
    }
}

/// `mark`/`state`/`record` only need `$SPIRA_RUN/archivist` — cheaper than the full
/// probe `sweep`/`list`/`now` need, but SPIRA_RUN itself may not be exported (conf.sh
/// sets it without `export`), so this still goes through the lib.sh seam for that one
/// key rather than trusting the process environment.
fn run_arc_dir() -> PathBuf {
    let (seam, _home) = real_seam();
    run_arc_dir_via(&seam)
}

fn run_arc_dir_via(seam: &dyn Seam) -> PathBuf {
    let run_dir = seam.conf("SPIRA_RUN");
    let run_dir = if run_dir.is_empty() { "/tmp".to_string() } else { run_dir };
    Path::new(&run_dir).join("archivist")
}
