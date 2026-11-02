//! hel as a library: the destruction chokepoint (DESIGN.md §5, "Decisions in Rust,
//! destruction in Rust") lives here so other binaries in this crate — and, in follow-up
//! beads, other crates — can call it in-process instead of shelling back into lib.sh.

use std::path::PathBuf;

pub mod telemetry;
pub mod git;
pub mod ports;
pub mod reap;
pub mod real;
pub mod seam;
pub mod stale;
pub mod sweep;
pub mod terminal;

#[cfg(test)]
mod tests;

use sweep::{Opts, Scope};

/// `SPIRA_HOME`, else `$SPIRA_RELEASE/spira` — declared, never searched for around this
/// executable. `None` when neither is set.
pub fn locate_home(env_home: Option<&str>, env_release: Option<&str>) -> Option<PathBuf> {
    match env_home.filter(|h| !h.is_empty()) {
        Some(h) => Some(PathBuf::from(h)),
        None => spira_config::release_env::release_root(env_release).ok().map(|r| r.join("spira")),
    }
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
            "--all" => {}
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
