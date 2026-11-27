//! Unit tests over a `Fake` [`crate::ports::World`] — one scenario per decision point
//! skew.sh's own dedicated suites exercised; those suites are retired and their coverage lives here.

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
    pub stash_err: RefCell<Option<String>>,
    pub restore_err: RefCell<Option<String>>,
    pub restored: RefCell<Vec<PathBuf>>,
    pub fetch_ok: RefCell<bool>,
    pub fetches: RefCell<Vec<(PathBuf, String)>>,
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
    pub lock_remove_err: RefCell<Option<String>>,
    pub locks_removed: std::cell::Cell<u32>,
    pub stdout: RefCell<Vec<String>>,
    pub stderr: RefCell<Vec<String>>,
    pub changed: RefCell<BTreeMap<String, Vec<String>>>,
    pub harness_dirs_ref: RefCell<BTreeMap<String, Vec<String>>>, // key "<repo>|<ref>"
    pub harness_dirs_wd: RefCell<BTreeMap<String, Vec<String>>>,  // key "<repo>"
    pub now: RefCell<String>,
    pub queue_locked: RefCell<bool>,
    pub lock_age: RefCell<Option<u64>>,
    pub git_live: RefCell<bool>,
    pub run_log: RefCell<Vec<(String, String)>>,
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
    fn repo_store(&self, path: &Path) -> Option<PathBuf> {
        Some(path.to_path_buf())
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
        match self.stash_err.borrow().clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
    fn restore_deleted(&self, repo: &Path) -> Result<(), String> {
        if let Some(e) = self.restore_err.borrow().clone() {
            return Err(e);
        }
        self.restored.borrow_mut().push(repo.to_path_buf());
        self.dirty.borrow_mut().remove(&repo.to_string_lossy().into_owned());
        Ok(())
    }
    fn fetch(&self, repo: &Path, remote: &str) -> bool {
        self.fetches.borrow_mut().push((repo.to_path_buf(), remote.to_string()));
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
    fn env(&self, k: &str) -> Result<Option<String>, String> {
        Ok(self.env.borrow().get(k).cloned())
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
    fn queue_lock_held(&self, _repo_name: &str) -> bool {
        *self.queue_locked.borrow()
    }
    fn stamp_read(&self, key: &str) -> Option<String> {
        self.stamps.borrow().get(key).cloned()
    }
    fn stamp_write(&self, key: &str, val: &str) {
        self.stamps.borrow_mut().insert(key.to_string(), val.to_string());
    }
    fn run_log_append(&self, name: &str, line: &str) {
        self.run_log.borrow_mut().push((name.to_string(), line.to_string()));
    }
    fn index_lock_age_secs(&self, _repo: &Path) -> Option<u64> {
        *self.lock_age.borrow()
    }
    fn remove_index_lock(&self, _repo: &Path) -> Result<(), String> {
        if let Some(e) = self.lock_remove_err.borrow().clone() {
            return Err(e);
        }
        *self.lock_age.borrow_mut() = None;
        self.locks_removed.set(self.locks_removed.get() + 1);
        Ok(())
    }
    fn git_running_in(&self, _repo: &Path) -> bool {
        *self.git_live.borrow()
    }
    fn out(&self, s: &str) {
        self.stdout.borrow_mut().push(s.to_string());
    }
    fn err(&self, s: &str) {
        self.stderr.borrow_mut().push(s.to_string());
    }
}

// ============================================================================ check()

#[test_id(ttd9j0f)]
#[test]
fn check_no_releases_env_cannot_check() {
    let f = Fake::default();
    assert_eq!(check(&f, false), EXIT_CANNOT_CHECK);
    assert!(f.stderr_joined().contains("SPIRA_RELEASES is not set"));
}

// covers: UC-instance-lifecycle-35
#[test_id(tqxnkb1)]
#[test]
fn check_not_latest_and_manifest_mismatch_both_fire() {
    let f = Fake::default();
    f.set_env("SPIRA_RELEASES", "releases");
    f.symlinks.borrow_mut().insert(PathBuf::from("releases/current"), "rel-2".into());
    f.files.borrow_mut().insert(PathBuf::from("releases/rel-2/MANIFEST"), format!("commit {}\n", "a".repeat(40)));
    f.set_env("SPIRA_REPO", "repo");
    f.git_repos.borrow_mut().push(PathBuf::from("repo"));
    f.repo_lands.borrow_mut().insert("home".into(), "push".into());
    *f.home_repo.borrow_mut() = "home".into();
    f.tags.borrow_mut().insert("repo".into(), vec!["spira-release-spira-20260101T000000Z".into(), "spira-0.1.104".into()]);
    // release_tag resolves to spira-release-spira-20260101T000000Z (not latest), whose tag commit differs from MANIFEST.
    f.rev_parse.borrow_mut().insert("repo|spira-release-spira-20260101T000000Z^{commit}".to_string(), "b".repeat(40));
    f.rev_parse.borrow_mut().insert("repo|spira-0.1.104^{commit}".to_string(), "c".repeat(40));
    f.files.borrow_mut().insert(PathBuf::from("releases/.tags/rel-2"), "spira-release-spira-20260101T000000Z".into());

    let rc = check(&f, false);
    assert_eq!(rc, EXIT_FINDING);
    let out = f.stdout_joined();
    assert!(out.contains("NOT-LATEST"), "{out}");
    assert!(out.contains("MANIFEST-MISMATCH"), "{out}");
}

// covers: UC-instance-lifecycle-35
#[test_id(t9kbjof)]
#[test]
fn check_clean_when_tag_matches_and_is_latest() {
    let f = Fake::default();
    f.set_env("SPIRA_RELEASES", "releases");
    f.symlinks.borrow_mut().insert(PathBuf::from("releases/current"), "rel-2".into());
    let sha = "a".repeat(40);
    f.files.borrow_mut().insert(PathBuf::from("releases/rel-2/MANIFEST"), format!("commit {sha}\n"));
    f.set_env("SPIRA_REPO", "repo");
    f.git_repos.borrow_mut().push(PathBuf::from("repo"));
    f.repo_lands.borrow_mut().insert("home".into(), "push".into());
    *f.home_repo.borrow_mut() = "home".into();
    f.tags.borrow_mut().insert("repo".into(), vec!["spira-release-spira-20260101T000000Z".into(), "spira-0.1.104".into()]);
    f.rev_parse.borrow_mut().insert("repo|spira-0.1.104^{commit}".into(), sha.clone());
    f.files.borrow_mut().insert(PathBuf::from("releases/.tags/rel-2"), "spira-0.1.104".into());

    assert_eq!(check(&f, false), EXIT_OK);
    assert!(f.stdout_joined().contains("in effect"));
}

/// LOCAL RELEASE SOURCE: `SPIRA_RELEASE_REPO` names a plain directory of tarballs with
/// `.tag` sidecars (not a git repo) — `resolve_all_tags` must read those sidecar files
/// directly, never dial `git tag -l` against the directory (which returns nothing and was
/// read as "no release tags found", caught live by testenv's "local-dir" cases, sp-yyk47).
#[test_id(th3mgtq)]
#[test]
fn resolve_all_tags_reads_local_tag_sidecars_not_git_tag() {
    let root = testkit::TempDir::new("skew-local-sidecars");
    let source = root.join("release-source");
    let f = Fake::default();
    f.set_env("SPIRA_RELEASE_REPO", source.to_str().unwrap());
    f.existing.borrow_mut().push(source.clone());
    f.local_tag_sidecars
        .borrow_mut()
        .insert(source.to_str().unwrap().into(), vec!["spira-release-spira-20260912T120000Z".into()]);
    // A decoy under `tags_matching` (the git-tag seam) that must never be consulted here —
    // if it were, this test would see the decoy instead of the sidecar-derived tag.
    f.tags.borrow_mut().insert("repo".into(), vec![]);

    let tags = resolve_all_tags(&f, Path::new("repo")).expect("local-dir tags resolve");
    assert_eq!(tags, vec!["spira-release-spira-20260912T120000Z".to_string()]);
}

/// FRESH RELEASE INSTALL (sp-wecsq): a release is activated (symlink + MANIFEST both
/// present), `home_repo()` is not `queue.local` (no `repo:spira` row at all on a box that
/// only runs installed releases), and `SPIRA_REPO` names no real git checkout -- exactly
/// release acceptance phase B's shape, where `spira-skew-prod.service` exited 3 on every
/// run. There is no git checkout anywhere for this branch to resolve a release tag's commit
/// against, so this is not a transient "could not check" (CANNOT-VERIFY, exit 3); the
/// question does not apply on this kind of install at all, and must say so on stdout at
/// exit 0 -- never a silent pass, and never the escalation path either.
#[test_id(td9caua)]
#[test]
fn check_no_harness_checkout_is_not_applicable_not_cannot_verify() {
    let f = Fake::default();
    f.set_env("SPIRA_RELEASES", "releases");
    f.symlinks.borrow_mut().insert(PathBuf::from("releases/current"), "rel-1".into());
    f.files.borrow_mut().insert(PathBuf::from("releases/rel-1/MANIFEST"), format!("commit {}\n", "a".repeat(40)));
    // No SPIRA_REPO set, no repo:spira row, no git checkout anywhere -- a release-only box.
    *f.home_repo.borrow_mut() = "spira".into();

    let rc = check(&f, true); // --escalate, to prove this path never mails anyone.
    assert_eq!(rc, EXIT_OK);
    assert!(f.stdout_joined().contains("not applicable"), "{}", f.stdout_joined());
    assert!(f.stderr_joined().is_empty(), "{}", f.stderr_joined());
    assert!(f.mail_calls.borrow().is_empty());
}

// ============================================================================ check_local()

#[test_id(twtx0ae)]
#[test]
fn check_local_tampered_and_behind_both_fire() {
    let f = Fake::default();
    *f.home_repo.borrow_mut() = "home".into();
    f.repo_roots.borrow_mut().insert("home".into(), PathBuf::from("repo"));
    f.git_repos.borrow_mut().push(PathBuf::from("repo"));
    f.landrefs.borrow_mut().insert("home".into(), "local/main".into());
    f.rev_parse.borrow_mut().insert("repo|local/main".into(), "tip123".into());
    *f.release_verify_ok.borrow_mut() = false;
    *f.release_verify_out.borrow_mut() = "hash mismatch".into();
    f.ancestors.borrow_mut().push(("old456".into(), "tip123".into()));
    f.rev_list_counts.borrow_mut().insert("repo|old456..tip123".into(), 3);

    let rc = check_local(&f, "rel-1", "old456", false);
    assert_eq!(rc, EXIT_FINDING);
    let out = f.stdout_joined();
    assert!(out.contains("LOCAL-TAMPERED"), "{out}");
    assert!(out.contains("LOCAL-BEHIND"), "{out}");
}

/// BY NAME, NOT BY PATH — same scar as `refresh_queue_local_resolves_landref_by_name_never_by_path`,
/// for `check_local`'s own call site.
#[test_id(tejvmpp)]
#[test]
fn check_local_resolves_landref_by_name_never_by_path() {
    let f = Fake::default();
    *f.home_repo.borrow_mut() = "home".into();
    f.repo_roots.borrow_mut().insert("home".into(), PathBuf::from("repo"));
    f.git_repos.borrow_mut().push(PathBuf::from("repo"));
    f.landrefs.borrow_mut().insert("home".into(), "local/main".into());
    // Decoy under the path key — must never be consulted.
    f.landrefs.borrow_mut().insert("repo".into(), "origin/decoy".into());
    f.rev_parse.borrow_mut().insert("repo|local/main".into(), "tip123".into());
    f.rev_parse.borrow_mut().insert("repo|origin/decoy".into(), "wrong-tip".into());
    *f.release_verify_ok.borrow_mut() = true;

    let rc = check_local(&f, "rel-1", "tip123", false);
    assert_eq!(rc, EXIT_OK);
    assert!(f.stdout_joined().contains("in effect"));
}

#[test_id(ti49b98)]
#[test]
fn check_local_hotfix_is_clean() {
    let f = Fake::default();
    *f.home_repo.borrow_mut() = "home".into();
    f.repo_roots.borrow_mut().insert("home".into(), PathBuf::from("repo"));
    f.git_repos.borrow_mut().push(PathBuf::from("repo"));
    f.landrefs.borrow_mut().insert("home".into(), "local/main".into());
    f.rev_parse.borrow_mut().insert("repo|local/main".into(), "tip123".into());
    *f.release_verify_ok.borrow_mut() = true;
    *f.release_status.borrow_mut() = "RUNNING UNLANDED hot999: the operator's own stop-the-world fix\n".into();

    let rc = check_local(&f, "rel-1", "hot999", false);
    assert_eq!(rc, EXIT_OK);
    assert!(f.stdout_joined().contains("recorded standing hotfix"));
}

#[test_id(tutlpgr)]
#[test]
fn check_local_unresolvable_divergence_is_cannot_verify() {
    let f = Fake::default();
    *f.home_repo.borrow_mut() = "home".into();
    f.repo_roots.borrow_mut().insert("home".into(), PathBuf::from("repo"));
    f.git_repos.borrow_mut().push(PathBuf::from("repo"));
    f.landrefs.borrow_mut().insert("home".into(), "local/main".into());
    f.rev_parse.borrow_mut().insert("repo|local/main".into(), "tip123".into());
    *f.release_verify_ok.borrow_mut() = true;
    // manifest_commit is neither the tip nor a recorded hotfix, and not an ancestor either.
    let rc = check_local(&f, "rel-1", "weird000", false);
    assert_eq!(rc, EXIT_CANNOT_CHECK);
    assert!(f.stdout_joined().contains("CANNOT-VERIFY"));
}

// ============================================================================ foreign()

// covers: UC-instance-lifecycle-36
#[test_id(t5dins9)]
#[test]
fn foreign_override_env_exempts() {
    let f = Fake::default();
    f.set_env("SPIRA_ALLOW_FOREIGN_HARNESS", "1");
    assert_eq!(foreign(&f, "some/repo", "base", "ref"), 0);
}

#[test_id(tli08q8)]
#[test]
fn foreign_missing_args_refuses() {
    let f = Fake::default();
    assert_eq!(foreign(&f, "", "base", "ref"), 1);
}

// covers: UC-instance-lifecycle-36
#[test_id(tgiahti)]
#[test]
fn foreign_exempts_own_repo() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "harness");
    assert_eq!(foreign(&f, "harness", "base", "ref"), 0);
}

