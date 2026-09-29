//! One aeon, one worktree, under the sanctioned root (law-one-aeon-one-worktree,
//! law-worktrees-in-the-sanctioned-root). The contract is what test-aeon-worktree-collision.sh,
//! test-aeon-worktree-evict-foreign.sh and test-aeon-resume-collision.sh assert:
//!
//! - a tree at the canonical path that belongs to ANOTHER repository (or resolves to
//!   nothing) is moved aside to `<path>.<its repo dir>`, never deleted;
//! - a branch checked out in ANOTHER BEAD's canonical worktree is never adopted: the bead's
//!   recorded `branch:` is reset to `spira/<bead>` and a fresh branch is cut;
//! - this bead's OWN branch checked out at a non-canonical path is moved aside to
//!   `<path>.prior` (HEAD detached so git frees the branch) and re-attached at the canonical
//!   path;
//! - a tree that cannot be moved is refused, with nothing lost.

use std::path::{Path, PathBuf};

use crate::ports::Git;

/// `worktree_move_aside <work> <suffix>`: `git worktree move`, else a plain rename plus a
/// best-effort `worktree repair`. None when the tree could not be moved at all.
pub fn move_aside(git: &dyn Git, work: &Path, suffix: &str, now: i64) -> Option<PathBuf> {
    let suffix = if suffix.is_empty() { "aside" } else { suffix };
    let mut aside = PathBuf::from(format!("{}.{suffix}", work.display()));
    if aside.exists() {
        aside = PathBuf::from(format!("{}.{now}", aside.display()));
    }
    let a = aside.display().to_string();
    let w = work.display().to_string();
    if !git.git(work, &["worktree", "move", &w, &a]).success() {
        if std::fs::rename(work, &aside).is_err() {
            return None;
        }
        let _ = git.git(&aside, &["worktree", "repair", &a]);
    }
    Some(aside)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Evict {
    /// Nothing at the path, or the tree is this repository's (rc 1).
    Kept,
    /// Moved aside to this path (rc 0).
    Moved(PathBuf),
    /// Belongs elsewhere and could not be moved (rc 2).
    Refused,
}

/// `worktree_evict_foreign <work> <repo>`.
pub fn evict_foreign(git: &dyn Git, work: &Path, repo: &Path, now: i64) -> Evict {
    if !work.join(".git").exists() {
        return Evict::Kept;
    }
    let want = git.git(repo, &["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    let want = if want.success() { want.text() } else { return Evict::Kept };
    if want.is_empty() {
        return Evict::Kept;
    }
    let have = git.git(work, &["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    let have = if have.success() { have.text() } else { String::new() };
    if have == want {
        return Evict::Kept;
    }
    // basename "$(dirname "$have")" — for an empty `have` that is ".", as in bash.
    let other = if have.is_empty() {
        ".".to_string()
    } else {
        Path::new(&have).parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| ".".into())
    };
    match move_aside(git, work, if other.is_empty() { "foreign" } else { &other }, now) {
        Some(p) => Evict::Moved(p),
        None => Evict::Refused,
    }
}

/// What setting up the worktree did besides git itself: log lines, bead notes, and a
/// corrected branch to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    Log(String),
    Note(String),
    SetBranch(String),
}

pub struct Ensure<'a> {
    pub git: &'a dyn Git,
    pub repo: &'a Path,
    pub work: &'a Path,
    pub bead_id: &'a str,
    /// `$SPIRA_RUN/worktree/` — the canonical root, to recognise another bead's tree.
    pub root: &'a Path,
    pub base: &'a str,
    pub base_fq: &'a str,
    pub fayth: &'a str,
    pub now: i64,
}

fn err_suffix(e: &str) -> String {
    let e = e.trim_end_matches('\n');
    if e.is_empty() {
        String::new()
    } else {
        format!(": {e}")
    }
}

/// The worktree for this bead, cut or re-attached. `branch` may be corrected. `prune` is
/// spira_prune_worktrees (lib.sh), run before any `worktree add`. Err is aeon.sh's `die`.
pub fn ensure(e: &Ensure, branch: &mut String, prune: &dyn Fn()) -> Result<Vec<Act>, String> {
    let mut acts = Vec::new();
    let work = e.work.display().to_string();
    if e.work.join(".git").is_dir() || e.work.join(".git").is_file() {
        return Ok(acts);
    }
    if let Some(p) = e.work.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    prune();
    let has = |b: &str| e.git.git(e.repo, &["show-ref", "--verify", "-q", &format!("refs/heads/{b}")]).success();
    if !has(branch) {
        let o = e.git.git(e.repo, &["worktree", "add", "-q", "-b", branch, &work, e.base_fq]);
        if !o.success() {
            return Err(format!("could not create a worktree at {work} from {}{}", e.base, err_suffix(&o.stderr)));
        }
        return Ok(acts);
    }
    let o = e.git.git(e.repo, &["worktree", "add", "-q", &work, branch]);
    if o.success() {
        return Ok(acts);
    }
    let wt_err = o.stderr.clone();
    let held = holder(e.git, e.repo, branch);
    let root = format!("{}/", e.root.display().to_string().trim_end_matches('/'));
    let held_id = held.as_deref().and_then(|h| h.strip_prefix(&root)).map(|s| s.to_string()).unwrap_or_default();
    let Some(held) = held.filter(|h| Path::new(h).is_dir()) else {
        return Err(format!("could not attach a worktree at {work} to existing branch {branch}{}", err_suffix(&wt_err)));
    };
    if !held_id.is_empty() && held_id != e.bead_id {
        // A mislabeled branch: (bd create --parent copies it to every child), not a resume.
        acts.push(Act::Log(format!(
            "{}: {}: recorded branch {branch} is checked out at {held}, which belongs to {held_id}, not {} — a mislabeled branch:, not a resume; taking a fresh branch instead of dying",
            e.fayth, e.bead_id, e.bead_id
        )));
        acts.push(Act::Note(format!(
            "Corrected by aeon.sh: this bead's recorded branch: label named {branch}, which belongs to {held_id}, not {}. Reset to spira/{} and started fresh.",
            e.bead_id, e.bead_id
        )));
        *branch = format!("spira/{}", e.bead_id);
        acts.push(Act::SetBranch(branch.clone()));
        if has(branch) {
            let o = e.git.git(e.repo, &["worktree", "add", "-q", &work, branch]);
            if !o.success() {
                return Err(format!(
                    "could not attach a worktree at {work} to this bead's own branch {branch} after correcting a mislabeled branch{}",
                    err_suffix(&o.stderr)
                ));
            }
        } else {
            let o = e.git.git(e.repo, &["worktree", "add", "-q", "-b", branch, &work, e.base_fq]);
            if !o.success() {
                return Err(format!("could not create a worktree at {work} from {} after correcting a mislabeled branch{}", e.base, err_suffix(&o.stderr)));
            }
        }
        return Ok(acts);
    }
    // This bead's own branch under a previous path: move it aside, never delete.
    let Some(aside) = move_aside(e.git, Path::new(&held), "prior", e.now) else {
        return Err(format!("{branch} is checked out at {held} and could not be moved aside"));
    };
    // The move relocates the directory, it does not free the branch: detach its HEAD.
    let _ = e.git.git(&aside, &["checkout", "-q", "--detach"]);
    acts.push(Act::Log(format!(
        "{}: {} — {branch} was checked out at {held} (a previous path); moved aside to {}, cutting {work} fresh",
        e.fayth,
        e.bead_id,
        aside.display()
    )));
    acts.push(Act::Note(format!(
        "Moved aside by aeon.sh: a stale worktree at {held} held this bead's own branch {branch} under a previous path. Preserved at {} — nothing deleted — and a fresh worktree cut at {work}.",
        aside.display()
    )));
    let o = e.git.git(e.repo, &["worktree", "add", "-q", &work, branch]);
    if !o.success() {
        return Err(format!("could not attach a worktree at {work} to existing branch {branch} after moving aside {held}{}", err_suffix(&o.stderr)));
    }
    Ok(acts)
}

/// The worktree path holding `refs/heads/<branch>`, per `git worktree list --porcelain`.
pub fn holder(git: &dyn Git, repo: &Path, branch: &str) -> Option<String> {
    let o = git.git(repo, &["worktree", "list", "--porcelain"]);
    let want = format!("refs/heads/{branch}");
    let mut cur = String::new();
    for l in o.stdout.lines() {
        if let Some(w) = l.strip_prefix("worktree ") {
            cur = w.to_string();
        } else if let Some(b) = l.strip_prefix("branch ") {
            if b == want {
                return Some(cur);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{Env, RealGit};
    use std::collections::BTreeMap;
    use std::process::Command;

    fn env() -> Env {
        let mut m = BTreeMap::new();
        for k in ["PATH", "HOME"] {
            if let Ok(v) = std::env::var(k) {
                m.insert(k.to_string(), v);
            }
        }
        for (k, v) in [("GIT_AUTHOR_NAME", "t"), ("GIT_AUTHOR_EMAIL", "t@t"), ("GIT_COMMITTER_NAME", "t"), ("GIT_COMMITTER_EMAIL", "t@t"), ("GIT_CONFIG_NOSYSTEM", "1")] {
            m.insert(k.into(), v.into());
        }
        Env::new(m.clone(), m)
    }

    fn sh(dir: &Path, args: &[&str]) {
        let o = Command::new("git").arg("-C").arg(dir).args(args).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }

    fn tmp(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aeon-wt-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    fn repo(dir: &Path) -> PathBuf {
        let r = dir.join("repo");
        std::fs::create_dir_all(&r).unwrap();
        sh(&r, &["init", "-q", "-b", "main"]);
        std::fs::write(r.join("f"), "seed\n").unwrap();
        sh(&r, &["add", "f"]);
        sh(&r, &["commit", "-qm", "seed"]);
        r
    }

    fn git_ok() -> bool {
        Command::new("git").arg("--version").output().is_ok()
    }

    #[test]
    fn fresh_bead_cuts_its_own_branch_under_the_root() {
        if !git_ok() {
            return;
        }
        let d = tmp("fresh");
        let r = repo(&d);
        let e = env();
        let g = RealGit { env: &e };
        let root = d.join("run/worktree");
        let work = root.join("sp-a");
        let mut br = "spira/sp-a".to_string();
        let pruned = std::cell::Cell::new(0);
        let acts = ensure(&Ensure { git: &g, repo: &r, work: &work, bead_id: "sp-a", root: &root, base: "main", base_fq: "main", fayth: "builder", now: 1 }, &mut br, &|| pruned.set(pruned.get() + 1)).unwrap();
        assert!(acts.is_empty());
        assert!(work.join(".git").exists());
        assert_eq!(pruned.get(), 1, "prune runs before cutting");
        // A second summon with the tree present is a no-op (resume).
        let acts = ensure(&Ensure { git: &g, repo: &r, work: &work, bead_id: "sp-a", root: &root, base: "main", base_fq: "main", fayth: "builder", now: 1 }, &mut br, &|| pruned.set(pruned.get() + 1)).unwrap();
        assert!(acts.is_empty());
        assert_eq!(pruned.get(), 1);
    }

    // test-aeon-worktree-collision.sh CASE 1 (sp-om71s): never adopt, never die.
    #[test]
    fn mislabeled_branch_takes_a_fresh_branch() {
        if !git_ok() {
            return;
        }
        let d = tmp("mislabel");
        let r = repo(&d);
        let e = env();
        let g = RealGit { env: &e };
        let root = d.join("run/worktree");
        let a = root.join("sp-cw-a");
        let mut bra = "spira/sp-cw-a".to_string();
        ensure(&Ensure { git: &g, repo: &r, work: &a, bead_id: "sp-cw-a", root: &root, base: "main", base_fq: "main", fayth: "builder", now: 1 }, &mut bra, &|| {}).unwrap();
        let b = root.join("sp-cw-b");
        let mut brb = "spira/sp-cw-a".to_string();
        let acts = ensure(&Ensure { git: &g, repo: &r, work: &b, bead_id: "sp-cw-b", root: &root, base: "main", base_fq: "main", fayth: "builder", now: 1 }, &mut brb, &|| {}).unwrap();
        assert_eq!(brb, "spira/sp-cw-b");
        assert!(b.join(".git").exists(), "B got its OWN fresh worktree");
        assert!(a.join(".git").exists(), "A's worktree is untouched");
        assert!(acts.contains(&Act::SetBranch("spira/sp-cw-b".into())));
        let log = acts.iter().find_map(|x| if let Act::Log(l) = x { Some(l.clone()) } else { None }).unwrap();
        assert!(log.contains("spira/sp-cw-a") && log.contains("belongs to sp-cw-a") && log.contains("taking a fresh branch instead of dying"));
        assert_eq!(holder(&g, &r, "spira/sp-cw-b").as_deref(), Some(b.to_str().unwrap()));
    }

    // CASE 2: this bead's own branch at a previous path — moved aside, fresh tree cut.
    #[test]
    fn own_branch_at_a_previous_path_is_moved_aside() {
        if !git_ok() {
            return;
        }
        let d = tmp("prior");
        let r = repo(&d);
        let e = env();
        let g = RealGit { env: &e };
        let old = d.join("handmade/sp-cw-c-legacy");
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        sh(&r, &["worktree", "add", "-q", "-b", "spira/sp-cw-c", old.to_str().unwrap(), "main"]);
        std::fs::write(old.join("g"), "old\n").unwrap();
        sh(&old, &["add", "g"]);
        sh(&old, &["commit", "-qm", "sp-cw-c — from a previous path"]);
        let root = d.join("run/worktree");
        let work = root.join("sp-cw-c");
        let mut br = "spira/sp-cw-c".to_string();
        let acts = ensure(&Ensure { git: &g, repo: &r, work: &work, bead_id: "sp-cw-c", root: &root, base: "main", base_fq: "main", fayth: "builder", now: 1 }, &mut br, &|| {}).unwrap();
        assert!(work.join(".git").exists());
        let prior = PathBuf::from(format!("{}.prior", old.display()));
        assert!(prior.is_dir(), "moved aside, not deleted");
        assert!(!old.exists());
        assert_eq!(std::fs::read_to_string(prior.join("g")).unwrap(), "old\n");
        assert!(acts.iter().any(|a| matches!(a, Act::Note(n) if n.contains("Preserved at"))));
        assert_eq!(br, "spira/sp-cw-c");
    }

    // test-aeon-worktree-evict-foreign.sh, row for row.
    #[test]
    fn evict_foreign_rows() {
        if !git_ok() {
            return;
        }
        let d = tmp("evict");
        let e = env();
        let g = RealGit { env: &e };
        let ra = d.join("repo-a");
        std::fs::create_dir_all(&ra).unwrap();
        sh(&ra, &["init", "-q", "-b", "main"]);
        sh(&ra, &["commit", "-q", "--allow-empty", "-m", "seed"]);
        assert_eq!(evict_foreign(&g, &d.join("nowhere"), &ra, 1), Evict::Kept);
        let own = d.join("own");
        sh(&ra, &["worktree", "add", "-q", own.to_str().unwrap(), "-b", "spira/own", "main"]);
        assert_eq!(evict_foreign(&g, &own, &ra, 1), Evict::Kept);
        assert!(own.join(".git").exists());
        let rb = d.join("repo-b");
        std::fs::create_dir_all(&rb).unwrap();
        sh(&rb, &["init", "-q", "-b", "main"]);
        sh(&rb, &["commit", "-q", "--allow-empty", "-m", "seed"]);
        let foreign = d.join("foreign");
        sh(&rb, &["worktree", "add", "-q", foreign.to_str().unwrap(), "-b", "spira/foreign", "main"]);
        std::fs::write(foreign.join("scratch.txt"), "uncommitted salvage bait\n").unwrap();
        match evict_foreign(&g, &foreign, &ra, 1) {
            Evict::Moved(p) => {
                assert!(p.display().to_string().starts_with(&format!("{}.", foreign.display())));
                assert!(p.display().to_string().contains("repo-b"), "the suffix names the repo it belonged to");
                assert!(!foreign.exists());
                assert_eq!(std::fs::read_to_string(p.join("scratch.txt")).unwrap(), "uncommitted salvage bait\n");
            }
            other => panic!("{other:?}"),
        }
        let broken = d.join("broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join(".git"), format!("gitdir: {}/nonexistent-gitdir\n", d.display())).unwrap();
        assert!(matches!(evict_foreign(&g, &broken, &ra, 1), Evict::Moved(_)));
        assert!(!broken.exists());
    }

    #[test]
    fn unmovable_tree_is_refused_and_kept() {
        if !git_ok() || unsafe { libc::geteuid() } == 0 {
            return;
        }
        let d = tmp("refuse");
        let e = env();
        let g = RealGit { env: &e };
        let ra = d.join("repo-a");
        let rb = d.join("repo-b");
        for r in [&ra, &rb] {
            std::fs::create_dir_all(r).unwrap();
            sh(r, &["init", "-q", "-b", "main"]);
            sh(r, &["commit", "-q", "--allow-empty", "-m", "seed"]);
        }
        let parent = d.join("locked-parent");
        std::fs::create_dir_all(&parent).unwrap();
        let t = parent.join("foreign");
        sh(&rb, &["worktree", "add", "-q", t.to_str().unwrap(), "-b", "spira/refused", "main"]);
        std::fs::write(t.join("scratch.txt"), "must not be lost\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();
        let r = evict_foreign(&g, &t, &ra, 1);
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(r, Evict::Refused);
        assert_eq!(std::fs::read_to_string(t.join("scratch.txt")).unwrap(), "must not be lost\n");
    }
}
