//! Contract tests (DESIGN.md §9): the pass driven through recording fakes of bd, git, the
//! lib.sh seam, the harness programs, liveness and the clock. Push mode's merge-and-push is
//! also run once against real temporary repositories.

use crate::halt::{self, HaltArgs, HaltCtx, HaltPorts};
use crate::model::*;
use crate::pass::Pass;
use crate::ports::*;
use crate::pr::{PrPass, PrTools};
use crate::records::Files;
use crate::report::Reporter;
use crate::testutil::tmpdir;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

// ──────────────────────────────────────────────────────────────────────────────
// Fakes
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct FakeBeads {
    rows: RefCell<HashMap<String, BeadRow>>,
    /// Status seen by a re-read (bead_land_status), when it differs from the scan.
    reread: RefCell<HashMap<String, String>>,
    fail: Cell<bool>,
}

impl Beads for FakeBeads {
    fn show(&self, ids: &[String]) -> Result<Vec<BeadRow>, String> {
        if self.fail.get() {
            return Err("unreachable".into());
        }
        let r = self.rows.borrow();
        Ok(ids.iter().filter_map(|i| r.get(i).cloned()).collect())
    }
    fn land_status(&self, id: &str) -> String {
        if let Some(s) = self.reread.borrow().get(id) {
            return s.clone();
        }
        self.rows.borrow().get(id).map(|b| b.status.clone()).unwrap_or_else(|| "-".into())
    }
    fn ask_open(&self, _label: &str, _subject: &str) -> bool {
        false
    }
    fn context(&self, id: &str, _now: i64) -> String {
        format!("(fixture context for {id})")
    }
}

#[derive(Default)]
struct FakeGit {
    /// branch → tip
    refs: RefCell<Vec<(String, String)>>,
    content: RefCell<HashSet<String>>,
    ancestors: RefCell<HashSet<(String, String)>>,
    shas: RefCell<HashMap<String, String>>,
    head: Cell<u32>,
    tree: Cell<bool>,
    merge_conflict: RefCell<HashSet<String>>,
    deleted: RefCell<Vec<String>>,
    matching: RefCell<HashMap<String, Vec<String>>>,
}

impl FakeGit {
    fn add(&self, br: &str, tip: &str) {
        self.refs.borrow_mut().push((br.into(), tip.into()));
    }
}

impl Git for FakeGit {
    fn spira_refs(&self, _: &Path) -> Vec<(String, String)> {
        self.refs.borrow().clone()
    }
    fn branch_exists(&self, _: &Path, b: &str) -> bool {
        self.refs.borrow().iter().any(|(x, _)| x == b)
    }
    fn rev_parse(&self, _: &Path, rev: &str) -> Option<String> {
        if let Some((_, t)) = self.refs.borrow().iter().find(|(b, _)| b == rev) {
            return Some(t.clone());
        }
        let r = rev.trim_end_matches("^{commit}");
        self.shas.borrow().get(r).cloned()
    }
    fn is_ancestor(&self, _: &Path, a: &str, b: &str) -> bool {
        self.ancestors.borrow().contains(&(a.to_string(), b.to_string()))
    }
    fn content_landed(&self, _: &Path, b: &str, _: &str) -> bool {
        self.content.borrow().contains(b)
    }
    fn count(&self, _: &Path, _: &str) -> Option<u64> {
        Some(2)
    }
    fn fetch(&self, _: &Path, _: &str) {}
    fn tree_ok(&self, _: &Path) -> bool {
        self.tree.get()
    }
    fn tree_add_detached(&self, _: &Path, _: &Path, _: &str) {
        self.tree.set(true);
    }
    fn tree_checkout_landing(&self, _: &Path, _: &str) -> bool {
        true
    }
    fn tree_merge(&self, _: &Path, _: &str, b: &str, _: &str, _: &str) -> Result<(), String> {
        if self.merge_conflict.borrow().contains(b) {
            return Err("f.txt".into());
        }
        self.head.set(self.head.get() + 1);
        Ok(())
    }
    fn tree_head(&self, _: &Path) -> Option<String> {
        Some(format!("head{}", self.head.get()))
    }
    fn tree_reset_hard(&self, _: &Path, _: &str) {}
    fn tree_merge_abort(&self, _: &Path) {}
    fn delete_branch(&self, _: &Path, b: &str) -> bool {
        self.deleted.borrow_mut().push(b.into());
        true
    }
    fn refs_matching(&self, _: &Path, p: &str) -> Vec<String> {
        self.matching.borrow().get(p).cloned().unwrap_or_default()
    }
    fn log_grep(&self, _: &Path, _: &str, _: &[String]) -> Option<String> {
        None
    }
    fn merge_base(&self, _: &Path, _: &str, _: &str) -> Option<String> {
        None
    }
    fn log_subjects(&self, _: &Path, _: &str, _: &[&str]) -> Option<String> {
        None
    }
    fn commit_body(&self, _: &Path, _: &str) -> Option<String> {
        None
    }
}

#[derive(Default)]
struct FakeLib {
    calls: RefCell<Vec<String>>,
    rebase_fail: RefCell<HashMap<String, Rebase>>,
    recut: RefCell<Option<Recut>>,
    requeues: Cell<u32>,
    incident_out: RefCell<Option<Result<String, i32>>>,
    push_err: RefCell<Option<String>>,
}

impl FakeLib {
    fn rec(&self, s: String) {
        self.calls.borrow_mut().push(s);
    }
    fn has(&self, prefix: &str) -> bool {
        self.calls.borrow().iter().any(|c| c.starts_with(prefix))
    }
    fn count(&self, prefix: &str) -> usize {
        self.calls.borrow().iter().filter(|c| c.starts_with(prefix)).count()
    }
    fn find(&self, prefix: &str) -> String {
        self.calls.borrow().iter().find(|c| c.starts_with(prefix)).cloned().unwrap_or_default()
    }
}

