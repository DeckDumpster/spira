//! `gh-intake [--dry-run]` — see DESIGN.md. Ported from `spira/gh-intake.sh` (deleted,
//! sp-8fsql); same env vars, same argv shape, called by bare name on the release PATH.

use gh_intake::logic::{self, Config};
use gh_intake::real::{RealBd, RealHttp, RealMail, RealRepo};
use std::process::ExitCode;

fn usage() -> ExitCode {
    println!("gh-intake — ingest public GitHub issues as beads, with author triage.");
    println!();
    println!("  gh-intake [--dry-run]");
    ExitCode::from(0)
}

fn main() -> ExitCode {
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

    let db = match std::env::var("SPIRA_DB") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            eprintln!("gh-intake: SPIRA_DB is not set");
            return ExitCode::from(1);
        }
    };
    let bd_bin = std::env::var("SPIRA_BD").unwrap_or_else(|_| "bd".to_string());
    let mail_bin = std::env::var("SPIRA_MAIL_BIN").unwrap_or_else(|_| "mail.sh".to_string());
    let spira_home = std::env::var("SPIRA_HOME").unwrap_or_else(|_| ".".to_string());

    let repo = std::env::var("SPIRA_GH_INTAKE_REPO").unwrap_or_default();
    let scope = std::env::var("SPIRA_SCOPE_LABEL").unwrap_or_else(|_| "spira".to_string());
    let lane = std::env::var("SPIRA_PLAN_LABEL").unwrap_or_else(|_| "plan".to_string());
    let bead_repo = std::env::var("SPIRA_GH_INTAKE_BEAD_REPO").unwrap_or_else(|_| {
        repo.rsplit('/').next().unwrap_or("").to_string()
    });
    let priority = std::env::var("SPIRA_GH_INTAKE_PRIORITY").unwrap_or_else(|_| "1".to_string());
    if !logic::valid_priority(&priority) {
        eprintln!("gh-intake: SPIRA_GH_INTAKE_PRIORITY is {priority} — it must be 0-4");
        return ExitCode::from(2);
    }
    let api = std::env::var("SPIRA_GH_INTAKE_API").unwrap_or_else(|_| "https://api.github.com".to_string());

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
