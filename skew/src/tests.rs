//! Unit tests over a `Fake` [`crate::ports::World`] — one scenario per decision point
//! skew.sh's own dedicated suites exercised (test-skew-check-release.sh, test-skew-copies.sh,
//! test-skew-escalate.sh, test-skew-foreign.sh, test-skew-local-release.sh,
//! test-skew-refresh.sh — all retired by this bead, their coverage moved here).

use crate::ports::{StatusRow, World};
use crate::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct Fake {
    pub env: RefCell<BTreeMap<String, String>>,
    pub repo_roots: RefCell<BTreeMap<String, PathBuf>>,
    pub repo_fields: RefCell<BTreeMap<(String, String), String>>,
    pub repo_lands: RefCell<BTreeMap<String, String>>,
    pub home_repo: RefCell<String>,
    pub landrefs: RefCell<BTreeMap<String, String>>, // keyed by repo path string
    pub ref_remotes: RefCell<BTreeMap<String, String>>,
    pub git_repos: RefCell<Vec<PathBuf>>,
    pub tags: RefCell<BTreeMap<String, Vec<String>>>, // keyed by repo path string
    pub local_tag_sidecars: RefCell<BTreeMap<String, Vec<String>>>, // keyed by dir path string
    pub rev_parse: RefCell<BTreeMap<String, String>>, // key "<repo>|<rev>"
    pub rev_parse_short: RefCell<BTreeMap<String, String>>,
    pub ancestors: RefCell<Vec<(String, String)>>, // (ancestor, descendant) pairs that ARE ancestors
    pub rev_list_counts: RefCell<BTreeMap<String, u64>>,
    pub current_branches: RefCell<BTreeMap<String, String>>,
    pub dirty: RefCell<BTreeMap<String, String>>,
    pub fetch_ok: RefCell<bool>,
    pub diff_statuses: RefCell<BTreeMap<String, Vec<StatusRow>>>,
    pub show_files: RefCell<BTreeMap<String, Vec<u8>>>, // key "<repo>|<rev>|<path>"
    pub file_modes: RefCell<BTreeMap<String, String>>,
    pub release_verify_ok: RefCell<bool>,
    pub release_verify_out: RefCell<String>,
    pub release_status: RefCell<String>,
    pub release_bva_ok: RefCell<bool>,
    pub overrides_applied: RefCell<Vec<PathBuf>>,
    pub install_diff_rc: RefCell<i32>,
    pub install_diff_out: RefCell<String>,
    pub which: RefCell<BTreeMap<String, String>>,
    pub not_executable: RefCell<Vec<PathBuf>>,
    pub gh_tags_json: RefCell<Option<Result<String, String>>>,
    pub mail_calls: RefCell<Vec<(String, String, String)>>,
    pub mail_ok: RefCell<bool>,
    pub files: RefCell<BTreeMap<PathBuf, String>>,
    pub symlinks: RefCell<BTreeMap<PathBuf, String>>,
    pub existing: RefCell<Vec<PathBuf>>,
    pub stamps: RefCell<BTreeMap<String, String>>,
    pub written: RefCell<BTreeMap<PathBuf, (Vec<u8>, bool)>>,
    pub removed: RefCell<Vec<PathBuf>>,
    pub stdout: RefCell<Vec<String>>,
    pub stderr: RefCell<Vec<String>>,
    pub changed: RefCell<BTreeMap<String, Vec<String>>>,
    pub harness_dirs_ref: RefCell<BTreeMap<String, Vec<String>>>, // key "<repo>|<ref>"
    pub harness_dirs_wd: RefCell<BTreeMap<String, Vec<String>>>,  // key "<repo>"
    pub now: RefCell<String>,
}

impl Fake {
    fn set_env(&self, k: &str, v: &str) {
        self.env.borrow_mut().insert(k.to_string(), v.to_string());
    }
    fn stdout_joined(&self) -> String {
        self.stdout.borrow().join("\n")
    }
    fn stderr_joined(&self) -> String {
        self.stderr.borrow().join("\n")
    }
}