#[test_id(tmuympx)]
#[test]
fn foreign_no_copy_in_ref_allows() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "elsewhere");
    *f.home_repo.borrow_mut() = "home".into();
    assert_eq!(foreign(&f, "some/repo", "base", "ref"), 0);
}

// covers: UC-instance-lifecycle-36
#[test_id(ts8shs9)]
#[test]
fn foreign_refuses_a_change_inside_the_vendored_copy() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "elsewhere");
    *f.home_repo.borrow_mut() = "home".into();
    f.harness_dirs_ref.borrow_mut().insert("some/repo|ref".into(), vec!["vendor/harness".into()]);
    f.changed.borrow_mut().insert("some/repo|base|ref".into(), vec!["vendor/harness/spira/lib.sh".into(), "README.md".into()]);

    let rc = foreign(&f, "some/repo", "base", "ref");
    assert_eq!(rc, 1);
    assert!(f.stdout_joined().contains("vendor/harness/spira/lib.sh"));
    assert!(!f.stdout_joined().contains("README.md"));
    assert!(f.stderr_joined().contains("REFUSED by skew"));
}

#[test_id(t4ikl17)]
#[test]
fn foreign_dot_directory_matches_every_changed_file() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "elsewhere");
    *f.home_repo.borrow_mut() = "home".into();
    f.harness_dirs_ref.borrow_mut().insert("some/repo|ref".into(), vec![".".into()]);
    f.changed.borrow_mut().insert("some/repo|base|ref".into(), vec!["anything.txt".into()]);

    assert_eq!(foreign(&f, "some/repo", "base", "ref"), 1);
}

