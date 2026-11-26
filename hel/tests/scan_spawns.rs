//! `scan_repo` costs no git process for a branch merged into the land ref, or unmoved since its last scan.

#[macro_use]
extern crate spira_test_id;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use hel::ports::{Base, Sent, World};
use hel::sweep::BranchScan;
use serde_json::Value;

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_TRACE2_EVENT")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

#[derive(Default)]
struct Quiet {
    landed: RefCell<BTreeSet<String>>,
}

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
    fn lc_landed(&self, id: &str) -> bool {
        self.landed.borrow().contains(id)
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
    let scan = hel::sweep::scan_repo(&Quiet::default(), &root, "submitted", false, None, std::time::Duration::from_secs(600)).ok().unwrap();
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

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().map(Result::unwrap) {
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}

fn scan(w: &Quiet, root: &Path) -> Vec<BranchScan> {
    hel::sweep::scan_repo(w, root, "submitted", false, None, std::time::Duration::from_secs(600)).ok().unwrap().branches
}

/// Scans `root`, checks the answer against the first scan of a copy at a path never scanned, and
/// returns which of the two unmerged branches the scan's own git processes named.
fn rescan(dir: &Path, w: &Quiet, root: &Path, step: &str) -> Vec<&'static str> {
    let fresh_root = dir.join(format!("fresh-{step}"));
    copy_tree(root, &fresh_root);
    let trace = dir.join(format!("trace-{step}"));
    let guard = testkit::env(&[("GIT_TRACE2_EVENT", Some(trace.to_str().unwrap()))]);
    let got = scan(w, root);
    drop(guard);
    let guard = testkit::env(&[("GIT_TRACE2_EVENT", None)]);
    let fresh = scan(w, &fresh_root);
    drop(guard);
    assert_eq!(got, fresh, "{step}: reuse changed a scan");
    let trace = std::fs::read_to_string(trace).unwrap();
    let starts: Vec<&str> = trace.lines().filter(|l| l.contains("\"event\":\"start\"")).collect();
    assert!(!starts.is_empty(), "{step}: the trace saw no git at all");
    ["spira/sp-u0", "spira/sp-u1"].into_iter().filter(|b| starts.iter().any(|l| l.contains(b))).collect()
}

#[test_id(tltbt7w)]
#[test]
fn a_rescan_starts_git_only_for_the_branches_whose_tip_bead_state_or_land_ref_moved() {
    let t = testkit::TempDir::new("hel-scan-memo");
    let root = t.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["commit", "-q", "--allow-empty", "-m", "base"]);
    git(&root, &["branch", "spira/sp-m"]);
    for i in 0..2 {
        git(&root, &["checkout", "-q", "-b", &format!("spira/sp-u{i}"), "main"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", &format!("sp-u{i}")]);
    }
    git(&root, &["checkout", "-q", "main"]);
    let w = Quiet::default();
    let both = ["spira/sp-u0", "spira/sp-u1"];

    assert_eq!(rescan(t.path(), &w, &root, "first"), both);
    assert_eq!(rescan(t.path(), &w, &root, "unmoved"), [""; 0]);

    git(&root, &["checkout", "-q", "spira/sp-u0"]);
    git(&root, &["commit", "-q", "--allow-empty", "-m", "sp-u0: more"]);
    git(&root, &["checkout", "-q", "main"]);
    assert_eq!(rescan(t.path(), &w, &root, "tip"), ["spira/sp-u0"]);

    w.landed.borrow_mut().insert("sp-u1".into());
    assert_eq!(rescan(t.path(), &w, &root, "landed"), ["spira/sp-u1"]);

    git(&root, &["commit", "-q", "--allow-empty", "-m", "main moves"]);
    assert_eq!(rescan(t.path(), &w, &root, "land-ref"), both);
}

#[test_id(t437hlh)]
#[test]
fn a_git_read_that_failed_is_asked_again_by_the_next_scan() {
    let t = testkit::TempDir::new("hel-scan-failed-read");
    let root = t.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["commit", "-q", "--allow-empty", "-m", "base"]);
    git(&root, &["checkout", "-q", "-b", "spira/sp-u0"]);
    std::fs::write(root.join("f"), "work\n").unwrap();
    git(&root, &["add", "f"]);
    git(&root, &["commit", "-q", "-m", "sp-u0"]);
    git(&root, &["checkout", "-q", "main"]);
    let out = Command::new("git").arg("-C").arg(&root).args(["rev-parse", "spira/sp-u0^{tree}"]).env_remove("GIT_TRACE2_EVENT").output().unwrap();
    let tree = String::from_utf8(out.stdout).unwrap().trim().to_string();
    let object = root.join(".git/objects").join(&tree[..2]).join(&tree[2..]);
    let hidden = t.path().join("hidden-tree");
    let w = Quiet::default();

    let _guard = testkit::env(&[("GIT_TRACE2_EVENT", None)]);
    std::fs::rename(&object, &hidden).unwrap();
    assert!(!scan(&w, &root)[0].merges_clean, "merge-tree cannot read the branch's tree");
    std::fs::rename(&hidden, &object).unwrap();
    assert!(scan(&w, &root)[0].merges_clean, "the failed merge-tree was held as a conflict");
}