impl World for Fake {
    fn repo_names(&self) -> Vec<String> {
        self.repo_roots.borrow().keys().cloned().collect()
    }
    fn repo_root(&self, name: &str) -> Option<PathBuf> {
        self.repo_roots.borrow().get(name).cloned()
    }
    fn repo_field(&self, name: &str, field: &str) -> Option<String> {
        self.repo_fields.borrow().get(&(name.to_string(), field.to_string())).cloned()
    }
    fn same_repo(&self, a: &Path, b: &Path) -> bool {
        a == b
    }
    fn home_repo(&self) -> String {
        self.home_repo.borrow().clone()
    }
    fn landref(&self, repo: &Path) -> Option<String> {
        self.landrefs.borrow().get(&repo.to_string_lossy().into_owned()).cloned()
    }
    fn repo_land(&self, name: &str) -> String {
        self.repo_lands.borrow().get(name).cloned().unwrap_or_default()
    }
    fn ref_remote(&self, base: &str, _repo: &Path) -> Option<String> {
        self.ref_remotes.borrow().get(base).cloned()
    }
    fn ref_branch(&self, base: &str) -> String {
        base.split_once('/').map(|(_, b)| b.to_string()).unwrap_or_else(|| base.to_string())
    }
    fn is_git_repo(&self, p: &Path) -> bool {
        self.git_repos.borrow().iter().any(|r| r == p)
    }
    fn harness_in(&self, repo: &Path) -> Vec<String> {
        self.harness_dirs_wd.borrow().get(&repo.to_string_lossy().into_owned()).cloned().unwrap_or_default()
    }
    fn harness_in_ref(&self, repo: &Path, ref_: &str) -> Vec<String> {
        let key = format!("{}|{ref_}", repo.to_string_lossy());
        self.harness_dirs_ref.borrow().get(&key).cloned().unwrap_or_default()
    }
    fn changed_files(&self, repo: &Path, base: &str, ref_: &str) -> Vec<String> {
        let key = format!("{}|{base}|{ref_}", repo.to_string_lossy());
        self.changed.borrow().get(&key).cloned().unwrap_or_default()
    }
    fn tags_matching(&self, repo: &Path, _pattern: &str) -> Vec<String> {
        self.tags.borrow().get(&repo.to_string_lossy().into_owned()).cloned().unwrap_or_default()
    }
    fn local_tag_sidecars(&self, dir: &Path) -> Vec<String> {
        self.local_tag_sidecars.borrow().get(&dir.to_string_lossy().into_owned()).cloned().unwrap_or_default()
    }
    fn rev_parse(&self, repo: &Path, rev: &str, _peel: bool) -> Option<String> {
        self.rev_parse.borrow().get(&format!("{}|{rev}", repo.to_string_lossy())).cloned()
    }
    fn rev_parse_short(&self, repo: &Path, rev: &str) -> Option<String> {
        self.rev_parse_short.borrow().get(&format!("{}|{rev}", repo.to_string_lossy())).cloned()
    }
    fn is_ancestor(&self, _repo: &Path, ancestor: &str, descendant: &str) -> bool {
        self.ancestors.borrow().iter().any(|(a, d)| a == ancestor && d == descendant)
    }
    fn rev_list_count(&self, repo: &Path, range: &str) -> Option<u64> {
        self.rev_list_counts.borrow().get(&format!("{}|{range}", repo.to_string_lossy())).copied()
    }
    fn current_branch(&self, repo: &Path) -> String {
        self.current_branches.borrow().get(&repo.to_string_lossy().into_owned()).cloned().unwrap_or_default()
    }
    fn dirty_tracked(&self, repo: &Path) -> String {
        self.dirty.borrow().get(&repo.to_string_lossy().into_owned()).cloned().unwrap_or_default()
    }
    fn stash_push(&self, _repo: &Path, _tag: &str) -> Result<(), String> {
        Ok(())
    }
    fn fetch(&self, _repo: &Path, _remote: &str) -> bool {
        *self.fetch_ok.borrow()
    }
    fn diff_status(&self, repo: &Path, range: &str) -> Vec<StatusRow> {
        self.diff_statuses.borrow().get(&format!("{}|{range}", repo.to_string_lossy())).cloned().unwrap_or_default()
    }
    fn show_file(&self, repo: &Path, rev: &str, path: &str) -> Option<Vec<u8>> {
        self.show_files.borrow().get(&format!("{}|{rev}|{path}", repo.to_string_lossy())).cloned()
    }
    fn file_mode(&self, repo: &Path, rev: &str, path: &str) -> Option<String> {
        self.file_modes.borrow().get(&format!("{}|{rev}|{path}", repo.to_string_lossy())).cloned()
    }
    fn reset_mixed(&self, _repo: &Path, _rev: &str) -> bool {
        true
    }
    fn merge_ff_only(&self, _repo: &Path, _rev: &str) -> bool {
        true
    }
    fn release_verify_no_pre_activate(&self, _name: &str, _releases: Option<&Path>) -> (bool, String) {
        (*self.release_verify_ok.borrow(), self.release_verify_out.borrow().clone())
    }
    fn release_status(&self) -> String {
        self.release_status.borrow().clone()
    }
    fn release_build_verify_activate(&self, _sha: &str, _repo: &Path, _base: &str, _releases: &Path) -> Result<(), String> {
        if *self.release_bva_ok.borrow() { Ok(()) } else { Err("build failed".into()) }
    }
    fn overrides_apply(&self, repo: &Path) {
        self.overrides_applied.borrow_mut().push(repo.to_path_buf());
    }
    fn install_diff(&self, _installer: &Path) -> (i32, String) {
        (*self.install_diff_rc.borrow(), self.install_diff_out.borrow().clone())
    }
    fn gh_release_list(&self, _slug: &str) -> Result<String, String> {
        self.gh_tags_json.borrow().clone().unwrap_or_else(|| Err("no gh fixture set".into()))
    }
    fn mail_send_question(&self, subject: &str, body: &str, default_action: &str) -> Result<(), String> {
        self.mail_calls.borrow_mut().push((subject.to_string(), body.to_string(), default_action.to_string()));
        if *self.mail_ok.borrow() { Ok(()) } else { Err("mail.sh failed".into()) }
    }
    fn now_stamp(&self) -> String {
        self.now.borrow().clone()
    }
    fn env(&self, k: &str) -> Option<String> {
        self.env.borrow().get(k).cloned()
    }
    fn is_symlink(&self, p: &Path) -> bool {
        self.symlinks.borrow().contains_key(p)
    }
    fn readlink(&self, p: &Path) -> Option<String> {
        self.symlinks.borrow().get(p).cloned()
    }
    fn exists(&self, p: &Path) -> bool {
        self.existing.borrow().contains(&p.to_path_buf()) || self.files.borrow().contains_key(p) || self.symlinks.borrow().contains_key(p)
    }
    fn read_to_string(&self, p: &Path) -> Option<String> {
        self.files.borrow().get(p).cloned()
    }
    fn which(&self, name: &str) -> Option<String> {
        self.which.borrow().get(name).cloned()
    }
    fn is_executable(&self, p: &Path) -> bool {
        !self.not_executable.borrow().contains(&p.to_path_buf())
    }
    fn write_staged(&self, p: &Path, content: &[u8], executable: bool) -> Result<(), String> {
        self.written.borrow_mut().insert(p.to_path_buf(), (content.to_vec(), executable));
        Ok(())
    }
    fn remove_file(&self, p: &Path) {
        self.removed.borrow_mut().push(p.to_path_buf());
    }
    fn mkdir_p(&self, _p: &Path) {}
    fn stamp_read(&self, key: &str) -> Option<String> {
        self.stamps.borrow().get(key).cloned()
    }
    fn stamp_write(&self, key: &str, val: &str) {
        self.stamps.borrow_mut().insert(key.to_string(), val.to_string());
    }
    fn out(&self, s: &str) {
        self.stdout.borrow_mut().push(s.to_string());
    }
    fn err(&self, s: &str) {
        self.stderr.borrow_mut().push(s.to_string());
    }
}

