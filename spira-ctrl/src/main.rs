//! ctrl.sh — operational control plane CLI. Replaces spira/ctrl.sh (bash+python3).
//!
//!   ctrl.sh suspend <subject> --reason <text> --owner <bead>
//!   ctrl.sh resume  <subject>
//!   ctrl.sh check   <subject>          exits 0 if suspended, 1 if not
//!   ctrl.sh reason  <subject>
//!   ctrl.sh list
//!   ctrl.sh suspended          every suspended subject, one `subject\treason` line each
//!   ctrl.sh divergence
//!
//! `$SPIRA_CTRL` names the one JSON file this reads and writes — resolved from the
//! environment exactly as conf.sh exports it (`: "${SPIRA_CTRL:=$SPIRA_RUN/control}"`);
//! this binary does not source conf.sh, since every caller that starts it already has.
//! `$SPIRA_SYSTEMCTL` is the systemctl seam `divergence` shells to, same variable name
//! world.sh carries, so one stub covers both in a test.

use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use spira_ctrl::{self as ctrl, CtrlData};

fn spira_run() -> PathBuf {
    env::var_os("SPIRA_RUN").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp/spira"))
}

fn ctrl_path() -> PathBuf {
    env::var_os("SPIRA_CTRL").map(PathBuf::from).unwrap_or_else(|| spira_run().join("control"))
}

fn systemctl_bin() -> String {
    env::var("SPIRA_SYSTEMCTL").unwrap_or_else(|_| "systemctl".to_string())
}

fn sc(args: &[&str]) -> String {
    Command::new(systemctl_bin())
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn sc_lines(args: &[&str]) -> Vec<String> {
    Command::new(systemctl_bin())
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// `today's date in $SPIRA_TZ` — shells to `date(1)` rather than carrying a timezone
/// database in this binary; `do_suspend`'s own `when` computation does the same (`TZ=...
/// date '+%Y-%m-%d'`), just from bash instead of Rust.
fn today() -> String {
    let tz = env::var("SPIRA_TZ").unwrap_or_default();
    let mut cmd = Command::new("date");
    cmd.arg("+%Y-%m-%d");
    if !tz.is_empty() {
        cmd.env("TZ", tz);
    }
    cmd.output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn usage() -> ! {
    eprintln!("ctrl.sh — operational control plane\n");
    eprintln!("  ctrl.sh suspend <subject> --reason <text> --owner <bead>");
    eprintln!("  ctrl.sh resume  <subject>");
    eprintln!("  ctrl.sh check   <subject>");
    eprintln!("  ctrl.sh reason  <subject>");
    eprintln!("  ctrl.sh list");
    eprintln!("  ctrl.sh suspended     every suspended subject, `subject\\treason` per line");
    eprintln!("  ctrl.sh divergence");
    std::process::exit(1);
}

fn load_or_die() -> CtrlData {
    match ctrl::read(&ctrl_path()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("ctrl: cannot read control file: {e}");
            std::process::exit(1);
        }
    }
}

fn cmd_suspend(args: &[String]) -> ExitCode {
    let Some(subject) = args.first() else {
        eprintln!("ctrl: suspend requires a subject");
        return ExitCode::from(1);
    };
    let mut reason = None;
    let mut owner = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--reason" => {
                reason = args.get(i + 1).cloned();
                i += 2;
            }
            "--owner" => {
                owner = args.get(i + 1).cloned();
                i += 2;
            }
            other => {
                eprintln!("ctrl: unknown flag: {other}");
                return ExitCode::from(1);
            }
        }
    }
    let Some(reason) = reason.filter(|r| !r.is_empty()) else {
        eprintln!("ctrl: --reason is required");
        return ExitCode::from(1);
    };
    let Some(owner) = owner.filter(|o| !o.is_empty()) else {
        eprintln!("ctrl: --owner is required");
        return ExitCode::from(1);
    };
    let by = env::var("USER").unwrap_or_else(|_| "operator".to_string());
    let mut data = load_or_die();
    ctrl::suspend(&mut data, subject, &reason, &owner, &today(), &by);
    if let Err(e) = ctrl::write_atomic(&ctrl_path(), &data) {
        eprintln!("ctrl: failed to update control file: {e}");
        return ExitCode::from(1);
    }
    println!("ctrl: suspended {subject} (owner: {owner})");
    ExitCode::SUCCESS
}

