//! cockpit-collect — Spira's own cockpit collector, replacing `spira/cockpit.sh` and
//! `spira/collect.sh` (sp-kt4l3). DESIGN.md has the full contract; this file is only CLI
//! dispatch, mirroring the two scripts' own subcommand names so every existing caller
//! (systemd, `sop.sh`'s METRIC seam, the `test-cockpit-*.sh` suites) needs only its
//! invocation repointed, never its assertions rewritten.

mod io;
mod probes;
mod quoting;
mod supervisor;

/// Test-only: cargo runs a crate's tests in parallel threads, but several probe tests
/// mutate process-global environment variables (`SPIRA_RUN` and friends) that `io::` reads
/// implicitly, the same way the bash read them. Any test that does so takes this lock first
/// so two such tests never interleave their env mutation and their assertion.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Mutex;
    pub static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Pins `SPIRA_HOME` beside `SPIRA_RUN`: `run_dir()` judges the run dir against the home's
    /// config, so a test that sets only `SPIRA_RUN` passes or exits the binary by test order.
    pub fn set_run(run: &std::path::Path) -> testkit::EnvGuard {
        testkit::env(&[
            ("SPIRA_HOME", Some(concat!(env!("CARGO_MANIFEST_DIR"), "/../spira"))),
            ("SPIRA_RUN", Some(run.to_str().unwrap())),
        ])
    }
}

fn usage() -> i32 {
    eprintln!(
        "usage: cockpit-collect [once|history|sweep-temps|collect|merge|probe <name>]\n  (no args: same as once, no attach — attaching the operator to the concierge stays root cockpit.sh's job)"
    );
    1
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = run(&args[1..]);
    std::process::exit(code);
}

