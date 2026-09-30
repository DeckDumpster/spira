//! sending — the Sending (DESIGN.md). Delete the branch and the worktree of every bead whose
//! work has landed, and nothing else.
//!
//!   sending                     one pass over every repository
//!   sending --dry-run           print each branch's disposition, change nothing
//!   sending <bead-id|branch>    send exactly one bead's branch and worktree
//!   sending --status-from <f>   read `id<TAB>status` from a file instead of bd (tests)
//!   sending --skip-queue        every repository EXCEPT queue/queue.local ones (the sentinel)
//!   sending --queue-only        ONLY queue/queue.local repositories (the straggler sweep)
//!   sending --no-fetch          judge against the base refs as they stand
//!
//! Exit 0 when nothing failed, 1 when a deletion failed, 2 on a usage or context error.

mod git;
mod ports;
mod real;
mod seam;
mod sweep;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use sweep::{Opts, Scope, Sweep};

/// SPIRA_HOME from the environment, else the first directory holding a lib.sh among the
/// release and cargo layouts around this executable (sentinel DESIGN.md §2.7).
pub fn locate_home(env_home: Option<&str>, exe: &Path) -> Option<PathBuf> {
    if let Some(h) = env_home.filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(h));
    }
    let dir = exe.parent()?;
    [dir.join("../spira"), dir.join("../../spira"), dir.join("../../../spira")]
        .into_iter()
        .find(|c| c.join("lib.sh").is_file())
        .map(|c| c.canonicalize().unwrap_or(c))
}

/// The command line; Err is the usage error to print.
pub fn parse(args: &[String]) -> Result<(Opts, Option<String>), String> {
    let mut o = Opts { dry: false, fetch: true, only: None, scope: Scope::All };
    let (mut skip, mut qonly, mut status) = (false, false, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dry-run" => o.dry = true,
            "--no-fetch" => o.fetch = false,
            "--status-from" => status = Some(it.next().cloned().ok_or("--status-from needs a file")?),
            "--skip-queue" => skip = true,
            "--queue-only" => qonly = true,
            f if f.starts_with('-') => return Err(format!("unknown flag: {f}")),
            id => o.only = Some(id.to_string()),
        }
    }
    o.scope = match (skip, qonly) {
        (true, true) => return Err("--skip-queue and --queue-only are mutually exclusive".into()),
        (true, false) => Scope::SkipQueue,
        (false, true) => Scope::QueueOnly,
        _ => Scope::All,
    };
    Ok((o, status))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (opts, status) = match parse(&args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("sending: {e}");
            std::process::exit(2);
        }
    };
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("sending"));
    let Some(home) = locate_home(std::env::var("SPIRA_HOME").ok().as_deref(), &exe) else {
        eprintln!("sending: cannot find lib.sh (set SPIRA_HOME)");
        std::process::exit(2);
    };
    let (w, repos) = match real::Real::new(home, status) {
        Ok(x) => x,
        Err(e) => {
            // FAIL CLOSED: a sweep that cannot read which repositories exist deletes nothing
            // and says so, rather than reporting a clean pass over none of them.
            eprintln!("sending: cannot read the harness context: {e}");
            std::process::exit(2);
        }
    };
    let label = w.submitted_label.clone();
    let mut s = Sweep { w: &w, opts, submitted_label: label, tally: Default::default() };
    std::process::exit(s.run(&repos));
}
