//! Everything skew.sh's ported logic needs from the world, as a trait (DESIGN.md §4).
//! `real.rs` implements it against the host (git, bd, release, gh, mail.sh, overrides.sh,
//! install.sh, exclude.sh, and the lib.sh repository-map seam); `tests.rs` implements it as a
//! fake recording calls and returning canned data — the same technique `forge`'s `FakeGh`
//! and `gate-check`'s fakes use.

use std::path::{Path, PathBuf};

/// One `git diff --name-status` row: the status letter (M/A/D) and the path.
pub type StatusRow = (char, String);

pub trait World {
    // ---- repository map / lib.sh seam (never re-derived; lib.sh stays the one authority) ----
    fn repo_names(&self) -> Vec<String>;
    fn repo_root(&self, name: &str) -> Option<PathBuf>;
    fn repo_field(&self, name: &str, field: &str) -> Option<String>;
    fn same_repo(&self, a: &Path, b: &Path) -> bool;
    fn home_repo(&self) -> String;
    /// `spira_landref <repo-path>` — the ref that repository lands on, or None if unresolvable.
    fn landref(&self, repo: &Path) -> Option<String>;
    fn repo_land(&self, name: &str) -> String;
    fn ref_remote(&self, base: &str, repo: &Path) -> Option<String>;
    fn ref_branch(&self, base: &str) -> String;

    // ---- git, always scoped to a repo path ----
    fn is_git_repo(&self, p: &Path) -> bool;
    /// `git ls-files` (working tree) piped through `exclude.sh harness-in`.
    fn harness_in(&self, repo: &Path) -> Vec<String>;
    /// `git ls-tree -r --name-only <ref>` piped through `exclude.sh harness-in`.
    fn harness_in_ref(&self, repo: &Path, ref_: &str) -> Vec<String>;
    /// `git diff --name-only <base>...<ref>`.
    fn changed_files(&self, repo: &Path, base: &str, ref_: &str) -> Vec<String>;
    /// `git tag -l '<pattern>'`, sorted.
    fn tags_matching(&self, repo: &Path, pattern: &str) -> Vec<String>;
    /// LOCAL RELEASE SOURCE: `cat <dir>/*.tag 2>/dev/null | tr -d ' \t' | grep '^spira-release-' |
    /// sort -u` — every `.tag` sidecar beside the release tarballs in `dir`, read as plain
    /// files (never `git tag -l`; `dir` is not a git repository, and `tags_matching` dialing
    /// out to git against it returns nothing — the bug `resolve_all_tags` hit before this
    /// method existed, caught live by testenv's "local-dir" cases, sp-yyk47).
    fn local_tag_sidecars(&self, dir: &Path) -> Vec<String>;
    /// `git rev-parse -q --verify <rev>^{commit}` (or plain rev-parse when `peel` is false).
    fn rev_parse(&self, repo: &Path, rev: &str, peel: bool) -> Option<String>;
    fn rev_parse_short(&self, repo: &Path, rev: &str) -> Option<String>;
    fn is_ancestor(&self, repo: &Path, ancestor: &str, descendant: &str) -> bool;
    /// `git rev-list --count <range>`.
    fn rev_list_count(&self, repo: &Path, range: &str) -> Option<u64>;
    /// `git branch --show-current`; empty string for a detached HEAD.
    fn current_branch(&self, repo: &Path) -> String;
    /// `git status --porcelain --untracked-files=no`; empty when clean.
    fn dirty_tracked(&self, repo: &Path) -> String;
    fn stash_push(&self, repo: &Path, tag: &str) -> Result<(), String>;
    fn fetch(&self, repo: &Path, remote: &str) -> bool;
    /// `git diff --diff-filter=MAD --name-status <range>`.
    fn diff_status(&self, repo: &Path, range: &str) -> Vec<StatusRow>;
    /// `git show <rev>:<path>`.
    fn show_file(&self, repo: &Path, rev: &str, path: &str) -> Option<Vec<u8>>;
    /// `git ls-tree <rev> <path>` mode column ("100755" or "100644", ...).
    fn file_mode(&self, repo: &Path, rev: &str, path: &str) -> Option<String>;
    fn reset_mixed(&self, repo: &Path, rev: &str) -> bool;
    fn merge_ff_only(&self, repo: &Path, rev: &str) -> bool;

    // ---- other Spira tools, called exactly as skew.sh called them ----
    /// `release verify <name> --no-pre-activate [--releases <releases>]` -> (ok, combined output).
    fn release_verify_no_pre_activate(&self, name: &str, releases: Option<&Path>) -> (bool, String);
    /// `release status` -> combined stdout.
    fn release_status(&self) -> String;
    /// `release build <sha> --repo <repo> --releases <releases>`, then `release verify <sha>
    /// --releases <releases>`, then `release activate <sha> --repo <repo> --landed-ref <base>
    /// --releases <releases>`, in that order; Err on the first failure.
    fn release_build_verify_activate(&self, sha: &str, repo: &Path, base: &str, releases: &Path) -> Result<(), String>;
    fn overrides_apply(&self, repo: &Path);
    /// `bash <installer> --diff` -> (exit code, combined output).
    fn install_diff(&self, installer: &Path) -> (i32, String);
    /// `gh release list --repo <slug> --json tagName,isDraft` -> Ok(json) or Err(stderr's first line).
    fn gh_release_list(&self, slug: &str) -> Result<String, String>;
    /// `mail.sh send operator --from "Skew check <skew@spira>" --subject <subject> --kind
    /// question --default <default_action>`, body on stdin -> Ok or Err(combined output).
    fn mail_send_question(&self, subject: &str, body: &str, default_action: &str) -> Result<(), String>;

    // ---- filesystem / env ----
    /// `date +%Y%m%dT%H%M%SZ` (UTC) — used only to build a unique stash tag.
    fn now_stamp(&self) -> String;
    fn env(&self, k: &str) -> Option<String>;
    fn is_symlink(&self, p: &Path) -> bool;
    fn readlink(&self, p: &Path) -> Option<String>;
    fn exists(&self, p: &Path) -> bool;
    fn read_to_string(&self, p: &Path) -> Option<String>;
    /// `cd <p> 2>/dev/null && pwd -P` — None when `p` does not exist or is not a directory.
    fn canonicalize_dir(&self, p: &Path) -> Option<PathBuf>;
    /// Writes `content`, chmod 755 when `executable`, else 644. Atomic (write-and-rename)
    /// when the target already exists so a reader mid-refresh never sees a truncated file.
    fn write_staged(&self, p: &Path, content: &[u8], executable: bool) -> Result<(), String>;
    fn remove_file(&self, p: &Path);
    fn mkdir_p(&self, p: &Path);

    // ---- escalate's once-per-condition-per-run stamp ----
    fn stamp_read(&self, key: &str) -> Option<String>;
    fn stamp_write(&self, key: &str, val: &str);

    // ---- output (so tests can capture without touching real stdout/stderr) ----
    fn out(&self, s: &str);
    fn err(&self, s: &str);
}