// ============================================================================ check()

#[test]
fn check_no_releases_env_cannot_check() {
    let f = Fake::default();
    assert_eq!(check(&f, false), EXIT_CANNOT_CHECK);
    assert!(f.stderr_joined().contains("SPIRA_RELEASES is not set"));
}

#[test]
fn check_not_latest_and_manifest_mismatch_both_fire() {
    let f = Fake::default();
    f.set_env("SPIRA_RELEASES", "/releases");
    f.symlinks.borrow_mut().insert(PathBuf::from("/releases/current"), "rel-2".into());
    f.files.borrow_mut().insert(PathBuf::from("/releases/rel-2/MANIFEST"), format!("commit {}\n", "a".repeat(40)));
    f.set_env("SPIRA_REPO", "/repo");
    f.git_repos.borrow_mut().push(PathBuf::from("/repo"));
    f.repo_lands.borrow_mut().insert("home".into(), "push".into());
    *f.home_repo.borrow_mut() = "home".into();
    f.tags.borrow_mut().insert("/repo".into(), vec!["spira-release-1".into(), "spira-release-2".into()]);
    // release_tag resolves to spira-release-1 (not latest), whose tag commit differs from MANIFEST.
    f.rev_parse.borrow_mut().insert(format!("/repo|spira-release-1^{{commit}}"), "b".repeat(40));
    f.rev_parse.borrow_mut().insert(format!("/repo|spira-release-2^{{commit}}"), "c".repeat(40));
    f.files.borrow_mut().insert(PathBuf::from("/releases/.tags/rel-2"), "spira-release-1".into());

    let rc = check(&f, false);
    assert_eq!(rc, EXIT_FINDING);
    let out = f.stdout_joined();
    assert!(out.contains("NOT-LATEST"), "{out}");
    assert!(out.contains("MANIFEST-MISMATCH"), "{out}");
}

#[test]
fn check_clean_when_tag_matches_and_is_latest() {
    let f = Fake::default();
    f.set_env("SPIRA_RELEASES", "/releases");
    f.symlinks.borrow_mut().insert(PathBuf::from("/releases/current"), "rel-2".into());
    let sha = "a".repeat(40);
    f.files.borrow_mut().insert(PathBuf::from("/releases/rel-2/MANIFEST"), format!("commit {sha}\n"));
    f.set_env("SPIRA_REPO", "/repo");
    f.git_repos.borrow_mut().push(PathBuf::from("/repo"));
    f.repo_lands.borrow_mut().insert("home".into(), "push".into());
    *f.home_repo.borrow_mut() = "home".into();
    f.tags.borrow_mut().insert("/repo".into(), vec!["spira-release-1".into(), "spira-release-2".into()]);
    f.rev_parse.borrow_mut().insert("/repo|spira-release-2^{commit}".into(), sha.clone());
    f.files.borrow_mut().insert(PathBuf::from("/releases/.tags/rel-2"), "spira-release-2".into());

    assert_eq!(check(&f, false), EXIT_OK);
    assert!(f.stdout_joined().contains("in effect"));
}

/// LOCAL RELEASE SOURCE: `SPIRA_RELEASE_REPO` names a plain directory of tarballs with
/// `.tag` sidecars (not a git repo) — `resolve_all_tags` must read those sidecar files
/// directly, never dial `git tag -l` against the directory (which returns nothing and was
/// read as "no release tags found", caught live by testenv's "local-dir" cases, sp-yyk47).
#[test]
fn resolve_all_tags_reads_local_tag_sidecars_not_git_tag() {
    let f = Fake::default();
    f.set_env("SPIRA_RELEASE_REPO", "/release-source");
    f.existing.borrow_mut().push(PathBuf::from("/release-source"));
    f.local_tag_sidecars
        .borrow_mut()
        .insert("/release-source".into(), vec!["spira-release-spira-20260912T120000Z".into()]);
    // A decoy under `tags_matching` (the git-tag seam) that must never be consulted here —
    // if it were, this test would see the decoy instead of the sidecar-derived tag.
    f.tags.borrow_mut().insert("/repo".into(), vec![]);

    let tags = resolve_all_tags(&f, Path::new("/repo")).expect("local-dir tags resolve");
    assert_eq!(tags, vec!["spira-release-spira-20260912T120000Z".to_string()]);
}

