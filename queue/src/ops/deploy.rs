//! land-local's deploy (DESIGN.md §8 D13): a landing of the harness repository publishes a
//! release. After the fast-forward, `release build <head> --bin-dir <the round's tested
//! build>` → `release verify <sha>` → `release activate <sha>`. Nothing here edits, builds in
//! or links into a checkout; the release crate is the only thing that makes, checks or
//! switches to `spira-releases/<sha>`.

use std::fs;
use std::path::{Path, PathBuf};

use super::World;

/// `<releases>/current` is a symlink: a release is in force, so a landing activates its own.
/// Absent: nothing runs a release yet (before the cutover), and the release step is skipped
/// (§8 D13) — the first activation is the cutover's.
pub fn release_in_force(releases: &Path) -> bool {
    fs::symlink_metadata(releases.join("current")).map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

/// What `current` names now (for the fault text: "current is untouched, still <x>").
pub fn current_name(releases: &Path) -> String {
    fs::read_link(releases.join("current")).map(|p| p.display().to_string()).unwrap_or_else(|_| "absent".into())
}

/// Everything the deploy needs, fixed before the CAS (every refusal is before it).
#[derive(Debug, Clone)]
pub struct Plan {
    /// The landing repository (`release build --repo`, `release activate --repo`).
    pub repo: PathBuf,
    /// The landing ref (`release activate --landed-ref`: the hotfix supersede rule).
    pub landref: String,
    /// `$SPIRA_RELEASES` (`--releases`).
    pub releases: PathBuf,
    /// `$SPIRA_RUN` (`--run`: history and the hotfix record).
    pub run: PathBuf,
    /// The round's own tested build of the head (`--bin-dir`, law-deploy-the-tested-artifacts).
    /// None only for rollback-local, which re-activates a release that already exists.
    pub bins: Option<PathBuf>,
    /// `$SPIRA_DB`, handed to every release child (verify's pre-activate store check).
    pub db: String,
}

/// How a deploy ended. `Fault` is loud and makes land-local exit 1; the landing itself is
/// already recorded and is never reverted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Activated(String),
    Fault(String),
}

impl Plan {
    fn common(&self) -> Vec<String> {
        vec!["--releases".into(), self.releases.display().to_string(), "--run".into(), self.run.display().to_string()]
    }
}

fn args(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// Run one `release` step; forward its stderr (release prefixes its own lines); Err is the
/// fault text for a non-zero exit.
fn step(w: &World, plan: &Plan, what: &str, mut a: Vec<String>) -> Result<String, String> {
    a.extend(plan.common());
    let r = w.scripts.release(&a, &plan.db);
    let err = r.err.trim_end_matches('\n');
    if !err.is_empty() {
        w.err(err);
    }
    if r.rc != 0 {
        let why = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no output").trim().to_string();
        return Err(format!("release {what} exited {}: {why}", r.rc));
    }
    Ok(r.out)
}

/// Build (from the tested binaries) and verify `head`'s release; the sha on success.
pub fn build_and_verify(w: &World, plan: &Plan, head: &str) -> Result<String, String> {
    let Some(bins) = &plan.bins else {
        return Err("no tested build to publish (--bin-dir)".into());
    };
    let out = step(
        w,
        plan,
        "build",
        args(&["build", head, "--repo", &plan.repo.display().to_string(), "--bin-dir", &bins.display().to_string()]),
    )?;
    let sha = out.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string();
    if sha != head {
        return Err(format!("release build answered {sha:?} for {head} — not the landed commit"));
    }
    step(w, plan, "verify", args(&["verify", &sha]))?;
    Ok(sha)
}

/// `release verify <sha>` alone (rollback-local: the release already exists; never rebuilt).
pub fn verify(w: &World, plan: &Plan, sha: &str) -> Result<(), String> {
    step(w, plan, "verify", args(&["verify", sha])).map(|_| ())
}

/// Switch the running system onto `sha` (`release activate`, which applies the hotfix
/// supersede rule against `--landed-ref`).
pub fn activate(w: &World, plan: &Plan, sha: &str) -> Result<(), String> {
    let out = step(
        w,
        plan,
        "activate",
        args(&["activate", sha, "--repo", &plan.repo.display().to_string(), "--landed-ref", &plan.landref]),
    )?;
    let out = out.trim_end_matches('\n');
    if !out.is_empty() {
        w.out(out);
    }
    Ok(())
}

/// D13 after the CAS: build → verify → activate.
pub fn run(w: &World, plan: &Plan, head: &str) -> Outcome {
    let sha = match build_and_verify(w, plan, head) {
        Ok(s) => s,
        Err(why) => return Outcome::Fault(why),
    };
    match activate(w, plan, &sha) {
        Ok(()) => Outcome::Activated(sha),
        Err(why) => Outcome::Fault(why),
    }
}
