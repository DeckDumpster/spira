//! slay.sh — stop one aeon cleanly and make its bead say what is true. Replaces
//! spira/slay.sh (bash).
//!
//!   slay.sh --bead <id> [--why "<text>"] [--keep-work] [--close "<reason>" | --reopen]
//!
//! NAMED ARGUMENTS ONLY, same as the bash version and for the same scar: a bare word here
//! used to become the bead id, so a reason written beside the id silently replaced it.
//!
//! WHAT MOVED WHERE (DESIGN.md Decisions has the full account): the marker write, the
//! hold-vs-aeon distinction, the systemd-unit-or-pid stop and the wait/escalate-to-KILL
//! loop are native Rust (`spira_world::proc`, `spira_world::sysctl`) — this is the part of
//! the original file that was genuinely process-control logic, and the part whose own
//! history (the positional-argument bug, the pidfile-vs-environment BEAD_ID bug, the
//! split-checkout "both homes" bug) shows it benefits most from a type system. Steps 0
//! (bead existence), 4a (claim release), 5 (salvage/park/destroy the work) and 4b/6 (the
//! note and the final status) still go through lib.sh's own chokepoints — `bdq`'s fencing,
//! and the ONE permitted door onto `git worktree remove`/`git branch -D` — via the
//! `spira_world::seam::SLAY_FINISH` seam, because lib.sh is explicitly out of this wave's
//! scope. That seam's stdout is slay.sh's own narration, forwarded verbatim.

use std::env;
use std::path::PathBuf;
use std::process::Command;

use spira_world::{seam, sysctl};

fn usage_text() -> &'static str {
    r#"slay.sh — stop one aeon cleanly and make its bead say what is true.

  slay.sh --bead <id> [--why "<text>"] [--keep-work] [--close "<reason>" | --reopen]

REQUIRED
  --bead <id>        The bead whose aeon is to be stopped. Must exist in SPIRA_DB;
                     an id no bead carries is refused rather than acted on.

OPTIONAL
  --why "<text>"     What the note on the bead records as the operator's reason.
                     Default: "slain by the operator". This is where a sentence goes.
  --keep-work        Leave the branch and worktree exactly as they are.
  --close "<reason>" Close the bead with this reason instead of releasing it.
  --reopen           Release the bead as open and unassigned. This is the default.
  -h, --help         This text.

WHAT HAPPENS TO THE WORK WITHOUT --keep-work
  It is RETIRED, not lost. Uncommitted changes are salvaged to SPIRA_RUN/reaped as a
  patch; a branch carrying commits the base does not have is parked at refs/slain/<id>,
  a real ref that survives gc; and if that parking fails the deletion is REFUSED. Only a
  branch holding nothing the base lacks is simply removed. Use --keep-work when you want
  the branch left in refs/heads, not because you fear losing the commits.

EXIT
  0  the aeon was stopped and the bead says what is true
  1  something could not be done — the message names it
  2  usage: no --bead, an unknown flag, a positional argument, two --bead values,
     or a bead id the store does not carry
"#
}

fn usage_err(msg: &str) -> ! {
    eprintln!("slay.sh: {msg}");
    eprint!("{}", usage_text());
    std::process::exit(2);
}

struct Args {
    id: String,
    mode_close: Option<String>,
    keep: bool,
    why: String,
}

fn parse_args(argv: &[String]) -> Args {
    let mut id: Option<String> = None;
    let mut mode_close: Option<String> = None;
    let mut keep = false;
    let mut why = "slain by the operator".to_string();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--bead" => {
                if id.is_some() {
                    usage_err(&format!("--bead given twice ({}, {})", id.unwrap(), argv.get(i + 1).cloned().unwrap_or_default()));
                }
                let Some(v) = argv.get(i + 1) else { usage_err("--bead needs a bead id") };
                id = Some(v.clone());
                i += 2;
            }
            "--close" => {
                let Some(v) = argv.get(i + 1) else { usage_err("--close needs a reason") };
                mode_close = Some(v.clone());
                i += 2;
            }
            "--reopen" => {
                mode_close = None;
                i += 1;
            }
            "--keep-work" => {
                keep = true;
                i += 1;
            }
            "--why" => {
                let Some(v) = argv.get(i + 1) else { usage_err("--why needs text") };
                why = v.clone();
                i += 2;
            }
            "-h" | "--help" => {
                print!("{}", usage_text());
                std::process::exit(0);
            }
            other if other.starts_with('-') => {
                usage_err(&format!("unknown flag {other}"));
            }
            other => {
                eprintln!("slay.sh: unexpected argument \"{other}\" — this tool takes named arguments only.");
                eprintln!("slay.sh: the bead goes in --bead, a reason goes in --why.");
                std::process::exit(2);
            }
        }
    }
    let Some(id) = id else { usage_err("--bead is required") };
    Args { id, mode_close, keep, why }
}

