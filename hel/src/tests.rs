//! The Hel against REAL git repositories (every disposition is a git question) and a
//! recording world for lib.sh's chokepoints, bd, the forge and the lifecycle machine. The
//! fixture is test-hel.sh's, branch for branch; the chokepoints themselves (salvage,
//! the verified deletion, the content fence) are lib.sh's and stay proven by the suites
//! that drive the binary end to end.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::git::Git;
use crate::ports::{Base, Sent, World};
use crate::sweep::Scope;

fn git(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[derive(Default)]
struct Fake {
    beads: BTreeMap<String, Value>,
    held: BTreeSet<String>,
    /// Held only at the mid-send recheck, not at the top of the loop.
    held_mid: BTreeSet<String>,
    queued: BTreeSet<String>,
    fail: BTreeMap<String, String>,
    prs: BTreeMap<String, String>,
    /// Beads the lifecycle record has LANDED (`spira-lc state`).
    lc_landed: BTreeSet<String>,
    /// Beads whose lifecycle row is past the builder (SUBMITTED or later).
    lc_past: BTreeSet<String>,
    /// Beads whose lifecycle row is terminal.
    lc_terminal: BTreeSet<String>,
    base: Option<Base>,
    base_asked: std::cell::Cell<u32>,
    wts: PathBuf,
    destroy_fails: bool,
    /// The store's text when it refuses the branch read.
    refuse: Option<String>,
    out: RefCell<Vec<String>>,
    calls: RefCell<Vec<String>>,
    prefetched: RefCell<Vec<String>>,
    resume: RefCell<Option<String>>,
    /// Each send takes this long on the fake clock, so a short budget cuts the sweep between branches.
    send_takes: std::time::Duration,
    /// Each branch judged takes this long on the fake clock (dry runs send nothing).
    judge_takes: std::time::Duration,
    /// Every read of the clock advances it by this much.
    step: std::time::Duration,
    started: std::cell::OnceCell<std::time::Instant>,
    elapsed: std::cell::Cell<std::time::Duration>,
}

impl Fake {
    fn call(&self, s: String) {
        self.calls.borrow_mut().push(s);
    }
    fn out(&self) -> String {
        self.out.borrow().join("\n")
    }
}

impl World for Fake {
    fn prefetch(&self, ids: &[String]) -> Result<(), String> {
        self.call("prefetch".into());
        self.prefetched.borrow_mut().extend(ids.iter().cloned());
        self.refuse.clone().map_or(Ok(()), Err)
    }
    fn base(&self, _root: &Path) -> Option<Base> {
        self.base_asked.set(self.base_asked.get() + 1);
        self.base.clone()
    }
    fn witness(&self, id: &str) -> Option<String> {
        self.elapsed.set(self.elapsed.get() + self.judge_takes);
        self.held.contains(id).then(|| "in_progress — the lease has not been released".to_string())
    }
    fn bead(&self, id: &str) -> Option<Value> {
        self.call(format!("bead {id}"));
        self.beads.get(id).cloned()
    }
    /// The verified deletion, done for real (worktree then branch), so what the sweep reads
    /// afterwards is what lib.sh would have left.
    fn send(&self, id: &str, br: &str, repo: &Path, why: &str, caller: &str) -> Sent {
        self.call(format!("send {id} {br} {why} {caller}"));
        self.elapsed.set(self.elapsed.get() + self.send_takes);
        if self.held_mid.contains(id) || self.held.contains(id) {
            return Sent::Held("in_progress — the lease has not been released".into());
        }
        if self.queued.contains(id) {
            return Sent::Queued;
        }
        if let Some(e) = self.fail.get(id) {
            return Sent::Failed(e.clone());
        }
        if let Some(w) = Git(repo).worktree_of(br) {
            git(repo, &["worktree", "remove", "--force", w.to_str().unwrap()]);
        }
        git(repo, &["branch", "-D", br]);
        Sent::Done
    }
    fn close_on_land(&self, id: &str, sha: &str) {
        self.call(format!("close {id} {sha}"));
    }
    fn destroy_worktree(&self, id: &str, w: &Path, repo: &Path, _why: &str) -> bool {
        self.call(format!("destroy {id}"));
        if self.destroy_fails {
            return false;
        }
        let _ = Command::new("git").arg("-C").arg(repo).args(["worktree", "remove", "--force"]).arg(w).output();
        true
    }
    fn now(&self) -> std::time::Instant {
        let e = self.elapsed.get();
        self.elapsed.set(e + self.step);
        *self.started.get_or_init(std::time::Instant::now) + e
    }
    fn rest(&self, d: std::time::Duration) {
        self.elapsed.set(self.elapsed.get() + d);
    }
    fn resume_after(&self, _repo: &str) -> Option<String> {
        self.resume.borrow().clone()
    }
    fn set_resume_after(&self, _repo: &str, br: Option<&str>) {
        *self.resume.borrow_mut() = br.map(String::from);
    }
    fn prune(&self, _repo: &Path) {
        self.call("prune".into());
    }
    fn lc_past_builder(&self, id: &str) -> bool {
        self.lc_past.contains(id) || self.lc_landed.contains(id)
    }
    fn lc_landed(&self, id: &str) -> bool {
        self.lc_landed.contains(id)
    }
    fn lc_terminal(&self, id: &str) -> bool {
        self.lc_terminal.contains(id)
    }
    fn content_on_base(&self, id: &str, proof: &str) {
        self.call(format!("lc {id} {proof}"));
    }
    fn pr_merged_tip(&self, _repo: &Path, br: &str) -> Option<String> {
        self.call(format!("gh {br}"));
        self.prs.get(br).cloned()
    }
    fn emit(&self, line: &str) {
        self.out.borrow_mut().push(line.to_string());
    }
    fn log(&self, msg: &str) {
        self.out.borrow_mut().push(format!("LOG {msg}"));
    }
    fn reaplog(&self) -> String {
        "run/reap.log".into()
    }
    fn worktrees(&self) -> PathBuf {
        self.wts.clone()
    }
}

#[test_id(t73x99i)]
#[test]
fn flags_parse_as_the_shells_did() {
    let a = |xs: &[&str]| crate::parse(&xs.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let (o, st) = a(&["--dry-run", "--no-fetch", "--status-from", "f", "sp-x"]).unwrap();
    assert!(o.dry && !o.fetch && o.only.as_deref() == Some("sp-x") && st.as_deref() == Some("f"));
    assert_eq!(a(&["--skip-queue"]).unwrap().0.scope, Scope::SkipQueue);
    assert_eq!(a(&["--queue-only"]).unwrap().0.scope, Scope::QueueOnly);
    assert!(a(&["--skip-queue", "--queue-only"]).is_err());
    assert!(a(&["--bogus"]).unwrap_err().contains("unknown flag: --bogus"));
    assert!(a(&["--status-from"]).is_err());
}

#[test_id(tnu3xgm)]
#[test]
fn harness_home_is_spira_home_then_the_declared_release_never_the_executable() {
    assert_eq!(crate::locate_home(Some("h"), Some("r")), Some(PathBuf::from("h")));
    assert_eq!(crate::locate_home(None, Some("r")), Some(PathBuf::from("r/spira")));
    assert_eq!(crate::locate_home(None, None), None);
}

#[test_id(tue0bhg)]
#[test]
fn budget_flag_parses() {
    let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(crate::parse(&a(&["--budget-secs", "480"])).unwrap().0.budget, Some(std::time::Duration::from_secs(480)));
    assert!(crate::parse(&a(&["--budget-secs", "soon"])).is_err());
    assert!(crate::parse(&a(&["--budget-secs"])).is_err());
}

// ---- terminal.rs: the worktree directory's reaper ---------------------------------------------

mod terminal_reaper {
    use super::*;
    use crate::terminal::{decide, run, Decision, Unsaved, Why};
    use std::collections::HashMap;
    use std::time::Duration;

    const OLD: Duration = Duration::from_secs(10_000);
    const MAX: Duration = Duration::from_secs(100);

    #[test_id(te1eua0)]
    #[test]
    fn a_terminal_bead_with_nothing_unsaved_is_removed() {
        for s in ["DONE", "LANDED", "DROPPED", "SUPERSEDED"] {
            assert_eq!(decide("sp-a", Some(s), true, Unsaved::No, Duration::ZERO, MAX, None), Decision::Remove(Why::Terminal), "{s}");
        }
    }

    #[test_id(tvms6n4)]
    #[test]
    fn a_live_bead_is_never_removed_however_old() {
        for s in ["READY", "WORKING", "REWORK", "SUBMITTED", "CERTIFIED", "IN_DELIVERY"] {
            assert_eq!(decide("sp-a", Some(s), true, Unsaved::No, OLD, MAX, None), Decision::Keep("live bead"), "{s}");
        }
    }

    #[test_id(t6ft8a3)]
    #[test]
    fn unsaved_or_unreadable_commits_keep_the_worktree() {
        assert!(matches!(decide("sp-a", Some("LANDED"), true, Unsaved::Yes, OLD, MAX, None), Decision::Keep(_)));
        assert!(matches!(decide("sp-a", Some("LANDED"), true, Unsaved::Unknown, OLD, MAX, None), Decision::Keep(_)));
        assert!(matches!(decide("chk-1", None, true, Unsaved::Yes, OLD, MAX, None), Decision::Keep(_)));
    }

    #[test_id(ttmy8e8)]
    #[test]
    fn a_row_less_entry_is_scratch_only_once_it_is_old_enough() {
        assert!(matches!(decide("chk-1", None, false, Unsaved::No, Duration::from_secs(5), MAX, None), Decision::Keep(_)));
        assert_eq!(decide("chk-1", None, false, Unsaved::No, OLD, MAX, None), Decision::Remove(Why::Scratch));
    }

    #[test_id(tg4npy2)]
    #[test]
    fn a_dot_named_tree_is_never_touched() {
        assert!(matches!(decide(".harness", None, false, Unsaved::No, OLD, MAX, None), Decision::Keep(_)));
        assert!(matches!(decide(".harness", Some("DONE"), false, Unsaved::No, OLD, MAX, None), Decision::Keep(_)));
    }

    #[test_id(tqlak87)]
    #[test]
    fn scan_asks_no_repository_about_a_dot_name_or_a_live_row() {
        let (t, f) = hold_world();
        for n in [".testbatch-1", "sp-live"] {
            std::fs::create_dir_all(f.wts.join(n)).unwrap();
            std::fs::write(f.wts.join(n).join(".git"), "gitdir: /nowhere").unwrap();
        }
        let states = HashMap::from([("sp-live".to_string(), "WORKING".to_string())]);
        let scanned = crate::terminal::scan(&f, &states).unwrap();
        assert_eq!(scanned.len(), 2);
        assert_eq!(f.base_asked.get(), 0, "the world was asked for a base");
        for e in scanned {
            let state = states.get(&e.name).map(String::as_str);
            assert!(matches!(decide(&e.name, state, e.is_git, e.unsaved, e.age, MAX, e.held), Decision::Keep(_)), "{}", e.name);
            assert!(e.is_git && e.repo.is_none());
        }
        drop(t);
    }

    fn hold_world() -> (testkit::TempDir, Fake) {
        let t = testkit::TempDir::new("hel-hold");
        let wts = t.path().join("run/worktree");
        std::fs::create_dir_all(&wts).unwrap();
        let f = Fake { wts, ..Default::default() };
        (t, f)
    }

    fn backdate(p: &Path) {
        let old = std::time::SystemTime::now() - Duration::from_secs(10_000);
        let f = std::fs::File::open(p).unwrap();
        f.set_modified(old).unwrap();
    }

    #[test_id(tvdqy4f)]
    #[test]
    fn a_keep_marker_outranks_age_and_a_terminal_row() {
        assert!(matches!(decide("acc", None, false, Unsaved::No, OLD, MAX, Some("keep marker")), Decision::Keep("keep marker")));
        let (_t, f) = hold_world();
        let d = f.wts.join("acc");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(".spira-keep"), "").unwrap();
        backdate(&d.join(".spira-keep"));
        backdate(&d);
        assert_eq!(run(&f, &HashMap::new(), MAX, false), 0);
        assert!(d.exists(), "{}", f.out());
    }

    #[test_id(teyc9w2)]
    #[test]
    fn a_tree_with_a_fresh_file_survives_and_a_stale_one_is_reaped() {
        let (_t, f) = hold_world();
        let busy = f.wts.join("busy");
        let stale = f.wts.join("stale");
        for d in [&busy, &stale] {
            std::fs::create_dir_all(d.join("sub")).unwrap();
            std::fs::write(d.join("sub/f"), "x").unwrap();
        }
        std::fs::write(stale.join("sub/f"), "x").unwrap();
        for d in [&busy, &stale] {
            backdate(&d.join("sub/f"));
            backdate(&d.join("sub"));
            backdate(d);
        }
        std::fs::write(busy.join("sub/f"), "fresh").unwrap();
        backdate(&busy.join("sub"));
        backdate(&busy);
        assert_eq!(run(&f, &HashMap::new(), Duration::from_secs(5000), false), 0);
        assert!(busy.exists(), "newest file is fresh: {}", f.out());
        assert!(!stale.exists(), "{}", f.out());
    }
}

mod unreadable_store {
    use super::*;

    #[test_id(t0pgblw)]
    #[test]
    fn a_stale_pass_over_a_store_that_refuses_reaps_nothing_and_says_why() {
        let t = testkit::TempDir::new("hel-refused");
        let wts = t.path().join("run/worktree");
        std::fs::create_dir_all(wts.join("sp-x")).unwrap();
        let f = Fake { wts, refuse: Some("store said no".into()), ..Default::default() };
        let tally = crate::stale::run(&f, &[], crate::stale::Opts { dry: false, idle_secs: 0 });
        assert_eq!(tally, Default::default());
        assert_eq!(*f.prefetched.borrow(), ["sp-x"]);
        assert!(f.out().contains("store unreadable: store said no"), "{}", f.out());
    }

    #[test_id(tu4f55z)]
    #[test]
    fn a_sweep_over_a_store_that_refuses_judges_no_branch_and_says_why() {
        let t = testkit::TempDir::new("hel-refused-sweep");
        let root = t.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", "x"]);
        git(&root, &["branch", "spira/sp-x"]);
        let base = Base { landref: "main".into(), landrefs: vec!["main".into()], remote: None };
        let f = Fake { base: Some(base), wts: t.path().join("wt"), refuse: Some("store said no".into()), ..Default::default() };
        let opts = crate::sweep::Opts { dry: false, fetch: false, only: None, scope: Scope::All, budget: None, pace: Default::default() };
        let mut s = crate::sweep::Sweep { w: &f, opts, submitted_label: "submitted".into(), tally: Default::default(), deadline: None, truncated: false };
        s.repo(&crate::ports::Repo { name: "spira".into(), root: Some(root), forge_queued: false });
        assert!(f.out().contains("SKIP   spira  store unreadable: store said no"), "{}", f.out());
        assert!(!f.calls.borrow().iter().any(|c| c.starts_with("bead ")), "{:?}", f.calls.borrow());
    }

    #[test_id(tct03wd)]
    #[test]
    fn a_scan_over_a_store_that_refuses_is_a_store_error_not_a_skip() {
        let t = testkit::TempDir::new("hel-refused-scan");
        let root = t.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", "x"]);
        git(&root, &["branch", "spira/sp-x"]);
        let base = Base { landref: "main".into(), landrefs: vec!["main".into()], remote: None };
        let f = Fake { base: Some(base), wts: t.path().join("wt"), refuse: Some("store said no".into()), ..Default::default() };
        let e = crate::sweep::scan_repo(&f, &root, "submitted", false, None, std::time::Duration::from_secs(60)).err().unwrap();
        assert!(matches!(&e, crate::sweep::ScanError::Store(m) if m == "store said no"), "{e:?}");
        assert_eq!(*f.prefetched.borrow(), ["sp-x"]);
    }
}

mod resume {
    use super::*;

    fn sent(f: &Fake) -> Vec<String> {
        f.calls.borrow().iter().filter_map(|c| c.strip_prefix("send ")).map(|c| c.split(' ').next().unwrap().to_string()).collect()
    }

    #[test_id(txfvf6m)]
    #[test]
    fn a_pass_cut_at_its_budget_resumes_after_the_last_branch_it_handled() {
        let t = testkit::TempDir::new("hel-resume");
        let root = t.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", "x"]);
        let ids = ["sp-a", "sp-b", "sp-c", "sp-d", "sp-e", "sp-f"];
        for id in ids {
            git(&root, &["branch", &format!("spira/{id}")]);
        }
        let base = Base { landref: "main".into(), landrefs: vec!["main".into()], remote: None };
        let beads = ids.iter().map(|id| (id.to_string(), serde_json::json!({"id": id, "labels": []}))).collect();
        let f = Fake { base: Some(base), beads, wts: t.path().join("wt"), send_takes: std::time::Duration::from_millis(150), ..Default::default() };
        let repo = crate::ports::Repo { name: "spira".into(), root: Some(root), forge_queued: false };
        let pass = |budget_ms: u64| {
            let opts = crate::sweep::Opts { dry: false, fetch: false, only: None, scope: Scope::All, budget: None, pace: Default::default() };
            let mut s = crate::sweep::Sweep { w: &f, opts, submitted_label: "submitted".into(), tally: Default::default(), deadline: Some(f.now() + std::time::Duration::from_millis(budget_ms)), truncated: false };
            s.repo(&repo);
        };
        pass(200);
        let first = sent(&f);
        assert!(!first.is_empty() && first.len() < ids.len(), "the budget cut the pass part-way: {first:?}");
        assert_eq!(f.resume.borrow().as_deref(), Some(format!("spira/{}", first.last().unwrap()).as_str()));
        pass(60_000);
        let all = sent(&f);
        assert_eq!(all[first.len()..], ids.iter().map(|s| s.to_string()).skip(first.len()).collect::<Vec<_>>(), "the second pass starts after the last handled branch: {all:?}");
        assert_eq!(*f.resume.borrow(), None, "a finished pass clears the cursor");
    }

    #[test_id(telv9ze)]
    #[test]
    fn a_dry_run_cut_at_its_budget_names_every_branch_it_did_not_reach_and_totals_them() {
        let t = testkit::TempDir::new("hel-dry-cut");
        let root = t.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", "x"]);
        let ids = ["sp-a", "sp-b", "sp-c", "sp-d", "sp-e"];
        for id in ids {
            git(&root, &["branch", &format!("spira/{id}")]);
        }
        let base = Base { landref: "main".into(), landrefs: vec!["main".into()], remote: None };
        let beads = ids.iter().map(|id| (id.to_string(), serde_json::json!({"id": id, "labels": []}))).collect();
        let f = Fake { base: Some(base), beads, wts: t.path().join("wt"), judge_takes: std::time::Duration::from_millis(100), ..Default::default() };
        let opts = crate::sweep::Opts { dry: true, fetch: false, only: None, scope: Scope::All, budget: Some(std::time::Duration::from_millis(200)), pace: Default::default() };
        let mut s = crate::sweep::Sweep { w: &f, opts, submitted_label: "submitted".into(), tally: Default::default(), deadline: None, truncated: false };
        s.run(&[crate::ports::Repo { name: "spira".into(), root: Some(root), forge_queued: false }]);
        let out = f.out.borrow().clone();
        let lines: Vec<&String> = out.iter().filter(|l| !l.starts_with("LOG ")).collect();
        let unjudged: Vec<&&String> = lines.iter().filter(|l| l.starts_with("UNJUDGED ")).collect();
        assert_eq!(lines.len(), 2 + 3 + 1, "{lines:?}");
        assert_eq!(unjudged.len(), 3, "{lines:?}");
        for id in ["sp-c", "sp-d", "sp-e"] {
            assert!(unjudged.iter().any(|l| **l == &format!("UNJUDGED {id}  budget spent")), "{lines:?}");
        }
        let summary = lines.last().unwrap();
        assert!(summary.starts_with("hel: 5 branches — "), "{summary}");
        assert!(summary.ends_with("3 UNJUDGED"), "{summary}");
        let n: u32 = summary.split(" — ").nth(1).unwrap().split(", ").map(|p| p.split(' ').next().unwrap().parse::<u32>().unwrap()).sum();
        assert_eq!(n, 5, "{summary}");
    }

    #[test_id(tx62jjw)]
    #[test]
    fn a_dry_run_with_no_budget_ends_with_a_summary_counting_each_branch() {
        let t = testkit::TempDir::new("hel-dry-total");
        let root = t.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", "x"]);
        git(&root, &["branch", "spira/sp-a"]);
        git(&root, &["branch", "spira/sp-b"]);
        let base = Base { landref: "main".into(), landrefs: vec!["main".into()], remote: None };
        let beads = ["sp-a", "sp-b"].iter().map(|id| (id.to_string(), serde_json::json!({"id": id, "labels": []}))).collect();
        let f = Fake { base: Some(base), beads, wts: t.path().join("wt"), held: ["sp-b".to_string()].into(), ..Default::default() };
        let opts = crate::sweep::Opts { dry: true, fetch: false, only: None, scope: Scope::All, budget: None, pace: Default::default() };
        let mut s = crate::sweep::Sweep { w: &f, opts, submitted_label: "submitted".into(), tally: Default::default(), deadline: None, truncated: false };
        s.run(&[crate::ports::Repo { name: "spira".into(), root: Some(root), forge_queued: false }]);
        let out = f.out.borrow().clone();
        assert_eq!(out.len(), 3, "{out:?}");
        assert_eq!(out[2], "hel: 2 branches — 1 WOULD, 0 KEEP, 1 HELD, 0 SKIP, 0 UNJUDGED", "{out:?}");
    }

    #[test_id(tohgfne)]
    #[test]
    fn a_cursor_naming_a_branch_already_sent_still_resumes_after_it() {
        let t = testkit::TempDir::new("hel-resume-gone");
        let root = t.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", "x"]);
        let ids = ["sp-a", "sp-b", "sp-d", "sp-e"];
        for id in ids {
            git(&root, &["branch", &format!("spira/{id}")]);
        }
        let base = Base { landref: "main".into(), landrefs: vec!["main".into()], remote: None };
        let beads = ids.iter().map(|id| (id.to_string(), serde_json::json!({"id": id, "labels": []}))).collect();
        let f = Fake { base: Some(base), beads, wts: t.path().join("wt"), ..Default::default() };
        *f.resume.borrow_mut() = Some("spira/sp-c".into());
        let repo = crate::ports::Repo { name: "spira".into(), root: Some(root), forge_queued: false };
        let opts = crate::sweep::Opts { dry: false, fetch: false, only: None, scope: Scope::All, budget: None, pace: Default::default() };
        let mut s = crate::sweep::Sweep { w: &f, opts, submitted_label: "submitted".into(), tally: Default::default(), deadline: None, truncated: false };
        s.repo(&repo);
        assert_eq!(sent(&f), ["sp-d", "sp-e"].map(String::from).to_vec());
    }

    #[test_id(tzk7ezr)]
    #[test]
    fn each_branch_sent_is_followed_by_the_configured_rest() {
        let t = testkit::TempDir::new("hel-pace");
        let root = t.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", "x"]);
        let ids = ["sp-a", "sp-b", "sp-c"];
        for id in ids {
            git(&root, &["branch", &format!("spira/{id}")]);
        }
        let base = Base { landref: "main".into(), landrefs: vec!["main".into()], remote: None };
        let beads = ids.iter().map(|id| (id.to_string(), serde_json::json!({"id": id, "labels": []}))).collect();
        let f = Fake { base: Some(base), beads, wts: t.path().join("wt"), ..Default::default() };
        let opts = crate::sweep::Opts { dry: false, fetch: false, only: None, scope: Scope::All, budget: None, pace: std::time::Duration::from_millis(80) };
        let mut s = crate::sweep::Sweep { w: &f, opts, submitted_label: "submitted".into(), tally: Default::default(), deadline: None, truncated: false };
        let began = f.now();
        s.repo(&crate::ports::Repo { name: "spira".into(), root: Some(root), forge_queued: false });
        assert_eq!(s.tally.sent, 3);
        assert_eq!(f.now() - began, std::time::Duration::from_millis(240));
    }
}

mod scan {
    use super::*;
    use crate::sweep::{disposition, judge, needs_evidence, scan_repo, Ctx, Disp, Facts};
    use std::time::Duration;

    fn commit_file(dir: &Path, name: &str, body: &str, msg: &str) {
        std::fs::write(dir.join(name), body).unwrap();
        git(dir, &["add", name]);
        git(dir, &["commit", "-q", "-m", msg]);
    }

    fn repo_with_main(t: &testkit::TempDir) -> PathBuf {
        let root = t.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        commit_file(&root, "f.txt", "base\n", "base");
        root
    }

    /// A branch off main carrying one commit; main is left checked out.
    fn branch_with(root: &Path, br: &str, file: &str, body: &str, msg: &str) {
        git(root, &["checkout", "-q", "-b", br, "main"]);
        commit_file(root, file, body, msg);
        git(root, &["checkout", "-q", "main"]);
    }

    fn base() -> Base {
        Base { landref: "main".into(), landrefs: vec!["main".into()], remote: None }
    }

    struct FromScan<'a> {
        f: &'a Fake,
        b: &'a crate::sweep::BranchScan,
    }

    impl Facts for FromScan<'_> {
        fn content_on_base(&self) -> bool {
            self.b.content_on_base
        }
        fn has_bead(&self) -> bool {
            self.f.beads.contains_key(&self.b.id)
        }
        fn superseded(&self) -> bool {
            self.b.superseded
        }
        fn ahead(&self) -> Option<u64> {
            self.b.ahead
        }
        fn merges_clean(&self) -> bool {
            self.b.merges_clean
        }
        fn terminal(&self) -> bool {
            self.f.lc_terminal.contains(&self.b.id)
        }
        fn submitted(&self) -> bool {
            self.b.labels.iter().any(|l| l == "submitted") || self.f.lc_past_builder(&self.b.id)
        }
        fn tip(&self) -> Option<String> {
            Some(self.b.tip.clone())
        }
        fn pr_merged_tip(&self) -> Option<String> {
            self.b.pr_merged_tip.clone()
        }
        fn landed(&self) -> bool {
            self.f.lc_landed.contains(&self.b.id)
        }
        fn cherry_unapplied(&self) -> bool {
            self.b.cherry_unapplied
        }
        fn landed_by_subject(&self) -> bool {
            self.b.landed_by_subject
        }
    }

    #[test_id(t7f9tlf)]
    #[test]
    fn a_scan_judges_every_disposition_as_the_live_facts_do() {
        let t = testkit::TempDir::new("hel-scan-disp");
        let root = repo_with_main(&t);
        let bead = |id: &str, labels: &[&str], supersedes: bool| {
            let deps = if supersedes { serde_json::json!([{"type": "supersedes"}]) } else { serde_json::json!([]) };
            (id.to_string(), serde_json::json!({"id": id, "labels": labels, "dependencies": deps}))
        };
        git(&root, &["branch", "spira/sp-anc"]);
        branch_with(&root, "spira/sp-squash", "g.txt", "g\n", "sp-squash: g");
        branch_with(&root, "spira/sp-other", "h.txt", "h1\n", "sp-other: h");
        branch_with(&root, "spira/sp-unsafe", "k.txt", "k\n", "sp-unsafe: k");
        branch_with(&root, "spira/sp-unsafet", "k2.txt", "k\n", "sp-unsafet: k");
        branch_with(&root, "spira/sp-safe", "f.txt", "branch\n", "sp-safe: f");
        branch_with(&root, "spira/sp-pr", "m.txt", "m\n", "sp-pr: m");
        branch_with(&root, "spira/sp-keep", "n.txt", "n\n", "sp-keep: n");
        branch_with(&root, "spira/sp-nobead", "o.txt", "o\n", "sp-nobead: o");
        branch_with(&root, "spira/sp-term", "p.txt", "p\n", "sp-term: p");
        branch_with(&root, "spira/sp-lost", "q.txt", "q\n", "sp-lost: q");
        commit_file(&root, "g.txt", "g\n", "land g");
        commit_file(&root, "h.txt", "h1\n", "sp-other landed under another hash");
        commit_file(&root, "h.txt", "h2\n", "later change");
        commit_file(&root, "f.txt", "main\n", "main moves f");
        let tip = |br: &str| git(&root, &["rev-parse", br]);
        let beads: BTreeMap<String, Value> = [
            bead("sp-anc", &[], false),
            bead("sp-squash", &[], false),
            bead("sp-other", &[], false),
            bead("sp-unsafe", &[], true),
            bead("sp-unsafet", &[], true),
            bead("sp-safe", &[], true),
            bead("sp-pr", &["submitted"], false),
            bead("sp-keep", &[], false),
            bead("sp-term", &[], false),
            bead("sp-lost", &[], false),
        ]
        .into();
        let f = Fake {
            base: Some(base()),
            beads,
            wts: t.path().join("wt"),
            lc_landed: ["sp-other".to_string(), "sp-lost".to_string()].into(),
            lc_terminal: ["sp-unsafet".to_string(), "sp-term".to_string(), "sp-other".to_string(), "sp-lost".to_string()].into(),
            prs: [("spira/sp-pr".to_string(), tip("spira/sp-pr"))].into(),
            ..Default::default()
        };
        let scan = scan_repo(&f, &root, "submitted", false, None, Duration::from_secs(600)).ok().unwrap();
        assert!(!scan.cut);
        let b = base();
        let c = Ctx { w: &f, repo: &root, name: "spira", base: &b, submitted_label: "submitted" };
        let mut seen = BTreeSet::new();
        for br in &scan.branches {
            let live = disposition(&c, &br.id, &br.br);
            let scanned = judge(&FromScan { f: &f, b: br });
            assert_eq!(scanned, live, "{}", br.br);
            seen.insert(live.code());
            let g = Git(&root);
            let evidence_asked = live == Disp::SendContentLanded && br.ahead != Some(0);
            let own = evidence_asked && g.has_own_commit("main", &br.br, &br.id);
            assert_eq!(br.has_own_commit, own, "{}", br.br);
            assert_eq!(br.only_stacked_base, evidence_asked && !own && g.only_stacked_base("main", &br.br), "{}", br.br);
            assert_eq!(br.base_names, evidence_asked && !own && !br.only_stacked_base && g.base_names("main", &br.id, 2000), "{}", br.br);
        }
        for d in [Disp::SendContentLanded, Disp::SendOtherPr, Disp::ReapSupersededSafe, Disp::ReapSquashMerged, Disp::KeepSupersededUnsafe, Disp::KeepUnlanded, Disp::OrphanNoBead, Disp::ArchiveTerminal] {
            assert!(seen.contains(d.code()), "the fixture reaches {}: {seen:?}", d.code());
        }
        let squash = scan.branches.iter().find(|b| b.id == "sp-squash").unwrap();
        let g = Git(&root);
        assert_eq!(squash.ahead, g.ahead("main", "spira/sp-squash"));
        assert_eq!(needs_evidence(squash.ahead.unwrap(), || squash.has_own_commit, || squash.only_stacked_base, || squash.base_names), needs_evidence(squash.ahead.unwrap(), || g.has_own_commit("main", "spira/sp-squash", "sp-squash"), || g.only_stacked_base("main", "spira/sp-squash"), || g.base_names("main", "sp-squash", 2000)));
    }

    #[test_id(t1eqxyj)]
    #[test]
    fn a_landed_beads_branch_whose_diff_is_on_the_base_is_sent_though_no_commit_names_it() {
        let t = testkit::TempDir::new("hel-terminal-evidence");
        let root = repo_with_main(&t);
        for id in ["sp-term", "sp-live"] {
            branch_with(&root, &format!("spira/{id}"), &format!("{id}.txt"), "x\n", "tweak");
            commit_file(&root, &format!("{id}.txt"), "x\n", "landed by another route");
        }
        let beads = ["sp-term", "sp-live"].iter().map(|id| (id.to_string(), serde_json::json!({"id": id, "labels": []}))).collect();
        let f = Fake { base: Some(base()), beads, wts: t.path().join("wt"), lc_terminal: ["sp-term".to_string()].into(), lc_landed: ["sp-term".to_string()].into(), ..Default::default() };
        let opts = crate::sweep::Opts { dry: true, fetch: false, only: None, scope: Scope::All, budget: None, pace: Default::default() };
        let mut s = crate::sweep::Sweep { w: &f, opts, submitted_label: "submitted".into(), tally: Default::default(), deadline: None, truncated: false };
        s.run(&[crate::ports::Repo { name: "spira".into(), root: Some(root), forge_queued: false }]);
        let out = f.out.borrow().clone();
        assert!(out.iter().any(|l| l.starts_with("WOULD  sp-term  send branch")), "{out:?}");
        assert!(out.iter().any(|l| l.starts_with("KEEP   sp-live  ")), "{out:?}");
    }

    #[test_id(t698spe)]
    #[test]
    fn an_observe_slice_spent_after_k_branches_judges_k_and_the_next_scan_returns_the_rest() {
        let t = testkit::TempDir::new("hel-scan-slice");
        let root = repo_with_main(&t);
        let ids = ["sp-a", "sp-b", "sp-c", "sp-d", "sp-e", "sp-f"];
        for id in ids {
            branch_with(&root, &format!("spira/{id}"), &format!("{id}.txt"), "x\n", &format!("{id}: x"));
        }
        let beads = ids.iter().map(|id| (id.to_string(), serde_json::json!({"id": id, "labels": []}))).collect();
        let f = Fake { base: Some(base()), beads, wts: t.path().join("wt"), step: Duration::from_millis(100), ..Default::default() };
        let slice = Duration::from_millis(250);
        let first = scan_repo(&f, &root, "submitted", false, None, slice).ok().unwrap();
        assert!(first.cut);
        assert_eq!(first.branches.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(), ["sp-a", "sp-b", "sp-c"]);
        let last = first.branches.last().unwrap().br.clone();
        let rest = scan_repo(&f, &root, "submitted", false, Some(&last), slice).ok().unwrap();
        assert!(!rest.cut, "a walk to the end is not cut");
        assert_eq!(rest.branches.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(), ["sp-d", "sp-e", "sp-f"]);
    }
}

