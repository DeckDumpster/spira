//! sending as a library: the destruction chokepoint (DESIGN.md §5, "Decisions in Rust,
//! destruction in Rust") lives here so other binaries in this crate — and, in follow-up
//! beads, other crates — can call it in-process instead of shelling back into lib.sh.

use std::path::{Path, PathBuf};

pub mod git;
pub mod ports;
pub mod reap;
pub mod real;
pub mod seam;
pub mod sweep;

#[cfg(test)]
mod tests;

use sweep::{Opts, Scope};

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

/// The sweep's own command line; Err is the usage error to print.
pub fn parse(args: &[String]) -> Result<(Opts, Option<String>), String> {
    let mut o = Opts { dry: false, fetch: true, only: None, scope: Scope::All, budget: None };
    let (mut skip, mut qonly, mut status) = (false, false, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dry-run" => o.dry = true,
            "--no-fetch" => o.fetch = false,
            "--status-from" => status = Some(it.next().cloned().ok_or("--status-from needs a file")?),
            "--budget-secs" => {
                let n = it.next().and_then(|n| n.parse::<u64>().ok()).ok_or("--budget-secs needs a whole number of seconds")?;
                o.budget = Some(std::time::Duration::from_secs(n));
            }
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
