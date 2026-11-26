//! `branch-guard.sh check` in process: per registered checkout, a non-merge dwarf commit at
//! the base branch's tip, and a base branch ahead of its remote. Every git read is held on
//! the shas it answers from, so an unmoved ref starts no git. The script's other verbs are
//! the git hooks and stay in shell.

use std::path::Path;

use spira_config::bounded::bounded;
use spira_config::git_refs::{git_dirs, held, read_ref_in, refs_digest, remotes, sha_of, Ref};
use spira_config::repos::{self, Column, Registry};

const DWARF_EMAIL_SUFFIX: &str = "@spira.local";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Audit {
    /// No registered repository is a checkout here: the script's exit 3.
    NothingToAudit,
    Clean { notes: Vec<String> },
    /// `lines` are the findings and the notes, in repository order.
    Found { lines: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Tip {
    email: String,
    parents: usize,
    short: String,
    subject: String,
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = bounded("git").arg("-C").arg(root).args(args).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `read`, held on `key`; a key the ref files cannot give is asked every time.
fn held_on<T: Clone + Send + 'static>(root: &Path, question: &str, key: Option<String>, read: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    match key {
        Some(k) => held(root, question, &k, read),
        None => read(),
    }
}

/// The text of the checkout's own config files, which name its remotes. A file they `include`
/// is not part of it.
fn config_key(root: &Path) -> Option<String> {
    let (gitdir, common) = git_dirs(root)?;
    let shared = std::fs::read_to_string(common.join("config")).ok()?;
    Some(format!("{shared}\0{}", std::fs::read_to_string(gitdir.join("config.worktree")).unwrap_or_default()))
}

/// A base the ref files cannot verify asks git. The answer is held until a ref or the config
/// moves; "unresolved" only when git says the declared ref is missing, never on a git fault.
fn landref(reg: &Registry, name: &str, root: &str) -> Option<String> {
    let declared = reg.field(name, Column::Base).unwrap_or_default();
    let path = Path::new(root);
    let key = refs_digest(path).zip(config_key(path)).map(|(refs, config)| format!("{refs}\0{config}\0{declared}"));
    let missing = (!declared.is_empty()).then(|| format!("tried {declared}: exit 1"));
    held_on(path, "branch-guard-landref", key, || match repos::landref_explained(reg, root) {
        Ok(found) => Ok(Some(found)),
        Err(why) if Some(&why) == missing.as_ref() => Ok(None),
        Err(why) => Err(why),
    })
    .ok()
    .flatten()
}

/// The remote `landref` names, if its first component is one: `local/main` is a local branch.
/// [`repos::ref_remote`] runs an unheld git for a config the files cannot answer for
/// (`worktreeConfig`, `include`), so that read is made here.
fn remote_of(root: &Path, landref: &str) -> Option<String> {
    let prefix = landref.split_once('/')?.0;
    let names = match remotes(root) {
        Some(names) => names,
        None => held_on(root, "branch-guard-remotes", config_key(root), || Ok(git(root, &["remote"])?.lines().map(str::to_string).collect::<Vec<_>>())).ok()?,
    };
    names.iter().any(|n| n == prefix).then(|| prefix.to_string())
}

/// `short` is a ref without its `refs/`: the ref files say no such ref, so git has nothing to read.
fn absent(root: &Path, short: &str) -> bool {
    read_ref_in(root, short) == Ref::Absent
}

fn tip(root: &Path, base: &str) -> Option<Tip> {
    if absent(root, &format!("heads/{base}")) {
        return None;
    }
    let full = format!("refs/heads/{base}");
    held_on(root, "branch-guard-tip", sha_of(root, &full), || {
        let text = git(root, &["log", "-1", "--format=%ae%x00%P%x00%h%x00%s", &full])?;
        let mut f = text.splitn(4, '\0');
        let mut next = || f.next().unwrap_or_default().to_string();
        let (email, parents, short, subject) = (next(), next().split_whitespace().count(), next(), next());
        Ok(Tip { email, parents, short, subject })
    })
    .ok()
}

fn ahead(root: &Path, remote: &str, base: &str) -> Result<u64, String> {
    if absent(root, &format!("heads/{base}")) || absent(root, &format!("remotes/{remote}/{base}")) {
        return Err("no such ref".to_string());
    }
    let (local, tracking) = (format!("refs/heads/{base}"), format!("refs/remotes/{remote}/{base}"));
    let key = sha_of(root, &local).zip(sha_of(root, &tracking)).map(|(l, t)| format!("{l} {t}"));
    held_on(root, "branch-guard-ahead", key, || git(root, &["rev-list", "--count", &format!("{remote}/{base}..{local}")])?.parse().map_err(|e| format!("not a count: {e}")))
}

pub fn check(reg: &Registry) -> Audit {
    let (mut lines, mut any, mut bad) = (Vec::new(), false, false);
    for name in reg.all() {
        let Some(root) = reg.root(&name) else { continue };
        let path = Path::new(&root);
        if !path.join(".git").is_dir() {
            continue;
        }
        any = true;
        let Some(landref) = landref(reg, &name, &root) else {
            lines.push(format!("branch-guard: {name} — could not resolve base branch"));
            continue;
        };
        let base = repos::ref_branch(&landref);
        if let Some(t) = tip(path, &base).filter(|t| t.email.ends_with(DWARF_EMAIL_SUFFIX) && t.parents < 2) {
            let subject = if t.subject.is_empty() { "(no subject)" } else { &t.subject };
            lines.push(format!("branch-guard: DWARF COMMIT ON {name}/{base} — {} by <{}>: {subject}", t.short, t.email));
            bad = true;
        }
        let Some(remote) = remote_of(path, &landref) else { continue };
        match ahead(path, &remote, &base) {
            Ok(0) => {}
            Ok(n) => {
                lines.push(format!("branch-guard: SHARED CHECKOUT AHEAD — {name} is {n} commit(s) ahead of {remote}/{base}"));
                bad = true;
            }
            Err(_) => lines.push(format!("branch-guard: {name} — could not count commits ahead of {remote}/{base}")),
        }
    }
    match (any, bad) {
        (false, _) => Audit::NothingToAudit,
        (true, false) => Audit::Clean { notes: lines },
        (true, true) => Audit::Found { lines },
    }
}

/// The sweep's "base branches" section.
pub fn block(audit: &Audit) -> String {
    match audit {
        Audit::NothingToAudit => "  (no registered repositories — nothing to audit)".to_string(),
        Audit::Clean { notes } => {
            let mut lines = notes.clone();
            lines.push("branch-guard: clean — no dwarf commits on base branches, no checkouts ahead of remote".to_string());
            format!("  {}", lines.join("\n"))
        }
        Audit::Found { lines } => lines.iter().map(|l| format!("  {l}")).collect::<Vec<_>>().join("\n"),
    }
}

// Exception to law-a-unit-test-calls-no-binary: what git answers about a real checkout is the
// code under test, so these tests build their fixtures with git.
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    const OPERATOR: &str = "op@example.com";
    const DWARF: &str = "dwarf-x@spira.local";

    fn run(repo: &Path, email: &str, args: &[&str]) {
        let email = format!("user.email={email}");
        testkit::git(repo, &[&["-c", &email][..], args].concat());
    }

    fn commit(repo: &Path, email: &str, msg: &str) {
        run(repo, email, &["commit", "-q", "--allow-empty", "-m", msg]);
    }

    fn checkout(dir: &Path) -> PathBuf {
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        run(&repo, OPERATOR, &["init", "-q", "-b", "trunk"]);
        commit(&repo, OPERATOR, "start");
        repo
    }

    fn registry(dir: &Path, repo: &Path, base: &str) -> Registry {
        let map = format!("harness|{}|push|{base}||\n", repo.display());
        let env = BTreeMap::from([("SPIRA_HOME_REPO".to_string(), "harness".to_string())]);
        Registry::new(Some(&map), &env, dir)
    }

    fn audited(reg: &Registry) -> String {
        block(&check(reg))
    }

    // covers: UC-safety-fences-19
    #[test_id(tetvpnb)]
    #[test]
    fn a_dwarf_commit_at_the_base_tip_is_flagged_and_an_operator_commit_on_top_clears_it() {
        let d = testkit::TempDir::new("guard-dwarf");
        let repo = checkout(d.path());
        let reg = registry(d.path(), &repo, "trunk");
        assert!(matches!(check(&reg), Audit::Clean { .. }), "{}", audited(&reg));
        commit(&repo, DWARF, "stray work");
        let found = audited(&reg);
        assert!(found.starts_with("  branch-guard: DWARF COMMIT ON harness/trunk — "), "{found}");
        assert!(found.ends_with(" by <dwarf-x@spira.local>: stray work"), "{found}");
        commit(&repo, OPERATOR, "on top");
        assert_eq!(audited(&reg), "  branch-guard: clean — no dwarf commits on base branches, no checkouts ahead of remote");
    }

    // covers: UC-safety-fences-19
    #[test_id(tazl55o)]
    #[test]
    fn a_merge_tip_by_a_dwarf_is_not_flagged() {
        let d = testkit::TempDir::new("guard-merge");
        let repo = checkout(d.path());
        run(&repo, OPERATOR, &["checkout", "-q", "-b", "side"]);
        commit(&repo, OPERATOR, "side work");
        run(&repo, OPERATOR, &["checkout", "-q", "trunk"]);
        run(&repo, DWARF, &["merge", "-q", "--no-ff", "-m", "land side", "side"]);
        let reg = registry(d.path(), &repo, "trunk");
        assert!(matches!(check(&reg), Audit::Clean { .. }), "{}", audited(&reg));
    }

    // covers: UC-safety-fences-19
    #[test_id(tnltd0r)]
    #[test]
    fn a_checkout_ahead_of_its_remote_is_flagged_until_the_tracking_ref_catches_up() {
        let d = testkit::TempDir::new("guard-ahead");
        let repo = checkout(d.path());
        run(&repo, OPERATOR, &["remote", "add", "up", "does-not-exist"]);
        run(&repo, OPERATOR, &["update-ref", "refs/remotes/up/trunk", "trunk"]);
        let reg = registry(d.path(), &repo, "up/trunk");
        assert!(matches!(check(&reg), Audit::Clean { .. }), "{}", audited(&reg));
        commit(&repo, OPERATOR, "unpushed");
        assert_eq!(audited(&reg), "  branch-guard: SHARED CHECKOUT AHEAD — harness is 1 commit(s) ahead of up/trunk");
        run(&repo, OPERATOR, &["update-ref", "refs/remotes/up/trunk", "trunk"]);
        assert!(matches!(check(&reg), Audit::Clean { .. }), "a moved tracking ref alone is read again: {}", audited(&reg));
    }

    // covers: UC-safety-fences-19
    #[test_id(tx5dv3w)]
    #[test]
    fn an_unmoved_ref_starts_no_git() {
        let d = testkit::TempDir::new("guard-held");
        let repo = checkout(d.path());
        run(&repo, OPERATOR, &["remote", "add", "up", "does-not-exist"]);
        run(&repo, OPERATOR, &["update-ref", "refs/remotes/up/trunk", "trunk"]);
        let reg = registry(d.path(), &repo, "up/trunk");
        let gits = |reg: &Registry| spira_config::bounded::counting(|| check(reg)).1.get("git").copied().unwrap_or(0);
        assert_eq!(gits(&reg), 2, "the first audit reads the tip and the count");
        assert_eq!(gits(&reg), 0);
        commit(&repo, OPERATOR, "moved");
        assert_eq!(gits(&reg), 2);
    }

    // covers: UC-safety-fences-19
    #[test_id(t0j6afk)]
    #[test]
    fn a_missing_ref_or_an_unresolved_base_is_not_asked_of_git_again_while_nothing_moves() {
        let d = testkit::TempDir::new("guard-missing");
        let repo = checkout(d.path());
        run(&repo, OPERATOR, &["remote", "add", "up", "does-not-exist"]);
        let gits = |reg: &Registry| spira_config::bounded::counting(|| check(reg)).1.get("git").copied().unwrap_or(0);

        run(&repo, OPERATOR, &["update-ref", "refs/remotes/up/gone", "trunk"]);
        let no_local = registry(d.path(), &repo, "up/gone");
        gits(&no_local);
        assert_eq!(gits(&no_local), 0, "no local base branch");
        assert!(audited(&no_local).contains("could not count commits ahead of up/gone"), "{}", audited(&no_local));

        run(&repo, OPERATOR, &["branch", "untracked"]);
        run(&repo, OPERATOR, &["update-ref", "refs/heads/up/untracked", "trunk"]);
        let no_tracking = registry(d.path(), &repo, "up/untracked");
        gits(&no_tracking);
        assert_eq!(gits(&no_tracking), 0, "no remote-tracking ref");

        let unresolved = registry(d.path(), &repo, "nonesuch");
        gits(&unresolved);
        assert_eq!(gits(&unresolved), 0, "a base that does not resolve");
    }

    // covers: UC-safety-fences-19
    #[test_id(tcw5v5f)]
    #[test]
    fn a_base_git_failed_to_verify_is_asked_again_though_nothing_moved() {
        let d = testkit::TempDir::new("guard-flaky");
        let repo = checkout(d.path());
        let failed = d.join("failed");
        let flaky = d.join("flaky-git");
        testkit::write_exe(&flaky, &format!("#!/bin/sh\n[ -e {0} ] || {{ : > {0}; echo 'fatal: transient' >&2; exit 128; }}\nexec git \"$@\"\n", failed.display()));
        // A revision expression: the ref files cannot answer, so the base is verified by git.
        let reg = registry(d.path(), &repo, "trunk~0").with_git(flaky);
        let first = audited(&reg);
        assert!(first.contains("could not resolve base branch"), "{first}");
        assert!(!audited(&reg).contains("could not resolve base branch"), "{}", audited(&reg));
    }

    // covers: UC-safety-fences-19
    #[test_id(tnjvm45)]
    #[test]
    fn a_base_that_does_not_resolve_is_a_note_beside_the_clean_line() {
        let d = testkit::TempDir::new("guard-unresolved");
        let repo = checkout(d.path());
        let reg = registry(d.path(), &repo, "nonesuch");
        assert_eq!(audited(&reg), "  branch-guard: harness — could not resolve base branch\nbranch-guard: clean — no dwarf commits on base branches, no checkouts ahead of remote");
    }

    // covers: UC-safety-fences-19
    #[test_id(ttxskfb)]
    #[test]
    fn no_registered_checkout_is_nothing_to_audit() {
        let d = testkit::TempDir::new("guard-none");
        let reg = registry(d.path(), &d.join("absent"), "trunk");
        assert_eq!(check(&reg), Audit::NothingToAudit);
        assert_eq!(audited(&reg), "  (no registered repositories — nothing to audit)");
    }
}
