//! cockpit-collect — Spira's own cockpit collector, replacing `spira/cockpit.sh` and
//! `spira/collect.sh` (sp-kt4l3). DESIGN.md has the full contract; this file is only CLI
//! dispatch, mirroring the two scripts' own subcommand names so every existing caller
//! (systemd, `sop.sh`'s METRIC seam, the `test-cockpit-*.sh` suites) needs only its
//! invocation repointed, never its assertions rewritten.

use cockpit_collect::{counts, io, metrics, probes, quoting, sparklines, supervisor, tiers, timeutil};

fn usage() -> i32 {
    eprintln!(
        "usage: cockpit-collect [once|history|sweep-temps|collect|merge|probe <name>|check-config]\n  (no args: same as once, no attach — attaching the operator to the concierge stays root cockpit.sh's job)"
    );
    1
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    const OPS: [&str; 9] = ["once", "history", "sweep-temps", "collect", "merge", "probe", "--supervised-run", "_probe_body_test", "check-config"];
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
    match args.first().map(String::as_str) {
        Some("check-config") => return spira_config::process::check_config_verb("cockpit-collect", &[]),
        Some("--supervised-run") => {
            if let Err(e) = spira_config::process::check_config(&[]) {
                eprintln!("cockpit-collect: {e}");
                return 1;
            }
        }
        _ => {}
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
        Some("collect") => cmd_collect(),
        Some("merge") => supervisor::merge_once(),
        Some("probe") => cmd_probe(args.get(1).map(String::as_str)),
        Some("--supervised-run") => cmd_supervised_run(&args[1..]),
        Some("_probe_body_test") => cmd_probe_body_test(&args[1..]),
        Some("--test-may-write") => cmd_test_may_write(),
        Some("--test-loop-guard") => cmd_test_loop_guard(),
        Some(other) => {
            eprintln!("cockpit-collect: unknown subcommand '{other}'");
            usage()
        }
    }
}

/// `SPIRA_INSTANCE`, through the one door (`spira_config::resolve::resolve_instance`, the
/// same call `loom`'s own `Config::from_env` makes). `None` only when resolution itself
/// fails (no `$SPIRA_TOML`, an unresolvable registry, …) — this binary degrades to the
/// single-instance write-admission check in that case rather than refusing outright, since
/// `may_write`'s own fence (`INVOCATION_ID`) is the hard gate here, not this name.
fn instance() -> Option<String> {
    let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    spira_config::resolve::resolve_instance(&env, &io::home_dir())
        .ok()
        .filter(|s| !s.is_empty())
}

/// `once`: the backward-compatible full serial pass. Writes the snapshot when this process
/// may write it; otherwise prints the same keys to stdout with a warning on stderr — never
/// silently drops the reading.
fn cmd_once() -> i32 {
    let run_dir = io::run_dir();
    let _ = std::fs::create_dir_all(&run_dir);
    sweep_stale_snapshot_tmps(&run_dir);

    let pcfg = match probes::Cfg::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cockpit-collect: {e}");
            return 1;
        }
    };
    let may_write = supervisor::may_write(instance().as_deref());
    let kv = probes::full_pass(&pcfg);

    if may_write {
        write_snapshot_and_history(&kv, &run_dir);
        let snap = run_dir.join("cockpit.env");
        let n = std::fs::read_to_string(&snap).map(|c| c.lines().count()).unwrap_or(0);
        println!("spira cockpit: {} ({n} keys)", snap.display());
    } else {
        print!("{}", probes::render(&kv));
        eprintln!("spira cockpit: keys printed to stdout (not the supervised process)");
    }
    0
}

fn write_snapshot_and_history(kv: &probes::Kv, run_dir: &std::path::Path) {
    let writer_unit = if std::env::var("INVOCATION_ID").ok().filter(|s| !s.is_empty()).is_some() {
        "spira-cockpit.service".to_string()
    } else {
        "force".to_string()
    };
    let mut full = kv.clone();
    full.push(("SP_WRITER".to_string(), format!("{}:{}", std::process::id(), writer_unit)));
    let deduped = probes::dedup_first_wins(full);

    let mut content = String::new();
    for (k, v) in &deduped {
        content.push_str(&format!("{k}={}\n", quoting::self_quote(v)));
    }
    let snap = run_dir.join("cockpit.env");
    let tmp = run_dir.join(format!(".cockpit.{}", std::process::id()));
    if std::fs::write(&tmp, &content).is_ok() {
        let _ = std::fs::rename(&tmp, &snap);
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
    append_history(run_dir);
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

const COLLECT_LOCK: &str = "cockpit-collect.lock";
const COLLECT_SKIP: &str = "cockpit-collect collect: previous collect still running (holds the lock) — skipping this start";

/// At most one `collect` per run dir: a second start exits 0 at once rather than stacking
/// load on a host that is already slow (law-fence-loops-on-shared-hardware).
fn collect_lock(run_dir: &std::path::Path) -> Result<Option<std::fs::File>, String> {
    let _ = std::fs::create_dir_all(run_dir);
    strand::state::try_lock(&run_dir.join(COLLECT_LOCK)).map_err(|e| format!("cockpit-collect collect: lock: {e}"))
}

fn cmd_collect() -> i32 {
    let _lock = match collect_lock(&io::run_dir()) {
        Ok(Some(f)) => f,
        Ok(None) => {
            eprintln!("{COLLECT_SKIP}");
            return 0;
        }
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    if !supervisor::may_write(instance().as_deref()) {
        eprintln!("cockpit-collect collect: write refused — not the supervised process. Set SPIRA_COCKPIT_FORCE=1 to override.");
        return 1;
    }
    sweep_stale_snapshot_tmps(&io::run_dir());
    let cfg = match supervisor::Config::from_env(io::run_dir()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cockpit-collect collect: {e}");
            return 1;
        }
    };
    let stale_s = match spira_config::process::cfg_parse::<u64>("SPIRA_SNAP_STALE_S") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("cockpit-collect collect: {e}");
            return 2;
        }
    };
    if let Err(e) = supervisor::check_tick(cfg.tick, stale_s) {
        eprintln!("cockpit-collect collect: {e}");
        return 2;
    }
    supervisor::run_loop(&cfg, |msg| eprintln!("{msg}"))
}

fn cmd_supervised_run(args: &[String]) -> i32 {
    let (Some(name), Some(timeout_s), Some(subcommand)) = (args.first(), args.get(1), args.get(2)) else {
        return usage();
    };
    let Ok(timeout_s) = timeout_s.parse::<u64>() else { return usage() };
    supervisor::supervised_run_main(name, timeout_s, subcommand);
    0
}

fn cmd_probe_body_test(args: &[String]) -> i32 {
    let (Some(name), Some(timeout_s), Some(subcommand)) = (args.first(), args.get(1), args.get(2)) else {
        return usage();
    };
    let Ok(timeout_s) = timeout_s.parse::<u64>() else { return usage() };
    supervisor::probe_body_test_main(name, timeout_s, subcommand)
}

fn cmd_test_may_write() -> i32 {
    println!("{}", if supervisor::may_write(instance().as_deref()) { 1 } else { 0 });
    0
}

fn cmd_test_loop_guard() -> i32 {
    if supervisor::may_write(instance().as_deref()) {
        0
    } else {
        eprintln!("cockpit-collect: loop guard refused — not the supervised process. Set SPIRA_COCKPIT_FORCE=1 to override.");
        1
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
