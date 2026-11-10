//! `gh-intake [--dry-run]` — see DESIGN.md. Ported from `spira/gh-intake.sh` (deleted,
//! sp-8fsql); same env vars, same argv shape, called by bare name on the release PATH.
//!
//! `gh-intake closeout <bead-id> <sha> <repo-path>`, `gh-intake unlanded-scan` and
//! `gh-intake backfill [--dry-run]` are the closeout half (sp-j3fim, "wave 4.31"):
//! `gh_issue_closeout`, `_gh_unlanded_scan` and `gh-issue-backfill.sh`, each a one-line
//! shim for its own bash form (`closeout`/`unlanded-scan` are landing-pass's and queue's
//! new seam targets; `backfill` is the operator's own manual tool).

use gh_intake::closeout::{self, Ctx, Deps};
use gh_intake::logic::{self, Config};
use gh_intake::real::{RealBd, RealGh, RealGit, RealHttp, RealLifecycle, RealMail, RealRepo, FileSeen, FileStamp};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn usage() -> ExitCode {
    println!("gh-intake — ingest public GitHub issues as beads, with author triage.");
    println!();
    println!("  gh-intake [--dry-run]");
    println!("  gh-intake closeout <bead-id> <sha> <repo-path>");
    println!("  gh-intake unlanded-scan");
    println!("  gh-intake backfill [--dry-run]");
    println!("  gh-intake provenance [--check]");
    ExitCode::from(0)
}

/// SPIRA_HOME/SPIRA_RUN, neither a registered key: SPIRA_HOME names where to find
/// `$SPIRA_TOML` in the first place, and SPIRA_RUN's resolution (`resolve_run_dir`) also
/// judges the result against the instance's containment.
fn closeout_env() -> Result<(String, PathBuf), String> {
    let home = spira_config::resolve::locate_home_for_process()?;
    let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let run = spira_config::resolve::resolve_run_dir(&env, &home)?;
    Ok((home.to_string_lossy().into_owned(), run))
}

fn closeout_ctx(run: PathBuf) -> Result<Ctx, String> {
    Ok(Ctx {
        run,
        ask_label: spira_config::process::cfg("SPIRA_ASK_LABEL")?,
        grace_secs: spira_config::process::cfg_parse("SPIRA_GH_ASK_GRACE_SECS")?,
    })
}

fn closeout_cmd(args: &[String]) -> ExitCode {
    if args.len() != 3 {
        eprintln!("usage: gh-intake closeout <bead-id> <sha> <repo-path>");
        return ExitCode::from(2);
    }
    let (home, run) = match closeout_env() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("gh-intake: {e}");
            return ExitCode::from(1);
        }
    };
    let ctx = match closeout_ctx(run) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("gh-intake: {e}");
            return ExitCode::from(1);
        }
    };
    let (bd, gh, git, repo, mail) = match ports(home) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("gh-intake: {e}");
            return ExitCode::from(1);
        }
    };
    let lc = match RealLifecycle::new() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("gh-intake: {e}");
            return ExitCode::from(1);
        }
    };
    let d = Deps { bd: &bd, gh: &gh, git: &git, repo: &repo, mail: &mail, lc: &lc };
    let log = closeout::gh_issue_closeout(&d, &ctx, &args[0], &args[1], Path::new(&args[2]));
    for l in log {
        println!("{l}");
    }
    ExitCode::from(0)
}

fn unlanded_scan_cmd(args: &[String]) -> ExitCode {
    if !args.is_empty() {
        eprintln!("usage: gh-intake unlanded-scan");
        return ExitCode::from(2);
    }
    let (home, run) = match closeout_env() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("gh-intake: {e}");
            return ExitCode::from(1);
        }
    };
    let ctx = match closeout_ctx(run) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("gh-intake: {e}");
            return ExitCode::from(1);
        }
    };
    let (bd, gh, git, repo, mail) = match ports(home) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("gh-intake: {e}");
            return ExitCode::from(1);
        }
    };
    let lc = match RealLifecycle::new() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("gh-intake: {e}");
            return ExitCode::from(1);
        }
    };
    let d = Deps { bd: &bd, gh: &gh, git: &git, repo: &repo, mail: &mail, lc: &lc };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let log = closeout::gh_unlanded_scan(&d, &ctx, now);
    for l in log {
        println!("{l}");
    }
    ExitCode::from(0)
}

