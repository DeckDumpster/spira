//! Where the tree under test is built and mounted (DESIGN.md §4.1): the caller's own
//! worktree when it already holds exactly that commit, clean; else a warm scratch slot;
//! else a throwaway worktree. The caller's worktree is never removed.

use crate::util::git;
use std::fs::{self, File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// The key that declares where the scratch and warm slots live.
pub const SCRATCH_ROOT_KEY: &str = "SPIRA_TESTENV_SCRATCH_ROOT";

/// Where the scratch and warm slots live: the absolute path [`SCRATCH_ROOT_KEY`] declares,
/// and nothing else. A slot is disposable build state (cargo's `target/`, 1.4–2.4 GB,
/// rewritten on every relink), so the key names a disk that can take build writes. Unset or
/// relative is a refusal naming the key; no other place is guessed.
pub fn scratch_root(declared: &str) -> Result<PathBuf, String> {
    let v = declared.trim();
    if v.is_empty() {
        return Err(format!("{SCRATCH_ROOT_KEY} is not set: declare where testenv's scratch and warm slots live"));
    }
    let p = PathBuf::from(v);
    if !p.is_absolute() {
        return Err(format!("{SCRATCH_ROOT_KEY} = {v:?} is not an absolute path"));
    }
    Ok(p)
}

/// Space this user can still write under `p`: statvfs free or quota headroom, whichever is
/// smaller. `pub(crate)` so [`crate::warm::shed`] and the runner probe the filesystem a build filled.
pub(crate) fn free_mib(p: &Path) -> Option<u64> {
    spira_config::room::avail_mib(p)
}

/// The mount point holding `p`: the longest `/proc/self/mountinfo` mount that prefixes it.
pub(crate) fn mount_point(p: &Path) -> PathBuf {
    let real = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let info = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    info.lines()
        .filter_map(|l| l.split(' ').nth(4))
        .map(|m| PathBuf::from(m.replace("\\040", " ")))
        .filter(|m| real.starts_with(m))
        .max_by_key(|m| m.as_os_str().len())
        .unwrap_or_else(|| PathBuf::from("/"))
}

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
    Slot {
        _lock: File,
    },
    /// A warm slot (DESIGN.md §11.2): a scratch slot with a pre-booted container of its own.
    Warm {
        index: usize,
        _lock: File,
    },
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
            Kind::Warm { .. } => "warm slot",
            Kind::Ephemeral => "throwaway worktree",
        }
    }
}