#[test]
fn check_cannot_verify_alone_is_exit_3_not_1() {
    let f = Fake::default();
    f.set_env("SPIRA_RELEASES", "/releases");
    f.symlinks.borrow_mut().insert(PathBuf::from("/releases/current"), "rel-1".into());
    f.files.borrow_mut().insert(PathBuf::from("/releases/rel-1/MANIFEST"), format!("commit {}\n", "a".repeat(40)));
    f.set_env("SPIRA_REPO", "/repo");
    f.git_repos.borrow_mut().push(PathBuf::from("/repo"));
    f.repo_lands.borrow_mut().insert("home".into(), "push".into());
    *f.home_repo.borrow_mut() = "home".into();
    // No tags resolve to the MANIFEST commit -> release_tag stays empty -> CANNOT-VERIFY only.
    f.tags.borrow_mut().insert("/repo".into(), vec!["spira-release-1".into()]);
    f.rev_parse.borrow_mut().insert("/repo|spira-release-1^{commit}".into(), "b".repeat(40));

    let rc = check(&f, false);
    assert_eq!(rc, EXIT_CANNOT_CHECK);
    assert!(f.stdout_joined().contains("CANNOT-VERIFY"));
}

/// FRESH RELEASE INSTALL (sp-wecsq): a release is activated (symlink + MANIFEST both
/// present), `home_repo()` is not `queue.local` (no `repo:spira` row at all on a box that
/// only runs installed releases), and `SPIRA_REPO` names no real git checkout -- exactly
/// release acceptance phase B's shape, where `spira-skew-prod.service` exited 3 on every
/// run. There is no git checkout anywhere for this branch to resolve a release tag's commit
/// against, so this is not a transient "could not check" (CANNOT-VERIFY, exit 3); the
/// question does not apply on this kind of install at all, and must say so on stdout at
/// exit 0 -- never a silent pass, and never the escalation path either.
#[test]
fn check_no_harness_checkout_is_not_applicable_not_cannot_verify() {
    let f = Fake::default();
    f.set_env("SPIRA_RELEASES", "/releases");
    f.symlinks.borrow_mut().insert(PathBuf::from("/releases/current"), "rel-1".into());
    f.files.borrow_mut().insert(PathBuf::from("/releases/rel-1/MANIFEST"), format!("commit {}\n", "a".repeat(40)));
    // No SPIRA_REPO set, no repo:spira row, no git checkout anywhere -- a release-only box.
    *f.home_repo.borrow_mut() = "spira".into();

    let rc = check(&f, true); // --escalate, to prove this path never mails anyone.
    assert_eq!(rc, EXIT_OK);
    assert!(f.stdout_joined().contains("not applicable"), "{}", f.stdout_joined());
    assert!(f.stderr_joined().is_empty(), "{}", f.stderr_joined());
    assert!(f.mail_calls.borrow().is_empty());
}

#[test]
fn check_checkout_mode_delegates_to_gap() {
    let f = Fake::default();
    f.set_env("SPIRA_RELEASES", "/releases"); // current is not a symlink -> checkout mode
    f.set_env("SPIRA_REPO", "/repo");
    f.git_repos.borrow_mut().push(PathBuf::from("/repo"));
    f.repo_lands.borrow_mut().insert("home".into(), "push".into());
    *f.home_repo.borrow_mut() = "home".into();
    f.landrefs.borrow_mut().insert("/repo".into(), "origin/main".into());
    f.rev_parse.borrow_mut().insert("/repo|origin/main".into(), "x".into());
    f.rev_list_counts.borrow_mut().insert("/repo|HEAD..origin/main".into(), 0);
    f.rev_parse_short.borrow_mut().insert("/repo|HEAD".into(), "abc1234".into());
    f.rev_parse_short.borrow_mut().insert("/repo|origin/main".into(), "x".into());

    assert_eq!(check(&f, false), EXIT_OK);
    assert!(f.stdout_joined().contains("0 commits behind"));
}

// ============================================================================ check_local()

#[test]
fn check_local_tampered_and_behind_both_fire() {
    let f = Fake::default();
    *f.home_repo.borrow_mut() = "home".into();
    f.repo_roots.borrow_mut().insert("home".into(), PathBuf::from("/repo"));
    f.git_repos.borrow_mut().push(PathBuf::from("/repo"));
    f.landrefs.borrow_mut().insert("home".into(), "local/main".into());
    f.rev_parse.borrow_mut().insert("/repo|local/main".into(), "tip123".into());
    *f.release_verify_ok.borrow_mut() = false;
    *f.release_verify_out.borrow_mut() = "hash mismatch".into();
    f.ancestors.borrow_mut().push(("old456".into(), "tip123".into()));
    f.rev_list_counts.borrow_mut().insert("/repo|old456..tip123".into(), 3);

    let rc = check_local(&f, "rel-1", "old456", false);
    assert_eq!(rc, EXIT_FINDING);
    let out = f.stdout_joined();
    assert!(out.contains("LOCAL-TAMPERED"), "{out}");
    assert!(out.contains("LOCAL-BEHIND"), "{out}");
}