fn backfill_cmd(args: &[String]) -> ExitCode {
    let mut dry_run = false;
    for a in args {
        match a.as_str() {
            "--dry-run" => dry_run = true,
            "-h" | "--help" => {
                println!("gh-intake backfill [--dry-run]");
                return ExitCode::from(0);
            }
            other => {
                eprintln!("gh-intake backfill: unknown argument: {other}");
                return ExitCode::from(2);
            }
        }
    }
    let (home, run) = match closeout_env() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("gh-intake: {e}");
            return ExitCode::from(1);
        }
    };
    let ctx = match closeout_ctx(run) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("gh-intake: {e}");
            return ExitCode::from(1);
        }
    };
    let (bd, gh, git, repo, mail) = match ports(home) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("gh-intake: {e}");
            return ExitCode::from(1);
        }
    };
    let lc = match RealLifecycle::new() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("gh-intake: {e}");
            return ExitCode::from(1);
        }
    };
    let d = Deps { bd: &bd, gh: &gh, git: &git, repo: &repo, mail: &mail, lc: &lc };
    let (log, _found, _closed, _skipped, _dry) = closeout::backfill(&d, &ctx, dry_run);
    for l in log {
        println!("{l}");
    }
    ExitCode::from(0)
}


/// `gh-intake provenance` — every external issue, its bead, the outcome that settles it.
/// `--check` exits 1 when any trail is broken, 2 when the trail could not be read.
fn provenance_cmd(args: &[String]) -> ExitCode {
    let check = match closeout::provenance_mode(args) {
        Ok(check) => check,
        Err(e) => {
            eprintln!("gh-intake provenance: {e}");
            return ExitCode::from(2);
        }
    };
    let (home, _) = match closeout_env() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("gh-intake: {e}");
            return ExitCode::from(1);
        }
    };
    let (bd, gh, git, repo, mail) = match ports(home) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("gh-intake: {e}");
            return ExitCode::from(1);
        }
    };
    let lc = match RealLifecycle::new() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("gh-intake: {e}");
            return ExitCode::from(1);
        }
    };
    let d = Deps { bd: &bd, gh: &gh, git: &git, repo: &repo, mail: &mail, lc: &lc };
    match closeout::provenance_read(&d) {
        Ok(rows) => {
            for r in &rows {
                println!("{}", r.line());
            }
            ExitCode::from(if check && rows.iter().any(|r| r.is_broken()) { 1 } else { 0 })
        }
        Err(e) => {
            eprintln!("gh-intake provenance: {e}");
            ExitCode::from(2)
        }
    }
}

/// The fixture's stand-in mail program (`SPIRA_MAIL_BIN`, registered config); none (empty)
/// sends in-process.
fn mail_stand_in() -> Option<String> {
    spira_config::process::cfg("SPIRA_MAIL_BIN").ok().filter(|m| !m.is_empty())
}

#[allow(clippy::type_complexity)]
fn ports(home: String) -> Result<(RealBd, RealGh, RealGit, RealRepo, RealMail), String> {
    // SPIRA_MAIL_BIN is registered config, read where the intake pass reads it: one source.
    let mail = RealMail::new(mail_stand_in());
    let bdq = spira_config::release_env::tool("bdq")?;
    Ok((RealBd { bdq: bdq.clone() }, RealGh { bdq_bin: bdq }, RealGit, RealRepo { spira_home: home }, mail))
}

