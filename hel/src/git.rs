//! Every git question the Hel asks. Mostly read-only (`fetch` and `update-ref` of an
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
        self.ok(&["-c", &format!("attr.tree={base}"), "merge-tree", "--write-tree", base, br])
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
        let Some(merged) = self.out(&["-c", &format!("attr.tree={base}"), "merge-tree", "--write-tree", base, br]) else { return false };
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

    /// A commit on <base> itself names <id>: a squash merge carries the bead id on the landing
    /// ref, not on the branch's own commits.
    pub fn base_names(&self, base: &str, id: &str, window: u32) -> bool {
        let n = format!("-n{window}");
        let grep = format!("--grep={id}");
        self.out(&["log", "--format=%s", n.as_str(), "-F", grep.as_str(), base]).is_some_and(|s| {
            s.lines().any(|l| l.match_indices(id).any(|(i, _)| !l[i + id.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '.' || c == '-')))
        })
    }

    /// Every commit in `base..br` is the claim-time stacked-base merge, which carries no change
    /// of its own; false when there are none.
    pub fn only_stacked_base(&self, base: &str, br: &str) -> bool {
        let range = format!("{base}..{br}");
        self.out(&["log", "--format=%s", range.as_str()])
            .is_some_and(|s| !s.trim().is_empty() && s.lines().all(|l| l.starts_with("stacked base: merge ")))
    }

    /// `git cherry base br` has a `+` line: a commit unique to <br> with no patch-equivalent
    /// on <base>.
    pub fn cherry_unapplied(&self, base: &str, br: &str) -> bool {
        self.out(&["cherry", base, br]).is_some_and(|s| s.lines().any(|l| l.starts_with('+')))
    }

    /// A queue cut rewrites hashes, so `cherry` cannot see a landed commit. Landed by name:
    /// <base> holds commits naming <id> (within the last `window`) that between them touch
    /// every path <br> changes since the merge base, so no path of the branch's own is unlanded.
    pub fn landed_by_subject(&self, base: &str, br: &str, id: &str, window: u32) -> bool {
        let n = format!("-n{window}");
        let grep = format!("--grep={id}");
        let Some(log) = self.out(&["log", "--format=%H %s", n.as_str(), "-F", grep.as_str(), base]) else { return false };
        let named = |s: &str| s.match_indices(id).any(|(i, _)| !s[i + id.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '.' || c == '-'));
        let shas: Vec<&str> = log.lines().filter(|l| named(l.split_once(' ').map_or("", |x| x.1))).filter_map(|l| l.split(' ').next()).collect();
        if shas.is_empty() {
            return false;
        }
        let Some(mb) = self.out(&["merge-base", base, br]) else { return false };
        let Some(mut own) = self.out(&["diff", "--name-only", mb.trim(), br]).map(|s| s.lines().map(String::from).collect::<Vec<_>>()) else { return false };
        if own.is_empty() {
            return false;
        }
        for sha in shas {
            let Some(files) = self.out(&["show", "--name-only", "--format=", sha]) else { return false };
            own.retain(|p| !files.lines().any(|f| f == p));
        }
        own.is_empty()
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
        let out = spira_config::bounded::bounded("git")
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

    /// The `.lock` file git's `Unable to create '<path>.lock': File exists` names, when it is
    /// inside this repository's git dir (or common dir). Removed when it is at least
    /// `stale_secs` old and no process has it open; otherwise kept, with the reason. A holder
    /// is found by the lock's own open descriptor, not by "some git runs here": on a busy box
    /// a git always runs somewhere in the repo, and that test never lets a dead lock go.
    /// None for a path outside the git dirs, no lock named, or `stale_secs == 0`.
    pub fn recover_stale_lock(&self, err: &str, stale_secs: u64) -> Option<LockVerdict> {
        if stale_secs == 0 {
            return None;
        }
        let lock = lock_named_in(err)?;
        let lock = lock.parent().and_then(|d| std::fs::canonicalize(d).ok()).map(|d| d.join(lock.file_name().unwrap_or_default())).unwrap_or(lock);
        let inside = [self.out(&["rev-parse", "--absolute-git-dir"]), self.git_common_dir().map(|p| p.to_string_lossy().into_owned())]
            .into_iter()
            .flatten()
            .any(|d| lock.starts_with(Path::new(d.trim())));
        if !inside {
            return None;
        }
        let age = std::fs::metadata(&lock).ok()?.modified().ok()?.elapsed().ok()?.as_secs();
        if age < stale_secs {
            return Some(LockVerdict::Kept(format!("lock {} kept (age {age}s, under the {stale_secs}s threshold)", lock.display())));
        }
        if let Some(pid) = holder_confirmed(&lock) {
            return Some(LockVerdict::Kept(format!("lock {} kept (age {age}s, held by pid {pid})", lock.display())));
        }
        if let Err(e) = std::fs::remove_file(&lock) {
            return Some(LockVerdict::Kept(format!("lock {} kept (age {age}s, no holder, but removal failed: {e})", lock.display())));
        }
        Some(LockVerdict::Removed(format!("stale {} ({age}s, no holder) removed", lock.display())))
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
        // batch-job: git history or network operation, as long as the repository is large
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
        // batch-job: git history or network operation, as long as the repository is large
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

fn lock_named_in(err: &str) -> Option<PathBuf> {
    let rest = err.split("Unable to create '").nth(1)?;
    let path = rest.split('\'').next()?;
    path.ends_with(".lock").then(|| PathBuf::from(path))
}

/// What `recover_stale_lock` did with the lock a failed command named.
pub enum LockVerdict {
    Removed(String),
    Kept(String),
}

/// A holder still there after a few looks: a process forked from the one that made the lock
/// holds its descriptors until it execs, and is not a holder.
fn holder_confirmed(lock: &Path) -> Option<u32> {
    let mut holder = None;
    for _ in 0..5 {
        holder = holder_of(Path::new("/proc"), lock);
        holder?;
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    holder
}

/// A pid with `lock` open. A process whose descriptors cannot be read is a holder only if it
/// is a live git (another uid's `sleep` cannot be holding a git lock; its git could; a zombie
/// holds nothing).
fn holder_of(proc_root: &Path, lock: &Path) -> Option<u32> {
    let me = std::process::id();
    for p in std::fs::read_dir(proc_root).ok()?.flatten() {
        let Ok(pid) = p.file_name().to_string_lossy().parse::<u32>() else { continue };
        if pid == me {
            continue;
        }
        match std::fs::read_dir(p.path().join("fd")) {
            Ok(fds) => {
                if fds.flatten().any(|fd| std::fs::read_link(fd.path()).map(|t| t == lock).unwrap_or(false)) {
                    return Some(pid);
                }
            }
            Err(_) => {
                let comm = std::fs::read_to_string(p.path().join("comm")).unwrap_or_default();
                let exited = std::fs::read(p.path().join("cmdline")).map(|c| c.is_empty()).unwrap_or(true);
                if comm.trim().starts_with("git") && !exited {
                    return Some(pid);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn union_repo(tag: &str) -> testkit::TempDir {
        let d = testkit::TempDir::new(tag);
        let p = d.path();
        let g = |args: &[&str]| {
            let o = std::process::Command::new("git").arg("-C").arg(p).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        };
        let commit = |file: &str, body: &str| {
            std::fs::write(p.join(file), body).unwrap();
            g(&["add", "-A"]);
            g(&["commit", "-q", "-m", "c"]);
        };
        g(&["init", "-q", "-b", "stale"]);
        commit("keep", "x\n");
        g(&["checkout", "-q", "-b", "main"]);
        commit(".gitattributes", "f merge=union\n");
        commit("f", "base\n");
        g(&["checkout", "-q", "-b", "b", "main"]);
        commit("f", "base\nfrom b\n");
        g(&["checkout", "-q", "main"]);
        commit("f", "base\nfrom a\n");
        g(&["checkout", "-q", "stale"]);
        d
    }

    #[test]
    fn holder_of_names_the_pid_with_the_lock_open_and_judges_unreadable_ones_by_being_a_live_git() {
        let t = testkit::TempDir::new("hel-proc");
        let proc = t.path().join("proc");
        let lock = t.path().join("packed-refs.lock");
        let mk = |pid: &str, comm: &str, cmdline: &str, fd_target: Option<&Path>| {
            let d = proc.join(pid);
            std::fs::create_dir_all(d.join("fd")).unwrap();
            std::fs::write(d.join("comm"), comm).unwrap();
            std::fs::write(d.join("cmdline"), cmdline).unwrap();
            if let Some(t) = fd_target {
                std::os::unix::fs::symlink(t, d.join("fd/3")).unwrap();
            }
        };
        mk("10", "sleep\n", "sleep", Some(Path::new("/elsewhere")));
        assert_eq!(holder_of(&proc, &lock), None);
        mk("11", "sleep\n", "sleep", Some(&lock));
        assert_eq!(holder_of(&proc, &lock), Some(11));
        std::fs::remove_dir_all(proc.join("11")).unwrap();
        // fd dir unreadable: a path that is a file, not a directory
        let unreadable = |pid: &str, comm: &str, cmdline: &str| {
            let d = proc.join(pid);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("comm"), comm).unwrap();
            std::fs::write(d.join("cmdline"), cmdline).unwrap();
        };
        unreadable("12", "sleep\n", "sleep");
        unreadable("13", "git\n", "");
        assert_eq!(holder_of(&proc, &lock), None);
        unreadable("14", "git\n", "git");
        assert_eq!(holder_of(&proc, &lock), Some(14));
    }

    #[test]
    fn a_lock_reached_through_a_symlinked_repo_path_is_still_seen_held() {
        let t = testkit::TempDir::new("hel-symlink");
        let real = t.path().join("real");
        std::fs::create_dir_all(real.join(".git")).unwrap();
        std::os::unix::fs::symlink(&real, t.path().join("link")).unwrap();
        let g = |args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(&real).args(args).status().unwrap().success());
        std::fs::remove_dir_all(real.join(".git")).unwrap();
        g(&["init", "-q"]);
        let lock = real.join(".git/packed-refs.lock");
        let f = std::fs::File::create(&lock).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200)).unwrap();
        let mut live = std::process::Command::new("sleep").arg("30").stdin(std::fs::File::open(&lock).unwrap()).spawn().unwrap();
        let via_link = t.path().join("link/.git/packed-refs.lock");
        let err = format!("error: Unable to create '{}': File exists.", via_link.display());
        let v = Git(&t.path().join("link")).recover_stale_lock(&err, 600);
        let _ = live.kill();
        let _ = live.wait();
        assert!(matches!(v, Some(LockVerdict::Kept(_))) && lock.exists());
    }

    #[test]
    fn merges_clean_honours_the_base_refs_union_attributes_not_the_checkouts() {
        let d = union_repo("hel-union");
        assert!(Git(d.path()).merges_clean("main", "b"));
    }
}