/// BY NAME, NOT BY PATH — same scar as `refresh_queue_local_resolves_landref_by_name_never_by_path`,
/// for `check_local`'s own call site.
#[test]
fn check_local_resolves_landref_by_name_never_by_path() {
    let f = Fake::default();
    *f.home_repo.borrow_mut() = "home".into();
    f.repo_roots.borrow_mut().insert("home".into(), PathBuf::from("/repo"));
    f.git_repos.borrow_mut().push(PathBuf::from("/repo"));
    f.landrefs.borrow_mut().insert("home".into(), "local/main".into());
    // Decoy under the path key — must never be consulted.
    f.landrefs.borrow_mut().insert("/repo".into(), "origin/decoy".into());
    f.rev_parse.borrow_mut().insert("/repo|local/main".into(), "tip123".into());
    f.rev_parse.borrow_mut().insert("/repo|origin/decoy".into(), "wrong-tip".into());
    *f.release_verify_ok.borrow_mut() = true;

    let rc = check_local(&f, "rel-1", "tip123", false);
    assert_eq!(rc, EXIT_OK);
    assert!(f.stdout_joined().contains("in effect"));
}

#[test]
fn check_local_hotfix_is_clean() {
    let f = Fake::default();
    *f.home_repo.borrow_mut() = "home".into();
    f.repo_roots.borrow_mut().insert("home".into(), PathBuf::from("/repo"));
    f.git_repos.borrow_mut().push(PathBuf::from("/repo"));
    f.landrefs.borrow_mut().insert("home".into(), "local/main".into());
    f.rev_parse.borrow_mut().insert("/repo|local/main".into(), "tip123".into());
    *f.release_verify_ok.borrow_mut() = true;
    *f.release_status.borrow_mut() = "RUNNING UNLANDED hot999: the operator's own stop-the-world fix\n".into();

    let rc = check_local(&f, "rel-1", "hot999", false);
    assert_eq!(rc, EXIT_OK);
    assert!(f.stdout_joined().contains("recorded standing hotfix"));
}

#[test]
fn check_local_unresolvable_divergence_is_cannot_verify() {
    let f = Fake::default();
    *f.home_repo.borrow_mut() = "home".into();
    f.repo_roots.borrow_mut().insert("home".into(), PathBuf::from("/repo"));
    f.git_repos.borrow_mut().push(PathBuf::from("/repo"));
    f.landrefs.borrow_mut().insert("home".into(), "local/main".into());
    f.rev_parse.borrow_mut().insert("/repo|local/main".into(), "tip123".into());
    *f.release_verify_ok.borrow_mut() = true;
    // manifest_commit is neither the tip nor a recorded hotfix, and not an ancestor either.
    let rc = check_local(&f, "rel-1", "weird000", false);
    assert_eq!(rc, EXIT_CANNOT_CHECK);
    assert!(f.stdout_joined().contains("CANNOT-VERIFY"));
}

// ============================================================================ foreign()

#[test]
fn foreign_override_env_exempts() {
    let f = Fake::default();
    f.set_env("SPIRA_ALLOW_FOREIGN_HARNESS", "1");
    assert_eq!(foreign(&f, "/some/repo", "base", "ref"), 0);
}

#[test]
fn foreign_missing_args_refuses() {
    let f = Fake::default();
    assert_eq!(foreign(&f, "", "base", "ref"), 1);
}

#[test]
fn foreign_exempts_own_repo() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "/harness");
    assert_eq!(foreign(&f, "/harness", "base", "ref"), 0);
}

#[test]
fn foreign_no_copy_in_ref_allows() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "/elsewhere");
    *f.home_repo.borrow_mut() = "home".into();
    assert_eq!(foreign(&f, "/some/repo", "base", "ref"), 0);
}

#[test]
fn foreign_refuses_a_change_inside_the_vendored_copy() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "/elsewhere");
    *f.home_repo.borrow_mut() = "home".into();
    f.harness_dirs_ref.borrow_mut().insert("/some/repo|ref".into(), vec!["vendor/harness".into()]);
    f.changed.borrow_mut().insert("/some/repo|base|ref".into(), vec!["vendor/harness/spira/lib.sh".into(), "README.md".into()]);

    let rc = foreign(&f, "/some/repo", "base", "ref");
    assert_eq!(rc, 1);
    assert!(f.stdout_joined().contains("vendor/harness/spira/lib.sh"));
    assert!(!f.stdout_joined().contains("README.md"));
    assert!(f.stderr_joined().contains("REFUSED by skew"));
}

#[test]
fn foreign_dot_directory_matches_every_changed_file() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "/elsewhere");
    *f.home_repo.borrow_mut() = "home".into();
    f.harness_dirs_ref.borrow_mut().insert("/some/repo|ref".into(), vec![".".into()]);
    f.changed.borrow_mut().insert("/some/repo|base|ref".into(), vec!["anything.txt".into()]);

    assert_eq!(foreign(&f, "/some/repo", "base", "ref"), 1);
}

