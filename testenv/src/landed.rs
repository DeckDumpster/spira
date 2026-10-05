//! Whether a worktree's branch has landed (sp-x9kbg) — the one question `target-reap`
//! ([`crate::reap`]) asks before freeing a `target/`. Independent of the bead's status and
//! of how the worktree directory is named: a worktree is reaped once its content is on the
//! ref its repo lands on, never before, whatever the bead says — a `law-a-binary-resolves-
//! the-config-it-reads` binary, not a shim onto `bd`.
//!
//! "Tip is an ancestor of the landing ref" is NOT enough on its own (the second sp-x9kbg
//! defect, worse than the first): it is trivially true for a worktree just cut from base —
//! a fresh aeon claim, zero commits of its own, mid-compile. Round 198 reaped two such
//! worktrees out from under their aeons. Landed therefore means BOTH: the tip is an
//! ancestor (so the work, if any, is not outstanding), AND the landing ref holds a commit
//! actually naming this bead — `law-aeon-commits-name-their-bead`'s own convention, the
//! same two subject shapes the retired landing-pass subject oracle trusted: the queue's
//! merge subject (`spira: land <id>`, `land_subject`'s output) or a bead's own commit
//! (`<id>:` — the colon must follow immediately, never a body mention).

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

/// Like [`git_out`], but a successful run with no output is `Some(vec![])`, not `None` —
/// `None` means the git command itself failed, never "no matching commits."
fn git_lines(repo: &Path, args: &[&str]) -> Option<Vec<String>> {
    let o = Command::new("git").arg("-C").arg(repo).args(args).output().ok()?;
    if !o.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect())
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

