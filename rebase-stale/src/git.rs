//! A thin, honest wrapper over the `git` binary. Every call names the directory it runs in;
//! nothing here relies on the process's own cwd.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The result of one git invocation: exit success plus both streams.
#[derive(Debug)]
pub struct Run {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

fn finish(o: std::io::Result<Output>) -> Run {
    match o {
        Ok(o) => Run {
            ok: o.status.success(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        },
        Err(e) => Run {
            ok: false,
            stdout: String::new(),
            stderr: e.to_string(),
        },
    }
}

/// A git working directory (a checkout or a worktree) plus the identity used for any
/// commit the rebase writes.
#[derive(Clone, Debug)]
pub struct Git {
    pub dir: PathBuf,
    pub name: String,
    pub email: String,
}

impl Git {
    pub fn new(dir: impl Into<PathBuf>, name: &str, email: &str) -> Git {
        Git {
            dir: dir.into(),
            name: name.to_string(),
            email: email.to_string(),
        }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new("git");
        c.arg("-C")
            .arg(&self.dir)
            .arg("-c")
            .arg(format!("user.name={}", self.name))
            .arg("-c")
            .arg(format!("user.email={}", self.email))
            // Plain two-way markers: the resolvers parse `<<<<<<< / ======= / >>>>>>>` only,
            // whatever conflict style the host's own git config prefers.
            .arg("-c")
            .arg("merge.conflictStyle=merge")
            .arg("-c")
            .arg("rebase.autoStash=false")
            .arg("-c")
            .arg("commit.gpgSign=false")
            .env("GIT_EDITOR", "true")
            .env("EDITOR", "true")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null());
        c
    }

    pub fn run<I, S>(&self, args: I) -> Run
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        finish(self.cmd().args(args).output())
    }

    /// Like `run`, but `input` is written to the child's stdin and closed — the one shape
    /// `commit -F -` needs (law-commit-messages-via-stdin: a message containing backticks or
    /// `$( )` must never be executed by the quoting meant to quote it).
    pub fn run_stdin<I, S>(&self, args: I, input: &str) -> Run
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        use std::io::Write;
        use std::process::Stdio;
        let mut c = self.cmd();
        c.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let Ok(mut child) = c.spawn() else {
            return Run { ok: false, stdout: String::new(), stderr: "spawn failed".into() };
        };
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(input.as_bytes());
        }
        finish(child.wait_with_output())
    }

    /// `diff -z --name-only <a> <b>`, NUL-split (unlike `out`, which assumes line-oriented
    /// output and would corrupt a filename containing a newline).
    pub fn diff_name_only_z(&self, a: &str, b: &str) -> Vec<String> {
        let r = self.run(["diff", "-z", "--name-only", a, b]);
        if !r.ok {
            return Vec::new();
        }
        r.stdout.split('\0').filter(|s| !s.is_empty()).map(String::from).collect()
    }

    /// stdout trimmed, or None on failure.
    pub fn out<I, S>(&self, args: I) -> Option<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let r = self.run(args);
        r.ok.then(|| r.stdout.trim().to_string())
    }

    pub fn ok<I, S>(&self, args: I) -> bool
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        self.run(args).ok
    }

    pub fn rev(&self, r: &str) -> Option<String> {
        self.out(["rev-parse", "--verify", "-q", &format!("{r}^{{commit}}")])
    }

    pub fn is_ancestor(&self, a: &str, b: &str) -> bool {
        self.ok(["merge-base", "--is-ancestor", a, b])
    }

    /// Resolved `--git-path <p>` (absolute), for rebase-state detection in a worktree whose
    /// `.git` is a gitfile.
    pub fn git_path(&self, p: &str) -> Option<PathBuf> {
        let s = self.out(["rev-parse", "--git-path", p])?;
        let pb = PathBuf::from(s);
        Some(if pb.is_absolute() {
            pb
        } else {
            self.dir.join(pb)
        })
    }

    pub fn mid_rebase(&self) -> bool {
        ["rebase-merge", "rebase-apply"]
            .iter()
            .any(|p| self.git_path(p).map(|d| d.is_dir()).unwrap_or(false))
    }

    pub fn mid_merge(&self) -> bool {
        ["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD"]
            .iter()
            .any(|p| self.git_path(p).map(|d| d.exists()).unwrap_or(false))
    }

    pub fn unmerged_paths(&self) -> Vec<String> {
        self.out(["diff", "--name-only", "--diff-filter=U"])
            .map(|s| {
                s.lines()
                    .filter(|l| !l.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Contents of index stage `n` of `path`, or None when that stage does not exist.
    pub fn stage(&self, n: u8, path: &str) -> Option<String> {
        let r = self.run(["show", &format!(":{n}:{path}")]);
        r.ok.then_some(r.stdout)
    }

    /// `git status --porcelain` — None when it cannot be read at all.
    pub fn porcelain(&self) -> Option<String> {
        let r = self.run(["status", "--porcelain"]);
        r.ok.then_some(r.stdout)
    }
}

/// One entry of `git worktree list --porcelain`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub branch: Option<String>,
}

pub fn parse_worktree_list(porcelain: &str) -> Vec<WorktreeEntry> {
    let mut out = Vec::new();
    let mut cur: Option<WorktreeEntry> = None;
    for line in porcelain.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if let Some(e) = cur.take() {
                out.push(e);
            }
            cur = Some(WorktreeEntry {
                path: PathBuf::from(p),
                branch: None,
            });
        } else if let Some(b) = line.strip_prefix("branch ") {
            if let Some(e) = cur.as_mut() {
                e.branch = Some(b.to_string());
            }
        }
    }
    if let Some(e) = cur {
        out.push(e);
    }
    out
}

/// The registered worktree holding `refs/heads/<branch>`, if any.
pub fn worktree_of(repo: &Git, branch: &str) -> Option<PathBuf> {
    let want = format!("refs/heads/{branch}");
    let list = repo.out(["worktree", "list", "--porcelain"])?;
    parse_worktree_list(&list)
        .into_iter()
        .find(|e| e.branch.as_deref() == Some(want.as_str()))
        .map(|e| e.path)
}

pub fn path_is_under(p: &Path, root: &Path) -> bool {
    p.starts_with(root) && p != root
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_worktree_list() {
        let s = "worktree /r\nHEAD abc\nbranch refs/heads/main\n\nworktree /w/x\nHEAD def\ndetached\n\nworktree /w/y\nHEAD 123\nbranch refs/heads/spira/sp-1\n";
        let v = parse_worktree_list(s);
        assert_eq!(v.len(), 3);
        assert_eq!(v[1].branch, None);
        assert_eq!(v[2].branch.as_deref(), Some("refs/heads/spira/sp-1"));
        assert_eq!(v[2].path, PathBuf::from("/w/y"));
    }
}