#[test]
fn foreign_change_outside_the_copy_allows() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "/elsewhere");
    *f.home_repo.borrow_mut() = "home".into();
    f.harness_dirs_ref.borrow_mut().insert("/some/repo|ref".into(), vec!["vendor/harness".into()]);
    f.changed.borrow_mut().insert("/some/repo|base|ref".into(), vec!["README.md".into()]);

    assert_eq!(foreign(&f, "/some/repo", "base", "ref"), 0);
}

// ============================================================================ copies()

#[test]
fn copies_found_reports_self_and_second() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "/repo-a");
    f.repo_roots.borrow_mut().insert("a".into(), PathBuf::from("/repo-a"));
    f.repo_roots.borrow_mut().insert("b".into(), PathBuf::from("/repo-b"));
    f.existing.borrow_mut().push(PathBuf::from("/repo-a"));
    f.existing.borrow_mut().push(PathBuf::from("/repo-b"));
    f.harness_dirs_wd.borrow_mut().insert("/repo-a".into(), vec!["vendor/h".into()]);
    f.harness_dirs_wd.borrow_mut().insert("/repo-b".into(), vec!["vendor/h".into()]);

    assert_eq!(copies(&f), 0);
    let out = f.stdout_joined();
    assert!(out.contains("a /repo-a vendor/h self"));
    assert!(out.contains("b /repo-b vendor/h second"));
}

#[test]
fn copies_none_found_is_a_finding() {
    let f = Fake::default();
    assert_eq!(copies(&f), 1);
}

// ============================================================================ gap()

#[test]
fn gap_clean() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "/repo");
    f.git_repos.borrow_mut().push(PathBuf::from("/repo"));
    f.landrefs.borrow_mut().insert("/repo".into(), "origin/main".into());
    f.rev_parse.borrow_mut().insert("/repo|origin/main".into(), "x".into());
    f.rev_list_counts.borrow_mut().insert("/repo|HEAD..origin/main".into(), 0);
    f.rev_parse_short.borrow_mut().insert("/repo|HEAD".into(), "abc".into());
    f.rev_parse_short.borrow_mut().insert("/repo|origin/main".into(), "x".into());

    assert_eq!(gap(&f, None), EXIT_OK);
}

#[test]
fn gap_not_a_checkout_cannot_check() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "/repo");
    assert_eq!(gap(&f, None), EXIT_CANNOT_CHECK);
    assert!(f.stderr_joined().contains("not a git checkout"));
}

#[test]
fn gap_behind_is_a_finding() {
    let f = Fake::default();
    f.git_repos.borrow_mut().push(PathBuf::from("/r"));
    f.landrefs.borrow_mut().insert("/r".into(), "origin/main".into());
    f.rev_parse.borrow_mut().insert("/r|origin/main".into(), "x".into());
    f.rev_list_counts.borrow_mut().insert("/r|HEAD..origin/main".into(), 4);
    f.rev_parse_short.borrow_mut().insert("/r|HEAD".into(), "abc".into());
    f.rev_parse_short.borrow_mut().insert("/r|origin/main".into(), "x".into());

    assert_eq!(gap(&f, Some("/r")), EXIT_FINDING);
    assert!(f.stdout_joined().contains("4 commit(s) behind"));
}

// ============================================================================ escalate()

#[test]
fn escalate_sends_once_per_condition_per_run() {
    let f = Fake::default();
    *f.mail_ok.borrow_mut() = true;
    escalate(&f, "v2:NOT-LATEST=1 MANIFEST-MISMATCH=0", "NOT-LATEST ...\n");
    assert_eq!(f.mail_calls.borrow().len(), 1);
    escalate(&f, "v2:NOT-LATEST=1 MANIFEST-MISMATCH=0", "NOT-LATEST ...\n");
    assert_eq!(f.mail_calls.borrow().len(), 1, "same condition key must not re-send");
    assert!(f.stdout_joined().contains("condition already reported"));
}

#[test]
fn escalate_distinct_conditions_both_send() {
    let f = Fake::default();
    escalate(&f, "v2:NOT-LATEST=1 MANIFEST-MISMATCH=0", "a");
    escalate(&f, "v2:NOT-LATEST=0 MANIFEST-MISMATCH=1", "b");
    assert_eq!(f.mail_calls.borrow().len(), 2);
}

#[test]
fn escalate_picks_the_local_behind_default_action() {
    let f = Fake::default();
    escalate(&f, "v2:LOCAL-BEHIND=1 LOCAL-TAMPERED=0", "LOCAL-BEHIND ...\n");
    let (_, _, default_action) = f.mail_calls.borrow()[0].clone();
    assert!(default_action.contains("queue land-local"));
}

// ============================================================================ refresh()

#[test]
fn refresh_queue_local_match_is_clean() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "/repo");
    *f.home_repo.borrow_mut() = "home".into();
    f.repo_roots.borrow_mut().insert("home".into(), PathBuf::from("/repo"));
    f.repo_lands.borrow_mut().insert("home".into(), "queue.local".into());
    f.git_repos.borrow_mut().push(PathBuf::from("/repo"));
    f.landrefs.borrow_mut().insert("home".into(), "local/main".into());
    f.rev_parse.borrow_mut().insert("/repo|local/main".into(), "tip".into());
    f.rev_parse.borrow_mut().insert("/repo|HEAD".into(), "tip".into());

    assert_eq!(refresh(&f, None), 0);
    assert!(f.stdout_joined().contains("nothing to deploy"));
}