impl Lib for FakeLib {
    fn reopen(&self, id: &str, cause: &str, note: &str) {
        self.rec(format!("reopen {id} {cause} {note}"));
    }
    fn event(&self, kind: &str, id: &str, title: &str, _: &str) {
        self.rec(format!("event {kind} {id} {title}"));
    }
    fn noverdict(&self, id: &str, _: &str, repo: &str, reason: &str, outcome: &str, _: &str) {
        self.rec(format!("noverdict {id} {repo} {reason} {outcome}"));
    }
    fn incident(&self, labels: &str, repo: &str, ext: &str, title: &str, payload: &str) -> Result<String, i32> {
        self.rec(format!("incident {labels} {repo} {ext} {title}\n{payload}"));
        self.incident_out.borrow().clone().unwrap_or(Ok("filing…\nsp-inc1\n".into()))
    }
    fn ask_rebase_loop(&self, a: &[&str]) {
        self.rec(format!("ask_rebase_loop {}", a.join(" ")));
    }
    fn ask_red_recurring(&self, id: &str, _: &str, _: &str, class: &str, _: &str) {
        self.rec(format!("ask_red_recurring {id} {class}"));
    }
    fn ask_rebase_refused(&self, id: &str, _: &str, _: &str, reason: &str) {
        self.rec(format!("ask_rebase_refused {id} {reason}"));
    }
    fn ask_budget_deferred(&self, br: &str, repo: &str, n: u32) {
        self.rec(format!("ask_budget_deferred {br} {repo} {n}"));
    }
    fn rebase(&self, br: &str, onto: &str, _: &Path, _: &str) -> Rebase {
        self.rec(format!("rebase {br} {onto}"));
        self.rebase_fail.borrow().get(br).cloned().unwrap_or(Rebase { ok: true, ..Default::default() })
    }
    fn recut(&self, br: &str, _: &str, _: &Path, _: &str) -> Recut {
        self.rec(format!("recut {br}"));
        self.recut.borrow().clone().unwrap_or_default()
    }
    fn bump_requeue(&self, id: &str, r: &str) {
        self.rec(format!("bump_requeue {id} {r}"));
    }
    fn requeues_of(&self, _: &str) -> u32 {
        self.requeues.get()
    }
    fn conflict_note(&self, a: &[&str]) -> String {
        format!("conflict-note {}", a[1])
    }
    fn other_beads(&self, _: &Path, _: &str, _: &str, _: &str) -> String {
        String::new()
    }
    fn pr_merged(&self, _: &Path, _: &str) -> bool {
        false
    }
    fn note(&self, id: &str, text: &str) {
        self.rec(format!("note {id} {text}"));
    }
    fn push(&self, _: &Path, remote: &str, refspec: &str) -> Result<(), String> {
        self.rec(format!("push {remote} {refspec}"));
        match self.push_err.borrow().clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
    fn land_subject(&self, id: &str) -> String {
        format!("spira: land {id}")
    }
    fn deliver_delivered(&self, id: &str, sha: &str) {
        self.rec(format!("deliver_delivered {id} {sha}"));
    }
    fn deliver_requeued(&self, id: &str, _: &str) {
        self.rec(format!("deliver_requeued {id}"));
    }
    fn deliver_returned(&self, id: &str, _: &str) {
        self.rec(format!("deliver_returned {id}"));
    }
    fn closeout(&self, id: &str, sha: &str, _: &Path) {
        self.rec(format!("closeout {id} {sha}"));
    }
    fn close_on_land(&self, id: &str, sha: &str) {
        self.rec(format!("close_on_land {id} {sha}"));
    }
    fn prune_worktrees(&self, _: &Path) {
        self.rec("prune_worktrees".into());
    }
    fn gh_unlanded_scan(&self) {
        self.rec("gh_unlanded_scan".into());
    }
    fn ask_refresh_loop(&self, _: &Path, name: &str, br: &str, id: &str, _: &str, n: u32) {
        self.rec(format!("ask_refresh_loop {id} {br} {name} {n}"));
    }
    fn deliver_pr_merged(&self, _: &Path, id: &str, br: &str, sha: &str) {
        self.rec(format!("deliver_pr_merged {id} {br} {sha}"));
    }
    fn deliver_pr_closed(&self, id: &str, reason: &str) {
        self.rec(format!("deliver_pr_closed {id} {reason}"));
    }
    fn force_push(&self, _: &Path, remote: &str, br: &str) -> Result<(), String> {
        self.rec(format!("force_push {remote} {br}"));
        match self.push_err.borrow().clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

#[derive(Default)]
struct FakeTools {
    gates: RefCell<HashMap<String, (i32, String)>>,
    gate_calls: RefCell<Vec<(String, String, String)>>,
    confine: RefCell<HashMap<String, (i32, String)>>,
    status: RefCell<Option<String>>,
    steps: RefCell<Vec<String>>,
    no_queue: Cell<bool>,
    skews: RefCell<Vec<PathBuf>>,
    /// Advance the fake clock by this much per gate (to exercise the budget).
    gate_cost: Cell<u64>,
    clock: RefCell<Option<std::rc::Rc<FakeClock>>>,
    /// Concurrent gates (D14): started and not yet collected, as (ticket, branch).
    running: RefCell<Vec<(u64, String)>>,
    next_ticket: Cell<u64>,
    /// `start <br>` / `done <br>` in the order they happened.
    trace: RefCell<Vec<String>>,
    max_running: Cell<usize>,
    /// Which running gate finishes next: the first one named here, else the oldest.
    finish_first: RefCell<Vec<String>>,
    slots_free: Cell<Option<usize>>,
    probes: Cell<u32>,
    /// `rebase_stale` calls, and the exit it answers (default 0).
    rebased: RefCell<Vec<String>>,
    rebase_rc: Cell<i32>,
    /// forge (sp-t4y60): `selector -> answer` for `pr-state`; absent means `None`.
    forge_pr_state: RefCell<HashMap<String, String>>,
    forge_pr_create_n: Cell<Option<u64>>,
    forge_pr_list_open: RefCell<Vec<(u64, String)>>,
    forge_automerge_ok: Cell<bool>,
    forge_calls: RefCell<Vec<String>>,
}

impl FakeTools {
    fn trace(&self) -> Vec<String> {
        self.trace.borrow().clone()
    }
    fn result_of(&self, br: &str) -> (i32, String) {
        self.gates.borrow().get(br).cloned().unwrap_or((0, "gate: VERDICT=PASS reason=ok branch=x repo=spira suite=-\n".into()))
    }
}

impl Tools for FakeTools {
    fn gate(&self, br: &str, _: &str, wait: &str, bead: &str) -> (i32, String) {
        self.gate_calls.borrow_mut().push((br.into(), wait.into(), bead.into()));
        self.trace.borrow_mut().push(format!("serial {br}"));
        if let Some(c) = self.clock.borrow().as_ref() {
            c.t.set(c.t.get() + self.gate_cost.get());
        }
        self.result_of(br)
    }
    fn gate_start(&self, br: &str, _: &str, wait: &str, bead: &str) -> u64 {
        self.gate_calls.borrow_mut().push((br.into(), wait.into(), bead.into()));
        let t = self.next_ticket.get() + 1;
        self.next_ticket.set(t);
        self.running.borrow_mut().push((t, br.into()));
        self.trace.borrow_mut().push(format!("start {br}"));
        let n = self.running.borrow().len();
        self.max_running.set(self.max_running.get().max(n));
        t
    }
    fn gate_wait_any(&self) -> Option<(u64, i32, String)> {
        let mut run = self.running.borrow_mut();
        if run.is_empty() {
            return None;
        }
        let first = self.finish_first.borrow().iter().find_map(|f| run.iter().position(|(_, b)| b == f));
        let (t, br) = run.remove(first.unwrap_or(0));
        drop(run);
        self.finish_first.borrow_mut().retain(|f| f != &br);
        if let Some(c) = self.clock.borrow().as_ref() {
            c.t.set(c.t.get() + self.gate_cost.get());
        }
        self.trace.borrow_mut().push(format!("done {br}"));
        let (rc, out) = self.result_of(&br);
        Some((t, rc, out))
    }
    fn gate_slots_free(&self, _: usize) -> Option<usize> {
        self.probes.set(self.probes.get() + 1);
        self.slots_free.get()
    }
    fn gate_status(&self, _: &str, _: &str) -> Option<String> {
        self.status.borrow().clone()
    }
    fn confine(&self, id: &str, _: &str, _: &Path, _: &str, _: &str) -> (i32, String) {
        self.confine.borrow().get(id).cloned().unwrap_or((0, String::new()))
    }
    fn queue_step(&self, repo: &str) -> Result<Vec<String>, String> {
        if self.no_queue.get() {
            return Err("no queue program — the queue step did not run".into());
        }
        self.steps.borrow_mut().push(repo.into());
        Ok(vec![format!("step {repo}")])
    }
    fn skew_refresh(&self, repo: &Path) -> String {
        self.skews.borrow_mut().push(repo.into());
        String::new()
    }
    fn ensure(&self, _: &Path) -> Vec<String> {
        Vec::new()
    }
    fn rebase_stale(&self, id: &str, repo: &str) -> i32 {
        self.rebased.borrow_mut().push(format!("{id} {repo}"));
        self.rebase_rc.get()
    }
    fn forge_pr_state(&self, _: &Path, selector: &str) -> Option<String> {
        self.forge_calls.borrow_mut().push(format!("pr-state {selector}"));
        self.forge_pr_state.borrow().get(selector).cloned()
    }
    fn forge_pr_create(&self, _: &Path, head: &str, base: &str, title: &str, _: &str) -> Option<u64> {
        self.forge_calls.borrow_mut().push(format!("pr-create {head} {base} {title}"));
        self.forge_pr_create_n.get()
    }
    fn forge_pr_list_open(&self, _: &Path) -> Vec<(u64, String)> {
        self.forge_calls.borrow_mut().push("pr-list-open".into());
        self.forge_pr_list_open.borrow().clone()
    }
    fn forge_pr_automerge(&self, _: &Path, selector: &str) -> bool {
        self.forge_calls.borrow_mut().push(format!("pr-automerge {selector}"));
        self.forge_automerge_ok.get()
    }
}

#[derive(Default)]
struct FakeProcs {
    live: RefCell<HashSet<String>>,
}
impl Procs for FakeProcs {
    fn holder_alive(&self, id: &str) -> bool {
        self.live.borrow().contains(id)
    }
}

#[derive(Default)]
struct FakeLc {
    probes: Cell<u32>,
    down: RefCell<Option<String>>,
    submitted: RefCell<HashMap<String, String>>,
    certified: RefCell<Vec<String>>,
    refuse_pass: Cell<bool>,
}
impl crate::lifecycle::Lc for FakeLc {
    fn submitted(&self) -> Result<HashMap<String, String>, String> {
        Ok(self.submitted.borrow().clone())
    }
    fn certify(&self, id: &str, tip: &str, outcome: &str, _: &str) -> Result<String, String> {
        self.certified.borrow_mut().push(format!("{id} {tip} {outcome}"));
        if outcome == "pass" && self.refuse_pass.get() {
            return Err("spira-lc certify exited 3: refused".into());
        }
        Ok("applied".into())
    }
    fn probe(&self) -> Result<(), String> {
        self.probes.set(self.probes.get() + 1);
        match self.down.borrow().clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

#[derive(Default)]
struct FakeClock {
    t: Cell<u64>,
}
impl Clock for FakeClock {
    fn now(&self) -> u64 {
        self.t.get()
    }
    fn sleep(&self, _: u64) {}
}

// ──────────────────────────────────────────────────────────────────────────────
// Harness
// ──────────────────────────────────────────────────────────────────────────────

struct H {
    dir: crate::testutil::TmpDir,
    s: Settings,
    repos: Vec<RepoRow>,
    beads: FakeBeads,
    git: FakeGit,
    lib: FakeLib,
    tools: FakeTools,
    procs: FakeProcs,
    clock: std::rc::Rc<FakeClock>,
    lc: FakeLc,
    out: Reporter,
}

fn repo_row(dir: &Path, name: &str, mode: LandMode) -> RepoRow {
    let path = dir.join(name);
    fs::create_dir_all(path.join(".git")).unwrap();
    let (landref, fq, remote, branch, forge) = match mode {
        LandMode::QueueLocal => ("local/main", "local/main", None, "main", "refs/remotes/origin/main"),
        _ => ("origin/main", "refs/remotes/origin/main", Some("origin".to_string()), "main", "refs/remotes/origin/main"),
    };
    RepoRow {
        name: name.into(),
        path,
        mode,
        landref: Some(landref.into()),
        base_fq: Some(fq.into()),
        base_remote: remote,
        base_branch: branch.into(),
        forge_ref: Some(forge.into()),
    }
}

impl H {
    fn new(mode: LandMode) -> H {
        let dir = tmpdir("pass");
        let s = Settings::for_run(dir.join("run"));
        fs::create_dir_all(&s.run).unwrap();
        let repos = vec![repo_row(&dir, "spira", mode)];
        let clock = std::rc::Rc::new(FakeClock { t: Cell::new(1000) });
        let tools = FakeTools::default();
        *tools.clock.borrow_mut() = Some(clock.clone());
        let out = Reporter::capture(Some(s.run.join("landing.progress")));
        H {
            dir,
            s,
            repos,
            beads: FakeBeads::default(),
            git: FakeGit::default(),
            lib: FakeLib::default(),
            tools,
            procs: FakeProcs::default(),
            clock,
            lc: FakeLc::default(),
            out,
        }
    }
    fn bead(&self, id: &str, status: &str, labels: &[&str]) -> BeadRow {
        let b = BeadRow {
            id: id.into(),
            status: status.into(),
            raw_status: status.into(),
            repo: "spira".into(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
            superseded: false,
            closed_at: "2026-09-01".into(),
            priority: 2,
            external_ref: None,
            title: String::new(),
            notes: Vec::new(),
        };
        self.beads.rows.borrow_mut().insert(id.into(), b.clone());
        b
    }
    fn closed(&self, id: &str, tip: &str) {
        self.bead(id, "closed", &[]);
        self.git.add(&format!("spira/{id}"), tip);
    }
    fn pass(&self) -> Pass<'_> {
        self.pass_with(&self.tools)
    }
    fn pass_with<'p>(&'p self, tools: &'p dyn Tools) -> Pass<'p> {
        Pass {
            s: &self.s,
            repos: &self.repos,
            beads: &self.beads,
            git: &self.git,
            lib: &self.lib,
            tools,
            procs: &self.procs,
            clock: &*self.clock,
            lc: &self.lc,
            lc_state: std::cell::OnceCell::new(),
            out: &self.out,
            files: Files::new(&self.s.run),
            start: 1000,
            pid: 4242,
            swept: Cell::new(0),
            swept_conflict: Cell::new(0),
        }
    }
    fn run(&self) {
        self.pass().run();
    }
    fn logged(&self, needle: &str) -> bool {
        self.out.lines().iter().any(|l| l.contains(needle))
    }
    fn mailbox(&self) -> String {
        fs::read_to_string(self.s.run.join("landing.progress")).unwrap_or_default()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Queue certification (§4.2)
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn a_submitted_labelled_open_bead_reads_as_done() {
    let v = serde_json::json!({"id":"sp-a","status":"open","labels":["spira-submitted","repo:spira"],"priority":1});
    let b = BeadRow::from_json(&v, "home", "spira-submitted").unwrap();
    assert_eq!((b.status.as_str(), b.raw_status.as_str(), b.repo.as_str()), ("closed", "open", "spira"));
    let sup = serde_json::json!({"id":"sp-b","status":"closed","dependencies":[{"type":"supersedes"}]});
    let b = BeadRow::from_json(&sup, "home", "x").unwrap();
    assert!(b.superseded);
    assert_eq!((b.repo.as_str(), b.closed_at.as_str(), b.priority), ("home", "9999-99-99", 9999));
}

#[test]
// covers: UC-gate-diag-01
fn a_red_gate_reopens_with_the_gates_own_words_and_marks_red() {
    let h = H::new(LandMode::QueueLocal);
    h.closed("sp-a", "t1");
    let out = (1..=25).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n")
        + "\ngate: VERDICT=FAIL reason=suite-red branch=spira/sp-a repo=spira suite=test-x.sh\n";
    h.tools.gates.borrow_mut().insert("spira/sp-a".into(), (1, out));
    *h.tools.status.borrow_mut() = Some("gate-run: gate PASS covered suites: test-y.sh".into());
    h.run();
    let reopen = h.lib.find("reopen sp-a cert-gate-red");
    assert!(reopen.contains("The branch carries 2 commit(s)"), "{reopen}");
    assert!(reopen.contains("covering: test-y.sh\nCertification just failed on: test-x.sh"), "{reopen}");
    assert!(reopen.contains("line 7\n") && !reopen.contains("line 6\n"), "tail -20: {reopen}");
    assert!(h.lib.has("event bead.reopened sp-a"));
    assert_eq!(h.mailbox(), "reopened sp-a — failed the certification gate\n");
}

#[test]
fn a_bead_reopened_while_its_gate_ran_is_not_reopened_or_certified() {
    let h = H::new(LandMode::Queue);
    h.closed("sp-a", "t1");
    h.closed("sp-b", "t2");
    h.tools.gates.borrow_mut().insert("spira/sp-a".into(), (1, "boom\n".into()));
    h.beads.reread.borrow_mut().insert("sp-a".into(), "in_progress".into());
    h.beads.reread.borrow_mut().insert("sp-b".into(), "open".into());
    h.run();
    assert!(!h.lib.has("reopen"));
    assert!(h.logged("CHECK6 sp-a: bead is now in_progress (was closed at scan time) — not reopening spira/sp-a"));
    assert!(h.logged("CHECK6 sp-b: bead is now open (was closed at scan time) — not certifying spira/sp-b"));
}

#[test]
fn the_base_red_id_is_found_among_the_intakes_trailing_lines() {
    let h = H::new(LandMode::QueueLocal);
    h.closed("sp-a", "t1");
    let out = "gate: VERDICT=BASE_FAIL reason=base-red branch=b repo=spira suite=test-x.sh\n".to_string();
    h.tools.gates.borrow_mut().insert("spira/sp-a".into(), (76, out));
    *h.lib.incident_out.borrow_mut() = Some(Ok("filing…\nsp-inc9\ndelivers: sp-inc9: delivers:action written\n".into()));
    fs::create_dir_all(h.s.incident.parent().unwrap()).unwrap();
    fs::write(&h.s.incident, "").unwrap();
    h.run();
    assert!(h.logged("CHECK6 spira: the base's own red is sp-inc9 (suite test-x.sh)"));
    assert!(!h.logged("the intake returned no bead id"));
}

#[test]
fn a_base_red_with_no_suite_name_is_keyed_by_the_base_sha() {
    let h = H::new(LandMode::QueueLocal);
    h.closed("sp-a", "t1");
    let out = "gate: VERDICT=BASE_FAIL reason=base-red branch=b repo=spira suite=-\n".to_string();
    h.tools.gates.borrow_mut().insert("spira/sp-a".into(), (76, out));
    fs::create_dir_all(h.s.incident.parent().unwrap()).unwrap();
    fs::write(&h.s.incident, "").unwrap();
    h.run();
    let inc = h.lib.find("incident ");
    assert!(inc.starts_with("incident plan spira basefail:spira:-@"), "{inc}");
    assert!(!inc.contains("basefail:spira:- "), "{inc}");
}

#[test]
fn a_red_base_files_one_incident_per_repository_per_pass_and_charges_nobody() {
    let h = H::new(LandMode::QueueLocal);
    h.closed("sp-a", "t1");
    h.closed("sp-b", "t2");
    let out = "--- base\ntest-x.sh RED\ngate: VERDICT=BASE_FAIL reason=base-red branch=b repo=spira suite=test-x.sh\n".to_string();
    h.tools.gates.borrow_mut().insert("spira/sp-a".into(), (76, out.clone()));
    h.tools.gates.borrow_mut().insert("spira/sp-b".into(), (76, out));
    fs::create_dir_all(h.s.incident.parent().unwrap()).unwrap();
    fs::write(&h.s.incident, "").unwrap();
    h.run();
    assert_eq!(h.lib.count("incident "), 1);
    let inc = h.lib.find("incident ");
    assert!(inc.starts_with("incident plan spira basefail:spira:test-x.sh spira's own gate fails against local/main — nothing can land"), "{inc}");
    assert!(inc.contains("  failing suite    test-x.sh\n  gate verdict     BASE_FAIL (base-red)"), "{inc}");
    assert!(h.logged("CHECK6 spira: the base's own red is sp-inc1 (suite test-x.sh)"));
    assert!(!h.lib.has("reopen"));
    assert!(h.logged("CHECK6 sp-a: held — the base fails its own gate (suite test-x.sh)"));
}

#[test]
fn a_base_fix_green_on_its_suite_is_certified_first_and_despite_the_budget() {
    let mut h = H::new(LandMode::QueueLocal);
    h.s.land_maxsec = 3600;
    h.s.gate_reserve = 2700;
    h.clock.t.set(1000 + 3000); // past the reserve: ordinary branches are cut
    h.closed("sp-a", "t1");
    let mut fix = h.bead("sp-fix", "closed", &[]);
    fix.external_ref = Some("basefail:spira:test-x.sh".into());
    fix.priority = 4;
    h.beads.rows.borrow_mut().insert("sp-fix".into(), fix);
    h.git.add("spira/sp-fix", "tf");
    let out = "--- base\ntest-x.sh RED\n--- this branch\ntest-x.sh ok\ngate: VERDICT=BASE_FAIL reason=base-red branch=b repo=spira suite=test-x.sh\n";
    h.tools.gates.borrow_mut().insert("spira/sp-fix".into(), (76, out.into()));
    h.run();
    assert!(h.logged("CHECK6 spira: base-fix branch(es) at front of queue: spira/sp-fix"));
    assert!(h.logged("CHECK6 sp-fix: base-fix branch — gating despite budget exhaustion"));
    assert!(h.mailbox().contains("certified spira/sp-fix in spira — base-fix (suite test-x.sh)"));
    assert_eq!(h.tools.gate_calls.borrow().len(), 1, "sp-a is cut by the budget");
    assert!(!h.lib.has("incident"));
}

#[test]
fn a_branch_that_no_longer_merges_is_red_no_rebase_and_returned() {
    let h = H::new(LandMode::Queue);
    h.closed("sp-a", "t1");
    h.tools.gates.borrow_mut().insert("spira/sp-a".into(), (1, "gate:   spira/lib.sh\ngate: VERDICT=FAIL reason=no-rebase branch=spira/sp-a repo=spira suite=-\n".into()));
    h.run();
    assert!(h.lib.has("reopen sp-a cert-gate-red"), "the bead is returned for a rebase");
    assert!(h.lib.calls.borrow().iter().any(|c| c.starts_with("reopen sp-a") && c.contains("gate:   spira/lib.sh")), "the note names the conflicting paths");
    assert!(!h.lib.has("noverdict"), "a conflict is never NO_VERDICT");
}

#[test]
fn a_base_fix_for_a_fence_red_base_certifies_on_a_fully_green_branch() {
    let mut h = H::new(LandMode::QueueLocal);
    h.s.land_maxsec = 3600;
    h.s.gate_reserve = 2700;
    h.clock.t.set(1000 + 3000); // past the reserve: ordinary branches are cut
    h.closed("sp-a", "t1");
    let mut fix = h.bead("sp-fix", "closed", &[]);
    fix.external_ref = Some("basefail:spira:-".into());
    fix.priority = 4;
    h.beads.rows.borrow_mut().insert("sp-fix".into(), fix);
    h.git.add("spira/sp-fix", "tf");
    let out = "--- base\ninventory.sh FAILED\n--- this branch\ntest-x.sh ok\ngate: VERDICT=BASE_FAIL reason=base-red branch=b repo=spira suite=-\n";
    h.tools.gates.borrow_mut().insert("spira/sp-fix".into(), (76, out.into()));
    h.run();
    assert!(h.logged("CHECK6 sp-fix: base-fix branch — gating despite budget exhaustion"));
    assert!(h.mailbox().contains("certified spira/sp-fix in spira — base-fix (suite -)"));
    assert!(!h.lib.has("incident"));
}

#[test]
fn no_verdict_is_counted_by_the_seam_and_never_reopens() {
    let h = H::new(LandMode::Queue);
    h.closed("sp-a", "t1");
    h.tools.gates.borrow_mut().insert("spira/sp-a".into(), (75, "gate: VERDICT=NO_VERDICT reason=lock-timeout branch=b repo=spira suite=-\n".into()));
    h.run();
    assert!(h.lib.has("noverdict sp-a spira lock-timeout NO_VERDICT"));
    assert!(!h.lib.has("reopen"));
    assert!(h.tools.rebased.borrow().is_empty(), "only a conflict goes to rebase-stale");
}

#[test]
fn a_pass_clears_the_branchs_noverdict_counters() {
    let h = H::new(LandMode::Queue);
    h.closed("sp-a", "t1");
    let nv = h.s.run.join("noverdict");
    fs::create_dir_all(&nv).unwrap();
    fs::write(nv.join("spira-sp-a"), "2").unwrap();
    fs::write(nv.join("spira-sp-a.asked"), "").unwrap();
    fs::write(nv.join("spira-harness-fault"), "1").unwrap();
    h.run();
    assert!(!nv.join("spira-sp-a").exists() && !nv.join("spira-sp-a.asked").exists());
    assert!(nv.join("spira-harness-fault").exists());
}

#[test]
fn the_budget_cut_defers_the_rest_writes_the_cursor_and_escalates_at_the_threshold() {
    let mut h = H::new(LandMode::QueueLocal);
    h.s.land_maxsec = 3600;
    h.s.gate_reserve = 2700;
    h.tools.gate_cost.set(1000); // the second gate would start with 2600s left < 2700 reserve
    for (i, id) in ["sp-a", "sp-b", "sp-c"].iter().enumerate() {
        let mut b = h.bead(id, "closed", &[]);
        b.priority = i as i64;
        h.beads.rows.borrow_mut().insert(id.to_string(), b);
        h.git.add(&format!("spira/{id}"), &format!("t{i}"));
    }
    let files = Files::new(&h.s.run);
    for _ in 0..4 {
        files.bump_deferred("spira/sp-c", "spira", 1);
    }
    h.run();
    assert_eq!(h.tools.gate_calls.borrow().len(), 1);
    assert!(h.logged("landing: budget cut at spira/sp-b — 2600s left, 2 branch(es) deferred in spira"));
    assert_eq!(files.cursor_repo().as_deref(), Some("spira"));
    assert!(h.lib.has("ask_budget_deferred spira/sp-c spira 5"));
    assert!(!h.lib.has("ask_budget_deferred spira/sp-b"));
    assert!(!files.deferred_dir().join("spira_sp-a").exists());
}

#[test]
fn every_early_exit_says_why() {
    let h = H::new(LandMode::Queue);
    h.bead("sp-open", "in_progress", &[]);
    h.git.add("spira/sp-open", "t0");
    h.procs.live.borrow_mut().insert("sp-open".into());
    h.bead("sp-idle", "open", &[]);
    h.git.add("spira/sp-idle", "t1");
    let mut other = h.bead("sp-other", "closed", &[]);
    other.repo = "elsewhere".into();
    h.beads.rows.borrow_mut().insert("sp-other".into(), other);
    h.git.add("spira/sp-other", "t2");
    let mut sup = h.bead("sp-sup", "closed", &[]);
    sup.superseded = true;
    h.beads.rows.borrow_mut().insert("sp-sup".into(), sup);
    h.git.add("spira/sp-sup", "t3");
    h.bead("sp-cut", "closed", &["cutover-round"]);
    h.git.add("spira/sp-cut", "t4");
    h.closed("sp-held", "t5");
    h.procs.live.borrow_mut().insert("sp-held".into());
    h.closed("sp-content", "t6");
    h.git.content.borrow_mut().insert("spira/sp-content".into());
    h.run();
    assert!(h.logged("CHECK6 sp-open: spira/sp-open not landed — its bead is in_progress, held by a live aeon"));
    assert!(h.logged("CHECK6 sp-idle: spira/sp-idle not landed — its bead is open and no aeon holds it"));
    assert!(h.logged("CHECK6 sp-other: spira/sp-other is in spira but the bead names repo:elsewhere — not landing it here"));
    assert!(h.logged("CHECK6 sp-sup: spira/sp-sup is superseded"));
    assert!(h.logged("CHECK6 sp-cut: spira/sp-cut is labelled cutover-round — leaving it for the cutover round"));
    assert!(h.logged("CHECK6 sp-held: a live aeon still holds spira/sp-held — deferring the land"));
    assert!(h.logged("origin/main already contains every change on spira/sp-content — nothing to land"));
    assert!(!h.s.run.join("landstate/sp-content.ejected").exists());
    assert!(h.tools.gate_calls.borrow().is_empty());
}

#[test]
fn a_branch_gone_mid_pass_is_never_evidence_of_unlanded_work() {
    let h = H::new(LandMode::Queue);
    h.closed("sp-a", "t1");
    let p = h.pass();
    // Enumerated, then removed before the walk reached it.
    let repo = h.repos[0].clone();
    h.git.ancestors.borrow_mut().insert(("t1".into(), "refs/remotes/origin/main".into()));
    struct Vanishing<'g>(&'g FakeGit, Cell<bool>);
    impl<'g> Git for Vanishing<'g> {
        fn spira_refs(&self, r: &Path) -> Vec<(String, String)> {
            self.0.spira_refs(r)
        }
        fn branch_exists(&self, _: &Path, _: &str) -> bool {
            false
        }
        fn rev_parse(&self, r: &Path, v: &str) -> Option<String> {
            self.0.rev_parse(r, v)
        }
        fn is_ancestor(&self, r: &Path, a: &str, b: &str) -> bool {
            self.0.is_ancestor(r, a, b)
        }
        fn content_landed(&self, r: &Path, b: &str, base: &str) -> bool {
            self.0.content_landed(r, b, base)
        }
        fn count(&self, r: &Path, x: &str) -> Option<u64> {
            self.0.count(r, x)
        }
        fn fetch(&self, _: &Path, _: &str) {}
        fn tree_ok(&self, _: &Path) -> bool {
            false
        }
        fn tree_add_detached(&self, _: &Path, _: &Path, _: &str) {}
        fn tree_checkout_landing(&self, _: &Path, _: &str) -> bool {
            false
        }
        fn tree_merge(&self, _: &Path, _: &str, _: &str, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn tree_head(&self, _: &Path) -> Option<String> {
            None
        }
        fn tree_reset_hard(&self, _: &Path, _: &str) {}
        fn tree_merge_abort(&self, _: &Path) {}
        fn delete_branch(&self, _: &Path, _: &str) -> bool {
            self.1.set(true);
            false
        }
        fn refs_matching(&self, _: &Path, _: &str) -> Vec<String> {
            vec![]
        }
        fn log_grep(&self, r: &Path, g: &str, refs: &[String]) -> Option<String> {
            self.0.log_grep(r, g, refs)
        }
        fn merge_base(&self, r: &Path, a: &str, b: &str) -> Option<String> {
            self.0.merge_base(r, a, b)
        }
        fn log_subjects(&self, r: &Path, range: &str, paths: &[&str]) -> Option<String> {
            self.0.log_subjects(r, range, paths)
        }
        fn commit_body(&self, r: &Path, sha: &str) -> Option<String> {
            self.0.commit_body(r, sha)
        }
    }
    let v = Vanishing(&h.git, Cell::new(false));
    let p2 = Pass { git: &v, ..p };
    p2.walk_repo(&repo);
    assert!(h.logged("CHECK6 sp-a: spira/sp-a is gone since this pass began and t1 is on origin/main — landed and reaped, not reopening"));
    assert!(!h.lib.has("reopen"));
}

#[test]
fn the_queue_step_runs_before_and_after_the_walk_and_a_missing_binary_is_said() {
    let h = H::new(LandMode::QueueLocal);
    h.closed("sp-a", "t1");
    h.run();
    assert_eq!(*h.tools.steps.borrow(), vec!["spira", "spira"]);
    let lines = h.out.lines();
    let early = lines.iter().position(|l| l == "queue early: step spira").unwrap();
    let cert = lines.iter().position(|l| l.contains("certified spira/sp-a")).unwrap();
    let late = lines.iter().position(|l| l == "queue late: step spira").unwrap();
    assert!(early < cert && cert < late);
    assert!(h.tools.skews.borrow().is_empty(), "queue.local's base is local: no skew refresh");

    let h = H::new(LandMode::Queue);
    h.tools.no_queue.set(true);
    h.run();
    assert!(h.logged("queue early: spira: no queue program — the queue step did not run"));
    assert_eq!(h.tools.skews.borrow().len(), 1, "queue(forge) refreshes the checkout");
}

#[test]
fn the_walk_starts_at_the_cursor() {
    let mut h = H::new(LandMode::Queue);
    let a = repo_row(&h.dir, "alpha", LandMode::Queue);
    let b = repo_row(&h.dir, "beta", LandMode::Queue);
    h.repos = vec![h.repos[0].clone(), a, b];
    Files::new(&h.s.run).set_cursor("beta");
    let names: Vec<String> = h.pass().rotated().iter().map(|r| r.name.clone()).collect();
    assert_eq!(names, vec!["beta", "spira", "alpha"]);
    Files::new(&h.s.run).set_cursor("gone");
    let names: Vec<String> = h.pass().rotated().iter().map(|r| r.name.clone()).collect();
    assert_eq!(names, vec!["spira", "alpha", "beta"]);
}

#[test]
fn express_branches_are_announced_and_certified_before_the_rest() {
    let h = H::new(LandMode::Queue);
    let mut a = h.bead("sp-a", "closed", &[]);
    a.priority = 0;
    h.beads.rows.borrow_mut().insert("sp-a".into(), a);
    h.git.add("spira/sp-a", "t1");
    h.bead("sp-x", "closed", &["express"]);
    h.git.add("spira/sp-x", "t2");
    h.run();
    assert!(h.logged("CHECK6 spira: express branch(es) certified first: spira/sp-x"));
    let gated: Vec<String> = h.tools.gate_calls.borrow().iter().map(|g| g.0.clone()).collect();
    assert_eq!(gated, vec!["spira/sp-x", "spira/sp-a"]);
}

#[test]
fn a_pr_repository_is_counted_and_left_to_the_pr_pass() {
    let h = H::new(LandMode::Pr);
    h.closed("sp-a", "t1");
    h.run();
    assert_eq!(h.out.branches(), 1);
    assert!(h.tools.gate_calls.borrow().is_empty());
}

// ──────────────────────────────────────────────────────────────────────────────
// The prune (§5)
// ──────────────────────────────────────────────────────────────────────────────

// ──────────────────────────────────────────────────────────────────────────────
// push and hold (§4.3, §4.5)
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn a_clean_sweep_rebase_carries_a_certified_verdict_to_the_new_tip() {
    let h = H::new(LandMode::Push);
    h.git.tree.set(true);
    for (i, id) in ["sp-b", "sp-c", "sp-d"].iter().enumerate() {
        let mut b = h.bead(id, "closed", &[]);
        b.priority = i as i64;
        h.beads.rows.borrow_mut().insert(id.to_string(), b);
        h.git.add(&format!("spira/{id}"), &format!("t{i}"));
    }
    let nv = "gate: VERDICT=NO_VERDICT reason=lock branch=b repo=spira suite=-\n";
    h.tools.gates.borrow_mut().insert("spira/sp-b".into(), (75, nv.into()));
    h.tools.gates.borrow_mut().insert("spira/sp-c".into(), (75, nv.into()));
    crate::landstate::land_mark(&h.s.run, "sp-b", "CERTIFIED", "t0", "", "");
    h.run();
}

#[test]
fn push_lands_records_first_and_closes_and_rebases_survivors_once_per_pass() {
    let h = H::new(LandMode::Push);
    h.git.tree.set(true);
    // B and C are judged but get no verdict; D and E land after them.
    for (i, id) in ["sp-b", "sp-c", "sp-d", "sp-e"].iter().enumerate() {
        let mut b = h.bead(id, "closed", &[]);
        b.priority = i as i64;
        h.beads.rows.borrow_mut().insert(id.to_string(), b);
        h.git.add(&format!("spira/{id}"), &format!("t{i}"));
    }
    let nv = "gate: VERDICT=NO_VERDICT reason=lock branch=b repo=spira suite=-\n";
    h.tools.gates.borrow_mut().insert("spira/sp-b".into(), (75, nv.into()));
    h.tools.gates.borrow_mut().insert("spira/sp-c".into(), (75, nv.into()));
    h.run();
    assert!(h.mailbox().contains("landed spira/sp-d\n") && h.mailbox().contains("landed spira/sp-e\n"));
    assert_eq!(h.lib.count("push origin landing:main"), 2);
    // sp-4hs0i: each survivor is replayed once after the walk, not once per landing.
    assert_eq!(h.lib.count("rebase spira/sp-b "), 2, "once in the walk, once in the sweep");
    assert_eq!(h.lib.count("rebase spira/sp-c "), 2);
    assert!(h.logged("landing: pass complete — 4 branch(es) seen, 2 movement(s), 2 survivor(s) rebased after a landing, 0 conflicted"));
    assert_eq!(h.tools.skews.borrow().len(), 1);
}

#[test]
fn escalation_at_the_threshold_still_reopens_and_asks() {
    let h = H::new(LandMode::Push);
    h.git.tree.set(true);
    h.closed("sp-a", "t1");
    h.lib.rebase_fail.borrow_mut().insert("spira/sp-a".into(), Rebase { ok: false, failure: "conflict".into(), conflicts: "f".into(), ..Default::default() });
    h.lib.requeues.set(3);
    h.run();
    assert!(h.lib.has("reopen sp-a rebase-conflict"));
    assert!(h.lib.has("ask_rebase_loop sp-a spira/sp-a spira 3 f"));
    assert!(h.mailbox().contains("escalated sp-a — rebase conflict x3 on spira/sp-a"));
}

#[test]
fn a_rebase_that_could_not_be_attempted_charges_nobody() {
    let h = H::new(LandMode::Push);
    h.closed("sp-a", "t1");
    h.lib.rebase_fail.borrow_mut().insert("spira/sp-a".into(), Rebase { ok: false, failure: "rebase-refused".into(), refused_reason: "dirty".into(), ..Default::default() });
    h.run();
    assert!(h.lib.has("ask_rebase_refused sp-a dirty"));
    assert!(!h.lib.has("reopen") && !h.lib.has("bump_requeue"));
}

#[test]
fn confinement_is_asked_before_the_gate() {
    let h = H::new(LandMode::Push);
    h.git.tree.set(true);
    h.closed("sp-a", "t1");
    h.closed("sp-b", "t2");
    h.tools.confine.borrow_mut().insert("sp-a".into(), (1, "spike touched src/x.rs\nmore".into()));
    h.tools.confine.borrow_mut().insert("sp-b".into(), (3, "library failed to load".into()));
    h.run();
    assert!(h.lib.has("reopen sp-a confine-fail Reopened by sentinel: spike touched src/x.rs\nmore"));
    assert!(h.logged("CHECK6 sp-b: spira/sp-b — confine.sh could not evaluate: library failed to load"));
    assert!(h.tools.gate_calls.borrow().is_empty());
}

#[test]
fn a_push_blocked_for_a_reason_other_than_a_race_leaves_the_bead_closed() {
    let h = H::new(LandMode::Push);
    h.git.tree.set(true);
    h.closed("sp-a", "t1");
    *h.lib.push_err.borrow_mut() = Some("remote: Permission denied\nfatal".into());
    h.run();
    assert!(h.logged("landing: push failed for spira/sp-a: remote: Permission denied"));
    assert!(h.logged("landing: spira/sp-a merges clean but push failed — leaving closed"));
}

#[test]
fn a_merge_conflict_on_already_landed_work_does_not_reopen() {
    let h = H::new(LandMode::Push);
    h.git.tree.set(true);
    h.closed("sp-a", "t1");
    h.git.merge_conflict.borrow_mut().insert("spira/sp-a".into());
    h.git.ancestors.borrow_mut().insert(("spira/sp-a".into(), "refs/remotes/origin/main".into()));
    h.run();
    assert!(h.logged("introduces nothing new to origin/main (ancestor=yes"));
    assert!(!h.lib.has("reopen"));

    let h = H::new(LandMode::Push);
    h.git.tree.set(true);
    h.closed("sp-a", "t1");
    h.git.merge_conflict.borrow_mut().insert("spira/sp-a".into());
    h.run();
    assert!(!h.lib.has("deliver_returned"), "lifecycle_enforce OFF");
    assert!(h.lib.has("reopen sp-a rebase-conflict Reopened by sentinel: branch spira/sp-a conflicts with origin/main. The branch carries 2 commit(s)"));
}

#[test]
fn hold_gates_notes_and_does_not_advance_the_base() {
    let h = H::new(LandMode::Hold);
    h.closed("sp-a", "t1");
    h.run();
    assert!(h.lib.has("note sp-a Gated and held: spira/sp-a passed spira's landing gate. Spira does not advance spira's main."));
    assert!(!h.lib.has("push"));
    assert!(h.mailbox().is_empty(), "a held branch is not a movement");
}

/// push mode's merge-and-push against real repositories: the branch's own commit lands on
/// the bare remote's main.
#[test]
fn push_mode_lands_on_a_real_remote() {
    use crate::real::RealGit;
    use std::process::Command;
    let dir = tmpdir("real");
    let sh = |args: &str| {
        let o = Command::new("bash").arg("-c").arg(args).current_dir(&dir).output().unwrap();
        assert!(o.status.success(), "{args}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    sh("git init -q --bare -b main remote.git && git init -q -b main work && cd work && git config user.email t@t && git config user.name t \
        && echo a > a && git add a && git commit -qm a && git remote add origin $PWD/../remote.git && git push -q origin main \
        && git fetch -q origin && git checkout -q -b spira/sp-a && echo b > b && git add b && git commit -qm b && git checkout -q main");
    let work = dir.join("work");
    let tip = sh("git -C work rev-parse spira/sp-a");
    let mut s = Settings::for_run(dir.join("run"));
    s.git_name = "t".into();
    s.git_email = "t@t".into();
    let repos = vec![RepoRow {
        name: "spira".into(),
        path: work.clone(),
        mode: LandMode::Push,
        landref: Some("origin/main".into()),
        base_fq: Some("refs/remotes/origin/main".into()),
        base_remote: Some("origin".into()),
        base_branch: "main".into(),
        forge_ref: Some("refs/remotes/origin/main".into()),
    }];
    struct PushLib<'l>(&'l FakeLib);
    impl<'l> Lib for PushLib<'l> {
        fn reopen(&self, a: &str, b: &str, c: &str) {
            self.0.reopen(a, b, c)
        }
        fn event(&self, a: &str, b: &str, c: &str, d: &str) {
            self.0.event(a, b, c, d)
        }
        fn noverdict(&self, a: &str, b: &str, c: &str, d: &str, e: &str, f: &str) {
            self.0.noverdict(a, b, c, d, e, f)
        }
        fn incident(&self, a: &str, b: &str, c: &str, d: &str, e: &str) -> Result<String, i32> {
            self.0.incident(a, b, c, d, e)
        }
        fn ask_rebase_loop(&self, a: &[&str]) {
            self.0.ask_rebase_loop(a)
        }
        fn ask_red_recurring(&self, a: &str, b: &str, c: &str, d: &str, e: &str) {
            self.0.ask_red_recurring(a, b, c, d, e)
        }
        fn ask_rebase_refused(&self, a: &str, b: &str, c: &str, d: &str) {
            self.0.ask_rebase_refused(a, b, c, d)
        }
        fn ask_budget_deferred(&self, a: &str, b: &str, n: u32) {
            self.0.ask_budget_deferred(a, b, n)
        }
        fn rebase(&self, a: &str, b: &str, c: &Path, d: &str) -> Rebase {
            self.0.rebase(a, b, c, d)
        }
        fn recut(&self, a: &str, b: &str, c: &Path, d: &str) -> Recut {
            self.0.recut(a, b, c, d)
        }
        fn bump_requeue(&self, a: &str, b: &str) {
            self.0.bump_requeue(a, b)
        }
        fn requeues_of(&self, a: &str) -> u32 {
            self.0.requeues_of(a)
        }
        fn conflict_note(&self, a: &[&str]) -> String {
            self.0.conflict_note(a)
        }
        fn other_beads(&self, a: &Path, b: &str, c: &str, d: &str) -> String {
            self.0.other_beads(a, b, c, d)
        }
        fn pr_merged(&self, a: &Path, b: &str) -> bool {
            self.0.pr_merged(a, b)
        }
        fn note(&self, a: &str, b: &str) {
            self.0.note(a, b)
        }
        fn push(&self, tree: &Path, remote: &str, refspec: &str) -> Result<(), String> {
            let o = Command::new("git").arg("-C").arg(tree).args(["push", "-q", remote, refspec]).output().unwrap();
            if o.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&o.stderr).into_owned()) }
        }
        fn land_subject(&self, a: &str) -> String {
            self.0.land_subject(a)
        }
        fn deliver_delivered(&self, a: &str, b: &str) {
            self.0.deliver_delivered(a, b)
        }
        fn deliver_requeued(&self, a: &str, b: &str) {
            self.0.deliver_requeued(a, b)
        }
        fn deliver_returned(&self, a: &str, b: &str) {
            self.0.deliver_returned(a, b)
        }
        fn closeout(&self, a: &str, b: &str, c: &Path) {
            self.0.closeout(a, b, c)
        }
        fn close_on_land(&self, a: &str, b: &str) {
            self.0.close_on_land(a, b)
        }
        fn prune_worktrees(&self, a: &Path) {
            self.0.prune_worktrees(a)
        }
        fn gh_unlanded_scan(&self) {}
        fn ask_refresh_loop(&self, a: &Path, b: &str, c: &str, d: &str, e: &str, n: u32) {
            self.0.ask_refresh_loop(a, b, c, d, e, n)
        }
        fn deliver_pr_merged(&self, a: &Path, b: &str, c: &str, d: &str) {
            self.0.deliver_pr_merged(a, b, c, d)
        }
        fn deliver_pr_closed(&self, a: &str, b: &str) {
            self.0.deliver_pr_closed(a, b)
        }
        fn force_push(&self, a: &Path, b: &str, c: &str) -> Result<(), String> {
            self.0.force_push(a, b, c)
        }
    }
    let fl = FakeLib::default();
    let lib = PushLib(&fl);
    let beads = FakeBeads::default();
    let mut b = BeadRow::from_json(&serde_json::json!({"id":"sp-a","status":"closed","labels":["repo:spira"]}), "spira", "x").unwrap();
    b.priority = 1;
    beads.rows.borrow_mut().insert("sp-a".into(), b);
    let tools = FakeTools::default();
    let procs = FakeProcs::default();
    let clock = FakeClock::default();
    let lc = FakeLc::default();
    let out = Reporter::capture(None);
    fs::create_dir_all(&s.run).unwrap();
    let p = Pass {
        s: &s,
        repos: &repos,
        beads: &beads,
        git: &RealGit,
        lib: &lib,
        tools: &tools,
        procs: &procs,
        clock: &clock,
        lc: &lc,
        lc_state: std::cell::OnceCell::new(),
        out: &out,
        files: Files::new(&s.run),
        start: 0,
        pid: 1,
        swept: Cell::new(0),
        swept_conflict: Cell::new(0),
    };
    p.run();
    assert!(fl.has("close_on_land sp-a"), "{:?} {:?}", fl.calls.borrow(), out.lines());
    let remote_main = sh("git --git-dir=remote.git rev-parse main");
    let anc = Command::new("git").arg("--git-dir=remote.git").args(["merge-base", "--is-ancestor", &tip, &remote_main]).current_dir(&dir).status().unwrap();
    assert!(anc.success(), "the branch's own commit is on the remote's main");
    // A branch already on top of the base fast-forwards, exactly as `git merge` did in bash.
    assert_eq!(remote_main, tip);
    // content_landed now answers yes for the landed branch (lib.sh semantics: ancestor).
    let _ = sh("git -C work fetch -q origin");
    assert!(RealGit.content_landed(&work, "spira/sp-a", "refs/remotes/origin/main"));
}

// ──────────────────────────────────────────────────────────────────────────────
// halt / sweep-red
// ──────────────────────────────────────────────────────────────────────────────

struct FakeHalt {
    alive: RefCell<HashSet<String>>,
    ignores_term: bool,
    signals: RefCell<Vec<(String, i32)>>,
    live_containers: Vec<String>,
    torn: RefCell<Vec<String>>,
}

impl HaltPorts for FakeHalt {
    fn pid_alive(&self, pid: &str) -> bool {
        self.alive.borrow().contains(pid)
    }
    fn signal(&self, pid: &str, sig: i32) {
        self.signals.borrow_mut().push((pid.into(), sig));
        if sig == libc::SIGKILL || !self.ignores_term {
            self.alive.borrow_mut().remove(pid);
        }
    }
    fn sleep1(&self) {}
    fn now(&self) -> u64 {
        1100
    }
    fn running_containers(&self) -> Vec<String> {
        self.live_containers.clone()
    }
    fn teardown(&self, name: &str) -> bool {
        self.torn.borrow_mut().push(name.into());
        true
    }
}

fn halt_setup(ignores_term: bool) -> (crate::testutil::TmpDir, FakeHalt) {
    let dir = tmpdir("halt");
    let f = Files::new(&dir);
    f.write_run(&halt::run_record_for_tests("777", "1000", "spira", "spira/sp-a", "gate"));
    fs::write(f.containers(), "c1\nc2\n").unwrap();
    let h = FakeHalt {
        alive: RefCell::new(["777".to_string()].into_iter().collect()),
        ignores_term,
        signals: RefCell::new(vec![]),
        live_containers: vec!["c2".into()],
        torn: RefCell::new(vec![]),
    };
    (dir, h)
}

#[test]
fn halt_dry_run_reports_and_touches_nothing() {
    let (dir, ports) = halt_setup(false);
    let cx = HaltCtx { files: Files::new(&dir), grace: 3, self_pid: 1, repos: None, queue_dir: dir.join("q") };
    let (rc, out, _) = halt::halt(&cx, &HaltArgs { reason: String::new(), dry_run: true }, &ports, &FakeGit::default());
    assert_eq!(rc, 0);
    assert_eq!(out[0], "landing: pass running — pid=777 elapsed=100s repo=spira branch=spira/sp-a phase=gate");
    assert_eq!(out[1..], ["landing: would tear down container c1", "landing: would tear down container c2"]);
    assert!(ports.signals.borrow().is_empty());
    assert!(dir.join("landing.run").exists());
    ports.alive.borrow_mut().clear();
    let (rc, out, _) = halt::halt(&cx, &HaltArgs { reason: String::new(), dry_run: true }, &ports, &FakeGit::default());
    assert_eq!((rc, out[0].as_str()), (1, "landing: no pass running"));
}

#[test]
fn halt_terms_then_kills_records_why_and_tears_down_only_live_containers() {
    let (dir, ports) = halt_setup(true);
    let mut repo = repo_row(&dir, "spira", LandMode::Queue);
    repo.path = dir.join("spira");
    let repos = vec![repo];
    fs::create_dir_all(dir.join("q/spira")).unwrap();
    fs::write(dir.join("q/spira/open"), "pr=1\nbranch=spira/queue/open1\n").unwrap();
    let git = FakeGit::default();
    git.matching.borrow_mut().insert("refs/heads/spira/queue/*".into(), vec!["spira/queue/open1".into(), "spira/queue/old".into(), "spira/queue/pushed".into()]);
    git.matching.borrow_mut().insert("refs/remotes/*/spira/queue/pushed".into(), vec!["origin/spira/queue/pushed".into()]);
    let cx = HaltCtx { files: Files::new(&dir), grace: 3, self_pid: 1, repos: Some(&repos), queue_dir: dir.join("q") };
    let (rc, out, _) = halt::halt(&cx, &HaltArgs { reason: "operator\nsaid so".into(), dry_run: false }, &ports, &git);
    assert_eq!(rc, 0);
    assert_eq!(*ports.signals.borrow(), vec![("777".to_string(), libc::SIGTERM), ("777".to_string(), libc::SIGKILL)]);
    assert!(out.iter().any(|l| l == "landing: pass did not stop after 3s — sending SIGKILL"));
    assert_eq!(*ports.torn.borrow(), vec!["c2"]);
    assert_eq!(*git.deleted.borrow(), vec!["spira/queue/old"]);
    let rec = fs::read_to_string(dir.join("landing.interrupted")).unwrap();
    assert!(rec.contains("reason=operator said so\n") && rec.contains("phase=gate\n") && rec.contains("elapsed=100s\n"));
    assert!(!dir.join("landing.run").exists() && !dir.join("landing.containers").exists());
    assert_eq!(out.last().unwrap(), "landing: halted — interrupted at phase=gate in spira (100s)");
}

#[test]
fn halt_refuses_when_no_pass_is_running_or_the_record_is_our_own_pid() {
    let (dir, ports) = halt_setup(false);
    let cx = HaltCtx { files: Files::new(&dir), grace: 3, self_pid: 777, repos: None, queue_dir: dir.join("q") };
    let (rc, _, err) = halt::halt(&cx, &HaltArgs { reason: String::new(), dry_run: false }, &ports, &FakeGit::default());
    assert_eq!((rc, err[0].as_str()), (1, "landing: no pass running — nothing to halt"));
}

// ──────────────────────────────────────────────────────────────────────────────
// The pr pass (kept behaviour, §8 D1–D2)
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct FakePr {
    delivered: RefCell<Vec<String>>,
    fail: RefCell<Option<String>>,
}
impl PrTools for FakePr {
    fn deliver_by_content(&self, id: &str, sha: &str) -> Result<(), String> {
        self.delivered.borrow_mut().push(format!("{id} {sha}"));
        match self.fail.borrow().clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

#[test]
fn the_pr_pass_hands_done_branches_to_pr_branch_and_proves_content_landings() {
    let h = H::new(LandMode::Pr);
    h.closed("sp-done", "t1");
    let mut sub = h.bead("sp-sub", "closed", &["spira-submitted"]);
    sub.raw_status = "open".into();
    h.beads.rows.borrow_mut().insert("sp-sub".into(), sub);
    h.git.add("spira/sp-sub", "t2");
    h.closed("sp-merged", "t3");
    h.git.content.borrow_mut().insert("spira/sp-merged".into());
    h.git.shas.borrow_mut().insert("refs/remotes/origin/main".into(), "M".into());
    h.bead("sp-wip", "in_progress", &[]);
    h.git.add("spira/sp-wip", "t4");
    // sp-done and sp-sub each rebase clean (FakeLib's default), confine clean (FakeTools'
    // default) and land_pr opens a fresh PR — forge_pr_create answers a PR number so land_pr
    // succeeds for both.
    h.tools.forge_pr_create_n.set(Some(1));
    let tools = FakePr::default();
    let files = Files::new(&h.s.run);
    let p = PrPass {
        s: &h.s,
        repos: &h.repos,
        beads: &h.beads,
        git: &h.git,
        lib: &h.lib,
        land_tools: &h.tools,
        procs: &h.procs,
        tools: &tools,
        files: &files,
        out: &h.out,
        loud: Default::default(),
    };
    let (seen, acted) = p.run();
    assert_eq!((seen, acted), (4, 2));
    // sp-merged is the content-landed fast path (never reaches pr_branch); sp-wip is not
    // closed (never reaches it either). sp-done and sp-sub both reach pr_branch's rebase.
    assert!(h.lib.has("rebase spira/sp-done"));
    assert!(h.lib.has("rebase spira/sp-sub"), "submitted reads as done, so it is walked too");
    assert!(h.logged("landing-pass spira: sp-wip not landed — its bead is in_progress"));
}

#[test]
fn delivery_rows_accept_a_numeric_version() {
    assert_eq!(crate::pr::delivery_of(br#"{"delivery":{"state":"PR_OPEN","version":3}}"#), Some(("PR_OPEN".into(), "3".into())));
    assert_eq!(crate::pr::delivery_of(br#"{"delivery":{"state":"PR_OPEN","version":"4"}}"#), Some(("PR_OPEN".into(), "4".into())));
    assert_eq!(crate::pr::delivery_of(br#"{"delivery":null}"#), None);
}

// ──────────────────────────────────────────────────────────────────────────────
// Records and parsing
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn the_context_answer_parses_into_settings_and_rows() {
    // The repository list itself (family U: spira_home_repo/spira_repos/repo_root/
    // repo_land) moved in-process (sp-k6lku, "wave 4.13"), same as the base-ref columns
    // (family W, sp-o88bx "wave 4.12"): parse_context no longer reads "repo=" records off
    // the wire at all — it builds the list from spira_config::repos against a real
    // SPIRA_REPO_MAP, read here from that same env var plus SPIRA_HOME_REPO (serialised:
    // process-global state). Neither "/h" nor the blank path below is a real checkout, so every base-ref
    // field reads None/empty, same as `repo_root` answering nothing — this test is about
    // the kv/record split and the repo list's shape, not git (see seam.rs's own
    // integration test for that resolution exercised against real git).
    let _serial = crate::testutil::serial();
    let dir = crate::testutil::tmpdir("context-answer");
    let map = dir.join("repomap-fixture");
    fs::write(&map, "spira | /h | queue.local\nother |\n").unwrap();
    let prev_map = std::env::var("SPIRA_REPO_MAP").ok();
    let prev_home_repo = std::env::var("SPIRA_HOME_REPO").ok();
    std::env::set_var("SPIRA_REPO_MAP", &map);
    std::env::set_var("SPIRA_HOME_REPO", "spira");

    let ans = "run=/r\0db=/db\0land_maxsec=3600\0gate_reserve=2700\0";
    let (s, repos) = crate::real::parse_context(ans, Path::new("/home")).unwrap();
    let no_map = crate::real::parse_context("db=x\0", Path::new("/h"));

    match prev_map {
        Some(v) => std::env::set_var("SPIRA_REPO_MAP", v),
        None => std::env::remove_var("SPIRA_REPO_MAP"),
    }
    match prev_home_repo {
        Some(v) => std::env::set_var("SPIRA_HOME_REPO", v),
        None => std::env::remove_var("SPIRA_HOME_REPO"),
    }

    assert_eq!((s.run.as_path(), s.land_maxsec, s.gate_reserve), (Path::new("/r"), 3600, 2700));
    assert_eq!(s.home_repo, "spira");
    assert_eq!(s.queue_bin.as_deref(), Some(Path::new("queue")), "queue by name, on the launcher's PATH");
    assert_eq!(repos[0].name, "spira");
    assert_eq!(repos[0].mode, LandMode::QueueLocal);
    assert_eq!(repos[0].base_remote, None);
    assert_eq!(repos[0].forge_ref, None);
    assert_eq!(repos[1].name, "other");
    assert_eq!((repos[1].path.as_os_str().is_empty(), repos[1].landref.clone(), &repos[1].mode), (true, None, &LandMode::Push));
    assert!(no_map.is_err());
    assert_eq!(s.path, None, "no path key: halt inherits the caller's PATH");
    let (s2, _) = crate::real::parse_context("run=/r\0path=/stub:/usr/bin\0", Path::new("/h")).unwrap();
    assert_eq!(s2.path.as_deref(), Some("/stub:/usr/bin"));
    assert_eq!(s.certify_par, 4, "unset SPIRA_CERTIFY_PAR is 4 (D14 (b))");
    let (s3, _) = crate::real::parse_context("run=/r certify_par=1 ", Path::new("/h")).unwrap();
    assert_eq!(s3.certify_par, 1);
    for (v, want) in [("", 4), ("0", 4), ("x", 4), (" 6", 6), ("10", 10)] {
        assert_eq!(crate::real::certify_par(Some(v)), want, "{v:?}");
    }
    assert_eq!(crate::real::json_only("warn: x\n[{\"id\":1}]"), "[{\"id\":1}]");
}

// ──────────────────────────────────────────────────────────────────────────────
// Concurrent certification (§8 D14, sp-kg14a)
// ──────────────────────────────────────────────────────────────────────────────

fn h_par(par: usize) -> H {
    let mut h = H::new(LandMode::QueueLocal);
    h.s.certify_par = par;
    h
}

fn starts(h: &H) -> Vec<String> {
    h.tools.trace().into_iter().filter_map(|t| t.strip_prefix("start spira/").map(String::from)).collect()
}

#[test]
fn par_n_starts_n_gates_before_any_finishes() {
    let h = h_par(3);
    for (i, id) in ["sp-a", "sp-b", "sp-c", "sp-d", "sp-e"].iter().enumerate() {
        h.closed(id, &format!("t{i}"));
    }
    h.run();
    let t = h.tools.trace();
    assert_eq!(&t[..3], &["start spira/sp-a", "start spira/sp-b", "start spira/sp-c"], "{t:?}");
    assert_eq!(t[3], "done spira/sp-a");
    assert_eq!(h.tools.max_running.get(), 3);
    assert_eq!(starts(&h), vec!["sp-a", "sp-b", "sp-c", "sp-d", "sp-e"]);
    assert!(!t.iter().any(|x| x.starts_with("serial")), "no serial gate at PAR>1: {t:?}");
    assert!(h.logged("landing: pass complete — 5 branch(es) seen, 5 movement(s)"));
    assert!(!h.s.run.join("landing.run").exists() || Files::new(&h.s.run).read_run().map(|r| r.phase != "gate").unwrap_or(true));
}

#[test]
fn a_base_fix_runs_alone_before_anything_else_starts() {
    let h = h_par(4);
    h.closed("sp-a", "ta");
    h.closed("sp-b", "tb");
    let mut fix = h.bead("sp-fix", "closed", &[]);
    fix.external_ref = Some("basefail:spira:test-x.sh".into());
    fix.priority = 4;
    h.beads.rows.borrow_mut().insert("sp-fix".into(), fix);
    h.git.add("spira/sp-fix", "tf");
    h.run();
    let t = h.tools.trace();
    assert_eq!(&t[..3], &["start spira/sp-fix", "done spira/sp-fix", "start spira/sp-a"], "{t:?}");
    assert_eq!(starts(&h), vec!["sp-fix", "sp-a", "sp-b"]);
}

#[test]
fn a_base_fix_that_becomes_ready_mid_pass_waits_for_the_walk_to_drain_then_runs_alone() {
    let h = h_par(3);
    for id in ["sp-a", "sp-b", "sp-c", "sp-d"] {
        h.closed(id, id);
    }
    let mut fix = h.bead("sp-fix", "in_progress", &[]);
    fix.external_ref = Some("basefail:spira:test-x.sh".into());
    h.beads.rows.borrow_mut().insert("sp-fix".into(), fix);
    h.git.add("spira/sp-fix", "tf");
    let inj = Injecting { t: &h.tools, h: &h, at_done: 1, n: Cell::new(0), inject: &|h: &H| {
        h.beads.rows.borrow_mut().get_mut("sp-fix").unwrap().status = "closed".into();
    } };
    h.pass_with(&inj).run();
    let t = h.tools.trace();
    let fix_start = t.iter().position(|x| x == "start spira/sp-fix").unwrap();
    let fix_done = t.iter().position(|x| x == "done spira/sp-fix").unwrap();
    // everything started before it had finished before it started, nothing started beside it
    assert!(t[..fix_start].iter().filter(|x| x.starts_with("start")).count() == t[..fix_start].iter().filter(|x| x.starts_with("done")).count(), "{t:?}");
    assert_eq!(fix_done, fix_start + 1, "{t:?}");
    assert_eq!(starts(&h), vec!["sp-a", "sp-b", "sp-c", "sp-fix", "sp-d"]);
}

/// Delegates to the fake tools; after the `at_done`-th completion, runs `inject` (a bead
/// submitted while the pass is running).
struct Injecting<'a> {
    t: &'a FakeTools,
    h: &'a H,
    at_done: usize,
    n: Cell<usize>,
    inject: &'a dyn Fn(&H),
}

impl Tools for Injecting<'_> {
    fn gate(&self, a: &str, b: &str, c: &str, d: &str) -> (i32, String) {
        self.t.gate(a, b, c, d)
    }
    fn gate_start(&self, a: &str, b: &str, c: &str, d: &str) -> u64 {
        self.t.gate_start(a, b, c, d)
    }
    fn gate_wait_any(&self) -> Option<(u64, i32, String)> {
        let r = self.t.gate_wait_any();
        self.n.set(self.n.get() + 1);
        if self.n.get() == self.at_done {
            (self.inject)(self.h);
        }
        r
    }
    fn gate_slots_free(&self, par: usize) -> Option<usize> {
        self.t.gate_slots_free(par)
    }
    fn gate_status(&self, a: &str, b: &str) -> Option<String> {
        self.t.gate_status(a, b)
    }
    fn confine(&self, a: &str, b: &str, c: &Path, d: &str, e: &str) -> (i32, String) {
        self.t.confine(a, b, c, d, e)
    }
    fn queue_step(&self, r: &str) -> Result<Vec<String>, String> {
        self.t.queue_step(r)
    }
    fn skew_refresh(&self, r: &Path) -> String {
        self.t.skew_refresh(r)
    }
    fn ensure(&self, s: &Path) -> Vec<String> {
        self.t.ensure(s)
    }
    fn rebase_stale(&self, id: &str, repo: &str) -> i32 {
        self.t.rebase_stale(id, repo)
    }
    fn forge_pr_state(&self, r: &Path, s: &str) -> Option<String> {
        self.t.forge_pr_state(r, s)
    }
    fn forge_pr_create(&self, r: &Path, h: &str, b: &str, t: &str, body: &str) -> Option<u64> {
        self.t.forge_pr_create(r, h, b, t, body)
    }
    fn forge_pr_list_open(&self, r: &Path) -> Vec<(u64, String)> {
        self.t.forge_pr_list_open(r)
    }
    fn forge_pr_automerge(&self, r: &Path, s: &str) -> bool {
        self.t.forge_pr_automerge(r, s)
    }
}

#[test]
fn a_p0_submitted_mid_pass_takes_the_next_free_slot() {
    let h = h_par(2);
    for id in ["sp-a", "sp-b", "sp-c", "sp-d"] {
        h.closed(id, id);
    }
    // In progress when the pass scanned; submitted while the first gates ran.
    let mut late = h.bead("sp-late", "in_progress", &[]);
    late.priority = 1;
    h.beads.rows.borrow_mut().insert("sp-late".into(), late);
    h.git.add("spira/sp-late", "tl");
    let inj = Injecting { t: &h.tools, h: &h, at_done: 1, n: Cell::new(0), inject: &|h: &H| {
        let mut hot = h.bead("sp-hot", "closed", &[]);
        hot.priority = 0;
        hot.closed_at = "2026-09-29".into();
        h.beads.rows.borrow_mut().insert("sp-hot".into(), hot);
        h.git.add("spira/sp-hot", "th");
        h.beads.rows.borrow_mut().get_mut("sp-late").unwrap().status = "closed".into();
    } };
    h.pass_with(&inj).run();
    assert_eq!(starts(&h), vec!["sp-a", "sp-b", "sp-hot", "sp-late", "sp-c", "sp-d"]);
    assert!(h.logged("CHECK6 spira: candidates refreshed — 2 newly ready: spira/sp-hot spira/sp-late"));
    assert!(h.logged("landing: pass complete — 6 branch(es) seen, 6 movement(s)"));
}

#[test]
fn a_full_admission_pool_holds_the_second_gate_back() {
    let h = h_par(3);
    for id in ["sp-a", "sp-b", "sp-c"] {
        h.closed(id, id);
    }
    h.tools.slots_free.set(Some(0));
    h.run();
    assert_eq!(h.tools.max_running.get(), 1, "the first gate always starts; no second into a full pool");
    assert_eq!(starts(&h), vec!["sp-a", "sp-b", "sp-c"]);
    assert_eq!(h.tools.probes.get(), 2, "asked only while a gate of this pass is already in flight");
}

#[test]
fn a_budget_cut_waits_for_the_gates_in_flight_then_defers_the_rest() {
    let mut h = h_par(2);
    h.s.land_maxsec = 3600;
    h.s.gate_reserve = 2700;
    h.tools.gate_cost.set(1000);
    for (i, id) in ["sp-a", "sp-b", "sp-c", "sp-d"].iter().enumerate() {
        let mut b = h.bead(id, "closed", &[]);
        b.priority = i as i64;
        h.beads.rows.borrow_mut().insert(id.to_string(), b);
        h.git.add(&format!("spira/{id}"), &format!("t{i}"));
    }
    let files = Files::new(&h.s.run);
    for _ in 0..4 {
        files.bump_deferred("spira/sp-d", "spira", 1);
    }
    files.bump_deferred("spira/sp-a", "spira", 1);
    h.run();
    // a and b start together; a's completion leaves 2600s < 2700 — c is the cut; b is still
    // decided.
    assert_eq!(starts(&h), vec!["sp-a", "sp-b"]);
    assert!(h.logged("landing: budget cut at spira/sp-c — 2600s left, 2 branch(es) deferred in spira"));
    assert_eq!(files.cursor_repo().as_deref(), Some("spira"));
    assert!(h.lib.has("ask_budget_deferred spira/sp-d spira 5"));
    assert!(!h.lib.has("ask_budget_deferred spira/sp-c"));
    assert!(!files.deferred_dir().join("spira_sp-a").exists());
}

#[test]
fn par_one_is_the_serial_walk_unchanged() {
    // The same scenario at PAR=1 through the concurrent-capable binary: serial gates only,
    // no probe, no refresh, and the same lines and calls as a walk that knows nothing of D14.
    let run = |par: usize| {
        let h = h_par(par);
        h.closed("sp-a", "ta");
        h.closed("sp-b", "tb");
        h.bead("sp-open", "in_progress", &[]);
        h.git.add("spira/sp-open", "to");
        h.tools.gates.borrow_mut().insert("spira/sp-b".into(), (1, "gate: VERDICT=FAIL reason=r branch=b repo=spira suite=s\n".into()));
        h.run();
        let lines: Vec<String> = h.out.lines().iter().map(|l| l.split_once(' ').map(|x| x.1).unwrap_or("").to_string()).collect();
        let calls: Vec<String> = h.lib.calls.borrow().clone();
        (lines, calls, h.tools.trace(), h.tools.probes.get(), h.mailbox())
    };
    let (lines, calls, trace, probes, mail) = run(1);
    assert_eq!(trace, vec!["serial spira/sp-a", "serial spira/sp-b"]);
    assert_eq!(probes, 0);
    assert!(!lines.iter().any(|l| l.contains("candidates refreshed")));
    // PAR=2 reaches the same mailbox for this input
    let (_, calls2, _, _, mail2) = run(2);
    assert_eq!(calls2, calls);
    assert_eq!(mail2, mail);
}

#[test]
fn sigterm_reaches_every_running_gate_and_its_children() {
    use crate::real::GatePool;
    use crate::util::{command, ChildSet};
    static SET: ChildSet = ChildSet::new();
    let pool = GatePool::new(&SET);
    for _ in 0..3 {
        let mut c = command("sh");
        // a gate with a child of its own, as gate.sh's suites are
        c.args(["-c", "sleep 120 & wait"]);
        pool.start(c);
    }
    assert_eq!(SET.pids().len(), 3, "every gate registered before start returns");
    let t0 = std::time::Instant::now();
    assert_eq!(SET.term_all().len(), 3);
    let mut got = Vec::new();
    while let Some((_, rc, _)) = pool.wait_any() {
        got.push(rc);
    }
    assert_eq!(got.len(), 3);
    assert!(got.iter().all(|rc| *rc != 0), "{got:?}");
    assert!(t0.elapsed().as_secs() < 100, "the gates (and their sleeps, which hold the pipe) died at once");
    assert!(SET.pids().is_empty());
}

#[test]
fn the_admission_probe_counts_free_slots_and_releases_them() {
    use std::os::unix::io::AsRawFd;
    let d = tmpdir("adm");
    assert_eq!(crate::real::admission_free(&d.join("absent"), 4), 4);
    let held = fs::OpenOptions::new().create(true).write(true).truncate(false).open(d.join("slot.2.lock")).unwrap();
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
    assert_eq!(crate::real::admission_free(&d, 3), 2);
    // Released with LOCK_UN, not by drop: a child forked by another test thread while `held`
    // is open keeps a copy of its description until it execs, and a close releases the lock
    // only once every copy is gone — this assertion flipped on exactly that (1 in 5 runs).
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_UN) }, 0);
    drop(held);
    assert_eq!(crate::real::admission_free(&d, 3), 3, "the probe itself holds nothing afterwards");
    // Run again: a probe that released by close would leave a lock behind in any child forked
    // during it, and this second count would read one short.
    assert_eq!(crate::real::admission_free(&d, 3), 3);
}

#[test]
fn push_mode_stays_serial_at_any_par() {
    let mut h = H::new(LandMode::Hold);
    h.s.certify_par = 4;
    h.closed("sp-a", "t1");
    h.closed("sp-b", "t2");
    h.run();
    assert!(h.tools.trace().iter().all(|t| t.starts_with("serial")), "{:?}", h.tools.trace());
}

// ──────────────────────────────────────────────────────────────────────────────
// The lifecycle switch (§9)
// ──────────────────────────────────────────────────────────────────────────────

fn pr_run(h: &H, tools: &FakePr) -> Vec<String> {
    let files = Files::new(&h.s.run);
    let p = PrPass {
        s: &h.s,
        repos: &h.repos,
        beads: &h.beads,
        git: &h.git,
        lib: &h.lib,
        land_tools: &h.tools,
        procs: &h.procs,
        tools,
        files: &files,
        out: &h.out,
        loud: Default::default(),
    };
    p.run();
    p.loud.into_inner()
}

fn push_fixture(on: bool) -> H {
    let mut h = H::new(LandMode::Push);
    h.s.lifecycle_enforce = on;
    h.git.tree.set(true);
    h.closed("sp-a", "t1");
    h.closed("sp-b", "t2");
    h.git.merge_conflict.borrow_mut().insert("spira/sp-b".into());
    h
}

#[test]
fn off_push_mode_never_invokes_spira_lc() {
    let h = push_fixture(false);
    h.run();
    assert!(h.lib.has("reopen sp-b rebase-conflict"));
    assert_eq!(h.lc.probes.get(), 0);
    assert!(!h.lib.has("deliver_"), "{:?}", h.lib.calls.borrow());
}

#[test]
fn on_push_mode_records_deliveries_through_the_machine() {
    let h = push_fixture(true);
    h.run();
    assert!(h.lib.has("deliver_delivered sp-a head1"));
    assert!(h.lib.has("deliver_returned sp-b"));
    assert_eq!(h.lc.probes.get(), 1, "probed once per pass");
}

#[test]
fn on_with_the_machine_unreachable_a_push_landing_is_refused_loudly() {
    let h = push_fixture(true);
    *h.lc.down.borrow_mut() = Some("Access denied".into());
    h.run();
    assert!(h.logged("landing: lifecycle_enforce is on and spira-lc is unreachable (Access denied) — not landing spira/sp-a this pass"));
}

#[test]
fn on_a_lifecycle_submitted_bead_without_the_label_is_certified_and_recorded() {
    let mut h = H::new(LandMode::QueueLocal);
    h.s.lifecycle_enforce = true;
    h.bead("sp-a", "open", &[]);
    h.git.add("spira/sp-a", "t1");
    h.lc.submitted.borrow_mut().insert("sp-a".into(), "t1".into());
    h.run();
    assert_eq!(*h.lc.certified.borrow(), vec!["sp-a t1 pass"]);
}

#[test]
fn a_refused_gatepass_leaves_the_bead_uncertified_in_landstate() {
    let mut h = H::new(LandMode::QueueLocal);
    h.s.lifecycle_enforce = true;
    h.bead("sp-a", "open", &[]);
    h.git.add("spira/sp-a", "t1");
    h.lc.submitted.borrow_mut().insert("sp-a".into(), "t1".into());
    h.lc.refuse_pass.set(true);
    h.run();
    assert_eq!(*h.lc.certified.borrow(), vec!["sp-a t1 pass"]);
}

#[test]
fn on_a_lifecycle_row_at_another_tip_is_not_certified() {
    let mut h = H::new(LandMode::QueueLocal);
    h.s.lifecycle_enforce = true;
    h.bead("sp-a", "open", &[]);
    h.git.add("spira/sp-a", "t2");
    h.lc.submitted.borrow_mut().insert("sp-a".into(), "t1".into());
    h.run();
    assert!(h.lc.certified.borrow().is_empty());
}

#[test]
fn off_an_unlabelled_open_bead_stays_uncertified_whatever_lifecycle_says() {
    let h = H::new(LandMode::QueueLocal);
    h.bead("sp-a", "open", &[]);
    h.git.add("spira/sp-a", "t1");
    h.lc.submitted.borrow_mut().insert("sp-a".into(), "t1".into());
    h.run();
    assert!(h.lc.certified.borrow().is_empty());
}

#[test]
fn submitted_parse_keeps_only_submitted_rows_with_a_tip() {
    let j = br#"[{"bead_id":"a","state":"SUBMITTED","tip":"t"},{"bead_id":"b","state":"WORKING","tip":"u"},{"bead_id":"c","state":"SUBMITTED","tip":""}]"#;
    let m = crate::lifecycle::parse_submitted(j).unwrap();
    assert_eq!(m.len(), 1);
    assert_eq!(m["a"], "t");
}

#[test]
fn off_queue_certification_never_invokes_spira_lc() {
    let h = H::new(LandMode::QueueLocal);
    h.closed("sp-a", "t1");
    h.run();
    assert_eq!(h.lc.probes.get(), 0);
}

#[test]
fn on_the_pr_pass_proves_content_deliveries_and_is_loud_when_the_machine_fails() {
    let mut h = H::new(LandMode::Pr);
    h.s.lifecycle_enforce = true;
    h.closed("sp-merged", "t3");
    h.git.content.borrow_mut().insert("spira/sp-merged".into());
    h.git.shas.borrow_mut().insert("refs/remotes/origin/main".into(), "M".into());
    let tools = FakePr::default();
    assert!(pr_run(&h, &tools).is_empty());
    assert_eq!(*tools.delivered.borrow(), vec!["sp-merged M"]);

    let tools = FakePr::default();
    *tools.fail.borrow_mut() = Some("show exited 1: Access denied".into());
    let loud = pr_run(&h, &tools);
    assert_eq!(
        loud,
        vec!["landing-pass: sp-merged: LIFECYCLE: lifecycle_enforce is on and the Delivered event did not happen (show exited 1: Access denied) — the delivery row stays PR_OPEN"]
    );
}

#[test]
fn halt_children_run_under_conf_path_then_spira_path_then_inherit() {
    use crate::halt::child_path;
    // conf.sh's answer wins: it already put SPIRA_PATH (from env or the typed config) first.
    assert_eq!(child_path(Some("/conf:/bin"), Some("/sp"), Some("/usr/bin")).as_deref(), Some("/conf:/bin"));
    // No context (unloadable conf): SPIRA_PATH is prepended, as conf.sh would have.
    assert_eq!(child_path(None, Some("/sp"), Some("/usr/bin")).as_deref(), Some("/sp:/usr/bin"));
    assert_eq!(child_path(Some(""), Some("/sp"), None).as_deref(), Some("/sp"));
    // Neither: inherit (None leaves the Command's PATH alone).
    assert_eq!(child_path(None, None, Some("/usr/bin")), None);
    assert_eq!(child_path(None, Some(""), Some("/usr/bin")), None);
}

#[test]
fn real_halt_finds_podman_and_testenv_on_its_path() {
    use crate::halt::{HaltPorts, RealHalt};
    let dir = testkit::TempDir::new("lp-halt-path");
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = dir.join("log");
    let podman = bin.join("podman");
    testkit::write_exe(&podman, "#!/bin/sh\n[ \"$1\" = ps ] && echo spira-batch-stubbed\nexit 0\n");
    testkit::write_exe(&bin.join("testenv"), &format!("#!/bin/sh\necho \"$*\" >> {}\ncommand -v podman >> {}\n", log.display(), log.display()));
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let h = RealHalt { path: Some(path) };
    assert_eq!(h.running_containers(), vec!["spira-batch-stubbed".to_string()]);
    assert!(h.teardown("spira-batch-stubbed"));
    let got = std::fs::read_to_string(&log).unwrap();
    assert!(got.contains("container down --name spira-batch-stubbed --volumes --force-foreign"), "{got}");
    assert!(got.contains(&podman.display().to_string()), "testenv inherits the path: {got}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ──────────────────────────────────────────────────────────────────────────────
// gate-worker: the pass queues the gate and applies the verdict it filed
// ──────────────────────────────────────────────────────────────────────────────

fn h_worker(mode: LandMode) -> H {
    let mut h = H::new(mode);
    h.s.gate_worker = true;
    h
}

fn file_verdict(h: &H, id: &str, tip: &str, rc: i32, out: &str) {
    let q = crate::gateq::GateQueue::new(&h.s.run);
    let job = crate::gateq::Job::new("spira", &format!("spira/{id}"), id, tip, false);
    q.enqueue(&job).unwrap();
    q.claim(0).unwrap();
    q.complete(0, &crate::gateq::Done { job, run: GateRun::parse(rc, out.into()), started_ms: 1, finished_ms: 2 }).unwrap();
}

#[test]
fn a_pass_with_a_gate_worker_queues_every_branch_and_no_budget_can_cut_it() {
    let mut h = h_worker(LandMode::QueueLocal);
    h.s.land_maxsec = 3600;
    h.s.gate_reserve = 99_999;
    for (i, id) in ["sp-a", "sp-b", "sp-c", "sp-d"].iter().enumerate() {
        h.closed(id, &format!("t{i}"));
    }
    h.run();
    assert!(h.tools.gate_calls.borrow().is_empty(), "the pass must not run a gate itself");
    assert!(!h.logged("budget cut"), "{:?}", h.out.lines());
    let q = crate::gateq::GateQueue::new(&h.s.run);
    assert_eq!(q.queued().len(), 4);
    assert!(!h.lib.has("CERTIFIED"));
}

#[test]
fn a_branch_already_queued_is_not_queued_twice() {
    let h = h_worker(LandMode::Queue);
    h.closed("sp-a", "t1");
    h.run();
    h.run();
    assert_eq!(crate::gateq::GateQueue::new(&h.s.run).queued().len(), 1);
    assert!(h.logged("CHECK6 sp-a: spira/sp-a is already with gate-worker at t1"));
}

#[test]
fn a_filed_pass_certifies() {
    let h = h_worker(LandMode::Queue);
    h.closed("sp-a", "t1");
    file_verdict(&h, "sp-a", "t1", 0, "gate: VERDICT=PASS\n");
    h.run();
    assert!(h.tools.gate_calls.borrow().is_empty());
}

#[test]
fn a_filed_fail_is_the_branchs_and_reopens_it() {
    let h = h_worker(LandMode::Queue);
    h.closed("sp-a", "t1");
    file_verdict(&h, "sp-a", "t1", 1, "gate: VERDICT=FAIL reason=suite-red branch=b repo=spira suite=test-x.sh\n");
    h.run();
    assert!(h.lib.has("reopen sp-a cert-gate-red"));
}

#[test]
fn a_filed_no_verdict_is_nobodys_and_charges_nothing() {
    let h = h_worker(LandMode::Queue);
    h.closed("sp-a", "t1");
    file_verdict(&h, "sp-a", "t1", 75, "gate: VERDICT=NO_VERDICT reason=gate-did-not-start\n");
    h.run();
    assert!(h.lib.has("noverdict sp-a spira gate-did-not-start NO_VERDICT"));
    assert!(!h.lib.has("reopen"));
    assert!(!h.lib.has("RED"));
}

#[test]
fn a_filed_base_fail_holds_the_branch_and_files_the_bases_own_red() {
    let h = h_worker(LandMode::Queue);
    h.closed("sp-a", "t1");
    fs::create_dir_all(h.s.incident.parent().unwrap()).unwrap();
    fs::write(&h.s.incident, "").unwrap();
    file_verdict(&h, "sp-a", "t1", 76, "--- base\ntest-x.sh RED\ngate: VERDICT=BASE_FAIL reason=base-red branch=b repo=spira suite=test-x.sh\n");
    h.run();
    assert_eq!(h.lib.count("incident "), 1);
    assert!(!h.lib.has("reopen"));
    assert!(!h.lib.has("RED"));
}

#[test]
fn a_verdict_for_a_tree_the_branch_no_longer_has_is_not_applied() {
    let h = h_worker(LandMode::Queue);
    h.closed("sp-a", "t2");
    file_verdict(&h, "sp-a", "t1", 0, "gate: VERDICT=PASS\n");
    h.run();
    assert!(!h.lib.has("CERTIFIED"));
}
