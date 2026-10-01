//! Whether a worktree's branch has landed (sp-x9kbg) — the one question `target-reap`
//! ([`crate::reap`]) asks before freeing a `target/`. Independent of the bead's status and
//! of how the worktree directory is named: a worktree is reaped once its content is on the
//! ref its repo lands on, never before, whatever the bead says — a `law-a-binary-resolves-
//! the-config-it-reads` binary, not a shim onto `bd`.

use spira_config::repos::{landref, Registry};
use std::path::{Path, PathBuf};
use std::process::Command;

fn git_out(repo: &Path, args: &[&str]) -> Option<String> {
    let o = Command::new("git").arg("-C").arg(repo).args(args).output().ok()?;
    if !o.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// The checkout a worktree shares its objects and refs with (`git rev-parse
/// --git-common-dir`) — never the worktree's own `.git`, which is a FILE naming that
/// checkout's `.git/worktrees/<name>`. `concierge-<id>` and `<id>` worktrees resolve
/// through this identically; what matters is the shared `.git`, not the directory's name.
pub fn main_repo_of(worktree: &Path) -> Option<PathBuf> {
    let common = PathBuf::from(git_out(worktree, &["rev-parse", "--git-common-dir"])?);
    let common = if common.is_absolute() { common } else { worktree.join(common) };
    common.parent().map(Path::to_path_buf)
}

/// `Some(true)`: `worktree`'s tip is an ancestor of the ref its repo lands on — content
/// landed, whatever the bead's status. `Some(false)`: commits are still outstanding — NEVER
/// reaped. `None`: the shared checkout, the landing ref, or the tip could not be resolved —
/// fail closed, same posture an unreadable bead store used to get.
pub fn landed(reg: &Registry, worktree: &Path) -> Option<bool> {
    let repo = main_repo_of(worktree)?;
    let base = landref(reg, &repo.to_string_lossy())?;
    let tip = git_out(worktree, &["rev-parse", "HEAD"])?;
    let st = Command::new("git").arg("-C").arg(worktree).args(["merge-base", "--is-ancestor", &tip, &base]).status().ok()?;
    match st.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;

    fn run_git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed in {dir:?}");
    }

    fn commit(dir: &Path, file: &str, text: &str) {
        fs::write(dir.join(file), text).unwrap();
        run_git(dir, &["add", file]);
        run_git(dir, &["commit", "-q", "-m", text]);
    }

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// A repo with a `main` branch (the landing ref, declared in the map) and a worktree
    /// checked out on its own branch, under `<root>/worktrees/<name>`.
    fn repo_with_worktree(root: &Path, branch: &str, name: &str) -> PathBuf {
        let repo = root.join("repo");
        if !repo.exists() {
            fs::create_dir_all(&repo).unwrap();
            run_git(&repo, &["init", "-q", "-b", "main"]);
            commit(&repo, "f", "base");
        }
        let wt = root.join("worktrees").join(name);
        fs::create_dir_all(wt.parent().unwrap()).unwrap();
        run_git(&repo, &["worktree", "add", "-q", "-b", branch, wt.to_str().unwrap(), "main"]);
        wt
    }

    fn registry_declaring(repo: &Path) -> Registry {
        let map = format!("r | {} | push | main |  | \n", repo.display());
        Registry::new(Some(&map), &env(&[("SPIRA_HOME_REPO", "r")]), Path::new("/nonexistent"))
    }

    #[test]
    fn main_repo_of_resolves_through_the_shared_git_dir_not_the_worktree_name() {
        let t = testkit::TempDir::new("landed-common-dir");
        let wt = repo_with_worktree(t.path(), "feature", "concierge-sp-whatever");
        let repo = t.path().join("repo").canonicalize().unwrap();
        assert_eq!(main_repo_of(&wt), Some(repo));
    }

    #[test]
    fn landed_true_when_the_tip_is_an_ancestor_of_the_landing_ref() {
        let t = testkit::TempDir::new("landed-yes");
        let wt = repo_with_worktree(t.path(), "feature-landed", "sp-landed");
        // feature-landed branches off main with no further commits: its tip IS main's tip.
        let reg = registry_declaring(&t.path().join("repo"));
        assert_eq!(landed(&reg, &wt), Some(true));
    }

    #[test]
    fn landed_false_when_commits_are_still_outstanding() {
        let t = testkit::TempDir::new("landed-no");
        let wt = repo_with_worktree(t.path(), "feature-unlanded", "sp-unlanded");
        commit(&wt, "g", "work in progress — not on main");
        let reg = registry_declaring(&t.path().join("repo"));
        assert_eq!(landed(&reg, &wt), Some(false));
    }

    #[test]
    fn landed_is_none_when_the_landing_ref_cannot_be_resolved() {
        let t = testkit::TempDir::new("landed-unknown");
        let wt = repo_with_worktree(t.path(), "feature", "sp-ghost");
        // No map at all: rung 1 (declared) is skipped, no remote, so rung 4 falls back to
        // the checkout's OWN current branch — which for the main repo is "main" itself,
        // still resolvable. Force genuine unresolvability with a declared base that does
        // not exist, which refuses outright rather than falling through (law of landref).
        let map = format!("r | {} | push | no-such-branch |  | \n", t.path().join("repo").display());
        let reg = Registry::new(Some(&map), &env(&[("SPIRA_HOME_REPO", "r")]), Path::new("/nonexistent"));
        assert_eq!(landed(&reg, &wt), None);
    }

    /// THE POSITIVE CONTROL (sp-x9kbg): one landed worktree and one unlanded worktree in
    /// the same fixture, run through the real [`crate::reap::reap`] with THIS module's real
    /// git-backed `landed` as its seam (no mocking of git or of landed-ness at all) — the
    /// exact shape asked for: "a fixture with one landed and one unlanded worktree, showing
    /// exactly the landed one reaped." The landed worktree's directory is Concierge-named
    /// (`concierge-<id>`) and carries no bead at all, pinning that neither the bead's
    /// status nor the directory's naming has any say in the outcome.
    #[test]
    fn sp_x9kbg_positive_control_exactly_the_landed_worktree_is_reaped() {
        let t = testkit::TempDir::new("landed-positive-control");
        let worktrees = t.path().join("worktrees");
        fs::create_dir_all(&worktrees).unwrap();

        let landed_wt = repo_with_worktree(t.path(), "feature-landed", "concierge-sp-land1");
        let unlanded_wt = repo_with_worktree(t.path(), "feature-unlanded", "sp-land2");
        commit(&unlanded_wt, "g", "still outstanding");

        for (wt, bytes) in [(&landed_wt, 4096usize), (&unlanded_wt, 2048usize)] {
            let d = wt.join("target/aeon");
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("x"), vec![1u8; bytes]).unwrap();
        }

        let reg = registry_declaring(&t.path().join("repo"));
        let landed_fn = |dir: &Path| landed(&reg, dir);
        let r = crate::reap::reap(&worktrees, false, &landed_fn).unwrap();

        assert_eq!(r.removed, vec!["sp-land1".to_string()], "exactly the landed worktree is reaped");
        assert!(!landed_wt.join("target").exists(), "landed worktree's target/ is gone");
        assert!(unlanded_wt.join("target/aeon/x").is_file(), "unlanded worktree's target/ survives untouched");
    }
}