/// BY NAME, NOT BY PATH (skew.sh's own `spira_landref "$name"`, not `"$repo"`): a decoy
/// landref registered under the PATH key, disagreeing with the real one under the NAME
/// key, must never be consulted. Caught live by testenv against sp-yyk47's merged tree —
/// the first port used `w.landref(repo)` here and silently resolved the wrong ref whenever
/// a caller's path-derived guess disagreed with the name already in hand.
#[test]
fn refresh_queue_local_resolves_landref_by_name_never_by_path() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "/repo");
    *f.home_repo.borrow_mut() = "home".into();
    f.repo_roots.borrow_mut().insert("home".into(), PathBuf::from("/repo"));
    f.repo_lands.borrow_mut().insert("home".into(), "queue.local".into());
    f.git_repos.borrow_mut().push(PathBuf::from("/repo"));
    f.landrefs.borrow_mut().insert("home".into(), "local/main".into());
    // Decoy: a DIFFERENT ref under the path key. If the implementation ever regresses to
    // `landref(repo)`, this is what it would pick up instead, resolving to "wrong-tip"
    // rather than "tip" and turning a clean match into a false LOCAL-SKEW.
    f.landrefs.borrow_mut().insert("/repo".into(), "origin/decoy".into());
    f.rev_parse.borrow_mut().insert("/repo|local/main".into(), "tip".into());
    f.rev_parse.borrow_mut().insert("/repo|origin/decoy".into(), "wrong-tip".into());
    f.rev_parse.borrow_mut().insert("/repo|HEAD".into(), "tip".into());

    assert_eq!(refresh(&f, None), 0);
    assert!(f.stdout_joined().contains("nothing to deploy"));
    assert!(f.mail_calls.borrow().is_empty());
}

#[test]
fn refresh_queue_local_mismatch_escalates_local_skew() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "/repo");
    *f.home_repo.borrow_mut() = "home".into();
    f.repo_roots.borrow_mut().insert("home".into(), PathBuf::from("/repo"));
    f.repo_lands.borrow_mut().insert("home".into(), "queue.local".into());
    f.git_repos.borrow_mut().push(PathBuf::from("/repo"));
    f.landrefs.borrow_mut().insert("home".into(), "local/main".into());
    f.rev_parse.borrow_mut().insert("/repo|local/main".into(), "tip".into());
    f.rev_parse.borrow_mut().insert("/repo|HEAD".into(), "stale".into());

    let rc = refresh(&f, None);
    assert_eq!(rc, 1);
    assert!(f.stdout_joined().contains("LOCAL-SKEW"));
    assert_eq!(f.mail_calls.borrow().len(), 1);
}

#[test]
fn refresh_queue_local_hotfix_never_resets() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "/repo");
    *f.home_repo.borrow_mut() = "home".into();
    f.repo_roots.borrow_mut().insert("home".into(), PathBuf::from("/repo"));
    f.repo_lands.borrow_mut().insert("home".into(), "queue.local".into());
    f.git_repos.borrow_mut().push(PathBuf::from("/repo"));
    f.landrefs.borrow_mut().insert("home".into(), "local/main".into());
    f.rev_parse.borrow_mut().insert("/repo|local/main".into(), "tip".into());
    f.rev_parse.borrow_mut().insert("/repo|HEAD".into(), "stale".into());
    *f.release_status.borrow_mut() = "RUNNING UNLANDED stale: deliberate\n".into();

    assert_eq!(refresh(&f, None), 0);
    assert!(f.stdout_joined().contains("standing hotfix"));
    assert!(f.mail_calls.borrow().is_empty());
}

#[test]
fn refresh_release_mode_already_at_base() {
    let f = Fake::default();
    f.set_env("SPIRA_RELEASES", "/releases");
    f.symlinks.borrow_mut().insert(PathBuf::from("/releases/current"), "rel-1".into());
    f.set_env("SPIRA_REPO", "/repo");
    f.git_repos.borrow_mut().push(PathBuf::from("/repo"));
    f.landrefs.borrow_mut().insert("/repo".into(), "origin/main".into());
    f.rev_list_counts.borrow_mut().insert("/repo|HEAD..origin/main".into(), 0);

    assert_eq!(refresh(&f, Some("/repo")), 0);
    assert!(f.stdout_joined().contains("already at"));
}

#[test]
fn refresh_release_mode_builds_and_activates() {
    let f = Fake::default();
    f.set_env("SPIRA_RELEASES", "/releases");
    f.symlinks.borrow_mut().insert(PathBuf::from("/releases/current"), "rel-1".into());
    f.set_env("SPIRA_REPO", "/repo");
    f.git_repos.borrow_mut().push(PathBuf::from("/repo"));
    f.landrefs.borrow_mut().insert("/repo".into(), "origin/main".into());
    f.rev_list_counts.borrow_mut().insert("/repo|HEAD..origin/main".into(), 2);
    f.rev_parse.borrow_mut().insert("/repo|HEAD".into(), "newsha".into());
    *f.release_bva_ok.borrow_mut() = true;

    assert_eq!(refresh(&f, Some("/repo")), 0);
    assert!(f.stdout_joined().contains("refreshed — new release installed (2 commit(s))"));
    assert_eq!(f.overrides_applied.borrow().len(), 1);
}

