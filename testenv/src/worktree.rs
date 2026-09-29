//! Where the tree under test is built and mounted (DESIGN.md §4.1): the caller's own
//! worktree when it already holds exactly that commit, clean; else a warm scratch slot;
//! else a throwaway worktree. The caller's worktree is never removed.

use crate::util::git;
use std::fs::{self, File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub head: String,
    /// `refs/heads/<name>` when a branch is checked out; None when detached.
    pub branch: Option<String>,
}

/// `git worktree list --porcelain`.
pub fn parse_list(text: &str) -> Vec<WorktreeEntry> {
    let mut out = Vec::new();
    let mut cur: Option<WorktreeEntry> = None;
    for line in text.lines().chain(std::iter::once("")) {
        if let Some(p) = line.strip_prefix("worktree ") {
            if let Some(e) = cur.take() {
                out.push(e);
            }
            cur = Some(WorktreeEntry {
                path: PathBuf::from(p),
                head: String::new(),
                branch: None,
            });
        } else if let Some(h) = line.strip_prefix("HEAD ") {
            if let Some(e) = cur.as_mut() {
                e.head = h.to_string();
            }
        } else if let Some(b) = line.strip_prefix("branch ") {
            if let Some(e) = cur.as_mut() {
                e.branch = Some(b.to_string());
            }
        } else if line.is_empty() {
            if let Some(e) = cur.take() {
                out.push(e);
            }
        }
    }
    out
}

/// Candidates for building in place, best first: the worktree containing `cwd`, then the
/// one with `rev` checked out as a branch — each only if its HEAD is `commit`.
pub fn in_place_candidates(
    entries: &[WorktreeEntry],
    cwd: &Path,
    rev: &str,
    commit: &str,
) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let containing = entries
        .iter()
        .filter(|e| cwd.starts_with(&e.path))
        .max_by_key(|e| e.path.as_os_str().len());
    let want_branch = format!(
        "refs/heads/{}",
        rev.strip_prefix("refs/heads/").unwrap_or(rev)
    );
    let by_branch = entries
        .iter()
        .find(|e| e.branch.as_deref() == Some(want_branch.as_str()));
    for e in [containing, by_branch].into_iter().flatten() {
        if e.head == commit && !out.contains(&e.path) {
            out.push(e.path.clone());
        }
    }
    out
}

#[derive(Debug)]
pub enum Kind {
    InPlace,
    Slot { _lock: File },
    Ephemeral,
}

#[derive(Debug)]
pub struct Worktree {
    pub path: PathBuf,
    pub kind: Kind,
    repo: PathBuf,
}

