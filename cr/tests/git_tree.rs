//! `GitTree` against a real throwaway repository, and `cr pick` over it.

#[macro_use]
extern crate spira_test_id;
use cr::cli::run;
use cr::ledger::Ledger;
use cr::ports::{Config, GitTree, Ports, SourceTree, VirtualClock};
use std::path::Path;
use std::process::Command;

struct NoGh;
impl forge::ports::Gh for NoGh {
    fn call(&self, _: Option<&Path>, _: &[&str]) -> forge::ports::GhOut {
        unreachable!("cr pick makes no forge call")
    }
    fn call_merged(&self, _: Option<&Path>, _: &[&str], _: Option<&[u8]>) -> forge::ports::GhOut {
        unreachable!("cr pick makes no forge call")
    }
}

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
    let config = Config { page_lines: 60, oversize_pages: 6, exclude: vec![], churn_days: 60, count_limit: 50 };
    let ports = Ports { run_dir: dir.path().join("unused-run"), gh: Box::new(NoGh), repo_owner: String::new(), repo_name: String::new(), findings_dir: String::new(), committer: Box::new(cr::ports::GitCommitter), source_repo: "k".into(), stdin: String::new(), clock: Box::new(VirtualClock::new(1_700_000_000_000)), tree: Box::new(tree), ledger: Ledger::default(), config, env_seed: None };
    let o = run(&["pick".into(), "--seed".into(), "1".into(), "--json".into()], &ports);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let v: serde_json::Value = serde_json::from_str(&o.stdout).unwrap();
    assert_eq!(v["unit"]["id"], "module:k/src/lib.rs");
    assert_eq!(v["weight"]["commits"], 2);
}