/// `git merge-base --is-ancestor a b` → `Some(true)` a is an ancestor of b, `Some(false)` it
/// is not, `None` the command itself could not be run or answered neither exit code.
fn is_ancestor(repo: &Path, a: &str, b: &str) -> Option<bool> {
    let st = Command::new("git").arg("-C").arg(repo).args(["merge-base", "--is-ancestor", a, b]).status().ok()?;
    match st.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// Whether `base`'s own history holds a commit naming `id` — `landing-pass`'s
/// `land_verify::landed` search, ported verbatim (same two trusted subject shapes, same
/// `-F` fixed-string pre-filter so this never treats a substring elsewhere in a subject,
/// or a mention in a commit BODY, as citing the bead). `Some(false)` is a real answer
/// ("ran clean, found nothing"), never confused with `None` ("the git command itself
/// failed").
fn landing_commit_cites(repo: &Path, base: &str, id: &str) -> Option<bool> {
    let lines = git_lines(repo, &["log", "--format=%H%x09%s", "--grep", id, "-F", base])?;
    let land = format!("spira: land {id}");
    let land_sp = format!("{land} ");
    let own = format!("{id}:");
    Some(lines.iter().any(|line| match line.split_once('\t') {
        Some((_, subj)) => subj == land || subj.starts_with(&land_sp) || subj.starts_with(&own),
        None => false,
    }))
}

/// Whether `base` holds the queue's own merge subject `spira: land <id>` — a Concierge
/// branch lands by a cut that rewrites its hashes, so its tip is never an ancestor.
fn land_subject_cited(repo: &Path, base: &str, id: &str) -> Option<bool> {
    let land = format!("spira: land {id}");
    let lines = git_lines(repo, &["log", "--format=%s", "--grep", &land, "-F", base])?;
    Some(lines.iter().any(|s| *s == land || s.starts_with(&format!("{land} "))))
}

/// `Some(true)`: `worktree`'s tip is an ancestor of the ref its repo lands on AND that ref
/// holds a commit naming `id` — content landed, whatever the bead's status. `Some(false)`:
/// either commits are still outstanding, or the tip is an ancestor but nothing on the
/// landing ref names this bead (a fresh, just-claimed worktree) — NEVER reaped either way.
/// `None`: the shared checkout, the landing ref, or either git answer could not be
/// resolved — fail closed, same posture an unreadable bead store used to get.
pub fn landed(reg: &Registry, worktree: &Path, id: &str) -> Option<bool> {
    let repo = main_repo_of(worktree)?;
    let base = landref(reg, &repo.to_string_lossy())?;
    if worktree.file_name().is_some_and(|n| n.to_string_lossy().starts_with("concierge-"))
        && land_subject_cited(&repo, &base, id) == Some(true)
    {
        return Some(true);
    }
    let tip = git_out(worktree, &["rev-parse", "HEAD"])?;
    if !is_ancestor(worktree, &tip, &base)? {
        return Some(false);
    }
    landing_commit_cites(&repo, &base, id)
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
    /// checked out on its own fresh branch (zero extra commits — cut from `main` and left
    /// exactly there), under `<root>/worktrees/<name>`.
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

    /// A repo whose `main` has genuinely absorbed `id`'s work: a commit subject `"<id>:
    /// ..."` (`law-aeon-commits-name-their-bead`'s own convention) that `main` is then
    /// fast-forwarded onto — exactly what a push-mode landing does, no merge commit
    /// required — and a worktree checked out on that same commit, under
    /// `<root>/worktrees/<name>`.
    fn repo_with_landed_worktree(root: &Path, id: &str, name: &str) -> PathBuf {
        let repo = root.join("repo");
        if !repo.exists() {
            fs::create_dir_all(&repo).unwrap();
            run_git(&repo, &["init", "-q", "-b", "main"]);
            commit(&repo, "f", "base");
        }
        let branch = format!("feature-{id}");
        run_git(&repo, &["checkout", "-q", "-b", &branch, "main"]);
        commit(&repo, "g", &format!("{id}: finished the work"));
        run_git(&repo, &["checkout", "-q", "main"]);
        run_git(&repo, &["merge", "-q", "--ff-only", &branch]);
        run_git(&repo, &["checkout", "-q", "main"]);
        let wt = root.join("worktrees").join(name);
        fs::create_dir_all(wt.parent().unwrap()).unwrap();
        run_git(&repo, &["worktree", "add", "-q", wt.to_str().unwrap(), &branch]);
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
    fn landed_true_when_the_landing_ref_both_contains_the_tip_and_names_the_bead() {
        let t = testkit::TempDir::new("landed-yes");
        let wt = repo_with_landed_worktree(t.path(), "sp-landed", "sp-landed");
        let reg = registry_declaring(&t.path().join("repo"));
        assert_eq!(landed(&reg, &wt, "sp-landed"), Some(true));
    }

    #[test]
    fn landed_false_when_commits_are_still_outstanding() {
        let t = testkit::TempDir::new("landed-no");
        let wt = repo_with_worktree(t.path(), "feature-unlanded", "sp-unlanded");
        commit(&wt, "g", "work in progress — not on main");
        let reg = registry_declaring(&t.path().join("repo"));
        assert_eq!(landed(&reg, &wt, "sp-unlanded"), Some(false));
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
        assert_eq!(landed(&reg, &wt, "sp-ghost"), None);
    }

    #[test]
    fn sp_x9kbg_negative_control_a_fresh_worktree_with_zero_commits_is_not_landed() {
        // THE SECOND sp-x9kbg DEFECT, worse than the first: round 198 reaped target/ from
        // two IN-PROGRESS aeons (concierge-sp-tcarr, concierge-sp-xtdqi) whose branch tip
        // happened to equal the base it was just cut from — zero commits of its own yet.
        // "tip is an ancestor of base" is trivially true for ANY fresh worktree, landed or
        // not. Landed also requires the landing ref to hold a commit actually naming this
        // bead (law-aeon-commits-name-their-bead's own convention: "spira: land <id>" or
        // "<id>: ..."). Seen red first against the old ancestor-only check, which answered
        // `Some(true)` here.
        let t = testkit::TempDir::new("landed-fresh");
        let wt = repo_with_worktree(t.path(), "feature-fresh", "sp-fresh");
        let reg = registry_declaring(&t.path().join("repo"));
        assert_eq!(landed(&reg, &wt, "sp-fresh"), Some(false), "a freshly cut worktree with zero commits must never read as landed");
    }

    fn never_busy(_: &Path) -> bool {
        false
    }

    /// THE POSITIVE CONTROL (sp-x9kbg): a truly landed worktree, an unlanded one with
    /// outstanding commits, AND a freshly cut one with zero commits, all in one fixture,
    /// run through the real [`crate::reap::reap`] with THIS module's real git-backed
    /// `landed` as its seam (no mocking of git, of landed-ness, or of the bead-citation
    /// check at all) — exactly the landed worktree is reaped. The landed worktree's
    /// directory is Concierge-named (`concierge-<id>`) and carries no bead at all, pinning
    /// that neither the bead's status nor the directory's naming has any say in the
    /// outcome, and the fresh worktree pins the second defect's fix end-to-end.
    #[test]
    fn sp_x9kbg_positive_control_exactly_the_landed_worktree_is_reaped() {
        let t = testkit::TempDir::new("landed-positive-control");
        let worktrees = t.path().join("worktrees");
        fs::create_dir_all(&worktrees).unwrap();

        let landed_wt = repo_with_landed_worktree(t.path(), "sp-land1", "concierge-sp-land1");
        let unlanded_wt = repo_with_worktree(t.path(), "feature-unlanded", "sp-land2");
        commit(&unlanded_wt, "g", "still outstanding");
        let fresh_wt = repo_with_worktree(t.path(), "feature-fresh", "sp-fresh");

        for (wt, bytes) in [(&landed_wt, 4096usize), (&unlanded_wt, 2048usize), (&fresh_wt, 1024usize)] {
            let d = wt.join("target/aeon");
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("x"), vec![1u8; bytes]).unwrap();
        }

        let reg = registry_declaring(&t.path().join("repo"));
        let landed_fn = |id: &str, dir: &Path| landed(&reg, dir, id);
        let r = crate::reap::reap(&worktrees, false, &landed_fn, &never_busy).unwrap();

        assert_eq!(r.removed, vec!["sp-land1".to_string()], "exactly the truly landed worktree is reaped");
        assert!(!landed_wt.join("target").exists(), "landed worktree's target/ is gone");
        assert!(unlanded_wt.join("target/aeon/x").is_file(), "unlanded worktree's target/ survives untouched");
        assert!(fresh_wt.join("target/aeon/x").is_file(), "a freshly cut, zero-commit worktree must never be reaped");
    }

    #[test]
    fn sp_blsz9_a_concierge_worktree_is_landed_when_the_queue_cut_names_it_though_its_tip_is_not_an_ancestor() {
        let t = testkit::TempDir::new("landed-concierge");
        let wt = repo_with_worktree(t.path(), "concierge/sp-cq", "concierge-sp-cq");
        commit(&wt, "g", "work whose hash the queue cut rewrote");
        let repo = t.path().join("repo");
        let reg = registry_declaring(&repo);
        assert_eq!(landed(&reg, &wt, "sp-cq"), Some(false), "before the land commit: kept");
        commit(&repo, "h", "spira: land sp-cq");
        assert_eq!(landed(&reg, &wt, "sp-cq"), Some(true));
        let aeon = repo_with_worktree(t.path(), "feature-aeon", "sp-cq");
        commit(&aeon, "i", "unmerged");
        assert_eq!(landed(&reg, &aeon, "sp-cq"), Some(false), "only concierge-named dirs get this rule");
    }
}