#[test_id(tk5psvw)]
#[test]
fn foreign_change_outside_the_copy_allows() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "elsewhere");
    *f.home_repo.borrow_mut() = "home".into();
    f.harness_dirs_ref.borrow_mut().insert("some/repo|ref".into(), vec!["vendor/harness".into()]);
    f.changed.borrow_mut().insert("some/repo|base|ref".into(), vec!["README.md".into()]);

    assert_eq!(foreign(&f, "some/repo", "base", "ref"), 0);
}

// ============================================================================ copies()

#[test_id(tv43gei)]
#[test]
fn copies_found_reports_self_and_second() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "repo-a");
    f.repo_roots.borrow_mut().insert("a".into(), PathBuf::from("repo-a"));
    f.repo_roots.borrow_mut().insert("b".into(), PathBuf::from("repo-b"));
    f.existing.borrow_mut().push(PathBuf::from("repo-a"));
    f.existing.borrow_mut().push(PathBuf::from("repo-b"));
    f.harness_dirs_wd.borrow_mut().insert("repo-a".into(), vec!["vendor/h".into()]);
    f.harness_dirs_wd.borrow_mut().insert("repo-b".into(), vec!["vendor/h".into()]);

    assert_eq!(copies(&f), 0);
    let out = f.stdout_joined();
    assert!(out.contains("a repo-a vendor/h self"));
    assert!(out.contains("b repo-b vendor/h second"));
}

#[test_id(tkekhgi)]
#[test]
fn copies_none_found_is_a_finding() {
    let f = Fake::default();
    assert_eq!(copies(&f), 1);
}

// ============================================================================ gap()

#[test_id(tlvbryn)]
#[test]
fn gap_clean() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "repo");
    f.git_repos.borrow_mut().push(PathBuf::from("repo"));
    f.landrefs.borrow_mut().insert("repo".into(), "origin/main".into());
    f.rev_parse.borrow_mut().insert("repo|origin/main".into(), "x".into());
    f.rev_list_counts.borrow_mut().insert("repo|HEAD..origin/main".into(), 0);
    f.rev_parse_short.borrow_mut().insert("repo|HEAD".into(), "abc".into());
    f.rev_parse_short.borrow_mut().insert("repo|origin/main".into(), "x".into());

    assert_eq!(gap(&f, None), EXIT_OK);
}

#[test_id(tquppdn)]
#[test]
fn gap_not_a_checkout_cannot_check() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "repo");
    assert_eq!(gap(&f, None), EXIT_CANNOT_CHECK);
    assert!(f.stderr_joined().contains("not a git checkout"));
}