fn run(args: &[String]) -> i32 {
    // spira/cockpit.sh sourced lib.sh (which cascades into conf.sh) unconditionally at its
    // own top level before dispatching to any subcommand; see io::bootstrap_config for why
    // this binary must do the same instead of reading unresolved env vars.
    io::bootstrap_config();
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

fn instance() -> Option<String> {
    std::env::var("SPIRA_INSTANCE").ok().filter(|s| !s.is_empty())
}

/// `once`: the backward-compatible full serial pass. Writes the snapshot when this process
/// may write it; otherwise prints the same keys to stdout with a warning on stderr — never
/// silently drops the reading.
fn cmd_once() -> i32 {
    let run_dir = io::run_dir();
    let _ = std::fs::create_dir_all(&run_dir);
    sweep_stale_snapshot_tmps(&run_dir);

    let may_write = supervisor::may_write(instance().as_deref());
    let kv = probes::full_pass();

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

/// `append_history`: read the snapshot back (never the probe's own in-memory output — the
/// snapshot is what every other reader sees) and append one row, rotating the file on a
/// column-set change rather than overwriting it.
fn append_history(run_dir: &std::path::Path) {
    let snap_path = run_dir.join("cockpit.env");
    let snap: std::collections::HashMap<String, String> = std::fs::read_to_string(&snap_path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.trim_matches('\'').to_string()))
        .collect();
    let get = |k: &str| snap.get(k).cloned().unwrap_or_else(|| "?".to_string());
    let at = snap.get("SP_AT").cloned().unwrap_or_else(|| io::now().to_string());
    let row = format!(
        "{at},{},{},{},{},{},{},{},{}",
        get("SP_TOK_WIN"), get("SP_TOK_AEON_WIN"), get("SP_TOK_SESS_WIN"),
        get("SP_TOK_AEON_TURNS"), get("SP_TOK_SESS_TURNS"),
        get("SP_CTX_NOW"), get("SP_RATELIM_5H"), get("SP_RATELIM_7D"),
    );
    const HIST_COLS: &str = "ts,tok_win,tok_aeon_win,tok_sess_win,tok_aeon_turns,tok_sess_turns,ctx_now,ratelim_5h,ratelim_7d";
    let hist_path = run_dir.join("cockpit-history.csv");
    let existing = std::fs::read_to_string(&hist_path).ok();
    let needs_header = match &existing {
        None => true,
        Some(c) => c.lines().next() != Some(HIST_COLS),
    };
    if needs_header {
        if let Some(c) = &existing {
            if !c.is_empty() {
                let _ = std::fs::rename(&hist_path, run_dir.join(format!("cockpit-history.csv.{}", io::now())));
            }
        }
        let _ = std::fs::write(&hist_path, format!("{HIST_COLS}\n"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&hist_path) {
        use std::io::Write;
        let _ = writeln!(f, "{row}");
    }

    let max: usize = std::env::var("SPIRA_COCKPIT_HISTORY_MAX").ok().and_then(|v| v.parse().ok()).unwrap_or(20160);
    if let Ok(content) = std::fs::read_to_string(&hist_path) {
        let lines: Vec<&str> = content.lines().collect();
        if lines.len() > max + 1 {
            let mut kept = String::new();
            kept.push_str(lines[0]);
            kept.push('\n');
            for l in &lines[lines.len() - max..] {
                kept.push_str(l);
                kept.push('\n');
            }
            let _ = std::fs::write(&hist_path, kept);
        }
    }
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
            if name.starts_with(".cockpit.") {
                if std::fs::remove_file(e.path()).is_ok() {
                    n += 1;
                }
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
    match probes::run(name) {
        Some(kv) => {
            print!("{}", probes::render(&kv));
            0
        }
        None => {
            eprintln!(
                "usage: cockpit-collect probe [now|core|core_detail|unsent|strands|sops|ratelim|reachable|sphere|repo_labels|livelock|dup_refs|statute|drift|mail|czar_triggers|sending|queue|slots|admission]"
            );
            1
        }
    }
}

fn cmd_collect() -> i32 {
    if !supervisor::may_write(instance().as_deref()) {
        eprintln!("cockpit-collect collect: write refused — not the supervised process. Set SPIRA_COCKPIT_FORCE=1 to override.");
        return 1;
    }
    sweep_stale_snapshot_tmps(&io::run_dir());
    let cfg = supervisor::Config::from_env(io::run_dir());
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty probe value must round-trip as exactly `KEY=''` (two characters, sourceable
    /// as the empty string) through `once`'s write path — not the doubled `KEY=''\'''\'''`
    /// production's bash collector produced for ten keys (SP_HOTFIX_LINE, SP_HOTFIX_ALERT,
    /// SP_AURON_KEYS, SP_OVERRIDES_LIST, and the six `*_NAMES` lists) by quoting its own
    /// value before the merge/write step quoted the line a second time (the operator,
    /// 2026-09-30, found in production's live cockpit.env). Every probe now emits these
    /// keys raw, same as any other key; this is the ONE quoting layer, at write time.
    #[test]
    fn empty_value_round_trips_as_two_char_empty_quotes() {
        let run = testkit::TempDir::new("cc-empty-quote");
        let kv: probes::Kv = vec![
            ("SP_HOTFIX_LINE".to_string(), String::new()),
            ("SP_AURON_KEYS".to_string(), String::new()),
            ("SP_PROTECTED_NAMES".to_string(), String::new()),
            ("SP_AT".to_string(), "123".to_string()),
        ];
        write_snapshot_and_history(&kv, run.path());
        let snap = std::fs::read_to_string(run.path().join("cockpit.env")).unwrap();
        for k in ["SP_HOTFIX_LINE", "SP_AURON_KEYS", "SP_PROTECTED_NAMES"] {
            let line = snap.lines().find(|l| l.starts_with(&format!("{k}="))).unwrap_or_else(|| panic!("{k} missing from snapshot"));
            assert_eq!(line, format!("{k}=''"), "empty {k} must be exactly '' (2 chars), not doubled");
        }
        // And it must actually source as the empty string, not the two-character literal "''".
        let sourced = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(". {:?}; printf '%s' \"$SP_HOTFIX_LINE\"", snap_path(run.path())))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&sourced.stdout), "");
    }

    fn snap_path(run_dir: &std::path::Path) -> String {
        run_dir.join("cockpit.env").to_string_lossy().into_owned()
    }
}

