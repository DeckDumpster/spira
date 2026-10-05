//! Every git question the Sending asks. Mostly read-only (`fetch` and `update-ref` of an
//! archive ref being the exceptions noted in the original header) plus, since the
//! destruction chokepoint moved here from lib.sh (sp-9envm), the mutations that chokepoint
//! itself makes: `worktree remove`, `branch -D` under `SPIRA_REF_SANCTIONED`, and the
//! worktree-prune/repair pair. Each answers what the lib.sh function of the same name
//! answered.

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

    /// <base> already holds every change <br> makes: an ancestor, or a merge whose tree is the
    /// base's own (the proof that survives a squash). Same answer as `spira-lc content-landed`.
    pub fn content_on_base(&self, br: &str, base: &str) -> bool {
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

    /// A commit in `base..br` whose subject names <id>: the branch did work for the bead.
    /// Without one an empty diff proves nothing — any branch's diff against the base can be
    /// empty.
    pub fn has_own_commit(&self, base: &str, br: &str, id: &str) -> bool {
        let range = format!("{base}..{br}");
        let grep = format!("--grep={id}");
        self.out(&["log", "--format=%s", "-F", grep.as_str(), range.as_str()]).is_some_and(|s| {
            s.lines().any(|l| l.match_indices(id).any(|(i, _)| !l[i + id.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '.' || c == '-')))
        })
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

    // ---- the destruction chokepoint's own mutations (sp-9envm) -------------------------

    /// `worktree remove --force <path>`. lib.sh's spira_destroy_worktree falls back to
    /// `rm -rf` plus a prune when this fails (a worktree whose registration is already
    /// broken); the caller does that fallback, not this method.
    pub fn worktree_remove_force(&self, path: &Path) -> bool {
        self.cmd(&["worktree", "remove", "--force", &path.to_string_lossy()]).status().map(|s| s.success()).unwrap_or(false)
    }

    /// `branch -D <name>` with `SPIRA_REF_SANCTIONED=1` set on this one command — the only
    /// site that may set it (the reference-transaction hook's sanctioned path). Returns the
    /// first line of combined output on failure (empty on success); the caller re-checks
    /// `branch_exists` itself exactly as lib.sh does, because `git branch -D` can exit
    /// non-zero yet still have removed the ref (or vice versa with a racing writer).
    pub fn branch_delete_sanctioned(&self, name: &str) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(self.0)
            .args(["branch", "-D", name])
            .env("SPIRA_REF_SANCTIONED", "1")
            .stdin(Stdio::null())
            .output();
        match out {
            Ok(o) if o.status.success() => String::new(),
            Ok(o) => {
                let mut s = String::from_utf8_lossy(&o.stderr).into_owned();
                if s.trim().is_empty() {
                    s = String::from_utf8_lossy(&o.stdout).into_owned();
                }
                s.lines().next().unwrap_or("").trim().to_string()
            }
            Err(e) => e.to_string(),
        }
    }

    /// `push -q <remote> --delete <branch>`.
    pub fn push_delete(&self, remote: &str, branch: &str) -> bool {
        self.ok(&["push", "-q", remote, "--delete", branch])
    }

    /// `rev-parse --git-common-dir`, absolute (lib.sh resolves a relative answer against
    /// the repo root itself).
    pub fn git_common_dir(&self) -> Option<PathBuf> {
        let s = self.out(&["rev-parse", "--git-common-dir"])?.trim().to_string();
        if s.is_empty() {
            return None;
        }
        let p = PathBuf::from(&s);
        Some(if p.is_absolute() { p } else { self.0.join(p) })
    }

    /// `worktree prune -n -v`'s STDERR (that command reports on stderr, not stdout; reading
    /// it any other way yields nothing and a guard fed an empty list approves everything).
    pub fn worktree_prune_dry(&self) -> String {
        Command::new("git")
            .arg("-C")
            .arg(self.0)
            .args(["worktree", "prune", "-n", "-v"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stderr).into_owned())
            .unwrap_or_default()
    }

    pub fn worktree_prune(&self) {
        let _ = self.cmd(&["worktree", "prune"]).status();
    }

    pub fn worktree_repair(&self, path: &Path) {
        let _ = Command::new("git")
            .arg("-C")
            .arg(self.0)
            .args(["worktree", "repair"])
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    /// `status --porcelain`; None on a tree whose status could not be read at all (a
    /// corrupted `.git` file), distinct from `Some("")` (clean) — salvage must fail closed
    /// on the former and no-op on the latter.
    pub fn status_porcelain(&self) -> Option<String> {
        self.out(&["status", "--porcelain"])
    }

    pub fn diff_head(&self) -> Option<String> {
        self.out(&["diff", "HEAD"])
    }

    /// `ls-files --others --exclude-standard -z`, raw NUL-separated bytes (untracked file
    /// names may themselves contain anything but NUL).
    pub fn ls_files_others_nul(&self) -> Vec<u8> {
        self.cmd(&["ls-files", "--others", "--exclude-standard", "-z"]).output().map(|o| o.stdout).unwrap_or_default()
    }
}