// covers: UC-instance-lifecycle-37
#[test_id(t3qfe6p)]
#[test]
fn gap_behind_is_a_finding() {
    let f = Fake::default();
    f.git_repos.borrow_mut().push(PathBuf::from("r"));
    f.landrefs.borrow_mut().insert("r".into(), "origin/main".into());
    f.rev_parse.borrow_mut().insert("r|origin/main".into(), "x".into());
    f.rev_list_counts.borrow_mut().insert("r|HEAD..origin/main".into(), 4);
    f.rev_parse_short.borrow_mut().insert("r|HEAD".into(), "abc".into());
    f.rev_parse_short.borrow_mut().insert("r|origin/main".into(), "x".into());

    assert_eq!(gap(&f, Some("r")), EXIT_FINDING);
    assert!(f.stdout_joined().contains("4 commit(s) behind"));
}

// ============================================================================ escalate()

#[test_id(tho40c3)]
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

#[test_id(t2ffkcu)]
#[test]
fn escalate_distinct_conditions_both_send() {
    let f = Fake::default();
    escalate(&f, "v2:NOT-LATEST=1 MANIFEST-MISMATCH=0", "a");
    escalate(&f, "v2:NOT-LATEST=0 MANIFEST-MISMATCH=1", "b");
    assert_eq!(f.mail_calls.borrow().len(), 2);
}

#[test_id(t77edlb)]
#[test]
fn escalate_picks_the_local_behind_default_action() {
    let f = Fake::default();
    escalate(&f, "v2:LOCAL-BEHIND=1 LOCAL-TAMPERED=0", "LOCAL-BEHIND ...\n");
    let (_, _, default_action) = f.mail_calls.borrow()[0].clone();
    assert!(default_action.contains("queue land-local"));
}

fn behind_latest_release() -> Fake {
    let f = Fake::default();
    f.set_env("SPIRA_RELEASES", "releases");
    f.symlinks.borrow_mut().insert(PathBuf::from("releases/current"), "rel-2".into());
    f.files.borrow_mut().insert(PathBuf::from("releases/rel-2/MANIFEST"), format!("commit {}\n", "a".repeat(40)));
    f.set_env("SPIRA_REPO", "repo");
    f.git_repos.borrow_mut().push(PathBuf::from("repo"));
    f.repo_lands.borrow_mut().insert("home".into(), "push".into());
    *f.home_repo.borrow_mut() = "home".into();
    f.tags.borrow_mut().insert("repo".into(), vec!["spira-release-spira-20260101T000000Z".into(), "spira-0.1.104".into()]);
    f.rev_parse.borrow_mut().insert("repo|spira-release-spira-20260101T000000Z^{commit}".to_string(), "a".repeat(40));
    f.rev_parse.borrow_mut().insert("repo|spira-0.1.104^{commit}".to_string(), "c".repeat(40));
    f.files.borrow_mut().insert(PathBuf::from("releases/.tags/rel-2"), "spira-release-spira-20260101T000000Z".into());
    f
}

// covers: UC-operator-channel-25
#[test_id(t7u21va)]
#[test]
fn an_escalation_asks_a_question_that_carries_its_default() {
    let f = behind_latest_release();
    *f.mail_ok.borrow_mut() = true;
    assert_eq!(check(&f, true), EXIT_FINDING);
    let calls = f.mail_calls.borrow();
    assert_eq!(calls.len(), 1);
    let (_, body, default_action) = &calls[0];
    assert!(body.contains("## Question") && body.contains("## Default"), "{body}");
    assert!(!default_action.is_empty() && body.contains(default_action.as_str()), "{body}");
    assert!(f.stdout_joined().contains("escalated"), "{}", f.stdout_joined());
}

// covers: UC-operator-channel-25
#[test_id(tr0lvkt)]
#[test]
fn a_mailer_that_refuses_is_reported_with_its_output_and_the_finding_still_exits_one() {
    let f = behind_latest_release();
    assert_eq!(check(&f, true), EXIT_FINDING);
    let out = f.stdout_joined();
    assert!(out.contains("escalation failed") && out.contains("mail.sh failed"), "{out}");
    assert!(!out.contains("skew: escalated"), "{out}");
    assert!(f.stamps.borrow().is_empty(), "a failed send must not stamp the condition as reported");
}

// covers: UC-operator-channel-25
#[test_id(tea8sbt)]
#[test]
fn a_check_without_escalate_prints_the_finding_and_mails_nobody() {
    let f = behind_latest_release();
    *f.mail_ok.borrow_mut() = true;
    assert_eq!(check(&f, false), EXIT_FINDING);
    assert!(f.stdout_joined().contains("NOT-LATEST"), "{}", f.stdout_joined());
    assert!(f.mail_calls.borrow().is_empty());
}

// ============================================================================ refresh()

#[test_id(te27n35)]
#[test]
fn refresh_queue_local_match_is_clean() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "repo");
    *f.home_repo.borrow_mut() = "home".into();
    f.repo_roots.borrow_mut().insert("home".into(), PathBuf::from("repo"));
    f.repo_lands.borrow_mut().insert("home".into(), "queue.local".into());
    f.git_repos.borrow_mut().push(PathBuf::from("repo"));
    f.landrefs.borrow_mut().insert("home".into(), "local/main".into());
    f.rev_parse.borrow_mut().insert("repo|local/main".into(), "tip".into());
    f.rev_parse.borrow_mut().insert("repo|HEAD".into(), "tip".into());

    assert_eq!(refresh(&f, None, false), 0);
    assert!(f.stdout_joined().contains("nothing to deploy"));
}