mod closed_bead_branch {
    use super::*;

    fn sweep(terminal: bool, dry: bool) -> (PathBuf, Vec<String>) {
        let t = Box::leak(Box::new(testkit::TempDir::new("hel-closed")));
        let root = t.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", "base"]);
        git(&root, &["checkout", "-q", "-b", "spira/sp-x", "main"]);
        std::fs::write(root.join("f.txt"), "work\n").unwrap();
        git(&root, &["add", "f.txt"]);
        git(&root, &["commit", "-q", "-m", "the work"]);
        git(&root, &["checkout", "-q", "main"]);
        std::fs::write(root.join("f.txt"), "work\n").unwrap();
        git(&root, &["add", "f.txt"]);
        git(&root, &["commit", "-q", "-m", "landed by another route"]);
        let base = Base { landref: "main".into(), landrefs: vec!["main".into()], remote: None };
        let beads = [("sp-x".to_string(), serde_json::json!({"id": "sp-x", "labels": []}))].into();
        let lc_terminal = if terminal { ["sp-x".to_string()].into() } else { Default::default() };
        let f = Fake { base: Some(base), beads, wts: t.path().join("wt"), lc_terminal, ..Default::default() };
        let opts = crate::sweep::Opts { dry, fetch: false, only: None, scope: Scope::All, budget: None, pace: Default::default() };
        let mut s = crate::sweep::Sweep { w: &f, opts, submitted_label: "submitted".into(), tally: Default::default(), deadline: None, truncated: false };
        s.run(&[crate::ports::Repo { name: "spira".into(), root: Some(root.clone()), forge_queued: false }]);
        let out = f.out.borrow().clone();
        (root, out)
    }

    fn has_branch(root: &Path) -> bool {
        Git(root).branch_exists("spira/sp-x")
    }

    #[test_id(tjac421)]
    #[test]
    fn a_closed_beads_branch_whose_content_is_on_the_base_is_archived_though_no_commit_names_the_bead() {
        let (root, out) = sweep(true, false);
        assert!(!has_branch(&root), "{out:?}");
        assert!(Git(&root).verify("refs/archive/spira/sp-x").is_some(), "{out:?}");
    }

    #[test_id(trqe4ku)]
    #[test]
    fn a_dry_run_says_it_would_archive_a_closed_beads_content_landed_branch_and_touches_nothing() {
        let (root, out) = sweep(true, true);
        assert!(has_branch(&root));
        assert!(out.iter().any(|l| l.starts_with("WOULD  sp-x  archive")), "{out:?}");
    }

    #[test_id(tfkqgen)]
    #[test]
    fn a_live_beads_content_landed_branch_with_no_naming_commit_is_still_kept() {
        let (root, out) = sweep(false, false);
        assert!(has_branch(&root));
        assert!(out.iter().any(|l| l.starts_with("KEEP   sp-x")), "{out:?}");
    }
}
