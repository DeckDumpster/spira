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
        self.base.clone()
    }
    fn witness(&self, id: &str) -> Option<String> {
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
        *self.started.get_or_init(std::time::Instant::now) + self.elapsed.get()
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
        "/run/reap.log".into()
    }
    fn worktrees(&self) -> PathBuf {
        self.wts.clone()
    }
}

#[test]
fn flags_parse_as_the_shells_did() {
    let a = |xs: &[&str]| crate::parse(&xs.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let (o, st) = a(&["--dry-run", "--no-fetch", "--status-from", "/f", "sp-x"]).unwrap();
    assert!(o.dry && !o.fetch && o.only.as_deref() == Some("sp-x") && st.as_deref() == Some("/f"));
    assert_eq!(a(&["--skip-queue"]).unwrap().0.scope, Scope::SkipQueue);
    assert_eq!(a(&["--queue-only"]).unwrap().0.scope, Scope::QueueOnly);
    assert!(a(&["--skip-queue", "--queue-only"]).is_err());
    assert!(a(&["--bogus"]).unwrap_err().contains("unknown flag: --bogus"));
    assert!(a(&["--status-from"]).is_err());
}

#[test]
fn harness_home_is_spira_home_then_the_declared_release_never_the_executable() {
    assert_eq!(crate::locate_home(Some("/h"), Some("/r")), Some(PathBuf::from("/h")));
    assert_eq!(crate::locate_home(None, Some("/r")), Some(PathBuf::from("/r/spira")));
    assert_eq!(crate::locate_home(None, None), None);
}

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

    #[test]
    fn a_terminal_bead_with_nothing_unsaved_is_removed() {
        for s in ["DONE", "LANDED", "DROPPED", "SUPERSEDED"] {
            assert_eq!(decide("sp-a", Some(s), true, Unsaved::No, Duration::ZERO, MAX, None), Decision::Remove(Why::Terminal), "{s}");
        }
    }

    #[test]
    fn a_live_bead_is_never_removed_however_old() {
        for s in ["READY", "WORKING", "REWORK", "SUBMITTED", "CERTIFIED", "IN_DELIVERY"] {
            assert_eq!(decide("sp-a", Some(s), true, Unsaved::No, OLD, MAX, None), Decision::Keep("live bead"), "{s}");
        }
    }

    #[test]
    fn unsaved_or_unreadable_commits_keep_the_worktree() {
        assert!(matches!(decide("sp-a", Some("LANDED"), true, Unsaved::Yes, OLD, MAX, None), Decision::Keep(_)));
        assert!(matches!(decide("sp-a", Some("LANDED"), true, Unsaved::Unknown, OLD, MAX, None), Decision::Keep(_)));
        assert!(matches!(decide("chk-1", None, true, Unsaved::Yes, OLD, MAX, None), Decision::Keep(_)));
    }

    #[test]
    fn a_row_less_entry_is_scratch_only_once_it_is_old_enough() {
        assert!(matches!(decide("chk-1", None, false, Unsaved::No, Duration::from_secs(5), MAX, None), Decision::Keep(_)));
        assert_eq!(decide("chk-1", None, false, Unsaved::No, OLD, MAX, None), Decision::Remove(Why::Scratch));
    }

    #[test]
    fn a_dot_named_tree_is_never_touched() {
        assert!(matches!(decide(".harness", None, false, Unsaved::No, OLD, MAX, None), Decision::Keep(_)));
        assert!(matches!(decide(".harness", Some("DONE"), false, Unsaved::No, OLD, MAX, None), Decision::Keep(_)));
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
        let e = crate::sweep::scan_repo(&f, &root, "submitted", false).err().unwrap();
        assert!(matches!(&e, crate::sweep::ScanError::Store(m) if m == "store said no"), "{e:?}");
        assert_eq!(*f.prefetched.borrow(), ["sp-x"]);
    }
}

mod resume {
    use super::*;

    fn sent(f: &Fake) -> Vec<String> {
        f.calls.borrow().iter().filter_map(|c| c.strip_prefix("send ")).map(|c| c.split(' ').next().unwrap().to_string()).collect()
    }

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