/// BY NAME, NOT BY PATH (skew.sh's own `spira_landref "$name"`, not `"$repo"`): a decoy
/// landref registered under the PATH key, disagreeing with the real one under the NAME
/// key, must never be consulted. Caught live by testenv against sp-yyk47's merged tree —
/// the first port used `w.landref(repo)` here and silently resolved the wrong ref whenever
/// a caller's path-derived guess disagreed with the name already in hand.
#[test_id(tov80jj)]
#[test]
fn refresh_queue_local_resolves_landref_by_name_never_by_path() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "repo");
    *f.home_repo.borrow_mut() = "home".into();
    f.repo_roots.borrow_mut().insert("home".into(), PathBuf::from("repo"));
    f.repo_lands.borrow_mut().insert("home".into(), "queue.local".into());
    f.git_repos.borrow_mut().push(PathBuf::from("repo"));
    f.landrefs.borrow_mut().insert("home".into(), "local/main".into());
    // Decoy: a DIFFERENT ref under the path key. If the implementation ever regresses to
    // `landref(repo)`, this is what it would pick up instead, resolving to "wrong-tip"
    // rather than "tip" and turning a clean match into a false LOCAL-SKEW.
    f.landrefs.borrow_mut().insert("repo".into(), "origin/decoy".into());
    f.rev_parse.borrow_mut().insert("repo|local/main".into(), "tip".into());
    f.rev_parse.borrow_mut().insert("repo|origin/decoy".into(), "wrong-tip".into());
    f.rev_parse.borrow_mut().insert("repo|HEAD".into(), "tip".into());

    assert_eq!(refresh(&f, None, false), 0);
    assert!(f.stdout_joined().contains("nothing to deploy"));
    assert!(f.mail_calls.borrow().is_empty());
}

#[test_id(tqud69e)]
#[test]
fn refresh_queue_local_mismatch_escalates_local_skew() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "repo");
    *f.home_repo.borrow_mut() = "home".into();
    f.repo_roots.borrow_mut().insert("home".into(), PathBuf::from("repo"));
    f.repo_lands.borrow_mut().insert("home".into(), "queue.local".into());
    f.git_repos.borrow_mut().push(PathBuf::from("repo"));
    f.landrefs.borrow_mut().insert("home".into(), "local/main".into());
    f.rev_parse.borrow_mut().insert("repo|local/main".into(), "tip".into());
    f.rev_parse.borrow_mut().insert("repo|HEAD".into(), "stale".into());

    let rc = refresh(&f, None, false);
    assert_eq!(rc, 1);
    assert!(f.stdout_joined().contains("LOCAL-SKEW"));
    assert_eq!(f.mail_calls.borrow().len(), 1);
}

#[test_id(tu0pa4t)]
#[test]
fn refresh_queue_local_mismatch_during_a_land_local_does_not_escalate() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "repo");
    *f.home_repo.borrow_mut() = "home".into();
    f.repo_roots.borrow_mut().insert("home".into(), PathBuf::from("repo"));
    f.repo_lands.borrow_mut().insert("home".into(), "queue.local".into());
    f.git_repos.borrow_mut().push(PathBuf::from("repo"));
    f.landrefs.borrow_mut().insert("home".into(), "local/main".into());
    f.rev_parse.borrow_mut().insert("repo|local/main".into(), "tip".into());
    f.rev_parse.borrow_mut().insert("repo|HEAD".into(), "stale".into());
    *f.queue_locked.borrow_mut() = true;

    assert_eq!(refresh(&f, None, false), 0);
    assert!(f.stdout_joined().contains("land-local in flight"));
    assert!(f.mail_calls.borrow().is_empty());
}

#[test_id(tm5iedq)]
#[test]
fn refresh_queue_local_hotfix_never_resets() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "repo");
    *f.home_repo.borrow_mut() = "home".into();
    f.repo_roots.borrow_mut().insert("home".into(), PathBuf::from("repo"));
    f.repo_lands.borrow_mut().insert("home".into(), "queue.local".into());
    f.git_repos.borrow_mut().push(PathBuf::from("repo"));
    f.landrefs.borrow_mut().insert("home".into(), "local/main".into());
    f.rev_parse.borrow_mut().insert("repo|local/main".into(), "tip".into());
    f.rev_parse.borrow_mut().insert("repo|HEAD".into(), "stale".into());
    *f.release_status.borrow_mut() = "RUNNING UNLANDED stale: deliberate\n".into();

    assert_eq!(refresh(&f, None, false), 0);
    assert!(f.stdout_joined().contains("standing hotfix"));
    assert!(f.mail_calls.borrow().is_empty());
}

#[test_id(t4iy8ey)]
#[test]
fn refresh_release_mode_already_at_base() {
    let f = Fake::default();
    f.set_env("SPIRA_RELEASES", "releases");
    f.symlinks.borrow_mut().insert(PathBuf::from("releases/current"), "rel-1".into());
    f.set_env("SPIRA_REPO", "repo");
    f.git_repos.borrow_mut().push(PathBuf::from("repo"));
    f.landrefs.borrow_mut().insert("repo".into(), "origin/main".into());
    f.rev_list_counts.borrow_mut().insert("repo|HEAD..origin/main".into(), 0);

    assert_eq!(refresh(&f, Some("repo"), false), 0);
    assert!(f.stdout_joined().contains("already at"));
}

#[test_id(tpcep3p)]
#[test]
fn refresh_release_mode_builds_and_activates() {
    let f = Fake::default();
    f.set_env("SPIRA_RELEASES", "releases");
    f.symlinks.borrow_mut().insert(PathBuf::from("releases/current"), "rel-1".into());
    f.set_env("SPIRA_REPO", "repo");
    f.git_repos.borrow_mut().push(PathBuf::from("repo"));
    f.landrefs.borrow_mut().insert("repo".into(), "origin/main".into());
    f.rev_list_counts.borrow_mut().insert("repo|HEAD..origin/main".into(), 2);
    f.rev_parse.borrow_mut().insert("repo|HEAD".into(), "newsha".into());
    *f.release_bva_ok.borrow_mut() = true;

    assert_eq!(refresh(&f, Some("repo"), false), 0);
    assert!(f.stdout_joined().contains("refreshed — new release installed (2 commit(s))"));
    assert_eq!(f.overrides_applied.borrow().len(), 1);
}

#[test_id(t7fim1m)]
#[test]
fn refresh_release_failure_logs_releases_first_error_line() {
    let f = Fake::default();
    f.set_env("SPIRA_RELEASES", "releases");
    f.symlinks.borrow_mut().insert(PathBuf::from("releases/current"), "rel-1".into());
    f.set_env("SPIRA_REPO", "repo");
    f.git_repos.borrow_mut().push(PathBuf::from("repo"));
    f.landrefs.borrow_mut().insert("repo".into(), "origin/main".into());
    f.rev_list_counts.borrow_mut().insert("repo|HEAD..origin/main".into(), 2);
    f.rev_parse.borrow_mut().insert("repo|HEAD".into(), "newsha".into());

    assert_eq!(refresh(&f, Some("repo"), false), 1);
    assert!(f.stdout_joined().contains("of newsha failed: build failed"));
}