fn say(s: &str) {
    println!("{s}");
}

fn spira_run() -> PathBuf {
    spira_world::spira_run()
}

fn now_iso() -> String {
    Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn epoch() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `$SPIRA_RUN/aeon-*-<id>.pid`, first match (bash's `ls ... | head -1`; glob order, which
/// on a healthy box is at most one match — two pidfiles for the same bead is itself a
/// defect this refuses to silently pick between by taking whichever sorts first).
fn find_aeon_pidfile(run: &std::path::Path, id: &str) -> Option<PathBuf> {
    let rd = std::fs::read_dir(run).ok()?;
    let suffix = format!("-{id}.pid");
    let mut matches: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("aeon-") && n.ends_with(&suffix))
                .unwrap_or(false)
        })
        .collect();
    matches.sort();
    matches.into_iter().next()
}

fn main() {
    let argv: Vec<String> = env::args().skip(1).collect();
    let args = parse_args(&argv);
    let id = &args.id;
    let run = spira_run();
    let exe = env::current_exe().ok();
    let lib_sh = exe.as_deref().and_then(spira_world::locate_home).map(|h| h.join("lib.sh"));

    // ---- 0. the bead must exist -------------------------------------------------------
    let found = lib_sh
        .as_deref()
        .map(|lib| seam::run(lib, "slay-exists", seam::SLAY_EXISTS, &[("SLAY_ID", id)], ""))
        .filter(|(_, ok)| *ok)
        .map(|(out, _)| out.trim().to_string())
        .unwrap_or_default();
    if found != *id {
        let db = env::var("SPIRA_DB").unwrap_or_else(|_| "?".to_string());
        eprintln!("slay.sh: no bead {id} in {db} — refusing to act");
        if !found.is_empty() {
            eprintln!("slay.sh: the store answered with {found} instead (prefix match)");
        }
        std::process::exit(2);
    }

    let mut fail = false;

    // ---- 1. the marker ------------------------------------------------------------------
    let marker = run.join(format!("{id}.slain"));
    let _ = std::fs::write(&marker, format!("{}\t{}\n", now_iso(), args.why));

    // ---- 2/3. the holder (aeon OR manual hold) ------------------------------------------
    let mut name = String::new();
    let mut pid = String::new();
    let mut unit = String::new();

    let hold_pid_file = run.join(format!("hold-{id}.pid"));
    if hold_pid_file.is_file() {
        let hpid = std::fs::read_to_string(&hold_pid_file).unwrap_or_default().trim().to_string();
        let hb_file = run.join(format!("hold-{id}.hb"));
        let hbpid = if hb_file.is_file() {
            std::fs::read_to_string(&hb_file).unwrap_or_default().trim().to_string()
        } else {
            String::new()
        };
        if !hbpid.is_empty() {
            let _ = Command::new("kill").arg(&hbpid).status();
        }
        let _ = std::fs::remove_file(&hold_pid_file);
        let _ = std::fs::remove_file(&hb_file);
        let _ = Command::new("spira-lc").args(["unhold", id, "operator", "slay"]).output();
        say(&format!(
            "hold: manual hold released for {id} (holder pid {}, heartbeat {})",
            if hpid.is_empty() { "?" } else { &hpid },
            if hbpid.is_empty() { "none" } else { &hbpid }
        ));
    } else {
        let pf = find_aeon_pidfile(&run, id);
        if let Some(pf) = &pf {
            pid = std::fs::read_to_string(pf).unwrap_or_default().trim().to_string();
            let name_file = pf.with_extension("name");
            name = std::fs::read_to_string(&name_file).unwrap_or_default().trim().to_string();
        }
        if !pid.is_empty() {
            for u in sysctl::run_lines(&["list-units", "spira-aeon-*", "--no-legend"]) {
                if let Some(unit_name) = sysctl::first_field(&u) {
                    if sysctl::run(&["show", "-p", "MainPID", "--value", unit_name]) == pid {
                        unit = unit_name.to_string();
                        break;
                    }
                }
            }
        }
        let alive = !pid.is_empty() && std::path::Path::new(&format!("/proc/{pid}")).is_dir();
        if !alive {
            say(&format!("aeon: none running for {id} (no live pid file) — setting the bead and the work only"));
            if let Some(pf) = &pf {
                let _ = std::fs::remove_file(pf);
                let _ = std::fs::remove_file(pf.with_extension("name"));
            }
        } else {
            if !unit.is_empty() {
                say(&format!("aeon: {} pid {pid} is {unit} — stopping the unit", if name.is_empty() { "?" } else { &name }));
                if !sysctl::run_ok(&["stop", &unit]) {
                    say(&format!("aeon: systemctl stop failed — sending TERM to {pid}"));
                    let _ = Command::new("kill").args(["-TERM", &pid]).status();
                }
            } else {
                say(&format!("aeon: {} pid {pid} has no unit — sending TERM", if name.is_empty() { "?" } else { &name }));
                let _ = Command::new("kill").args(["-TERM", &pid]).status();
            }
            let t0 = epoch();
            loop {
                let pf_exists = pf.as_ref().map(|p| p.is_file()).unwrap_or(false);
                let proc_exists = std::path::Path::new(&format!("/proc/{pid}")).is_dir();
                if !pf_exists && !proc_exists {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
                if epoch() - t0 >= 60 {
                    say("aeon: still alive after 60s — KILL");
                    let _ = Command::new("kill").args(["-KILL", &pid]).status();
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    if let Some(pf) = &pf {
                        let _ = std::fs::remove_file(pf);
                        let _ = std::fs::remove_file(pf.with_extension("name"));
                    }
                    break;
                }
            }
            if std::path::Path::new(&format!("/proc/{pid}")).is_dir() {
                say(&format!("aeon: pid {pid} SURVIVED a KILL — investigate by hand"));
                fail = true;
            } else {
                say(&format!("aeon: stopped ({}s)", epoch() - t0));
            }
        }
    }
    let _ = std::fs::remove_file(&marker);

    // ---- 4a/5/4b/6 — everything downstream that still needs lib.sh's chokepoints -------
    let (mode, reason) = match &args.mode_close {
        Some(r) => ("close", r.as_str()),
        None => ("reopen", ""),
    };
    let seam_fail = match lib_sh.as_deref() {
        Some(lib) => {
            let (out, ok) = seam::run(
                lib,
                "slay-finish",
                seam::SLAY_FINISH,
                &[
                    ("SLAY_ID", id.as_str()),
                    ("SLAY_WHY", &args.why),
                    ("SLAY_MODE", mode),
                    ("SLAY_REASON", reason),
                    ("SLAY_KEEP", if args.keep { "1" } else { "0" }),
                    ("SLAY_NAME", &name),
                    ("SLAY_PID", &pid),
                    ("SLAY_UNIT", &unit),
                ],
                "",
            );
            let mut result_fail = !ok;
            for line in out.lines() {
                if let Some(v) = line.strip_prefix("___SLAY_RESULT___\t") {
                    result_fail = result_fail || v.trim() != "0";
                } else {
                    println!("{line}");
                }
            }
            result_fail
        }
        None => {
            eprintln!("slay.sh: cannot locate lib.sh — the bead's claim, note and work were NOT touched");
            true
        }
    };
    fail = fail || seam_fail;

    // ---- 6. verify (the process-side half; the seam already verified the branch) -------
    if !pid.is_empty() && std::path::Path::new(&format!("/proc/{pid}")).is_dir() {
        fail = true;
    }
    if find_aeon_pidfile(&run, id).is_some() {
        say(&format!("verify: a pid file for {id} remains"));
        fail = true;
    }

    if fail {
        say(&format!("slay: INCOMPLETE for {id} — see above"));
        std::process::exit(1);
    }
    say(&format!("slain: {id}"));
}
