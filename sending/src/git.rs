//! Every git question the Sending asks, read-only except `fetch` and `update-ref` of an
//! archive ref. Each answers what the lib.sh function of the same name answered.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Git<'a>(pub &'a Path);

impl Git<'_> {
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new("git");
        c.arg("-C").arg(self.0).args(args).stdin(Stdio::null()).stderr(Stdio::null());
        c
    }
    /// stdout on success.
    pub fn out(&self, args: &[&str]) -> Option<String> {
        let o = self.cmd(args).output().ok()?;
        o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
    }
    pub fn ok(&self, args: &[&str]) -> bool {
        self.cmd(args).stdout(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
    }

    /// `refs/heads/spira/*`, short names, in git's own order.
    pub fn spira_branches(&self) -> Vec<String> {
        self.out(&["for-each-ref", "--format=%(refname:short)", "refs/heads/spira/*"])
            .map(|s| s.lines().filter(|l| !l.is_empty()).map(String::from).collect())
            .unwrap_or_default()
    }
    /// `rev-list --count base..br`; None is the shell's `?`.
    pub fn ahead(&self, base: &str, br: &str) -> Option<u64> {
        self.out(&["rev-list", "--count", &format!("{base}..{br}")]).and_then(|s| s.trim().parse().ok())
    }
    pub fn is_ancestor(&self, a: &str, b: &str) -> bool {
        self.ok(&["merge-base", "--is-ancestor", a, b])
    }
    pub fn rev_parse(&self, r: &str) -> Option<String> {
        self.out(&["rev-parse", r]).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }
    pub fn verify(&self, r: &str) -> Option<String> {
        self.out(&["rev-parse", "-q", "--verify", r]).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }
    pub fn branch_exists(&self, br: &str) -> bool {
        self.ok(&["show-ref", "--verify", "-q", &format!("refs/heads/{br}")])
    }
    /// merge-tree --write-tree exits 0: the merge is clean (no conflict).
    pub fn merges_clean(&self, base: &str, br: &str) -> bool {
        self.ok(&["merge-tree", "--write-tree", base, br])
    }

    /// lib.sh content_landed: <base> already holds every change <br> makes.
    pub fn content_landed(&self, br: &str, base: &str) -> bool {
        let Some(ahead) = self.ahead(base, br) else { return false };
        if self.is_ancestor(br, base) {
            return true;
        }
        if ahead == 0 {
            return false;
        }
        let Some(merged) = self.out(&["merge-tree", "--write-tree", base, br]) else { return false };
        let merged = merged.lines().next().unwrap_or("").to_string();
        !merged.is_empty() && self.rev_parse(&format!("{base}^{{tree}}")).is_some_and(|t| t == merged)
    }

    /// lib.sh landed: a LANDING RECORD on the land refs names <id> — the queue's own merge
    /// subject (`spira: land <id>`, optionally ` — <title>`) or the bead's own commit
    /// (`<id>:`). --grep only narrows; the subject is what is trusted.
    pub fn landed(&self, id: &str, refs: &[String]) -> bool {
        if refs.is_empty() {
            return false;
        }
        let grep = format!("--grep={id}");
        let mut args = vec!["log", "--format=%s", grep.as_str(), "-F"];
        args.extend(refs.iter().map(String::as_str));
        let Some(out) = self.out(&args) else { return false };
        let (land, own) = (format!("spira: land {id}"), format!("{id}:"));
        out.lines().any(|s| s == land || s.starts_with(&format!("{land} ")) || s.starts_with(&own))
    }

    /// `git cherry base br` has a `+` line: a commit unique to <br> with no patch-equivalent
    /// on <base>.
    pub fn cherry_unapplied(&self, base: &str, br: &str) -> bool {
        self.out(&["cherry", base, br]).is_some_and(|s| s.lines().any(|l| l.starts_with('+')))
    }

    /// The first five paths `diff --name-only base br` names.
    pub fn diff_names(&self, base: &str, br: &str) -> String {
        self.out(&["diff", "--name-only", base, br])
            .map(|s| s.lines().take(5).collect::<Vec<_>>().join("\n"))
            .unwrap_or_default()
    }

    /// `worktree list --porcelain` as (path, branch-if-any) pairs.
    pub fn worktrees(&self) -> Vec<(PathBuf, Option<String>)> {
        let Some(out) = self.out(&["worktree", "list", "--porcelain"]) else { return Vec::new() };
        let mut v: Vec<(PathBuf, Option<String>)> = Vec::new();
        for line in out.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                v.push((PathBuf::from(p), None));
            } else if let Some(b) = line.strip_prefix("branch ") {
                if let Some(last) = v.last_mut() {
                    last.1 = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
                }
            }
        }
        v
    }
    /// lib.sh worktree_of: the registered worktree holding <br>.
    pub fn worktree_of(&self, br: &str) -> Option<PathBuf> {
        self.worktrees().into_iter().find(|(_, b)| b.as_deref() == Some(br)).map(|(p, _)| p)
    }

    pub fn fetch(&self, remote: &str) {
        let _ = self.ok(&["fetch", "-q", remote]);
    }
    pub fn update_ref(&self, r: &str, sha: &str) -> bool {
        self.ok(&["update-ref", r, sha])
    }
}