#[test_id(t21j84b)]
#[test]
fn refresh_product_repo_never_calls_release() {
    let f = Fake::default();
    f.set_env("SPIRA_RELEASES", "releases");
    f.symlinks.borrow_mut().insert(PathBuf::from("releases/current"), "rel-1".into());
    *f.home_repo.borrow_mut() = "home".into();
    f.repo_roots.borrow_mut().insert("home".into(), PathBuf::from("home-repo"));
    f.repo_roots.borrow_mut().insert("prod".into(), PathBuf::from("prod"));
    f.git_repos.borrow_mut().push(PathBuf::from("prod"));
    f.landrefs.borrow_mut().insert("prod".into(), "origin/main".into());
    f.rev_list_counts.borrow_mut().insert("prod|HEAD..origin/main".into(), 1);
    f.current_branches.borrow_mut().insert("prod".into(), "some-feature".into());
    *f.release_bva_ok.borrow_mut() = false;

    let rc = refresh(&f, Some("prod"), false);
    assert_eq!(rc, 1);
    let out = f.stdout_joined();
    assert!(out.contains("declined"), "{out}");
    assert!(!out.contains("release build"), "{out}");
}

#[test_id(tkhtidt)]
#[test]
fn refresh_with_no_fetch_leaves_the_remote_alone() {
    let fetched = |no_fetch, behind| {
        let f = Fake::default();
        *f.home_repo.borrow_mut() = "home".into();
        f.repo_roots.borrow_mut().insert("prod".into(), PathBuf::from("prod"));
        f.git_repos.borrow_mut().push(PathBuf::from("prod"));
        f.landrefs.borrow_mut().insert("prod".into(), "origin/main".into());
        f.ref_remotes.borrow_mut().insert("origin/main".into(), "origin".into());
        f.rev_list_counts.borrow_mut().insert("prod|HEAD..origin/main".into(), behind);
        f.current_branches.borrow_mut().insert("prod".into(), "main".into());
        assert_eq!(refresh(&f, Some("prod"), no_fetch), 0);
        assert_eq!(f.stdout_joined().contains("refreshed to origin/main (2 commit(s))"), behind == 2);
        f.fetches.take()
    };
    for behind in [0, 2] {
        assert_eq!(fetched(true, behind), [], "behind={behind}");
        assert_eq!(fetched(false, behind), [(PathBuf::from("prod"), "origin".to_string())], "behind={behind}");
    }
}

#[test_id(tk2ncu6)]
#[test]
fn refresh_checkout_mode_declines_off_base_branch() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "repo");
    f.git_repos.borrow_mut().push(PathBuf::from("repo"));
    f.landrefs.borrow_mut().insert("repo".into(), "origin/main".into());
    f.rev_list_counts.borrow_mut().insert("repo|HEAD..origin/main".into(), 1);
    f.current_branches.borrow_mut().insert("repo".into(), "some-feature".into());

    let rc = refresh(&f, Some("repo"), false);
    assert_eq!(rc, 1);
    assert!(f.stdout_joined().contains("declined"));
}

// covers: UC-instance-lifecycle-37
#[test_id(tizpmdw)]
#[test]
fn refresh_checkout_mode_stages_and_swaps() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "repo");
    f.git_repos.borrow_mut().push(PathBuf::from("repo"));
    f.landrefs.borrow_mut().insert("repo".into(), "origin/main".into());
    f.rev_list_counts.borrow_mut().insert("repo|HEAD..origin/main".into(), 2);
    f.current_branches.borrow_mut().insert("repo".into(), "main".into());
    f.diff_statuses.borrow_mut().insert(
        "repo|HEAD..origin/main".into(),
        vec![('M', "spira/conf.sh".into()), ('A', "spira/new.sh".into()), ('D', "spira/old.sh".into())],
    );
    f.show_files.borrow_mut().insert("repo|origin/main|spira/conf.sh".into(), b"new conf".to_vec());
    f.show_files.borrow_mut().insert("repo|origin/main|spira/new.sh".into(), b"#!/bin/bash\n".to_vec());
    f.file_modes.borrow_mut().insert("repo|origin/main|spira/new.sh".into(), "100755".into());

    let rc = refresh(&f, Some("repo"), false);
    assert_eq!(rc, 0);
    assert!(f.written.borrow().contains_key(&PathBuf::from("repo/spira/conf.sh")));
    let (content, executable) = f.written.borrow().get(&PathBuf::from("repo/spira/new.sh")).cloned().unwrap();
    assert_eq!(content, b"#!/bin/bash\n");
    assert!(executable);
    assert!(f.removed.borrow().contains(&PathBuf::from("repo/spira/old.sh")));
    assert_eq!(f.overrides_applied.borrow().len(), 1);
}

// ============================================================================ units()

// covers: UC-instance-lifecycle-34
#[test_id(th9s2y6)]
#[test]
fn units_installer_missing_cannot_check() {
    let f = Fake::default();
    // units-install is a compiled binary now (sp-31dm0): neither SPIRA_INSTALL_SH nor a
    // PATH hit resolves it -- units() must refuse, not guess a bash-script path.
    assert_eq!(units(&f), EXIT_CANNOT_CHECK);
    assert!(f.stderr_joined().contains("units-install"));
}

// covers: UC-instance-lifecycle-34
#[test_id(tpam9am)]
#[test]
fn units_clean() {
    let f = Fake::default();
    f.which.borrow_mut().insert("units-install".into(), "bin/units-install".into());
    *f.install_diff_rc.borrow_mut() = 0;

    assert_eq!(units(&f), EXIT_OK);
    assert!(f.stdout_joined().contains("match what this box renders"));
}

// covers: UC-instance-lifecycle-34
#[test_id(t6vjtyn)]
#[test]
fn units_stale_is_a_finding() {
    let f = Fake::default();
    f.which.borrow_mut().insert("units-install".into(), "bin/units-install".into());
    *f.install_diff_rc.borrow_mut() = 1;
    *f.install_diff_out.borrow_mut() = "DIFFERS spira-gate.service\n".into();

    assert_eq!(units(&f), EXIT_FINDING);
    assert!(f.stdout_joined().contains("DIFFERS"));
}

