//! `scan_repo` over merged branches costs no git process per branch.

#[macro_use]
extern crate spira_test_id;
use std::path::{Path, PathBuf};
use std::process::Command;

use hel::ports::{Base, Sent, World};
use serde_json::Value;

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

struct Quiet;

impl World for Quiet {
    fn base(&self, _root: &Path) -> Option<Base> {
        Some(Base { landref: "main".into(), landrefs: vec!["main".into()], remote: None })
    }
    fn witness(&self, _id: &str) -> Option<String> {
        None
    }
    fn bead(&self, id: &str) -> Option<Value> {
        Some(serde_json::json!({"id": id, "labels": []}))
    }
    fn send(&self, _: &str, _: &str, _: &Path, _: &str, _: &str) -> Sent {
        unreachable!()
    }
    fn close_on_land(&self, _: &str, _: &str) {}
    fn destroy_worktree(&self, _: &str, _: &Path, _: &Path, _: &str) -> bool {
        unreachable!()
    }
    fn prune(&self, _: &Path) {}
    fn lc_past_builder(&self, _: &str) -> bool {
        false
    }
    fn lc_landed(&self, _: &str) -> bool {
        false
    }
    fn lc_terminal(&self, _: &str) -> bool {
        false
    }
    fn content_on_base(&self, _: &str, _: &str) {}
    fn pr_merged_tip(&self, _: &Path, _: &str) -> Option<String> {
        None
    }
    fn emit(&self, _: &str) {}
    fn log(&self, _: &str) {}
    fn reaplog(&self) -> String {
        String::new()
    }
    fn worktrees(&self) -> PathBuf {
        PathBuf::from("nonexistent-run/worktree")
    }
}

/// A repo with `merged` branches at main's tip and two that are not; the git processes one
/// `scan_repo` of it starts.
fn spawns(dir: &Path, merged: usize) -> usize {
    let root = dir.join(format!("repo{merged}"));
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["commit", "-q", "--allow-empty", "-m", "base"]);
    for i in 0..merged {
        git(&root, &["branch", &format!("spira/sp-m{i:03}")]);
    }
    for i in 0..2 {
        git(&root, &["checkout", "-q", "-b", &format!("spira/sp-u{i}"), "main"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", &format!("sp-u{i}")]);
    }
    git(&root, &["checkout", "-q", "main"]);
    let trace = dir.join(format!("trace{merged}"));
    let guard = testkit::env(&[("GIT_TRACE2_EVENT", Some(trace.to_str().unwrap()))]);
    let scan = hel::sweep::scan_repo(&Quiet, &root, "submitted", false, None, std::time::Duration::from_secs(600)).ok().unwrap();
    drop(guard);
    assert_eq!(scan.branches.len(), merged + 2);
    assert!(scan.branches.iter().filter(|b| b.br.starts_with("spira/sp-m")).all(|b| b.content_on_base && b.ahead == Some(0)));
    std::fs::read_to_string(trace).unwrap().lines().filter(|l| l.contains("\"event\":\"start\"")).count()
}

#[test_id(txhdaon)]
#[test]
fn the_git_processes_a_scan_starts_do_not_grow_with_the_branches_merged_into_the_land_ref() {
    let t = testkit::TempDir::new("hel-scan-spawns");
    let few = spawns(t.path(), 5);
    let many = spawns(t.path(), 50);
    assert_eq!(few, many, "5 merged branches cost {few} git processes, 50 cost {many}");
}
