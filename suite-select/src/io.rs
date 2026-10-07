//! Git reads, behind [`Git`] so the pipelines are testable, and the diff-mode entry point the
//! round's runner (`testenv`) calls. Every read that fails is a [`Refusal`], except the
//! `file#function` narrowing, which only ever removes suites and so falls back to keeping
//! them.

use crate::corpus::Corpus;
use crate::select::{self, Buckets, Change, Fail, Options, Selection};
use crate::{refuse, Refusal};
use std::path::{Path, PathBuf};
use std::process::Command;

pub trait Git {
    /// `git diff --name-status <base>...<head>`.
    fn diff_name_status(&self, repo: &Path, base: &str, head: &str) -> Result<String, Refusal>;
    /// `git diff --raw <base>...<head>`.
    fn diff_raw(&self, repo: &Path, base: &str, head: &str) -> Result<String, Refusal>;
    /// `git diff --unified=0 <base>...<head> -- <file>`; `None` on failure.
    fn diff_u0(&self, repo: &Path, base: &str, head: &str, file: &str) -> Option<String>;
    /// `git show <rev>:<file>`; `None` on failure.
    fn show(&self, repo: &Path, rev: &str, file: &str) -> Option<String>;
    /// `git ls-tree -r --name-only <rev>`.
    fn ls_tree(&self, repo: &Path, rev: &str) -> Result<Vec<String>, Refusal>;
    /// `git ls-files`.
    fn ls_files(&self, repo: &Path) -> Result<Vec<String>, Refusal>;
}

pub struct RealGit;

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let o = Command::new("timeout").arg("5").arg("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !o.status.success() {
        let err = String::from_utf8_lossy(&o.stderr);
        return Err(format!(
            "git -C {} {} failed ({}): {}",
            repo.display(),
            args.join(" "),
            o.status,
            err.lines().next().unwrap_or("")
        ));
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

impl Git for RealGit {
    fn diff_name_status(&self, repo: &Path, base: &str, head: &str) -> Result<String, Refusal> {
        git(repo, &["diff", "--name-status", &format!("{base}...{head}")]).map_err(Refusal)
    }
    fn diff_raw(&self, repo: &Path, base: &str, head: &str) -> Result<String, Refusal> {
        git(repo, &["diff", "--raw", &format!("{base}...{head}")]).map_err(Refusal)
    }
    fn diff_u0(&self, repo: &Path, base: &str, head: &str, file: &str) -> Option<String> {
        git(repo, &["diff", "--unified=0", &format!("{base}...{head}"), "--", file]).ok()
    }
    fn show(&self, repo: &Path, rev: &str, file: &str) -> Option<String> {
        git(repo, &["show", &format!("{rev}:{file}")]).ok()
    }
    fn ls_tree(&self, repo: &Path, rev: &str) -> Result<Vec<String>, Refusal> {
        git(repo, &["ls-tree", "-r", "--name-only", rev])
            .map(|s| s.lines().map(str::to_string).collect())
            .map_err(Refusal)
    }
    fn ls_files(&self, repo: &Path) -> Result<Vec<String>, Refusal> {
        git(repo, &["ls-files"])
            .map(|s| s.lines().map(str::to_string).collect())
            .map_err(Refusal)
    }
}

/// The changes `<base>...<head>`: name-status for adds and deletes, `--raw` for mode changes.
pub fn diff_changes(g: &dyn Git, repo: &Path, base: &str, head: &str) -> Result<Vec<Change>, Refusal> {
    let mut changes = select::parse_name_status(&g.diff_name_status(repo, base, head)?);
    let modes = select::parse_mode_changes(&g.diff_raw(repo, base, head)?);
    for c in changes.iter_mut() {
        c.mode_changed = modes.iter().any(|m| *m == c.path);
    }
    Ok(changes)
}

/// Read a `--files` list. An unreadable file is a refusal.
pub fn file_changes(path: &Path) -> Result<Vec<Change>, Refusal> {
    match std::fs::read(path) {
        Ok(b) => Ok(select::parse_name_status(&String::from_utf8_lossy(&b))),
        Err(e) => refuse(format!("cannot read the changed-file list {}: {e}", path.display())),
    }
}

/// Selection over `<base>...<head>` in `repo`, with `file#function` narrowing — the ONE
/// diff-mode entry point (the binary's `select --base --head`, and `testenv`).
pub fn select_diff(
    g: &dyn Git,
    repo: &Path,
    corpus: &Corpus,
    base: &str,
    head: &str,
    buckets: &Buckets,
    opts: &Options,
) -> Result<Selection, Fail> {
    let changes = diff_changes(g, repo, base, head)?;
    let mut fns = |file: &str| -> Vec<String> {
        match (g.diff_u0(repo, base, head, file), g.show(repo, head, file)) {
            (Some(d), Some(t)) => select::changed_functions(&d, &t),
            _ => vec![],
        }
    };
    select::select(corpus, &changes, &mut fns, buckets, opts)
}

/// The report file's `unclaimed:` half needs the tracked files.
pub fn report(g: &dyn Git, repo: &Path, corpus: &Corpus, unplaced: &[String]) -> Result<String, Refusal> {
    let tracked = g.ls_files(repo)?;
    Ok(select::report_text(unplaced, &select::unclaimed_tracked(corpus, &tracked)))
}

/// The directory's parent, absolute — `select.sh`'s default repository.
pub fn parent_of(dir: &Path) -> PathBuf {
    let abs = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    abs.parent().map(Path::to_path_buf).unwrap_or(abs)
}
