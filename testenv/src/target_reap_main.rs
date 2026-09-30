//! target-reap [--dry-run] [--worktrees DIR] — remove the target/ of every worktree whose bead
//! is closed (sp-z61hj; testenv::reap, spira-config/DESIGN-build-cache.md §2.4).
//!
//! Reads SPIRA_RUN (worktrees default to $SPIRA_RUN/worktree) and SPIRA_DB (the bead store
//! `bd -C` reads). Exit 0 on a completed pass, 1 when bd could not answer or a removal failed
//! (nothing guessed), 2 on usage or a missing variable.

use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use testenv::reap;

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
    let Some(db) = var("SPIRA_DB") else {
        eprintln!("target-reap: SPIRA_DB is empty — refusing to let bd auto-discover a store");
        return ExitCode::from(2);
    };
    // One bd call; an id bd does not know makes it exit non-zero while still printing the
    // others, so the answer is read from stdout and judged by its shape (reap::statuses).
    let show = |ids: &[String]| -> Result<String, String> {
        let o = Command::new("timeout")
            .arg("60")
            .arg("bd")
            .arg("-C")
            .arg(&db)
            .arg("show")
            .args(ids)
            .arg("--json")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run bd: {e}"))?;
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    };
    match reap::reap(&dir, dry, &show) {
        Ok(r) => {
            println!("{}", reap::describe(&r, dry));
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("target-reap: {e} — nothing removed on an answer it cannot read");
            ExitCode::from(1)
        }
    }
}