fn main() -> ExitCode {
    if let Err(e) = spira_config::release_env::ensure_release_path() {
        eprintln!("gh-intake: {e}");
        std::process::exit(2);
    }
    let op = match std::env::args().nth(1).as_deref() {
        Some("closeout") => "closeout",
        Some("unlanded-scan") => "unlanded-scan",
        Some("backfill") => "backfill",
        Some("provenance") => "provenance",
        _ => "intake",
    };
    let run = spira_otel::metrics::Run::begin(spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref(), "gh-intake", op);
    let code = dispatch();
    run.end(spira_otel::metrics::outcome_of_exit(code));
    code
}

fn dispatch() -> ExitCode {
    let all_args: Vec<String> = std::env::args().skip(1).collect();
    match all_args.first().map(String::as_str) {
        Some("closeout") => return closeout_cmd(&all_args[1..]),
        Some("unlanded-scan") => return unlanded_scan_cmd(&all_args[1..]),
        Some("backfill") => return backfill_cmd(&all_args[1..]),
        Some("provenance") => return provenance_cmd(&all_args[1..]),
        _ => {}
    }

    let mut dry_run = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "-h" | "--help" => return usage(),
            other => {
                eprintln!("gh-intake: unknown argument: {other}");
                return ExitCode::from(2);
            }
        }
    }

    let spira_home = match spira_config::resolve::locate_home_for_process().map(|home| home.to_string_lossy().into_owned()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("gh-intake: {e}");
            return ExitCode::from(1);
        }
    };
    macro_rules! cfg_or_refuse {
        ($key:expr) => {
            match spira_config::process::cfg($key) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("gh-intake: {e}");
                    return ExitCode::from(1);
                }
            }
        };
    }
    let stand_in = mail_stand_in();

    let repo = cfg_or_refuse!("SPIRA_GH_INTAKE_REPO");
    let scope = cfg_or_refuse!("SPIRA_SCOPE_LABEL");
    let lane = cfg_or_refuse!("SPIRA_PLAN_LABEL");
    let bead_repo = cfg_or_refuse!("SPIRA_GH_INTAKE_BEAD_REPO");
    let priority = cfg_or_refuse!("SPIRA_GH_INTAKE_PRIORITY");
    if !logic::valid_priority(&priority) {
        eprintln!("gh-intake: SPIRA_GH_INTAKE_PRIORITY is {priority} — it must be 0-4");
        return ExitCode::from(2);
    }
    let api = spira_config::process::cfg("SPIRA_GH_INTAKE_API").ok().filter(|a| !a.is_empty()).unwrap_or_else(|| logic::GITHUB_API.to_string());

    let cfg = Config {
        repo,
        bead_repo,
        scope,
        lane,
        priority,
        api,
        untrusted_label: logic::UNTRUSTED_LABEL.to_string(),
        dry_run,
    };

    let http = RealHttp::from_broker();
    let bd = match spira_config::release_env::tool("bdq") {
        Ok(bdq) => RealBd { bdq },
        Err(e) => {
            eprintln!("gh-intake: {e}");
            return ExitCode::from(1);
        }
    };
    let repo_port = RealRepo { spira_home };
    let mail = RealMail::new(stand_in);

    let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let run_dir = match spira_config::resolve::locate_home_for_process().and_then(|h| spira_config::resolve::resolve_run_dir(&env, &h)) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("gh-intake: {e}");
            return ExitCode::from(1);
        }
    };
    let stamp = FileStamp(run_dir.join("gh-intake.last-ok"));

    let seen = FileSeen(run_dir.join("gh-intake.seen"));
    let report = logic::run(&http, &bd, &repo_port, &mail, &stamp, &seen, &cfg);
    for line in &report.out {
        println!("{line}");
    }
    for line in &report.err {
        eprintln!("{line}");
    }
    ExitCode::from(report.code.clamp(0, 255) as u8)
}
