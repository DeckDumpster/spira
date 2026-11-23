//! `GitTree` against a real throwaway repository, and `cr pick` over it.

#[macro_use]
extern crate spira_test_id;

use cr::cli::run;
use cr::ports::{Config, GitTree, Ports, SourceTree, VirtualClock};
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

fn repo() -> testkit::TempDir {
    let dir = testkit::TempDir::new("cr-git");
    let p = dir.path();
    git(p, &["init", "-q", "-b", "main"]);
    std::fs::write(p.join("Cargo.toml"), "[workspace]\nmembers = [\"k\"]\n").unwrap();
    std::fs::create_dir_all(p.join("k/src")).unwrap();
    std::fs::write(p.join("k/Cargo.toml"), "[package]\nname = \"k\"\n").unwrap();
    std::fs::write(p.join("k/src/lib.rs"), "//! K.\npub fn a() {}\n").unwrap();
    git(p, &["add", "-A"]);
    git(p, &["commit", "-q", "-m", "one"]);
    std::fs::write(p.join("k/src/lib.rs"), "//! K.\npub fn a() {}\npub fn b() {}\n").unwrap();
    git(p, &["commit", "-q", "-am", "two"]);
    std::fs::write(p.join("k/src/lib.rs"), "uncommitted").unwrap();
    dir
}

#[test_id(tj1lnr3)]
#[test]
fn git_tree_reads_the_resolved_commit_not_the_working_tree() {
    let dir = repo();
    let t = GitTree::new(dir.path().to_str().unwrap(), "main").unwrap();
    assert_eq!(t.files().unwrap(), ["Cargo.toml", "k/Cargo.toml", "k/src/lib.rs"]);
    assert_eq!(String::from_utf8(t.read("k/src/lib.rs").unwrap()).unwrap(), "//! K.\npub fn a() {}\npub fn b() {}\n");
    assert_eq!(t.blob("k/src/lib.rs").unwrap().len(), 40);
    let h = t.history("k/src/lib.rs", 0);
    assert_eq!((h.commits, h.newest_ms), (2, Some(1_700_000_000_000)));
    assert_eq!(t.history("k/src/lib.rs", 1_700_000_000_001).commits, 0);
    assert!(GitTree::new(dir.path().to_str().unwrap(), "no-such-ref").is_err());
}

#[test_id(tdd7mwv)]
#[test]
fn cr_pick_runs_over_a_real_git_tree() {
    let dir = repo();
    let tree = GitTree::new(dir.path().to_str().unwrap(), "main").unwrap();
    let findings = testkit::TempDir::new("cr-git-findings");
    let init = std::process::Command::new("git").arg("-C").arg(findings.path()).args(["init", "-q", "-b", "main"]).status().unwrap();
    assert!(init.success());
    let config = Config { size_unit_lines: 60, exclude: vec![], churn_days: 60, rewrite_days: 90, count_limit: 50, status_limit: 10 };
    let ports = Ports { run_dir: dir.path().join("unused-run"), margin: Box::new(margin_client::FakeMargin::default()), findings_dir: findings.path().to_str().unwrap().to_string(), committer: Box::new(cr::ports::GitCommitter), source_repo: "k".into(), source_url: Ok("https://github.com/acme/k".into()), stdin: String::new(), clock: Box::new(VirtualClock::new(1_700_000_000_000)), tree: Box::new(tree), beads: Box::new(cr::ports::FixtureBeads::default()), registry: Box::new(cr::ports::FixtureRegistry::default()), config, env_seed: None };
    let o = run(&["pick".into(), "--seed".into(), "1".into(), "--json".into()], &ports);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let v: serde_json::Value = serde_json::from_str(&o.stdout).unwrap();
    assert_eq!(v["unit"]["id"], "module:k/src/lib.rs");
    assert_eq!(v["weight"]["commits"], 2);
}

#[test_id(t967db9)]
#[test]
fn git_tree_reads_the_full_log_line_times_and_deleted_tests_of_a_real_history() {
    let dir = testkit::TempDir::new("cr-git-history");
    let p = dir.path();
    git(p, &["init", "-q", "-b", "main"]);
    let file = p.join("lib.rs");
    std::fs::write(&file, "pub fn a() {}\n\n#[test]\nfn gone() {\n    a();\n}\n\n#[test]\nfn kept() {\n    a();\n}\n").unwrap();
    git(p, &["add", "-A"]);
    git(p, &["commit", "-q", "-m", "sp-aaa111: first"]);
    std::fs::write(&file, "pub fn a() {}\n\n#[test]\nfn kept() {\n    a();\n}\n").unwrap();
    git(p, &["commit", "-q", "-am", "sp-bbb222: drop a test", "-m", "This reverts commit abc"]);
    let t = GitTree::new(p.to_str().unwrap(), "main").unwrap();
    let log = t.log("lib.rs");
    assert_eq!(log.iter().map(|c| (c.subject.as_str(), c.reverts)).collect::<Vec<_>>(), [("sp-bbb222: drop a test", true), ("sp-aaa111: first", false)]);
    assert_eq!(t.line_times("lib.rs"), vec![1_700_000_000_000; 6]);
    let removed = t.removed_tests(&["lib.rs".to_string()]);
    assert_eq!(removed.iter().map(|r| (r.name.as_str(), r.sha.as_str())).collect::<Vec<_>>(), [("gone", log[0].sha.as_str())]);
}

#[test_id(tqtoxhv)]
#[test]
fn renames_and_changed_lines_come_from_git() {
    let dir = repo();
    let p = dir.path();
    let first = String::from_utf8(Command::new("git").arg("-C").arg(p).args(["rev-parse", "HEAD"]).output().unwrap().stdout).unwrap().trim().to_string();
    let old_blob = GitTree::new(p.to_str().unwrap(), "main").unwrap().blob("k/src/lib.rs").unwrap();
    git(p, &["mv", "k/src/lib.rs", "k/src/core.rs"]);
    std::fs::write(p.join("k/src/core.rs"), "//! K.\npub fn a() {}\npub fn b() {}\npub fn c() {}\n").unwrap();
    git(p, &["add", "-A"]);
    git(p, &["commit", "-q", "-m", "rename and grow"]);
    let t = GitTree::new(p.to_str().unwrap(), "main").unwrap();
    let renames = t.renamed_since(&first).unwrap();
    assert_eq!(renames.get("k/src/lib.rs").map(String::as_str), Some("k/src/core.rs"), "{renames:?}");
    assert!(t.renamed_since("no-such-commit").is_err());
    let changed = t.changed_lines(&old_blob, "k/src/core.rs").unwrap();
    assert_eq!(changed, 1);
    assert_eq!(t.changed_lines(&old_blob, "k/src/missing.rs"), None);
}