fn cmd_resume(args: &[String]) -> ExitCode {
    let Some(subject) = args.first() else {
        eprintln!("ctrl: resume requires a subject");
        return ExitCode::from(1);
    };
    let path = ctrl_path();
    if !path.is_file() {
        println!("ctrl: {subject} is not suspended (no control file)");
        return ExitCode::SUCCESS;
    }
    let mut data = load_or_die();
    let was = ctrl::resume(&mut data, subject);
    if let Err(e) = ctrl::write_atomic(&path, &data) {
        eprintln!("ctrl: failed to update control file: {e}");
        return ExitCode::from(1);
    }
    if was {
        println!("ctrl: resumed {subject}");
    } else {
        println!("ctrl: {subject} was not suspended");
    }
    ExitCode::SUCCESS
}

fn cmd_reason(args: &[String]) -> ExitCode {
    let Some(subject) = args.first() else {
        eprintln!("ctrl: reason requires a subject");
        return ExitCode::from(1);
    };
    if !ctrl_path().is_file() {
        return ExitCode::SUCCESS;
    }
    let data = load_or_die();
    if let Some(r) = ctrl::reason(&data, subject) {
        if !r.is_empty() {
            println!("{r}");
        }
    }
    ExitCode::SUCCESS
}

fn cmd_check(args: &[String]) -> ExitCode {
    let Some(subject) = args.first() else {
        eprintln!("ctrl: check requires a subject");
        return ExitCode::from(1);
    };
    if !ctrl_path().is_file() {
        return ExitCode::from(1);
    }
    let data = load_or_die();
    if ctrl::is_suspended(&data, subject) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// `ctrl suspended` — every currently suspended subject and its reason, one
/// tab-separated `subject\treason` line per entry, for a caller that used to source
/// `ctrl.sh` as a bash library (`CTRL_LIB=1 . ctrl.sh; ctrl_load_suspended ...`) to read
/// every suspension in one process instead of a `check`/`reason` round trip per unit
/// (install.sh's per-timer apply loop, watchtower.sh's degraded-timer check). Sourcing a
/// Rust binary is not a thing, so this is the seam those two callers now read instead —
/// still one process, and `spira_ctrl::load_suspended` is the same function `ctrl.sh
/// list`'s own data comes from.
fn cmd_suspended() -> ExitCode {
    if !ctrl_path().is_file() {
        return ExitCode::SUCCESS;
    }
    let data = load_or_die();
    for (subject, reason) in ctrl::load_suspended(&data) {
        println!("{subject}\t{reason}");
    }
    ExitCode::SUCCESS
}

fn cmd_list() -> ExitCode {
    if !ctrl_path().is_file() {
        println!("ctrl: no control file — no entries");
        return ExitCode::SUCCESS;
    }
    let data = load_or_die();
    print!("{}", ctrl::render_list(&data));
    ExitCode::SUCCESS
}

fn cmd_divergence() -> ExitCode {
    let data = match ctrl::read(&ctrl_path()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("ctrl: cannot read control file: {e}");
            return ExitCode::from(2);
        }
    };
    let inst = env::var("SPIRA_INSTANCE").unwrap_or_else(|_| "prod".to_string());
    let mut found = false;

    // Direction 1: declared-but-running.
    let suspended = ctrl::load_suspended(&data);
    for subject in suspended.keys() {
        for unit in ctrl::unit_forms(subject, &inst) {
            let state = sc(&["--user", "is-active", &unit]);
            let enabled = sc(&["--user", "is-enabled", &unit]);
            if state == "active" || enabled == "enabled" {
                println!(
                    "ctrl: DIVERGENCE {subject} declared suspended but {unit} is {} (enabled: {})",
                    if state.is_empty() { "inactive" } else { &state },
                    if enabled.is_empty() { "?" } else { &enabled }
                );
                found = true;
            }
        }
    }

    // Direction 2: undeclared-but-masked.
    let entries: BTreeMap<&str, ()> = suspended.keys().map(|s| (s.as_str(), ())).collect();
    for line in sc_lines(&["--user", "list-unit-files", "spira*", "--no-legend", "--no-pager"]) {
        let mut cols = line.split_whitespace();
        let (Some(unit_name), Some(state)) = (cols.next(), cols.next()) else { continue };
        if state != "masked" {
            continue;
        }
        let subject = ctrl::subject_of_masked_unit(unit_name, &inst);
        if !entries.contains_key(subject.as_str()) {
            println!("ctrl: DIVERGENCE {unit_name} is masked but has no control-plane entry (undeclared suspension)");
            found = true;
        }
    }

    if found {
        ExitCode::from(1)
    } else {
        println!("ctrl: 0 divergences");
        ExitCode::SUCCESS
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(cmd) = args.first() else { usage() };
    let rest = &args[1..];
    match cmd.as_str() {
        "suspend" => cmd_suspend(rest),
        "resume" => cmd_resume(rest),
        "check" => cmd_check(rest),
        "reason" => cmd_reason(rest),
        "list" => cmd_list(),
        "suspended" => cmd_suspended(),
        "divergence" => cmd_divergence(),
        _ => usage(),
    }
}