impl Worktree {
    /// The warm slot's index, when this is one.
    pub fn warm_index(&self) -> Option<usize> {
        match self.kind {
            Kind::Warm { index, .. } => Some(index),
            _ => None,
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

pub(crate) fn try_lock(path: &Path) -> Option<File> {
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

/// Prefixes of the scratch directories `testkit::TempDir` names `<tag>-<pid>-<n>-<nanos>`
/// for tests that copy binaries; a killed test process never runs `Drop`, so they are reaped here.
const TEST_SCRATCH_PREFIXES: &[&str] = &["sp-hh599-unit-env-"];
const STALE_SCRATCH_AGE: std::time::Duration = std::time::Duration::from_secs(3600);

/// Remove test scratch under `root` that is older than `min_age` and whose owning pid is gone.
/// Returns the number removed.
pub(crate) fn reap_stale_test_scratch(
    root: &Path,
    min_age: std::time::Duration,
    pid_alive: &dyn Fn(u32) -> bool,
    log: &dyn Fn(&str),
) -> usize {
    let Ok(rd) = fs::read_dir(root) else { return 0 };
    let mut n = 0;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = TEST_SCRATCH_PREFIXES.iter().find_map(|p| name.strip_prefix(p)) else { continue };
        let mut parts = rest.rsplitn(4, '-');
        let (_nanos, _n, pid) = (parts.next(), parts.next(), parts.next().and_then(|p| p.parse::<u32>().ok()));
        let Some(pid) = pid else { continue };
        if pid_alive(pid) {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= min_age);
        if old && fs::remove_dir_all(e.path()).is_ok() {
            log(&format!("reaped stale test scratch {}", e.path().display()));
            n += 1;
        }
    }
    n
}

fn head_of(wt: &Path) -> Option<String> {
    git(wt, &["rev-parse", "--verify", "-q", "HEAD"]).ok()
}

/// Point an existing slot at `commit`, or create it. Checkout rewrites only the files that
/// differ, so cargo's fingerprints keep `target/` warm; everything else untracked or ignored
/// is removed, so the slot is the commit and nothing else.
pub(crate) fn prepare_slot(repo: &Path, slot: &Path, commit: &str) -> Result<(), String> {
    let mut registered = slot.join(".git").exists() && head_of(slot).is_some();
    if registered && git(slot, &["cat-file", "-e", &format!("{commit}^{{tree}}")]).is_err() {
        // The slot's object store lacks the commit's objects: rebuild it from the repo's.
        registered = false;
    }
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
    /// Where the slots live ([`scratch_root`]).
    pub scratch: &'a Path,
    pub slots: usize,
}

/// The first free warm slot (DESIGN.md §11.2), locked and checked out at the commit; None
/// when every slot is busy or unusable (the caller falls back to [`acquire`]).
pub fn acquire_warm(req: &Request, slots: usize, log: &dyn Fn(&str)) -> Option<Worktree> {
    let base = req.scratch;
    if let Err(e) = fs::create_dir_all(base) {
        log(&format!("no warm slot: cannot create {}: {e}", base.display()));
        return None;
    }
    for i in 0..slots {
        let (slot, lock_path, _) = crate::warm::paths(base, i);
        let Some(lock) = try_lock(&lock_path) else {
            log(&format!("warm slot {i} is locked — trying the next"));
            continue;
        };
        match prepare_slot(req.repo, &slot, req.commit) {
            Ok(()) => {
                return Some(Worktree {
                    path: slot,
                    kind: Kind::Warm {
                        index: i,
                        _lock: lock,
                    },
                    repo: req.repo.to_path_buf(),
                })
            }
            Err(e) => log(&format!("warm slot {i} unusable ({e}) — trying the next")),
        }
    }
    None
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
    let base = req.scratch;
    fs::create_dir_all(base)
        .map_err(|e| format!("cannot create worktree directory {}: {e}", base.display()))?;
    reap_stale_test_scratch(&std::env::temp_dir(), STALE_SCRATCH_AGE, &|pid| Path::new(&format!("/proc/{pid}")).exists(), log);
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


    const LIST: &str = "worktree /srv/harness\nHEAD aaa\nbranch refs/heads/main\n\nworktree /run/worktree/sp-x\nHEAD bbb\nbranch refs/heads/spira/sp-x\n\nworktree /run/worktree/round-5\nHEAD ccc\ndetached\n";

    #[test]
    fn stale_unheld_test_scratch_is_reaped_and_the_rest_is_kept() {
        let root = testkit::TempDir::new("reap-root");
        for n in [
            "sp-hh599-unit-env-cfg-mail-111-1-5",
            "sp-hh599-unit-env-call-svc-222-2-6",
            "sp-hh599-unit-env-cfg-gh-intake-333-3-7",
            "unrelated-444-4-8",
            "sp-hh599-unit-env-nopid",
        ] {
            fs::create_dir(root.join(n)).unwrap();
            fs::write(root.join(n).join("f"), "x").unwrap();
        }
        let alive = |pid: u32| pid == 222;
        let zero = std::time::Duration::ZERO;
        let day = std::time::Duration::from_secs(86400);
        assert_eq!(reap_stale_test_scratch(&root, day, &alive, &|_| {}), 0, "too young");
        assert_eq!(reap_stale_test_scratch(&root, zero, &alive, &|_| {}), 2);
        assert!(!root.join("sp-hh599-unit-env-cfg-mail-111-1-5").exists());
        assert!(!root.join("sp-hh599-unit-env-cfg-gh-intake-333-3-7").exists());
        assert!(root.join("sp-hh599-unit-env-call-svc-222-2-6").exists(), "pid held");
        assert!(root.join("unrelated-444-4-8").exists());
        assert!(root.join("sp-hh599-unit-env-nopid").exists());
    }

    #[test]
    fn the_declared_scratch_root_is_used_whatever_tmp_is() {
        for declared in ["/srv/test-disk/testenv-scratch", "/tmp/declared", " /srv/padded "] {
            assert_eq!(scratch_root(declared), Ok(PathBuf::from(declared.trim())));
        }
    }

    #[test]
    fn an_undeclared_or_relative_scratch_root_refuses_naming_the_key() {
        for declared in ["", "   "] {
            let e = scratch_root(declared).unwrap_err();
            assert!(e.contains("SPIRA_TESTENV_SCRATCH_ROOT") && e.contains("not set"), "{e}");
        }
        let e = scratch_root("scratch/slots").unwrap_err();
        assert!(e.contains("SPIRA_TESTENV_SCRATCH_ROOT") && e.contains("absolute"), "{e}");
    }

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
}
