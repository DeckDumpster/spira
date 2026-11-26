//! `scan_repo` costs no git process for a branch merged into the land ref, or unmoved since its last scan.

#[macro_use]
extern crate spira_test_id;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
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

fn rev_parse(dir: &Path, rev: &str) -> String {
    let o = Command::new("git").arg("-C").arg(dir).args(["rev-parse", rev]).env_remove("GIT_TRACE2_EVENT").output().unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

#[derive(Default)]
struct Quiet {
    landed: RefCell<BTreeSet<String>>,
    terminal: RefCell<BTreeSet<String>>,
    past_builder: RefCell<BTreeSet<String>>,
    /// Beads that differ from the plain one: another record, or none.
    beads: RefCell<BTreeMap<String, Option<Value>>>,
    /// Run once, when the scan reaches its first branch: after the tips are listed, before git is asked about it.
    mid_scan: RefCell<Option<Box<dyn FnOnce()>>>,
}

impl World for Quiet {
    fn base(&self, _root: &Path) -> Option<Base> {
        Some(Base { landref: "main".into(), landrefs: vec!["main".into()], remote: None })
    }
    fn witness(&self, _id: &str) -> Option<String> {
        if let Some(f) = self.mid_scan.take() {
            f();
        }
        None
    }
    fn bead(&self, id: &str) -> Option<Value> {
        self.beads.borrow().get(id).cloned().unwrap_or_else(|| Some(serde_json::json!({"id": id, "labels": []})))
    }
    fn send(&self, _: &str, _: &str, _: &Path, _: &str, _: &str) -> Sent {
        unreachable!()
    }
    fn close_on_land(&self, _: &str, _: &str) {}
    fn destroy_worktree(&self, _: &str, _: &Path, _: &Path, _: &str) -> bool {
        unreachable!()
    }
    fn prune(&self, _: &Path) {}
    fn lc_past_builder(&self, id: &str) -> bool {
        self.past_builder.borrow().contains(id)
    }
    fn lc_landed(&self, id: &str) -> bool {
        self.landed.borrow().contains(id)
    }
    fn lc_terminal(&self, id: &str) -> bool {
        self.terminal.borrow().contains(id)
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
    // `base_names` names the bead, not the branch.
    ["spira/sp-u0", "spira/sp-u1"].into_iter().filter(|b| starts.iter().any(|l| l.contains(b) || l.contains(&format!("--grep={}\"", &b[6..])))).collect()
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
    let tree = rev_parse(&root, "spira/sp-u0^{tree}");
    let object = root.join(".git/objects").join(&tree[..2]).join(&tree[2..]);
    let hidden = t.path().join("hidden-tree");
    let w = Quiet::default();

    let _guard = testkit::env(&[("GIT_TRACE2_EVENT", None)]);
    std::fs::rename(&object, &hidden).unwrap();
    assert!(!scan(&w, &root)[0].merges_clean, "merge-tree cannot read the branch's tree");
    std::fs::rename(&hidden, &object).unwrap();
    assert!(scan(&w, &root)[0].merges_clean, "the failed merge-tree was held as a conflict");
}

fn commit_file(root: &Path, file: &str, msg: &str) {
    std::fs::write(root.join(file), "work\n").unwrap();
    git(root, &["add", file]);
    git(root, &["commit", "-q", "-m", msg]);
}

/// `main` and `spira/sp-u0`, `spira/sp-u1`, each one commit of its own file whose subject is `subject(i)`.
fn two_branches(dir: &Path, subject: impl Fn(usize) -> String) -> PathBuf {
    let root = dir.join("repo");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["commit", "-q", "--allow-empty", "-m", "base"]);
    for i in 0..2 {
        git(&root, &["checkout", "-q", "-b", &format!("spira/sp-u{i}"), "main"]);
        commit_file(&root, &format!("f{i}"), &subject(i));
    }
    git(&root, &["checkout", "-q", "main"]);
    root
}

#[test_id(ty5z6nx)]
#[test]
fn every_git_fact_of_an_unmoved_branch_is_reused_as_it_was_read() {
    let t = testkit::TempDir::new("hel-scan-memo-facts");
    let root = two_branches(t.path(), |_| "tweak".into());
    // sp-u1's change reaches main by another route, under its name; sp-u0's never does.
    commit_file(&root, "f1", "sp-u1: by another route");
    let w = Quiet::default();
    w.landed.borrow_mut().insert("sp-u0".into());

    assert_eq!(rescan(t.path(), &w, &root, "first"), ["spira/sp-u0", "spira/sp-u1"]);
    assert_eq!(rescan(t.path(), &w, &root, "unmoved"), [""; 0]);

    let _guard = testkit::env(&[("GIT_TRACE2_EVENT", None)]);
    let got = scan(&w, &root);
    assert!(got[0].cherry_unapplied && !got[0].landed_by_subject, "the LANDED path was reached: {:?}", got[0]);
    assert!(!got[1].has_own_commit && !got[1].only_stacked_base && got[1].base_names, "the evidence path was walked to its end: {:?}", got[1]);
}

#[test_id(t5fmsjk)]
#[test]
fn each_bead_fact_in_the_key_rereads_only_the_branch_whose_fact_changed() {
    let t = testkit::TempDir::new("hel-scan-memo-bead");
    let root = two_branches(t.path(), |i| format!("sp-u{i}"));
    let w = Quiet::default();
    rescan(t.path(), &w, &root, "first");
    let bead = |v: Option<Value>| {
        w.beads.borrow_mut().insert("sp-u1".into(), v);
    };
    let changes: [(&str, &dyn Fn()); 7] = [
        ("terminal", &|| {
            w.terminal.borrow_mut().insert("sp-u1".into());
        }),
        ("past-builder", &|| {
            w.past_builder.borrow_mut().insert("sp-u1".into());
        }),
        ("submitted", &|| bead(Some(serde_json::json!({"id": "sp-u1", "labels": ["submitted"]})))),
        ("superseded", &|| bead(Some(serde_json::json!({"id": "sp-u1", "labels": ["submitted"], "dependencies": [{"type": "supersedes"}]})))),
        // Asks no new fact, so only the key can make this one read again.
        ("unsuperseded", &|| bead(Some(serde_json::json!({"id": "sp-u1", "labels": ["submitted"]})))),
        ("plain", &|| bead(Some(serde_json::json!({"id": "sp-u1", "labels": []})))),
        ("no-bead", &|| bead(None)),
    ];
    for (step, change) in changes {
        change();
        assert_eq!(rescan(t.path(), &w, &root, step), ["spira/sp-u1"], "{step}");
    }
}

#[test_id(ticccb5)]
#[test]
fn a_branch_that_left_the_tips_is_forgotten() {
    let t = testkit::TempDir::new("hel-scan-memo-prune");
    let root = two_branches(t.path(), |i| format!("sp-u{i}"));
    let w = Quiet::default();
    rescan(t.path(), &w, &root, "first");
    let tip = rev_parse(&root, "spira/sp-u0");

    git(&root, &["branch", "-q", "-D", "spira/sp-u0"]);
    {
        let _guard = testkit::env(&[("GIT_TRACE2_EVENT", None)]);
        assert_eq!(scan(&w, &root).len(), 1);
    }
    git(&root, &["branch", "spira/sp-u0", &tip]);
    assert_eq!(rescan(t.path(), &w, &root, "recreated"), ["spira/sp-u0"]);
}

#[test_id(tn00d8k)]
#[test]
fn answers_read_after_a_branch_moved_mid_scan_are_not_held_for_the_tip_it_left() {
    let t = testkit::TempDir::new("hel-scan-memo-moved");
    let root = two_branches(t.path(), |i| format!("sp-u{i}"));
    let listed = rev_parse(&root, "spira/sp-u0");
    let w = Quiet::default();
    let moved = root.clone();
    *w.mid_scan.borrow_mut() = Some(Box::new(move || git(&moved, &["update-ref", "refs/heads/spira/sp-u0", "main"])));
    {
        let _guard = testkit::env(&[("GIT_TRACE2_EVENT", None)]);
        scan(&w, &root);
    }
    git(&root, &["update-ref", "refs/heads/spira/sp-u0", &listed]);
    assert_eq!(rescan(t.path(), &w, &root, "back"), ["spira/sp-u0"]);
}
