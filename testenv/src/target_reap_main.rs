//! target-reap [--dry-run] [--worktrees DIR] — remove the target/ of every worktree whose
//! branch has landed (sp-z61hj, then sp-x9kbg; testenv::reap, testenv::landed,
//! spira-config/DESIGN-build-cache.md §2.4).
//!
//! Reads SPIRA_RUN (worktrees default to $SPIRA_RUN/worktree) and SPIRA_HOME (the harness
//! this binary resolves its own repo registry against — law-a-binary-resolves-the-config-
//! it-reads: no shim onto a `bd` subprocess, no bead status consulted at all). Exit 0 on a
//! completed pass (even one that reaped nothing), 1 when a removal failed (nothing else is
//! fatal — a worktree whose landed-ness cannot be told is simply kept), 2 on usage or a
//! missing variable.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use testenv::{landed, reap};

fn main() -> ExitCode {
    let mut dry = false;
    let mut dir: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dry-run" => dry = true,
            "--worktrees" => dir = args.next().map(PathBuf::from),
            _ => {
                eprintln!("usage: target-reap [--dry-run] [--worktrees DIR]");
                return ExitCode::from(2);
            }
        }
    }
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let dir = match dir.or_else(|| var("SPIRA_RUN").map(|r| PathBuf::from(r).join("worktree"))) {
        Some(d) => d,
        None => {
            eprintln!("target-reap: SPIRA_RUN is unset and no --worktrees given — refusing to guess");
            return ExitCode::from(2);
        }
    };
    let Some(home) = var("SPIRA_HOME") else {
        eprintln!("target-reap: SPIRA_HOME is empty — refusing to let the repo registry auto-discover");
        return ExitCode::from(2);
    };
    let home = PathBuf::from(home);
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let reg = spira_config::repos::Registry::from_env(env, &home);
    let landed_fn = |wt: &std::path::Path| landed::landed(&reg, wt);

    match reap::reap(&dir, dry, &landed_fn) {
        Ok(r) => {
            println!("{}", reap::describe(&r, dry));
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("target-reap: {e}");
            ExitCode::from(1)
        }
    }
}