/// SPIRA_INSTALL_SH, when set, is an explicit pin that wins over a PATH lookup — matching
/// skew.sh's own fix (sp-31dm0): an operator who built units-install somewhere non-standard
/// must be able to point skew at it without touching PATH.
#[test_id(tipup3l)]
#[test]
fn units_spira_install_sh_env_wins_over_path() {
    let f = Fake::default();
    f.set_env("SPIRA_INSTALL_SH", "explicit/units-install");
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
#[test_id(tza7qtr)]
#[test]
fn units_spira_install_sh_set_but_not_executable_cannot_check() {
    let f = Fake::default();
    f.set_env("SPIRA_INSTALL_SH", "explicit/units-install");
    f.not_executable.borrow_mut().push(PathBuf::from("explicit/units-install"));
    // If units() fell through to install_diff anyway, this would be exit 0 -- the fixture
    // deliberately makes "ran it successfully" distinguishable from "refused to run it".
    *f.install_diff_rc.borrow_mut() = 0;

    assert_eq!(units(&f), EXIT_CANNOT_CHECK);
    assert!(f.stderr_joined().contains("units-install"));
}

fn dirty_checkout(dirty: &str) -> Fake {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "repo");
    f.git_repos.borrow_mut().push(PathBuf::from("repo"));
    f.landrefs.borrow_mut().insert("repo".into(), "origin/main".into());
    f.rev_list_counts.borrow_mut().insert("repo|HEAD..origin/main".into(), 2);
    f.current_branches.borrow_mut().insert("repo".into(), "main".into());
    f.dirty.borrow_mut().insert("repo".into(), dirty.into());
    f
}

#[test_id(tx2tash)]
#[test]
fn refresh_checkout_with_every_tracked_file_deleted_is_restored_not_stashed() {
    let f = dirty_checkout(" D a.sh\n D b/c.sh\n");
    *f.stash_err.borrow_mut() = Some("stash must not be tried".into());
    assert_eq!(refresh(&f, Some("repo"), false), 0);
    assert_eq!(f.restored.borrow().len(), 1);
    let out = f.stdout_joined();
    assert!(out.contains("restored 2 deleted tracked file(s)"), "{out}");
    assert!(!out.contains("declined"), "{out}");
}

#[test_id(t1sb3an)]
#[test]
fn refresh_declines_naming_the_restore_failure() {
    let f = dirty_checkout(" D a.sh\n");
    *f.restore_err.borrow_mut() = Some("fatal: Unable to create index.lock".into());
    assert_eq!(refresh(&f, Some("repo"), false), 1);
    assert!(f.stdout_joined().contains("restoring them failed: fatal: Unable to create index.lock"));
}

#[test_id(tt40eqq)]
#[test]
fn refresh_declines_naming_the_stash_failure_for_a_mixed_dirty_tree() {
    let f = dirty_checkout(" D a.sh\n M b.sh\n");
    *f.stash_err.borrow_mut() = Some("error: could not write index".into());
    assert_eq!(refresh(&f, Some("repo"), false), 1);
    assert!(f.restored.borrow().is_empty());
    assert!(f.stdout_joined().contains("stash failed: error: could not write index"));
}

// ============================================================================ refresh() decline streak

fn declining_checkout() -> Fake {
    let f = Fake::default();
    f.set_env("SPIRA_RELEASES", "");
    f.git_repos.borrow_mut().push(PathBuf::from("prod"));
    f.landrefs.borrow_mut().insert("prod".into(), "origin/main".into());
    f.rev_list_counts.borrow_mut().insert("prod|HEAD..origin/main".into(), 2);
    f.dirty.borrow_mut().insert("prod".into(), " M ci.yml\n M CLAUDE.md\n".into());
    *f.stash_err.borrow_mut() = Some("Unable to create index.lock".into());
    f
}

#[test_id(tt8vu6r)]
#[test]
fn refresh_decline_names_dirty_files_and_a_stale_lock_and_raises_a_row_on_the_third_pass() {
    let f = declining_checkout();
    *f.lock_age.borrow_mut() = Some(86_400);
    *f.lock_remove_err.borrow_mut() = Some("Permission denied".into());
    for pass in 1..=3 {
        assert_eq!(refresh(&f, Some("prod"), false), 1);
        let rows = f.run_log.borrow().iter().filter(|r| r.1.contains("consecutive")).count();
        assert_eq!(rows, if pass < 3 { 0 } else { 1 }, "pass {pass}");
    }
    let said = f.stdout_joined();
    assert!(said.contains("STALE-INDEX-LOCK"), "{said}");
    assert!(said.contains("M ci.yml, M CLAUDE.md"), "{said}");
    let log = f.run_log.borrow();
    let row = log.iter().find(|r| r.1.contains("3 consecutive")).unwrap();
    assert_eq!(row.0, REFRESH_ALARM_LOG);
    assert!(row.1.contains("prod") && row.1.contains("STALE-INDEX-LOCK"), "{}", row.1);
    assert!(f.removed.borrow().is_empty(), "the dirt is never touched");
}

#[test_id(tqk6vin)]
#[test]
fn refresh_clears_a_stale_lock_with_no_live_git_before_stashing() {
    let f = declining_checkout();
    *f.lock_age.borrow_mut() = Some(1_188_963);
    refresh(&f, Some("prod"), false);
    assert_eq!(f.locks_removed.get(), 1);
    assert!(f.stdout_joined().contains("removed stale index.lock"), "{}", f.stdout_joined());
}

#[test_id(tg2kh4m)]
#[test]
fn refresh_leaves_a_fresh_or_owned_lock_alone() {
    for (age, live) in [(30, false), (86_400, true)] {
        let f = declining_checkout();
        *f.lock_age.borrow_mut() = Some(age);
        *f.git_live.borrow_mut() = live;
        refresh(&f, Some("prod"), false);
        assert_eq!(f.locks_removed.get(), 0, "age {age} live {live}");
    }
}

#[test_id(tr3ealg)]
#[test]
fn refresh_escalates_a_lock_it_cannot_clear_once_not_every_pass() {
    let f = declining_checkout();
    *f.lock_age.borrow_mut() = Some(86_400);
    *f.lock_remove_err.borrow_mut() = Some("Permission denied".into());
    for _ in 0..5 {
        assert_eq!(refresh(&f, Some("prod"), false), 1);
    }
    let rows: Vec<_> = f.run_log.borrow().iter().filter(|r| r.1.contains("cannot remove stale index.lock")).cloned().collect();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].1.contains("Permission denied"));
}

