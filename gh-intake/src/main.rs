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
use gh_intake::real::{RealBd, RealGh, RealGit, RealHttp, RealLifecycle, RealMail, RealRepo};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn usage() -> ExitCode {
    println!("gh-intake — ingest public GitHub issues as beads, with author triage.");
    println!();
    println!("  gh-intake [--dry-run]");
    println!("  gh-intake closeout <bead-id> <sha> <repo-path>");
    println!("  gh-intake unlanded-scan");
    println!("  gh-intake backfill [--dry-run]");
    ExitCode::from(0)
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// SPIRA_DB/SPIRA_BD/SPIRA_HOME/SPIRA_RUN — every closeout-side verb needs all four.
fn closeout_env() -> Result<(String, String, String, PathBuf), String> {
    let home = spira_config::resolve::locate_home_for_process()?;
    let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let db = spira_config::resolve::resolve_key(&env, &home, "SPIRA_DB")?;
    let bd_bin = env_nonempty("SPIRA_BD").unwrap_or_else(|| "bd".to_string());
    let run = spira_config::resolve::resolve_run_dir(&env, &home)?;
    Ok((db, bd_bin, home.to_string_lossy().into_owned(), run))
}

fn closeout_ctx(run: PathBuf) -> Ctx {
    Ctx {
        run,
        ask_label: env_nonempty("SPIRA_ASK_LABEL").unwrap_or_default(),
        grace_secs: std::env::var("SPIRA_GH_ASK_GRACE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(3600),
    }
}

fn closeout_cmd(args: &[String]) -> ExitCode {
    if args.len() != 3 {
        eprintln!("usage: gh-intake closeout <bead-id> <sha> <repo-path>");
        return ExitCode::from(2);
    }
    let (db, bd_bin, home, run) = match closeout_env() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("gh-intake: {e}");
            return ExitCode::from(1);
        }
    };
    let (bd, gh, git, repo, mail) = ports(db, bd_bin, home);
    let lc = RealLifecycle::default();
    let d = Deps { bd: &bd, gh: &gh, git: &git, repo: &repo, mail: &mail, lc: &lc };
    let log = closeout::gh_issue_closeout(&d, &closeout_ctx(run), &args[0], &args[1], Path::new(&args[2]));
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
    let (db, bd_bin, home, run) = match closeout_env() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("gh-intake: {e}");
            return ExitCode::from(1);
        }
    };
    let (bd, gh, git, repo, mail) = ports(db, bd_bin, home);
    let lc = RealLifecycle::default();
    let d = Deps { bd: &bd, gh: &gh, git: &git, repo: &repo, mail: &mail, lc: &lc };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let log = closeout::gh_unlanded_scan(&d, &closeout_ctx(run), now);
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
    let (db, bd_bin, home, run) = match closeout_env() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("gh-intake: {e}");
            return ExitCode::from(1);
        }
    };
    let (bd, gh, git, repo, mail) = ports(db, bd_bin, home);
    let lc = RealLifecycle::default();
    let d = Deps { bd: &bd, gh: &gh, git: &git, repo: &repo, mail: &mail, lc: &lc };
    let (log, _found, _closed, _skipped, _dry) = closeout::backfill(&d, &closeout_ctx(run), dry_run);
    for l in log {
        println!("{l}");
    }
    ExitCode::from(0)
}

#[allow(clippy::type_complexity)]
fn ports(db: String, bd_bin: String, home: String) -> (RealBd, RealGh, RealGit, RealRepo, RealMail) {
    let mail_bin = env_nonempty("SPIRA_MAIL_BIN").unwrap_or_else(|| "mail".to_string());
    (
        RealBd { bd_bin, db },
        RealGh { bdq_bin: "bdq".to_string() },
        RealGit,
        RealRepo { spira_home: home },
        RealMail { mail_bin },
    )
}

fn main() -> ExitCode {
    let all_args: Vec<String> = std::env::args().skip(1).collect();
    match all_args.first().map(String::as_str) {
        Some("closeout") => return closeout_cmd(&all_args[1..]),
        Some("unlanded-scan") => return unlanded_scan_cmd(&all_args[1..]),
        Some("backfill") => return backfill_cmd(&all_args[1..]),
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

    let (db, spira_home) = match spira_config::resolve::locate_home_for_process().and_then(|home| {
        let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
        let db = spira_config::resolve::resolve_key(&env, &home, "SPIRA_DB")?;
        Ok((db, home.to_string_lossy().into_owned()))
    }) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("gh-intake: {e}");
            return ExitCode::from(1);
        }
    };
    let bd_bin = std::env::var("SPIRA_BD").unwrap_or_else(|_| "bd".to_string());
    let mail_bin = std::env::var("SPIRA_MAIL_BIN").unwrap_or_else(|_| "mail".to_string());

    let repo = spira_config::resolve::key_for_process("SPIRA_GH_INTAKE_REPO").unwrap_or_default();
    let scope = std::env::var("SPIRA_SCOPE_LABEL").unwrap_or_else(|_| "spira".to_string());
    let lane = std::env::var("SPIRA_PLAN_LABEL").unwrap_or_else(|_| "plan".to_string());
    let bead_repo = spira_config::resolve::key_for_process("SPIRA_GH_INTAKE_BEAD_REPO")
        .unwrap_or_else(|_| repo.rsplit('/').next().unwrap_or("").to_string());
    let priority = std::env::var("SPIRA_GH_INTAKE_PRIORITY").unwrap_or_else(|_| "1".to_string());
    if !logic::valid_priority(&priority) {
        eprintln!("gh-intake: SPIRA_GH_INTAKE_PRIORITY is {priority} — it must be 0-4");
        return ExitCode::from(2);
    }
    let api = std::env::var("SPIRA_GH_INTAKE_API").unwrap_or_else(|_| logic::GITHUB_API.to_string());

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

    let http = RealHttp;
    let bd = RealBd { bd_bin, db: db.clone() };
    let repo_port = RealRepo { spira_home };
    let mail = RealMail { mail_bin };

    let report = logic::run(&http, &bd, &repo_port, &mail, &cfg);
    for line in &report.out {
        println!("{line}");
    }
    for line in &report.err {
        eprintln!("{line}");
    }
    ExitCode::from(report.code.clamp(0, 255) as u8)
}