#[test]
fn refresh_checkout_mode_declines_off_base_branch() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "/repo");
    f.git_repos.borrow_mut().push(PathBuf::from("/repo"));
    f.landrefs.borrow_mut().insert("/repo".into(), "origin/main".into());
    f.rev_list_counts.borrow_mut().insert("/repo|HEAD..origin/main".into(), 1);
    f.current_branches.borrow_mut().insert("/repo".into(), "some-feature".into());

    let rc = refresh(&f, Some("/repo"));
    assert_eq!(rc, 1);
    assert!(f.stdout_joined().contains("declined"));
}

#[test]
fn refresh_checkout_mode_stages_and_swaps() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "/repo");
    f.git_repos.borrow_mut().push(PathBuf::from("/repo"));
    f.landrefs.borrow_mut().insert("/repo".into(), "origin/main".into());
    f.rev_list_counts.borrow_mut().insert("/repo|HEAD..origin/main".into(), 2);
    f.current_branches.borrow_mut().insert("/repo".into(), "main".into());
    f.diff_statuses.borrow_mut().insert(
        "/repo|HEAD..origin/main".into(),
        vec![('M', "spira/conf.sh".into()), ('A', "spira/new.sh".into()), ('D', "spira/old.sh".into())],
    );
    f.show_files.borrow_mut().insert("/repo|origin/main|spira/conf.sh".into(), b"new conf".to_vec());
    f.show_files.borrow_mut().insert("/repo|origin/main|spira/new.sh".into(), b"#!/bin/bash\n".to_vec());
    f.file_modes.borrow_mut().insert("/repo|origin/main|spira/new.sh".into(), "100755".into());

    let rc = refresh(&f, Some("/repo"));
    assert_eq!(rc, 0);
    assert!(f.written.borrow().contains_key(&PathBuf::from("/repo/spira/conf.sh")));
    let (content, executable) = f.written.borrow().get(&PathBuf::from("/repo/spira/new.sh")).cloned().unwrap();
    assert_eq!(content, b"#!/bin/bash\n");
    assert!(executable);
    assert!(f.removed.borrow().contains(&PathBuf::from("/repo/spira/old.sh")));
    assert_eq!(f.overrides_applied.borrow().len(), 1);
}

// ============================================================================ units()

#[test]
fn units_installer_missing_cannot_check() {
    let f = Fake::default();
    // units-install is a compiled binary now (sp-31dm0): neither SPIRA_INSTALL_SH nor a
    // PATH hit resolves it -- units() must refuse, not guess a bash-script path.
    assert_eq!(units(&f), EXIT_CANNOT_CHECK);
    assert!(f.stderr_joined().contains("units-install"));
}

#[test]
fn units_clean() {
    let f = Fake::default();
    f.which.borrow_mut().insert("units-install".into(), "/bin/units-install".into());
    *f.install_diff_rc.borrow_mut() = 0;

    assert_eq!(units(&f), EXIT_OK);
    assert!(f.stdout_joined().contains("match what this box renders"));
}

#[test]
fn units_stale_is_a_finding() {
    let f = Fake::default();
    f.which.borrow_mut().insert("units-install".into(), "/bin/units-install".into());
    *f.install_diff_rc.borrow_mut() = 1;
    *f.install_diff_out.borrow_mut() = "DIFFERS spira-gate.service\n".into();

    assert_eq!(units(&f), EXIT_FINDING);
    assert!(f.stdout_joined().contains("DIFFERS"));
}

/// SPIRA_INSTALL_SH, when set, is an explicit pin that wins over a PATH lookup — matching
/// skew.sh's own fix (sp-31dm0): an operator who built units-install somewhere non-standard
/// must be able to point skew at it without touching PATH.
#[test]
fn units_spira_install_sh_env_wins_over_path() {
    let f = Fake::default();
    f.set_env("SPIRA_INSTALL_SH", "/explicit/units-install");
    // Deliberately NOT registered in `which` -- if units() fell back to a PATH lookup
    // instead of honoring the env var, this would resolve to None and refuse.
    *f.install_diff_rc.borrow_mut() = 0;

    assert_eq!(units(&f), EXIT_OK);
}

/// EMPTY OR NOT EXECUTABLE both refuse — skew.sh's own
/// `[ -z "$installer" ] || [ ! -x "$installer" ]`. A SPIRA_INSTALL_SH pin pointing at
/// something that exists but is not executable (or does not exist at all) is exactly as
/// unanswerable as no pin and no PATH hit; it must never be handed to install_diff and
/// reported as a "finding" (exit 1) instead of CANNOT_CHECK (exit 3).
#[test]
fn units_spira_install_sh_set_but_not_executable_cannot_check() {
    let f = Fake::default();
    f.set_env("SPIRA_INSTALL_SH", "/explicit/units-install");
    f.not_executable.borrow_mut().push(PathBuf::from("/explicit/units-install"));
    // If units() fell through to install_diff anyway, this would be exit 0 -- the fixture
    // deliberately makes "ran it successfully" distinguishable from "refused to run it".
    *f.install_diff_rc.borrow_mut() = 0;

    assert_eq!(units(&f), EXIT_CANNOT_CHECK);
    assert!(f.stderr_joined().contains("units-install"));
}