#[test_id(tw8xgks)]
#[test]
fn refresh_decline_does_not_call_a_fresh_or_owned_lock_stale() {
    let f = declining_checkout();
    *f.lock_age.borrow_mut() = Some(30);
    refresh(&f, Some("prod"), false);
    assert!(!f.stdout_joined().contains("STALE-INDEX-LOCK"));
    *f.lock_age.borrow_mut() = Some(86_400);
    *f.git_live.borrow_mut() = true;
    refresh(&f, Some("prod"), false);
    assert!(!f.stdout_joined().contains("STALE-INDEX-LOCK"));
}

#[test_id(terutka)]
#[test]
fn refresh_success_resets_the_decline_streak() {
    let f = declining_checkout();
    refresh(&f, Some("prod"), false);
    refresh(&f, Some("prod"), false);
    f.rev_list_counts.borrow_mut().insert("prod|HEAD..origin/main".into(), 0);
    assert_eq!(refresh(&f, Some("prod"), false), 0);
    f.rev_list_counts.borrow_mut().insert("prod|HEAD..origin/main".into(), 2);
    refresh(&f, Some("prod"), false);
    refresh(&f, Some("prod"), false);
    assert!(f.run_log.borrow().is_empty(), "two passes after a reset is not three in a row");
}

#[test_id(t1v8amt)]
#[test]
fn refresh_escalates_again_for_a_new_uncleared_lock_after_the_old_one_vanished() {
    let f = declining_checkout();
    *f.lock_remove_err.borrow_mut() = Some("Permission denied".into());
    for _ in 0..2 {
        *f.lock_age.borrow_mut() = Some(86_400);
        refresh(&f, Some("prod"), false);
        refresh(&f, Some("prod"), false);
        *f.lock_age.borrow_mut() = None;
        refresh(&f, Some("prod"), false);
    }
    let rows = f.run_log.borrow().iter().filter(|r| r.1.contains("cannot remove stale index.lock")).count();
    assert_eq!(rows, 2);
}

#[test_id(tpznggy)]
#[test]
fn the_refresh_stamp_is_read_from_the_clock_in_process() {
    assert_eq!(crate::real::stamp_of(1_793_166_343), "20261028T054543Z");
    assert_eq!(crate::real::stamp_of(0), "19700101T000000Z");
}

// ============================================================================ plan cells 35-37

// covers: UC-instance-lifecycle-35
#[test_id(tauymgc)]
#[test]
fn check_cannot_check_without_current_or_manifest_and_reads_artifact_tags_through_gh() {
    let f = Fake::default();
    f.set_env("SPIRA_RELEASES", "releases");
    f.symlinks.borrow_mut().insert(PathBuf::from("releases/current"), "rel-1".into());
    assert_eq!(check(&f, false), EXIT_CANNOT_CHECK, "no MANIFEST");
    assert!(f.stderr_joined().contains("MANIFEST missing"), "{}", f.stderr_joined());

    let g = Fake::default();
    g.set_env("SPIRA_RELEASES", "releases");
    assert_eq!(check(&g, false), EXIT_CANNOT_CHECK, "no current");

    let sha = "a".repeat(40);
    let h = Fake::default();
    h.set_env("SPIRA_RELEASES", "releases");
    h.set_env("SPIRA_REPO", "repo");
    h.set_env("SPIRA_RELEASE_REPO", "owner/repo");
    h.git_repos.borrow_mut().push(PathBuf::from("repo"));
    h.symlinks.borrow_mut().insert(PathBuf::from("releases/current"), "rel-1".into());
    h.files.borrow_mut().insert(PathBuf::from("releases/rel-1/MANIFEST"), format!("commit {sha}\n"));
    h.files.borrow_mut().insert(PathBuf::from("releases/.tags/rel-1"), "spira-release-spira-20260101T000000Z".into());
    h.rev_parse.borrow_mut().insert("repo|spira-release-spira-20260101T000000Z^{commit}".into(), sha);
    *h.gh_tags_json.borrow_mut() = Some(Ok(
        r#"[{"isDraft":true,"tagName":"spira-release-spira-20260301T000000Z"},{"isDraft":false,"tagName":"spira-release-spira-20260101T000000Z"}]"#.into(),
    ));
    assert_eq!(check(&h, false), EXIT_OK, "{}{}", h.stdout_joined(), h.stderr_joined());
    assert!(h.stdout_joined().contains("in effect"));
}

// covers: UC-instance-lifecycle-36
#[test_id(t45sike)]
#[test]
fn init_guard_reports_could_not_check_when_config_cannot_resolve() {
    assert_eq!(init_verdict(false), Some(EXIT_CANNOT_CHECK));
    assert_eq!(init_verdict(true), None);
}

// covers: UC-instance-lifecycle-37
#[test_id(tnwzjsj)]
#[test]
fn refresh_stashes_dirty_files_and_advances_with_no_lease_consulted() {
    let f = dirty_checkout(" M conf.sh\n");
    f.diff_statuses.borrow_mut().insert("repo|HEAD..origin/main".into(), vec![('M', "spira/conf.sh".into())]);
    f.show_files.borrow_mut().insert("repo|origin/main|spira/conf.sh".into(), b"new".to_vec());
    assert_eq!(refresh(&f, Some("repo"), false), 0);
    assert!(f.stdout_joined().contains("stashed dirty tracked files"), "{}", f.stdout_joined());
    assert!(f.written.borrow().contains_key(&PathBuf::from("repo/spira/conf.sh")));
}

// covers: UC-instance-lifecycle-35
#[test_id(trh98ub)]
#[test]
fn check_cannot_check_when_gh_cannot_list_the_forge_releases_and_says_why() {
    let f = Fake::default();
    f.set_env("SPIRA_REPO", "repo");
    f.set_env("SPIRA_RELEASE_REPO", "owner/repo");
    f.git_repos.borrow_mut().push(PathBuf::from("repo"));
    *f.gh_tags_json.borrow_mut() = Some(Err("HTTP 401: bad credentials".into()));
    assert_eq!(resolve_all_tags(&f, Path::new("repo")), Err(EXIT_CANNOT_CHECK));
    let said = f.stderr_joined();
    assert!(said.contains("HTTP 401: bad credentials") && said.contains("gh credential"), "{said}");
}