impl Worktree {
    pub fn describe(&self) -> &'static str {
        match self.kind {
            Kind::InPlace => "in place",
            Kind::Slot { .. } => "scratch slot",
            Kind::Ephemeral => "throwaway worktree",
        }
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        if let Kind::Ephemeral = self.kind {
            let p = self.path.display().to_string();
            let _ = git(&self.repo, &["worktree", "remove", "-f", &p]);
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn is_clean(wt: &Path) -> bool {
    matches!(git(wt, &["status", "--porcelain"]), Ok(s) if s.is_empty())
}

fn try_lock(path: &Path) -> Option<File> {
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .ok()?;
    // SAFETY: flock on a descriptor we own; released when the File is dropped.
    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    (rc == 0).then_some(f)
}

fn head_of(wt: &Path) -> Option<String> {
    git(wt, &["rev-parse", "--verify", "-q", "HEAD"]).ok()
}

/// Point an existing slot at `commit`, or create it. Checkout rewrites only the files that
/// differ, so cargo's fingerprints keep `target/` warm; everything else untracked or ignored
/// is removed, so the slot is the commit and nothing else.
fn prepare_slot(repo: &Path, slot: &Path, commit: &str) -> Result<(), String> {
    let registered = slot.join(".git").exists() && head_of(slot).is_some();
    if !registered {
        let _ = git(repo, &["worktree", "prune"]);
        let _ = fs::remove_dir_all(slot);
        let p = slot.display().to_string();
        git(repo, &["worktree", "add", "-q", "--detach", &p, commit])?;
        return Ok(());
    }
    git(slot, &["checkout", "-q", "--detach", "--force", commit])?;
    git(slot, &["clean", "-ffdxq", "-e", "/target"])?;
    match head_of(slot) {
        Some(h) if h == commit => Ok(()),
        other => Err(format!(
            "slot {} is at {other:?} after checkout of {commit}",
            slot.display()
        )),
    }
}

pub struct Request<'a> {
    pub repo: &'a Path,
    pub rev: &'a str,
    pub commit: &'a str,
    pub cwd: &'a Path,
    pub run_dir: &'a Path,
    pub slots: usize,
}

pub fn acquire(req: &Request, log: &dyn Fn(&str)) -> Result<Worktree, String> {
    let list = git(req.repo, &["worktree", "list", "--porcelain"]).unwrap_or_default();
    let entries = parse_list(&list);
    let cwd = fs::canonicalize(req.cwd).unwrap_or_else(|_| req.cwd.to_path_buf());
    for cand in in_place_candidates(&entries, &cwd, req.rev, req.commit) {
        if is_clean(&cand) {
            return Ok(Worktree {
                path: cand,
                kind: Kind::InPlace,
                repo: req.repo.to_path_buf(),
            });
        }
        log(&format!(
            "{} holds {} but has uncommitted changes — not building there",
            cand.display(),
            req.rev
        ));
    }
    let base = req.run_dir.join("worktree");
    fs::create_dir_all(&base)
        .map_err(|e| format!("cannot create worktree directory {}: {e}", base.display()))?;
    for i in 0..req.slots {
        let slot = base.join(format!(".testenv-slot-{i}"));
        let Some(lock) = try_lock(&base.join(format!(".testenv-slot-{i}.lock"))) else {
            continue;
        };
        match prepare_slot(req.repo, &slot, req.commit) {
            Ok(()) => {
                return Ok(Worktree {
                    path: slot,
                    kind: Kind::Slot { _lock: lock },
                    repo: req.repo.to_path_buf(),
                })
            }
            Err(e) => log(&format!(
                "scratch slot {i} unusable ({e}) — trying the next"
            )),
        }
    }
    let eph = base.join(format!(".testenv-{}", std::process::id()));
    let _ = git(req.repo, &["worktree", "prune"]);
    let _ = fs::remove_dir_all(&eph);
    let p = eph.display().to_string();
    git(
        req.repo,
        &["worktree", "add", "-q", "--detach", &p, req.commit],
    )
    .map_err(|e| {
        format!(
            "cannot create worktree for {} in {}: {e}",
            req.rev,
            req.repo.display()
        )
    })?;
    Ok(Worktree {
        path: eph,
        kind: Kind::Ephemeral,
        repo: req.repo.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    const LIST: &str = "worktree /srv/harness\nHEAD aaa\nbranch refs/heads/main\n\nworktree /run/worktree/sp-x\nHEAD bbb\nbranch refs/heads/spira/sp-x\n\nworktree /run/worktree/round-5\nHEAD ccc\ndetached\n";

    #[test]
    fn porcelain_parses_detached_and_branches() {
        let e = parse_list(LIST);
        assert_eq!(e.len(), 3);
        assert_eq!(e[1].branch.as_deref(), Some("refs/heads/spira/sp-x"));
        assert_eq!(e[2].branch, None);
        assert_eq!(e[2].head, "ccc");
    }

    #[test]
    fn cwd_worktree_wins_when_its_head_is_the_commit() {
        let e = parse_list(LIST);
        assert_eq!(
            in_place_candidates(&e, Path::new("/run/worktree/round-5/spira"), "HEAD", "ccc"),
            vec![PathBuf::from("/run/worktree/round-5")]
        );
        assert_eq!(
            in_place_candidates(&e, Path::new("/elsewhere"), "spira/sp-x", "bbb"),
            vec![PathBuf::from("/run/worktree/sp-x")]
        );
        // the branch's worktree has moved on: nothing in place
        assert!(in_place_candidates(&e, Path::new("/elsewhere"), "spira/sp-x", "zzz").is_empty());
        // nested cwd picks the deepest worktree, not the outer checkout
        assert_eq!(
            in_place_candidates(&e, Path::new("/srv/harness"), "main", "aaa"),
            vec![PathBuf::from("/srv/harness")]
        );
    }

    fn sh(dir: &Path, cmd: &str) {
        let ok = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap()
            .success();
        assert!(ok, "{cmd}");
    }

    #[test]
    fn slots_are_reused_warm_and_ephemeral_is_removed() {
        let root = testkit::TempDir::new("testenv-wt");
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        sh(&repo, "git init -q -b main && printf 'target/\\n' > .gitignore && echo a > f && git add . && git commit -qm one && echo b > f && git commit -qam two");
        let c1 = git(&repo, &["rev-parse", "HEAD~1"]).unwrap();
        let c2 = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        let run = root.join("run");
        let r1 = Request {
            repo: &repo,
            rev: &c1,
            commit: &c1,
            cwd: &root,
            run_dir: &run,
            slots: 1,
        };
        let wt = acquire(&r1, &|_| {}).unwrap();
        assert!(matches!(wt.kind, Kind::Slot { .. }));
        fs::create_dir_all(wt.path.join("target")).unwrap();
        fs::write(wt.path.join("target/keep"), "warm").unwrap();
        fs::write(wt.path.join("stray"), "x").unwrap();
        let slot_path = wt.path.clone();
        // while slot 0 is held, a second acquire gets a throwaway worktree
        let r2 = Request {
            repo: &repo,
            rev: &c2,
            commit: &c2,
            cwd: &root,
            run_dir: &run,
            slots: 1,
        };
        let eph = acquire(&r2, &|_| {}).unwrap();
        assert!(matches!(eph.kind, Kind::Ephemeral));
        let eph_path = eph.path.clone();
        drop(eph);
        assert!(!eph_path.exists());
        drop(wt);
        // slot reuse: moved to c2, target/ kept, stray removed. The flock is released when
        // the last copy of the descriptor closes, and a process another test thread is
        // spawning at that instant holds a copy until it execs — so allow a brief retry.
        let mut wt = acquire(&r2, &|_| {}).unwrap();
        for _ in 0..50 {
            if matches!(wt.kind, Kind::Slot { .. }) {
                break;
            }
            drop(wt);
            std::thread::sleep(std::time::Duration::from_millis(20));
            wt = acquire(&r2, &|_| {}).unwrap();
        }
        assert_eq!(wt.path, slot_path);
        assert_eq!(fs::read_to_string(wt.path.join("f")).unwrap().trim(), "b");
        assert!(wt.path.join("target/keep").exists());
        assert!(!wt.path.join("stray").exists());
        drop(wt);
        // in place: the repo's own checkout holds main at c2 and is clean
        let r3 = Request {
            repo: &repo,
            rev: "main",
            commit: &c2,
            cwd: &root,
            run_dir: &run,
            slots: 1,
        };
        let wt = acquire(&r3, &|_| {}).unwrap();
        assert!(matches!(wt.kind, Kind::InPlace));
        assert_eq!(
            fs::canonicalize(&wt.path).unwrap(),
            fs::canonicalize(&repo).unwrap()
        );
        drop(wt);
        assert!(repo.join("f").exists());
        // dirty checkout is not used in place
        fs::write(repo.join("f"), "dirty").unwrap();
        let wt = acquire(&r3, &|_| {}).unwrap();
        assert!(!matches!(wt.kind, Kind::InPlace));
        drop(wt);
        let _ = fs::remove_dir_all(&root);
    }
}
