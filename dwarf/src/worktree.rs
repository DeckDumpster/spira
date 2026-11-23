//! One dwarf, one worktree, under the sanctioned root (law-one-dwarf-one-worktree,
//! law-worktrees-in-the-sanctioned-root). The contract, pinned by the tests below:
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

use crate::ports::{s, Exec, Git};

/// Pids of live processes, other than `me`, whose working directory is `work` or inside it.
pub fn live_occupants(proc_root: &Path, work: &Path, me: u32) -> Vec<u32> {
    let Ok(rd) = std::fs::read_dir(proc_root) else { return Vec::new() };
    let mut pids: Vec<u32> = rd
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|p| *p != me)
        .filter(|p| std::fs::read_link(proc_root.join(p.to_string()).join("cwd")).is_ok_and(|c| c.starts_with(work)))
        .collect();
    pids.sort_unstable();
    pids
}

/// Arms `work` with the release's git hooks. A worktree whose install failed has no ref guard
/// and no pre-commit fences, so the failure is returned for the caller to say aloud.
pub fn arm_hooks(exec: &dyn Exec, hooks_home: &Path, work: &Path) -> Result<(), String> {
    let o = exec.exec("env", &s(&[&format!("SPIRA_HOME={}", hooks_home.display()), "worktree-hooks.sh", "install", &work.display().to_string()]), None, None);
    if o.success() {
        return Ok(());
    }
    let why = [&o.stderr, &o.stdout].into_iter().flat_map(|t| t.lines()).find(|l| !l.trim().is_empty()).unwrap_or("no output");
    Err(format!("worktree-hooks.sh install exited {}: {why}", o.code))
}

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
    pub rune: &'a str,
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
/// spira_prune_worktrees (lib.sh), run before any `worktree add`. Err is dwarf.sh's `die`.
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
            e.rune, e.bead_id, e.bead_id
        )));
        acts.push(Act::Note(format!(
            "Corrected by dwarf.sh: this bead's recorded branch: label named {branch}, which belongs to {held_id}, not {}. Reset to spira/{} and started fresh.",
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
        e.rune,
        e.bead_id,
        aside.display()
    )));
    acts.push(Act::Note(format!(
        "Moved aside by dwarf.sh: a stale worktree at {held} held this bead's own branch {branch} under a previous path. Preserved at {} — nothing deleted — and a fresh worktree cut at {work}.",
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

    #[test_id(tax1bf0)]
    #[test]
    fn a_process_standing_in_the_worktree_is_an_occupant() {
        let t = testkit::TempDir::new("dwarf-occupants");
        let work = t.join("worktree/sp-a");
        let other = t.join("worktree/sp-b");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let proc = t.join("proc");
        for (pid, cwd) in [("100", work.join("sub")), ("200", other.clone()), ("300", work.clone())] {
            std::fs::create_dir_all(proc.join(pid)).unwrap();
            std::os::unix::fs::symlink(&cwd, proc.join(pid).join("cwd")).unwrap();
        }
        std::fs::create_dir_all(proc.join("self")).unwrap();
        assert_eq!(live_occupants(&proc, &work, 999), vec![100, 300]);
        assert_eq!(live_occupants(&proc, &work, 300), vec![100], "the caller is not its own occupant");
        assert!(live_occupants(&proc, &t.join("worktree/sp-none"), 999).is_empty());
    }

    use crate::util::Out;
    use std::sync::Mutex;

    struct Installer(Out, Mutex<Vec<Vec<String>>>);
    impl Exec for Installer {
        fn exec(&self, prog: &str, args: &[String], _: Option<Vec<u8>>, _: Option<&Path>) -> Out {
            self.1.lock().unwrap().push(std::iter::once(prog.to_string()).chain(args.iter().cloned()).collect());
            Out { code: self.0.code, stdout: self.0.stdout.clone(), stderr: self.0.stderr.clone() }
        }
    }

    // covers: UC-safety-fences-21
    #[test_id(tlsg8hl)]
    #[test]
    fn a_failed_hook_install_is_reported_with_its_reason_and_a_good_one_is_silent() {
        let ok = Installer(Out::ok("worktree-hooks: armed"), Mutex::default());
        assert_eq!(arm_hooks(&ok, Path::new("/rel/spira"), Path::new("/run/worktree/sp-a")), Ok(()));
        assert_eq!(ok.1.lock().unwrap()[0], ["env", "SPIRA_HOME=/rel/spira", "worktree-hooks.sh", "install", "/run/worktree/sp-a"]);
        let bad = Installer(Out::fail(1, "worktree-hooks: no hooks directory at /rel/spira/hooks\n"), Mutex::default());
        let e = arm_hooks(&bad, Path::new("/rel/spira"), Path::new("/run/worktree/sp-a")).unwrap_err();
        assert!(e.contains("exited 1") && e.contains("no hooks directory at /rel/spira/hooks"), "{e}");
        let missing = Installer(Out::fail(127, ""), Mutex::default());
        assert!(arm_hooks(&missing, Path::new("/r"), Path::new("/w")).unwrap_err().contains("exited 127"));
    }

    type Answer = Box<dyn Fn(&Path, &[&str], usize) -> Out + Send + Sync>;

    /// Answers each git call from a script and records every call, in order. The script is
    /// told how many calls of the same verb came before.
    struct Scripted {
        calls: Mutex<Vec<Vec<String>>>,
        answer: Answer,
    }
    impl Scripted {
        fn new(answer: impl Fn(&Path, &[&str], usize) -> Out + Send + Sync + 'static) -> Self {
            Scripted { calls: Mutex::default(), answer: Box::new(answer) }
        }
        fn called(&self, verb: &[&str]) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().iter().filter(|c| c.iter().map(String::as_str).take(verb.len()).eq(verb.iter().copied())).cloned().collect()
        }
    }
    impl Git for Scripted {
        fn git(&self, dir: &Path, args: &[&str]) -> Out {
            let n = self.called(args.get(..2).unwrap_or(args)).len();
            self.calls.lock().unwrap().push(args.iter().map(|a| a.to_string()).collect());
            (self.answer)(dir, args, n)
        }
    }

    fn ensure_args<'a>(git: &'a dyn Git, repo: &'a Path, work: &'a Path, root: &'a Path, bead: &'a str) -> Ensure<'a> {
        Ensure { git, repo, work, bead_id: bead, root, base: "main", base_fq: "origin/main", rune: "builder", now: 1000 }
    }

    // covers: UC-dwarf-execution-01
    #[test_id(tlw6ljb)]
    #[test]
    fn a_new_bead_gets_a_worktree_cut_from_the_fetched_remote_base_not_the_local_branch() {
        let t = testkit::TempDir::new("dwarf-wt-fresh");
        let git = Scripted::new(|_, a, _| if a[0] == "show-ref" { Out::fail(1, "") } else { Out::ok("") });
        let (repo, work, root) = (t.join("repo"), t.join("run/worktree/sp-a"), t.join("run/worktree"));
        let mut branch = "spira/sp-a".to_string();
        ensure(&ensure_args(&git, &repo, &work, &root, "sp-a"), &mut branch, &|| {}).unwrap();
        let add = git.called(&["worktree", "add"]);
        assert_eq!(add.len(), 1);
        assert_eq!(add[0], ["worktree", "add", "-q", "-b", "spira/sp-a", &work.display().to_string(), "origin/main"]);
    }

    fn foreign_git(work: &Path) -> Scripted {
        let w = work.to_path_buf();
        Scripted::new(move |dir, a, _| match a {
            ["rev-parse", ..] if dir == w => Out::ok("x/otherrepo/.git\n"),
            ["rev-parse", ..] => Out::ok("x/thisrepo/.git\n"),
            _ => Out::fail(1, "cannot move"),
        })
    }

    // covers: UC-dwarf-execution-01
    #[test_id(tkusswj)]
    #[test]
    fn a_tree_of_another_repository_at_the_worktree_path_is_moved_aside_with_its_contents() {
        let t = testkit::TempDir::new("dwarf-wt-foreign");
        let work = t.join("worktree/sp-a");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join(".git"), "gitdir: elsewhere\n").unwrap();
        std::fs::write(work.join("precious"), "unsaved").unwrap();
        let git = foreign_git(&work);
        let aside = t.join("worktree/sp-a.otherrepo");
        assert_eq!(evict_foreign(&git, &work, &t.join("repo"), 7), Evict::Moved(aside.clone()));
        assert!(!work.exists(), "the foreign tree no longer occupies the path");
        assert_eq!(std::fs::read_to_string(aside.join("precious")).unwrap(), "unsaved", "moved, never removed");
        assert!(git.called(&["worktree", "remove"]).is_empty() && git.called(&["clean"]).is_empty());
    }

    // covers: UC-dwarf-execution-01
    #[test_id(tzn999d)]
    #[test]
    fn this_repositorys_own_tree_at_the_path_is_left_alone() {
        let t = testkit::TempDir::new("dwarf-wt-own");
        let work = t.join("worktree/sp-a");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join(".git"), "gitdir: x\n").unwrap();
        let git = Scripted::new(|_, _, _| Out::ok("x/thisrepo/.git\n"));
        assert_eq!(evict_foreign(&git, &work, &t.join("repo"), 7), Evict::Kept);
        assert!(git.called(&["worktree", "move"]).is_empty());
        assert!(work.join(".git").exists());
    }

    // covers: UC-dwarf-execution-01
    #[test_id(tjxi9k6)]
    #[test]
    fn a_bead_whose_own_branch_sits_at_an_old_path_has_that_tree_moved_aside_and_the_branch_reattached() {
        let t = testkit::TempDir::new("dwarf-wt-prior");
        let (repo, root) = (t.join("repo"), t.join("run/worktree"));
        let work = root.join("sp-a");
        let old = t.join("elsewhere/sp-a");
        std::fs::create_dir_all(&old).unwrap();
        let porcelain = format!("worktree {}\nbranch refs/heads/spira/sp-a\n", old.display());
        let git = Scripted::new(move |_, a, n| match (a[0], a.get(1).copied()) {
            ("worktree", Some("add")) if n == 0 => Out::fail(128, "already checked out"),
            ("worktree", Some("list")) => Out::ok(porcelain.clone()),
            _ => Out::ok(""),
        });
        let mut branch = "spira/sp-a".to_string();
        let acts = ensure(&ensure_args(&git, &repo, &work, &root, "sp-a"), &mut branch, &|| {}).unwrap();
        let aside = format!("{}.prior", old.display());
        assert_eq!(git.called(&["worktree", "move"])[0][2..], [old.display().to_string(), aside.clone()]);
        assert_eq!(git.called(&["checkout"]).len(), 1, "the moved tree's HEAD is detached so the branch is free");
        assert_eq!(git.called(&["worktree", "add"]).len(), 2, "the branch is attached again at the canonical path");
        assert!(git.called(&["worktree", "remove"]).is_empty());
        assert!(acts.iter().any(|a| matches!(a, Act::Note(n) if n.contains("nothing deleted") && n.contains(&aside))));
        assert_eq!(branch, "spira/sp-a");
    }

    // covers: UC-dwarf-execution-01
    #[test_id(thgvy96)]
    #[test]
    fn a_branch_checked_out_in_another_beads_worktree_is_never_adopted() {
        let t = testkit::TempDir::new("dwarf-wt-collision");
        let (repo, root) = (t.join("repo"), t.join("run/worktree"));
        let work = root.join("sp-b");
        let other = root.join("sp-a");
        std::fs::create_dir_all(&other).unwrap();
        let porcelain = format!("worktree {}\nbranch refs/heads/spira/sp-a\n", other.display());
        let git = Scripted::new(move |_, a, _| match (a[0], a.get(1).copied()) {
            ("show-ref", _) if a.last() == Some(&"refs/heads/spira/sp-a") => Out::ok(""),
            ("show-ref", _) => Out::fail(1, ""),
            ("worktree", Some("add")) if !a.contains(&"-b") => Out::fail(128, "already checked out"),
            ("worktree", Some("list")) => Out::ok(porcelain.clone()),
            _ => Out::ok(""),
        });
        let mut branch = "spira/sp-a".to_string();
        let acts = ensure(&ensure_args(&git, &repo, &work, &root, "sp-b"), &mut branch, &|| {}).unwrap();
        assert_eq!(branch, "spira/sp-b");
        assert!(acts.contains(&Act::SetBranch("spira/sp-b".into())));
        assert!(git.called(&["worktree", "move"]).is_empty(), "the other bead's tree is untouched");
        assert_eq!(git.called(&["worktree", "add", "-q", "-b", "spira/sp-b"]).len(), 1, "a fresh branch is cut from the base");
    }
}
