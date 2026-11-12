//! cockpit-collect — Spira's own cockpit collector, replacing `spira/cockpit.sh` and
//! `spira/collect.sh` (sp-kt4l3). DESIGN.md has the full contract; this file is only CLI
//! dispatch, mirroring the two scripts' own subcommand names so every existing caller
//! (systemd, `sop.sh`'s METRIC seam, the `test-cockpit-*.sh` suites) needs only its
//! invocation repointed, never its assertions rewritten.

use cockpit_collect::{counts, io, metrics, probes, sparklines, tiers, timeutil};

fn usage() -> i32 {
    eprintln!(
        "usage: cockpit-collect [once|history|sweep-temps|probe <name>|check-config]\n  (no args: same as once, no attach — attaching the operator to the concierge stays root cockpit.sh's job)"
    );
    1
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    const OPS: [&str; 5] = ["once", "history", "sweep-temps", "probe", "check-config"];
    let op = match args.get(1).map(String::as_str) {
        None => "once",
        Some(a) if OPS.contains(&a) => a,
        Some(_) => "other",
    };
    let metered = spira_otel::metrics::Run::begin(spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref(), "cockpit-collect", op);
    let code = run(&args[1..]);
    metered.end(spira_otel::metrics::outcome_of(code));
    std::process::exit(code);
}

fn run(args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some("check-config") {
        return spira_config::process::check_config_verb("cockpit-collect", &[]);
    }
    match args.first().map(String::as_str) {
        Some("metrics") => return cmd_metrics(&args[1..]),
        Some("sparklines") => return cmd_sparklines(&args[1..]),
        _ => {}
    }
    // spira/cockpit.sh sourced lib.sh (which cascades into conf.sh) unconditionally at its
    // own top level before dispatching to any subcommand; see io::bootstrap_config for why
    // this binary must do the same instead of reading unresolved env vars.
    io::bootstrap_config();
    if let Err(e) = io::try_run_dir() {
        eprintln!("{e}");
        return 1;
    }
    match args.first().map(String::as_str) {
        None | Some("once") => cmd_once(),
        Some("history") => cmd_history(),
        Some("sweep-temps") => cmd_sweep_temps(),
        Some("probe") => cmd_probe(args.get(1).map(String::as_str)),
        Some(other) => {
            eprintln!("cockpit-collect: unknown subcommand '{other}'");
            usage()
        }
    }
}

/// `once`: the full serial pass, printed to stdout. It never writes `run/cockpit.env`; the
/// scheduler's cockpit tier tasks are its only writer.
fn cmd_once() -> i32 {
    let pcfg = match probes::Cfg::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cockpit-collect: {e}");
            return 1;
        }
    };
    print!("{}", probes::render(&probes::full_pass(&pcfg)));
    0
}

fn append_history(run_dir: &std::path::Path) {
    tiers::append_history(run_dir);
}

fn cmd_history() -> i32 {
    let run_dir = io::run_dir();
    append_history(&run_dir);
    let hist = run_dir.join("cockpit-history.csv");
    let rows = std::fs::read_to_string(&hist).map(|c| c.lines().count().saturating_sub(1)).unwrap_or(0);
    println!("spira cockpit: {} ({rows} rows)", hist.display());
    0
}

fn sweep_stale_snapshot_tmps(run_dir: &std::path::Path) {
    let mut n = 0;
    if let Ok(entries) = std::fs::read_dir(run_dir) {
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(".cockpit.")
                && std::fs::remove_file(e.path()).is_ok() {
                    n += 1;
                }
        }
    }
    if n > 0 {
        eprintln!("cockpit-collect: swept {n} stale temp(s) from {}", run_dir.display());
    }
}

fn cmd_sweep_temps() -> i32 {
    sweep_stale_snapshot_tmps(&io::run_dir());
    0
}

fn cmd_probe(name: Option<&str>) -> i32 {
    let Some(name) = name else { return usage() };
    let pcfg = match probes::Cfg::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cockpit-collect: {e}");
            return 1;
        }
    };
    match probes::run(name, &pcfg) {
        Some(kv) => {
            print!("{}", probes::render(&kv));
            0
        }
        None => {
            eprintln!(
                "usage: cockpit-collect probe [now|core|core_detail|unsent|strands|sops|ratelim|reachable|well|repo_labels|livelock|dup_refs|statute|drift|mail|czar_triggers|hel|queue|slots|admission|tsd]"
            );
            1
        }
    }
}

fn cmd_metrics(args: &[String]) -> i32 {
    let (Some(sent), Some(ledger)) = (args.first(), args.get(1)) else {
        eprintln!("usage: cockpit-collect metrics <sentinel.log> <dwarf-ledger.log> [hours]");
        return 1;
    };
    let hours = match args.get(2).map(|h| h.parse::<f64>()) {
        None => 24.0,
        Some(Ok(h)) => h,
        Some(Err(e)) => {
            eprintln!("cockpit-collect metrics: bad hours: {e}");
            return 1;
        }
    };
    let keys = metrics::collect(sent, ledger, hours, metrics::self_window_minutes(), timeutil::now_secs());
    print!("{}", metrics::render(&keys));
    0
}

fn cmd_sparklines(args: &[String]) -> i32 {
    use std::io::Read;
    let mut stdin = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin);
    if let Some(out) = sparklines::render(&stdin, args.first().map_or("", String::as_str), timeutil::now_secs(), &counts::from_config()) {
        print!("{out}");
    }
    0
}
