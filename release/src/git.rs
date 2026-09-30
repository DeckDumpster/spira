//! The git operations the release producer needs, behind a trait so activation's hotfix
//! rule is tested without a repository.

use std::path::Path;
use std::process::{Command, Stdio};

pub trait Git {
    /// `<rev>` in `repo` as a full sha.
    fn resolve(&self, repo: &Path, rev: &str) -> Result<String, String>;
    /// Extract the tree of `sha` into the existing, empty directory `into`.
    fn archive(&self, repo: &Path, sha: &str, into: &Path) -> Result<(), String>;
    /// Whether `ancestor` is an ancestor of (or equal to) `of`.
    fn is_ancestor(&self, repo: &Path, ancestor: &str, of: &str) -> Result<bool, String>;
}

pub struct RealGit;

impl Git for RealGit {
    fn resolve(&self, repo: &Path, rev: &str) -> Result<String, String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("{rev}^{{commit}}"))
            .output()
            .map_err(|e| format!("cannot run git: {e}"))?;
        let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !out.status.success() || !crate::is_sha(&sha) {
            return Err(format!("{rev} is not a commit in {}", repo.display()));
        }
        Ok(sha)
    }

    fn archive(&self, repo: &Path, sha: &str, into: &Path) -> Result<(), String> {
        let mut git = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["archive", "--format=tar", sha])
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run git archive: {e}"))?;
        let stdout = git.stdout.take().ok_or("git archive: no stdout")?;
        let tar = Command::new("tar").arg("-x").arg("-C").arg(into).stdin(stdout).status();
        let g = git.wait().map_err(|e| format!("git archive: {e}"))?;
        let t = tar.map_err(|e| format!("cannot run tar: {e}"))?;
        if !g.success() {
            return Err(format!("git archive {sha} failed ({g})"));
        }
        if !t.success() {
            return Err(format!("tar -x of {sha} failed ({t})"));
        }
        Ok(())
    }

    fn is_ancestor(&self, repo: &Path, ancestor: &str, of: &str) -> Result<bool, String> {
        let st = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["merge-base", "--is-ancestor", ancestor, of])
            .status()
            .map_err(|e| format!("cannot run git: {e}"))?;
        match st.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(format!("git merge-base --is-ancestor {ancestor} {of} failed in {} ({st})", repo.display())),
        }
    }
}
