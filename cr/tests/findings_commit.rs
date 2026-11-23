//! `GitCommitter` over a real repository: only the named paths are committed, the message
//! comes through stdin, and a rerun with nothing changed makes no commit.

#[macro_use]
extern crate spira_test_id;
use cr::ports::{Committer, GitCommitter};
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=a", "-c", "user.email=a@a", "-c", "commit.gpgsign=false"]).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

#[test_id(tvxcc7z)]
#[test]
fn commits_exactly_the_named_paths_with_the_message_and_makes_no_empty_commit() {
    let d = testkit::TempDir::new("cr-commit");
    let p = d.path();
    git(p, &["init", "-q", "-b", "main"]);
    git(p, &["-c", "user.name=a", "-c", "user.email=a@a", "commit", "-q", "--allow-empty", "-m", "root"]);
    std::fs::write(p.join("cr-001.md"), "page\n").unwrap();
    std::fs::write(p.join("patterns.md"), "catalogue\n").unwrap();
    std::fs::write(p.join("other.md"), "someone else's\n").unwrap();
    git(p, &["add", "other.md"]);
    let _env = testkit::env(&[("GIT_AUTHOR_NAME", Some("a")), ("GIT_AUTHOR_EMAIL", Some("a@a")), ("GIT_COMMITTER_NAME", Some("a")), ("GIT_COMMITTER_EMAIL", Some("a@a"))]);
    GitCommitter.commit(p, &["cr-001.md", "patterns.md"], "subject line\n\nbody\n").unwrap();
    assert_eq!(git(p, &["show", "--name-only", "--format=%B", "HEAD"]).trim(), "subject line\n\nbody\n\n\ncr-001.md\npatterns.md");
    assert!(git(p, &["status", "--porcelain"]).contains("A  other.md"));
    GitCommitter.commit(p, &["cr-001.md", "patterns.md"], "again\n").unwrap();
    assert_eq!(git(p, &["rev-list", "--count", "HEAD"]).trim(), "2");
}

#[test_id(tgl4lyb)]
#[test]
fn has_history_is_false_for_a_never_committed_path_and_true_once_it_was_committed_then_deleted() {
    let _env = testkit::env_read();
    let d = testkit::TempDir::new("cr-history");
    let p = d.path();
    git(p, &["init", "-q", "-b", "main"]);
    assert!(!GitCommitter.has_history(p, "ledger.jsonl").unwrap(), "an unborn branch has no history");
    git(p, &["-c", "user.name=a", "-c", "user.email=a@a", "commit", "-q", "--allow-empty", "-m", "root"]);
    assert!(!GitCommitter.has_history(p, "ledger.jsonl").unwrap());
    std::fs::write(p.join("ledger.jsonl"), "{}\n").unwrap();
    git(p, &["add", "ledger.jsonl"]);
    git(p, &["-c", "user.name=a", "-c", "user.email=a@a", "commit", "-q", "-m", "add"]);
    git(p, &["rm", "-q", "ledger.jsonl"]);
    git(p, &["-c", "user.name=a", "-c", "user.email=a@a", "commit", "-q", "-m", "gone"]);
    assert!(GitCommitter.has_history(p, "ledger.jsonl").unwrap());
}
