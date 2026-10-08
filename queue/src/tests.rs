//! Contract tests (DESIGN.md §2.2, §8), one world of fakes per test: git, bd, the lib.sh
//! seam, the scripts, the forge, spira-lc, the config and the clock are traits; the queue's
//! own files live in a scratch directory.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::cli::{self, Cmd};
use crate::model::{BeadRow, LandMode, LcBeadRow, RangeCommit};
use crate::ports::*;
use crate::testutil::tmpdir;

mod round;
mod verdict;

// ------------------------------------------------------------------------------- fakes

#[derive(Default)]
struct FGit {
    refs: RefCell<BTreeMap<String, String>>,
    anc: RefCell<BTreeSet<(String, String)>>,
    bases: RefCell<BTreeMap<(String, String), String>>,
    commits: RefCell<BTreeSet<String>>,
    trees: RefCell<BTreeMap<String, String>>,
    log: RefCell<Vec<RangeCommit>>,
    current: RefCell<Option<String>>,
    fetch_ok: Cell<bool>,
    merge_fail: RefCell<BTreeSet<String>>,
    branches: RefCell<Vec<(String, String)>>,
    calls: RefCell<Vec<String>>,
}

impl FGit {
    fn set(&self, r: &str, sha: &str) {
        self.refs.borrow_mut().insert(r.into(), sha.into());
        self.commits.borrow_mut().insert(sha.into());
    }
    fn get(&self, r: &str) -> Option<String> {
        self.refs.borrow().get(r).cloned()
    }
    fn ancestor(&self, a: &str, b: &str) {
        self.anc.borrow_mut().insert((a.into(), b.into()));
        self.commits.borrow_mut().insert(a.into());
        self.commits.borrow_mut().insert(b.into());
    }
}

impl Git for FGit {
    fn rev_parse(&self, _repo: &Path, rev: &str) -> Option<String> {
        if let Some(t) = rev.strip_suffix("^{tree}") {
            let sha = self.rev_parse(_repo, t)?;
            return self.trees.borrow().get(&sha).cloned();
        }
        let refs = self.refs.borrow();
        refs.get(rev)
            .or_else(|| refs.get(&format!("refs/heads/{rev}")))
            .cloned()
            .or_else(|| self.commits.borrow().contains(rev).then(|| rev.to_string()))
    }
    fn is_ancestor(&self, _: &Path, a: &str, b: &str) -> bool {
        a == b || self.anc.borrow().contains(&(a.to_string(), b.to_string()))
    }
    fn merge_base(&self, _: &Path, a: &str, b: &str) -> Option<String> {
        let m = self.bases.borrow();
        m.get(&(a.to_string(), b.to_string())).or_else(|| m.get(&(b.to_string(), a.to_string()))).cloned()
    }
    fn update_ref(&self, _: &Path, r: &str, new: &str, old: Option<&str>) -> bool {
        self.calls.borrow_mut().push(format!("update-ref {r} {new} {}", old.unwrap_or("-")));
        if let Some(o) = old {
            if self.get(r).as_deref() != Some(o) {
                return false;
            }
        }
        self.set(r, new);
        true
    }
    fn ref_exists(&self, _: &Path, r: &str) -> bool {
        self.refs.borrow().contains_key(r)
    }
    fn current_branch(&self, _: &Path) -> Option<String> {
        self.current.borrow().clone()
    }
    fn fetch(&self, _: &Path, remote: &str, branch: &str) -> bool {
        self.calls.borrow_mut().push(format!("fetch {remote} {branch}"));
        self.fetch_ok.get()
    }
    fn log_range(&self, _: &Path, range: &str) -> Result<Vec<RangeCommit>, String> {
        self.calls.borrow_mut().push(format!("log {range}"));
        Ok(self.log.borrow().clone())
    }
    fn commit_exists(&self, _: &Path, sha: &str) -> bool {
        self.commits.borrow().contains(sha)
    }
    fn branch_set(&self, _: &Path, name: &str, sha: &str, _force: bool) -> bool {
        self.set(&format!("refs/heads/{name}"), sha);
        true
    }
    fn branch_delete(&self, _: &Path, name: &str) -> bool {
        self.calls.borrow_mut().push(format!("branch -D {name}"));
        self.refs.borrow_mut().remove(&format!("refs/heads/{name}")).is_some()
    }
    fn branch_delete_sanctioned(&self, _: &Path, name: &str) -> bool {
        self.calls.borrow_mut().push(format!("branch -D sanctioned {name}"));
        self.refs.borrow_mut().remove(&format!("refs/heads/{name}")).is_some()
    }
    fn branches(&self, _: &Path, _prefix: &str) -> Vec<(String, String)> {
        self.branches.borrow().clone()
    }
    fn worktree_prune(&self, _: &Path) {}
    fn worktree_add_detached(&self, _: &Path, p: &Path, sha: &str) -> bool {
        self.refs.borrow_mut().insert(format!("{}:HEAD", p.display()), sha.into());
        true
    }
    fn worktree_remove(&self, _: &Path, _: &Path) {}
    fn merge_no_ff(&self, wt: &Path, msg: &str, tip: &str, _git_name: &str, _git_email: &str) -> bool {
        self.calls.borrow_mut().push(format!("merge {tip} {msg}"));
        if self.merge_fail.borrow().contains(tip) {
            return false;
        }
        self.refs.borrow_mut().insert("HEAD".into(), format!("merged-{tip}"));
        let _ = wt;
        true
    }
    fn merge_abort(&self, _: &Path) {}
    fn is_clean(&self, _: &Path) -> bool {
        true
    }
}

#[derive(Default)]
struct FBd {
    rows: RefCell<Vec<BeadRow>>,
    fail: Cell<bool>,
}

impl Bd for FBd {
    fn show(&self, ids: &[String]) -> Result<Vec<BeadRow>, String> {
        if self.fail.get() {
            return Err("bd down".into());
        }
        Ok(self.rows.borrow().iter().filter(|r| ids.contains(&r.id)).cloned().collect())
    }
}

struct FLib {
    s: Settings,
    r: RefCell<RepoCtx>,
    calls: RefCell<Vec<String>>,
    toml: Option<PathBuf>,
    readback: RefCell<(String, String)>,
    diverged: Cell<bool>,
    cannot_check: Cell<bool>,
    push_ok: Cell<bool>,
    /// What `create_bug` answers (None: bd created nothing).
    bug_id: RefCell<Option<String>>,
    amend_ok: Cell<bool>,
    pf_rc: Cell<i32>,
    conflict_with_base: RefCell<BTreeSet<String>>,
    /// R22's answer (step --all).
    repos: RefCell<Result<Vec<String>, String>>,
    /// Per-name contexts; a name not here resolves to `r`.
    by_name: RefCell<BTreeMap<String, Result<RepoCtx, String>>>,
    /// `bead_close` fails (bd refused the close).
    close_fail: Cell<bool>,
}

impl FLib {
    fn log(&self, s: String) {
        self.calls.borrow_mut().push(s);
    }
    fn has(&self, prefix: &str) -> bool {
        self.calls.borrow().iter().any(|c| c.starts_with(prefix))
    }
}

impl Lib for FLib {
    fn context(&self, repo: Option<&str>) -> Result<(Settings, RepoCtx), String> {
        if let Some(r) = repo.and_then(|n| self.by_name.borrow().get(n).cloned()) {
            return r.map(|r| (self.s.clone(), r));
        }
        Ok((self.s.clone(), self.r.borrow().clone()))
    }
    fn repos(&self) -> Result<Vec<String>, String> {
        self.repos.borrow().clone()
    }
    fn toml_path(&self) -> Option<PathBuf> {
        self.toml.clone()
    }
    fn readback(&self, _: &str) -> (String, String) {
        self.readback.borrow().clone()
    }
    fn bead_reopen(&self, id: &str, cause: &str, suites: &str) -> bool {
        self.log(format!("bead_reopen {id} {cause} {suites}"));
        true
    }
    fn cause_event(&self, id: &str, cause: &str) {
        self.log(format!("cause_event {id} {cause}"));
    }
    fn release_claim(&self, id: &str) {
        self.log(format!("release_claim {id}"));
    }
    /// Logged as `bead_close <id> <sha>`, the sha read back out of the reason it carries.
    fn bead_close(&self, id: &str, reason: &str) -> bool {
        let sha = reason.split("landed at ").nth(1).and_then(|r| r.split_whitespace().next()).unwrap_or("?");
        self.log(format!("bead_close {id} {sha}"));
        !self.close_fail.get()
    }
    fn reap_landed_branch(&self, id: &str, repo: &str, branch: &str, why: &str) -> Result<bool, String> {
        self.log(format!("reap {id} {repo} {branch} {why}"));
        Ok(true)
    }
    fn gh_issue_closeout(&self, id: &str, sha: &str, _: &Path) {
        self.log(format!("gh_closeout {id} {sha}"));
    }
    fn comment(&self, id: &str, text: &str) {
        self.log(format!("comment {id} {text}"));
    }
    fn notify(&self, _mailbox: &str, repo: &str, subject: &str, _body: &str) {
        self.log(format!("notify {repo} {subject}"));
    }
    fn event(&self, kind: &str, title: &str, detail: &str) {
        self.log(format!("event {kind} {title} {detail}"));
    }
    fn divergence(&self, _mailbox: &str, _: &Path, _: &str, _: &Path, forge: &str, local: &str) -> Divergence {
        self.log(format!("divergence {forge} {local}"));
        if self.cannot_check.get() {
            Divergence::CannotCheck("no queue dir".into())
        } else if self.diverged.get() {
            Divergence::Diverged(format!("{local}..{forge}"))
        } else {
            Divergence::Ancestor
        }
    }
    fn push(&self, _: &Path, remote: &str, refspec: &str) -> bool {
        self.log(format!("push {remote} {refspec}"));
        self.push_ok.get()
    }
    fn rebase(&self, br: &str, onto: &str, _: &Path, _: &str) -> Result<(), String> {
        self.log(format!("rebase {br} {onto}"));
        Ok(())
    }
    fn land_subject(&self, id: &str) -> String {
        format!("spira: land {id}")
    }
    fn sort_rows(&self, _express_label: &str, _: &Path, _: &str, prio: &str, rows: &str) -> Vec<(String, String)> {
        self.log(format!("sort_rows {prio}"));
        rows.lines().filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.to_string(), it.next()?.to_string()))
        }).collect()
    }
    fn cancel_runs(&self, _: &Path, _: &Path, br: &str) {
        self.log(format!("cancel_runs {br}"));
    }
    fn format_batch(&self, _: &Path, _: &str, _: &str) {
        self.log("format_batch".into());
    }
    fn base_conflict(&self, _: &Path, _: &str, tip: &str) -> bool {
        self.conflict_with_base.borrow().contains(tip)
    }
    fn pf_gate(&self, br: &str, _: &str, _: &str, wall: u64) -> (i32, String) {
        self.log(format!("pf_gate {br} {wall}"));
        (self.pf_rc.get(), "pf output".into())
    }
    fn create_bug(&self, actor: &str, title: &str, prio: &str, labels: &str, body: &str) -> Option<String> {
        self.log(format!("create_bug {actor} {prio} {labels} {title}\n{body}"));
        self.bug_id.borrow().clone()
    }
    fn amend_bug(&self, actor: &str, id: &str, note: &str) -> bool {
        self.log(format!("amend_bug {actor} {id} {note}"));
        self.amend_ok.get()
    }
}

#[derive(Default)]
struct FScripts {
    gate_rc: Cell<i32>,
    /// `release <sub>` exit status by subcommand (absent = 0), and the stderr line it prints.
    release_rc: RefCell<BTreeMap<String, (i32, String)>>,
    /// What `release build` answers on stdout (absent = the commit it was asked for).
    build_answers: RefCell<Option<String>>,
    /// Every `release` call: the bin it was run as, its argv joined, and the SPIRA_DB it
    /// was handed.
    release_calls: RefCell<Vec<(String, String, String)>>,
    fence_ok: Cell<bool>,
    calls: RefCell<Vec<String>>,
    /// What `batcher judgement-ci` answers.
    judgement: RefCell<RunOut>,
    /// What `round-vm run` answers, and the `<suite>.result` files it leaves in --results-dir.
    round_vm: RefCell<RunOut>,
    round_vm_results: RefCell<Vec<(String, String)>>,
}

impl Scripts for FScripts {
    fn gate(&self, br: &str, repo: &str, bead: &str, suites: &str) -> (i32, String) {
        self.calls.borrow_mut().push(format!("gate {br} {repo} bead={bead} suites={suites}"));
        (self.gate_rc.get(), "gate says".into())
    }
    fn judgement_ci(&self, bin: &Path, s: &Settings, repo: &str, suites: &str, members: &str, evidence: &str) -> RunOut {
        self.calls.borrow_mut().push(format!("judgement-ci {} {repo} {suites} {members} {evidence} home={}", bin.display(), s.home.display()));
        self.judgement.borrow().clone()
    }
    fn observe_flake(&self, suite: &str, sha: &str) {
        self.calls.borrow_mut().push(format!("observe-flake {suite} {sha}"));
    }
    fn mail_operator(&self, subject: &str, body: &str) {
        self.calls.borrow_mut().push(format!("mail-operator {subject}\n{body}"));
    }
    fn batcher_cut(&self, _: &Path, repo: &str, wz: bool) -> i32 {
        self.calls.borrow_mut().push(format!("cut {repo} wait0={wz}"));
        0
    }
    fn czar_fence(&self, class: &str) -> bool {
        self.calls.borrow_mut().push(format!("czar {class}"));
        self.fence_ok.get()
    }
    fn round_vm(&self, tree: &Path, results: &Path, base: &str, wall_secs: u64) -> RunOut {
        self.calls.borrow_mut().push(format!("round-vm {} base={base} wall={wall_secs}", tree.display()));
        fs::create_dir_all(results).unwrap();
        for (suite, status) in self.round_vm_results.borrow().iter() {
            fs::write(results.join(format!("{suite}.result")), format!("{status}\n")).unwrap();
        }
        self.round_vm.borrow().clone()
    }
    fn release(&self, bin: &Path, args: &[String], db: &str) -> RunOut {
        self.release_calls.borrow_mut().push((bin.display().to_string(), args.join(" "), db.to_string()));
        self.calls.borrow_mut().push(format!("release {}", args[0]));
        let (rc, err) = self.release_rc.borrow().get(&args[0]).cloned().unwrap_or((0, String::new()));
        let out = if args[0] == "build" && rc == 0 { format!("{}\n", self.build_answers.borrow().clone().unwrap_or_else(|| args[1].clone())) } else { String::new() };
        RunOut { rc, out, err }
    }
}

#[derive(Default)]
struct FForge {
    pr: RefCell<Option<String>>,
    calls: RefCell<Vec<String>>,
    /// check-status answers in order; when none is left the answer is `pending`. A `None`
    /// entry is a failed call.
    status: RefCell<Vec<Option<String>>>,
    /// pr-state answer; `None` reads as `open`.
    pr_state: RefCell<Option<String>>,
    run: RefCell<Option<String>>,
    meta: RefCell<String>,
}

impl Forge for FForge {
    fn pr_create(&self, _: &Path, _: &Path, head: &str, base: &str, title: &str, body: &str) -> Option<String> {
        self.calls.borrow_mut().push(format!("pr-create {head} {base} {title}\n{body}"));
        self.pr.borrow().clone()
    }
    fn pr_close(&self, _: &Path, _: &Path, pr: &str) {
        self.calls.borrow_mut().push(format!("pr-close {pr}"));
    }
    fn pr_comment(&self, _: &Path, _: &Path, pr: &str, line: &str) {
        self.calls.borrow_mut().push(format!("pr-comment {pr} {line}"));
    }
    fn branch_protect(&self, _: &Path, _: &Path, b: &str) -> bool {
        self.calls.borrow_mut().push(format!("protect {b}"));
        true
    }
    fn pr_state(&self, _: &Path, _: &Path, pr: &str) -> Option<String> {
        self.calls.borrow_mut().push(format!("pr-state {pr}"));
        Some(self.pr_state.borrow().clone().unwrap_or_else(|| "open".into()))
    }
    fn check_status(&self, _: &Path, _: &Path, pr: &str, branch: &str) -> Option<String> {
        self.calls.borrow_mut().push(format!("check-status {pr} {branch}"));
        let mut q = self.status.borrow_mut();
        if q.is_empty() {
            Some("pending\n".into())
        } else {
            q.remove(0)
        }
    }
    fn run_id(&self, _: &Path, _: &Path, branch: &str) -> Option<String> {
        self.calls.borrow_mut().push(format!("run-id {branch}"));
        self.run.borrow().clone()
    }
    fn run_metadata(&self, _: &Path, _: &Path, run: &str) -> String {
        self.calls.borrow_mut().push(format!("run-metadata {run}"));
        self.meta.borrow().clone()
    }
    fn run_cancel(&self, _: &Path, _: &Path, run: &str) {
        self.calls.borrow_mut().push(format!("run-cancel {run}"));
    }
    fn workflow_rerun(&self, _: &Path, _: &Path, run: &str) {
        self.calls.borrow_mut().push(format!("workflow-rerun {run}"));
    }
}

struct FLc {
    available: Cell<bool>,
    bead_rows: RefCell<std::collections::HashMap<String, (String, String)>>,
    rows: RefCell<Result<Vec<LcBeadRow>, String>>,
    certify_refused: Cell<bool>,
    land_refused: Cell<Option<&'static str>>,
    /// `show <bead>` cannot answer (the bulk `list` still does).
    row_fails: Cell<bool>,
    calls: RefCell<Vec<String>>,
}

impl Default for FLc {
    fn default() -> Self {
        FLc { available: Cell::new(false), bead_rows: RefCell::default(), rows: RefCell::new(Ok(Vec::new())), certify_refused: Cell::new(false), land_refused: Cell::new(None), row_fails: Cell::new(false), calls: RefCell::default() }
    }
}

impl FLc {
    fn has(&self, prefix: &str) -> bool {
        self.calls.borrow().iter().any(|c| c.starts_with(prefix))
    }
}

impl Lc for FLc {
    fn available(&self) -> bool {
        self.calls.borrow_mut().push("available".into());
        self.available.get()
    }
    fn probe(&self) -> Result<(), String> {
        self.calls.borrow_mut().push("probe".into());
        self.rows.borrow().clone().map(|_| ())
    }
    fn batch_state(&self, id: &str) -> Option<(String, String)> {
        self.calls.borrow_mut().push(format!("show-batch {id}"));
        Some(("CI_RUNNING".into(), "4".into()))
    }
    fn create_bead(&self, id: &str) {
        self.calls.borrow_mut().push(format!("create-bead {id}"));
    }
    fn cut(&self, id: &str, _: &str, _: &str, _: &str, members: &str, _: &str) -> Result<String, (i32, String)> {
        self.calls.borrow_mut().push(format!("cut {id} {members}"));
        Ok("1".into())
    }
    fn abandon_batch(&self, id: &str, s: &str, v: &str, _: &str, r: &str) -> Result<(), (i32, String)> {
        self.calls.borrow_mut().push(format!("abandon-batch {id} {s} {v} {r}"));
        Ok(())
    }
    fn eject_member(&self, id: &str, bead: &str, s: &str, v: &str, _: &str, r: &str) -> Result<(), (i32, String)> {
        self.calls.borrow_mut().push(format!("eject-member {id} {bead} {s} {v} {r}"));
        Ok(())
    }
    fn batch_event(&self, id: &str, s: &str, v: &str, _: &str, kind: &str) -> Result<(), (i32, String)> {
        self.calls.borrow_mut().push(format!("event batch {id} {s} {v} {kind}"));
        Ok(())
    }
    fn bead_state(&self, bead: &str) -> Option<(String, String)> {
        self.calls.borrow_mut().push(format!("show {bead}"));
        Some(self.bead_rows.borrow().get(bead).cloned().unwrap_or(("CERTIFIED".into(), "3".into())))
    }
    fn bead_event(&self, bead: &str, s: &str, v: &str, _: &str, kind: &str) -> Result<(), (i32, String)> {
        self.calls.borrow_mut().push(format!("event bead {bead} {s} {v} {kind}"));
        Ok(())
    }
    fn land_batch(&self, id: &str, v: &str, _: &str, sha: &str) -> Result<(), (i32, String)> {
        self.calls.borrow_mut().push(format!("land {id} {v} {sha}"));
        match self.land_refused.get() {
            Some(why) => Err((3, why.into())),
            None => Ok(()),
        }
    }
    fn bead_rows(&self, state: Option<&str>) -> Result<Vec<LcBeadRow>, String> {
        self.calls.borrow_mut().push(format!("list {}", state.unwrap_or("")));
        self.rows.borrow().clone().map(|r| r.into_iter().filter(|b| state.is_none_or(|s| b.state == s)).collect())
    }
    fn bead_row(&self, bead: &str) -> Option<LcBeadRow> {
        self.calls.borrow_mut().push(format!("row {bead}"));
        if self.row_fails.get() {
            return None;
        }
        self.rows.borrow().clone().ok()?.into_iter().find(|b| b.bead_id == bead)
    }
    fn certify(&self, bead: &str, tip: &str, _: &str, _: &str) -> Result<(), (i32, String)> {
        self.calls.borrow_mut().push(format!("certify {bead} {tip}"));
        if self.certify_refused.get() {
            return Err((3, "refused".into()));
        }
        Ok(())
    }
}

#[derive(Default)]
struct FConfig {
    rows: RefCell<BTreeMap<String, (String, String)>>,
    legacy: RefCell<BTreeMap<String, (String, String)>>,
    legacy_fail: Cell<bool>,
}

impl ConfigStore for FConfig {
    fn repo_row(&self, _: &Path, name: &str) -> Result<(String, String), String> {
        Ok(self.rows.borrow().get(name).cloned().unwrap_or_default())
    }
    fn set_repo_row(&self, _: &Path, name: &str, m: &str, b: &str) -> Result<(), String> {
        self.rows.borrow_mut().insert(name.into(), (m.into(), b.into()));
        Ok(())
    }
    fn set_legacy_map_row(&self, _: &Path, name: &str, l: &str, b: &str) -> Result<(), String> {
        if self.legacy_fail.get() {
            return Err("legacy map write failed".into());
        }
        self.legacy.borrow_mut().insert(name.into(), (l.into(), b.into()));
        Ok(())
    }
}

struct FClock {
    now: Cell<u64>,
}

impl Clock for FClock {
    fn now(&self) -> u64 {
        self.now.get()
    }
    fn stamp(&self) -> String {
        "20260929T010203Z".into()
    }
    fn sleep(&self, s: u64) {
        self.now.set(self.now.get() + s.max(1));
    }
}

#[derive(Default)]
struct FEnv {
    vars: RefCell<BTreeMap<String, String>>,
    stdin: RefCell<String>,
}

impl Env for FEnv {
    fn var(&self, k: &str) -> Option<String> {
        self.vars.borrow().get(k).cloned()
    }
    fn pid(&self) -> u32 {
        4242
    }
    fn read_stdin(&self) -> Result<String, String> {
        Ok(self.stdin.borrow().clone())
    }
    fn read_file(&self, p: &Path) -> Result<String, String> {
        fs::read_to_string(p).map_err(|e| e.to_string())
    }
}

#[derive(Default)]
struct Cap {
    out: RefCell<String>,
    err: RefCell<String>,
}

impl Emit for Cap {
    fn out(&self, s: &str) {
        self.out.borrow_mut().push_str(s);
    }
    fn err(&self, s: &str) {
        self.err.borrow_mut().push_str(s);
    }
}

struct T {
    _serial: crate::testutil::Serial,
    dir: testkit::TempDir,
    git: FGit,
    bd: FBd,
    lib: FLib,
    scripts: FScripts,
    forge: FForge,
    lc: FLc,
    config: FConfig,
    clock: FClock,
    env: FEnv,
    io: Cap,
}

const REPO: &str = "/repo";

impl T {
    fn new(mode: LandMode) -> T {
        let serial = crate::testutil::serial();
        let dir = tmpdir("ops");
        let s = Settings {
            home: dir.join("home"),
            run: dir.join("run"),
            queue_dir: dir.join("run/queue"),
            releases: Some(dir.join("releases")),
            forge: dir.join("home/forge.sh"),
            repo_map: None,
            batcher_bin: None,
            batcher_off: false,
            lc_bin: None,
            submitted_label: "spira-submitted".into(), // literal-ok: fixture vocabulary
            // The harness repository: not "spira", so the default fixture's landings are of an
            // ordinary queue.local repository and publish no release; the §8 D13 tests make
            // "spira" the harness (harness_world).
            home_repo: "harness".into(),
            db: "/db".into(),
            bd: "bd".into(),
            transition_pollsec: 5,
            transition_maxsec: 30,
            preflight_wall_secs: 240,
            verdict: VerdictSettings::default(),
            certify_suites: "on".into(), // literal-ok: fixture vocabulary
            git_name: "spira".into(),
            git_email: "spira@spira.invalid".into(),
            mailbox: "concierge".into(),
            express_label: "express".into(),
            round_wall_secs: 900,
        };
        fs::create_dir_all(s.queue_dir.join("spira")).unwrap();
        let landref = match mode {
            LandMode::QueueLocal => "local/main",
            _ => "origin/main",
        };
        let r = RepoCtx {
            name: "spira".into(),
            path: Some(PathBuf::from(REPO)),
            mode,
            landref: Some(landref.into()),
            map_land: mode.config_word().into(),
            map_base: landref.into(),
            publish: Some(("origin".into(), "main".into())),
            remotes: vec!["origin".into()],
        };
        let git = FGit::default();
        git.fetch_ok.set(true);
        let lib = FLib {
            s,
            r: RefCell::new(r),
            calls: RefCell::default(),
            toml: None,
            readback: RefCell::default(),
            diverged: Cell::new(false),
            cannot_check: Cell::new(false),
            push_ok: Cell::new(true),
            bug_id: RefCell::new(Some("sp-fix1".into())),
            amend_ok: Cell::new(true),
            pf_rc: Cell::new(0),
            conflict_with_base: RefCell::default(),
            repos: RefCell::new(Ok(vec!["spira".into()])),
            by_name: RefCell::default(),
            close_fail: Cell::new(false),
        };
        let scripts = FScripts::default();
        scripts.fence_ok.set(true);
        let forge = FForge::default();
        *forge.pr.borrow_mut() = Some("77".into());
        let lc = FLc::default();
        lc.available.set(true);
        T { _serial: serial, dir, git, bd: FBd::default(), lib, scripts, forge, lc, config: FConfig::default(), clock: FClock { now: Cell::new(1_000) }, env: FEnv::default(), io: Cap::default() }
    }

    fn run(&self, args: &[&str]) -> i32 {
        let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let cmd = match cli::parse(&argv) {
            Ok(c) => c,
            Err(cli::Usage(m)) => {
                self.io.err(&format!("{m}\n"));
                return 2;
            }
        };
        let w = World {
            git: &self.git,
            bd: &self.bd,
            lib: &self.lib,
            scripts: &self.scripts,
            forge: &self.forge,
            lc: &self.lc,
            config: &self.config,
            clock: &self.clock,
            env: &self.env,
            io: &self.io,
        };
        crate::dispatch(&w, &cmd)
    }

    fn s(&self) -> &Settings {
        &self.lib.s
    }
    fn qfile(&self, n: &str) -> PathBuf {
        self.s().queue_dir.join("spira").join(n)
    }
    fn out(&self) -> String {
        self.io.out.borrow().clone()
    }
    fn err(&self) -> String {
        self.io.err.borrow().clone()
    }
    /// A spira-lc bead row entered at `since`.
    fn lc_row(&self, id: &str, state: &str, tip: &str, since: u64) {
        let row = LcBeadRow { bead_id: id.into(), state: state.into(), tip: Some(tip.into()), since: Some(since) };
        self.lc.rows.borrow_mut().as_mut().unwrap().push(row);
    }
    fn open_record(&self, text: &str) {
        fs::write(self.qfile("open"), text).unwrap();
    }
    fn var(&self, k: &str, v: &str) {
        self.env.vars.borrow_mut().insert(k.into(), v.into());
    }
    fn landing_log(&self) -> String {
        fs::read_to_string(self.s().run.join("landing.log")).unwrap_or_default()
    }
    fn hold_lock(&self) -> crate::lock::Guard {
        match crate::lock::try_lock(&self.s().queue_dir, "spira") {
            crate::lock::Acquire::Held(g) => g,
            _ => panic!("lock"),
        }
    }
}


// ------------------------------------------------------------------------------ submit

#[test]
fn submit_queue_mode_refuses_a_branch_outside_spira() {
    let t = T::new(LandMode::QueueLocal);
    assert_eq!(t.run(&["submit", "feature/x"]), 1);
    assert!(t.err().contains("queue mode requires a branch under spira/"));
    assert!(t.scripts.calls.borrow().is_empty());
}

/// sp-lck63: a suite-state edit is a bead on spira/<id> like any other change; the old
/// bead-less `spira-suite-state/*` route, certified only in a queue record, is refused.
#[test]
fn submit_refuses_a_bead_less_suite_state_branch() {
    for mode in [LandMode::Queue, LandMode::QueueLocal] {
        let t = T::new(mode);
        t.git.set("refs/heads/spira-suite-state/test-a-20260101T000000Z", "t1");
        assert_eq!(t.run(&["submit", "spira-suite-state/test-a-20260101T000000Z"]), 1);
        assert!(t.err().contains("queue mode requires a branch under spira/"), "{}", t.err());
        assert!(t.scripts.calls.borrow().is_empty(), "no gate ran");
        assert!(!t.lc.has("certify"));
        assert!(!t.s().queue_dir.join("spira-suite-state").exists(), "no queue record");
    }
}

#[test]
fn submit_red_gate_logs_caught_and_propagates_the_gate_code() {
    let t = T::new(LandMode::Queue);
    t.git.set("refs/heads/spira/sp-a", "t1");
    t.scripts.gate_rc.set(75);
    assert_eq!(t.run(&["submit", "spira/sp-a"]), 75);
    assert!(t.err().contains("queue.sh submit: spira/sp-a failed the gate (NO_VERDICT)"));
    assert!(t.err().contains("gate says"));
    assert!(t.landing_log().contains("QUEUE CAUGHT 1000 branch=sp-a"));
    assert!(!t.lc.has("certify") && !t.lc.has("event"));
}


#[test]
fn submit_push_mode_rebases_pushes_and_closes() {
    let t = T::new(LandMode::Push);
    t.submitted(&["sp-a"]);
    t.git.set("refs/heads/spira/sp-a", "t1");
    assert_eq!(t.run(&["submit", "spira/sp-a"]), 0);
    assert!(t.lib.has("rebase spira/sp-a origin/main"));
    assert!(t.lib.has("push origin spira/sp-a:main"));
    assert!(t.lc.has("certify sp-a t1"));
    assert!(t.lib.has("bead_close sp-a t1"));
    assert!(t.out().contains("landed spira/sp-a (push)"));
}

#[test]
fn submit_pr_mode_certifies_and_names_the_mode() {
    let t = T::new(LandMode::Pr);
    t.git.set("refs/heads/topic", "t9");
    assert_eq!(t.run(&["submit", "topic"]), 0);
    assert!(t.out().contains("queue.sh submit: certified topic (pr)"));
}

#[test]
fn submit_missing_branch_is_refused() {
    let t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["submit", "spira/sp-zz"]), 1);
    assert!(t.err().contains("branch not found: spira/sp-zz"));
}

// --------------------------------------------------------------- protect/flush/step

#[test]
fn protect_writes_the_receipt_only_for_queue_forge() {
    let t = T::new(LandMode::QueueLocal);
    assert_eq!(t.run(&["protect"]), 1);
    assert!(t.err().contains("repo is not in queue mode (mode=queue.local)"));
    let t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["protect"]), 0);
    assert_eq!(fs::read_to_string(t.s().run.join("queue-protected-spira")).unwrap(), "main\n");
    assert_eq!(t.out().lines().count(), 5);
}

#[test]
fn flush_refuses_without_a_batcher_and_forces_wait_zero_with_one() {
    let mut t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["flush"]), 1);
    assert!(t.err().contains("no batcher program"));
    let bin = t.dir.join("batcher");
    testkit::write_exe(&bin, "#!/bin/sh\n");
    t.lib.s.batcher_bin = Some(bin);
    assert_eq!(t.run(&["flush"]), 0);
    let calls = t.scripts.calls.borrow().clone();
    assert!(calls.contains(&"cut spira wait0=true".to_string()));
}

#[test]
fn flush_with_the_batcher_switched_off_cuts_nothing_and_succeeds() {
    let mut t = T::new(LandMode::Queue);
    t.lib.s.batcher_off = true;
    assert_eq!(t.run(&["flush"]), 0);
    assert!(t.out().contains("SPIRA_BATCHER_ENABLE=0"), "{}", t.out());
    let calls = t.scripts.calls.borrow().clone();
    assert!(!calls.iter().any(|c| c.starts_with("cut ")), "{calls:?}");
}

#[test]
fn step_queue_local_publishes_with_stderr_folded_into_stdout() {
    let t = T::new(LandMode::QueueLocal);
    t.git.set("refs/remotes/origin/main", "f0");
    t.git.set("refs/heads/local/main", "f0");
    let rc = t.run(&["step", "spira"]);
    assert_eq!(rc, 0);
    // the verdict runs in process (nothing to settle) and batch.sh's old sweep is retired
    // (sp-uwhx0) — with no batcher configured, the cut refuses before calling anything, so
    // no Scripts call happens at all.
    assert!(t.scripts.calls.borrow().is_empty(), "{:?}", t.scripts.calls.borrow());
    // the missing batcher's refusal is on stderr (it is _batch_cut's own), the publish's on stdout
    assert!(t.err().contains("no batcher program"));
    assert!(t.out().contains("nothing to publish for spira"));
}

#[test]
fn step_on_queue_forge_is_status_zero_whatever_the_cut_did() {
    let t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["step", "spira"]), 0);
}

// ------------------------------------------------------------------------ step --all

impl T {
    /// Each id is an open bead carrying the submitted label (and its repo:/branch: labels):
    /// the shape a landed member's close acts on.
    fn submitted(&self, ids: &[&str]) {
        for id in ids {
            self.bd.rows.borrow_mut().push(BeadRow {
                id: (*id).into(),
                status: Some("open".into()),
                labels: vec!["spira-submitted".into(), "repo:spira".into(), format!("branch:spira/{id}")], // literal-ok: fixture vocabulary
                ..Default::default()
            });
        }
    }
}

impl T {
    /// Register a repository `name` in `mode` for step --all.
    fn repo(&self, name: &str, mode: LandMode) {
        let mut r = self.lib.r.borrow().clone();
        r.name = name.into();
        r.mode = mode;
        if mode == LandMode::QueueLocal {
            r.landref = Some("local/main".into());
            r.map_base = "local/main".into();
        }
        // A queue.forge repository gets an open batch named after it, so the in-process
        // verdict's check-status call shows which repositories a step reached.
        if mode == LandMode::Queue {
            let d = self.lib.s.queue_dir.join(name);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("open"), format!("pr={name}\nhead=h\nbase=b\nmembers=\nopened=1000\nbranch=spira/queue/{name}\n")).unwrap();
        }
        self.lib.by_name.borrow_mut().insert(name.into(), Ok(r));
    }
    /// The queue.forge repositories whose verdict ran (see `repo`).
    fn verdicts(&self) -> Vec<String> {
        self.forge.calls.borrow().iter().filter_map(|c| c.strip_prefix("check-status ")).map(|c| format!("verdict {}", c.split(' ').next().unwrap_or(""))).collect()
    }
}

#[test]
fn step_all_parses_and_refuses_a_repository_beside_it() {
    let p = |a: &[&str]| cli::parse(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert_eq!(p(&["step", "--all"]), Ok(Cmd::StepAll));
    assert_eq!(p(&["step", "spira"]), Ok(Cmd::Step { repo: "spira".into() }));
    assert_eq!(p(&["step", "--all", "spira"]), Err(cli::Usage("queue.sh step: --all takes no repository".into())));
    assert_eq!(p(&["step", "--bogus"]), Err(cli::Usage("queue.sh step: unknown option: --bogus".into())));
    assert_eq!(p(&["step"]), Err(cli::Usage("queue.sh step: repo required".into())));
    assert!(cli::USAGE.contains("queue.sh step --all"));
}

#[test]
fn step_all_steps_every_queue_mode_repo_in_order_and_skips_the_rest() {
    let t = T::new(LandMode::Queue);
    t.repo("spira", LandMode::QueueLocal);
    t.repo("svc", LandMode::Queue);
    t.repo("pushy", LandMode::Push);
    t.repo("held", LandMode::Hold);
    t.lib.by_name.borrow_mut().insert("broken".into(), Err("seam exited 1".into()));
    *t.lib.repos.borrow_mut() = Ok(vec!["spira".into(), "pushy".into(), "broken".into(), "svc".into(), "held".into()]);
    t.git.set("refs/remotes/origin/main", "f0");
    t.git.set("refs/heads/local/main", "f0");
    assert_eq!(t.run(&["step", "--all"]), 0);
    // svc's open batch was judged (pending); spira (queue.local) had no publish record
    assert_eq!(t.verdicts(), vec!["verdict svc"]);
    assert!(t.out().contains("verdict svc: PR svc pending (run age 0s)"), "{}", t.out());
    let out = t.out();
    assert!(out.contains("queue.sh step --all: spira\n") && out.contains("queue.sh step --all: svc\n"));
    assert!(!out.contains("step --all: pushy") && !out.contains("step --all: held"));
    // queue.local still publishes inside the step
    assert!(out.contains("nothing to publish for spira"));
    assert!(t.err().contains("cannot resolve the harness configuration: seam exited 1"));
    assert!(out.ends_with("queue.sh step --all: stepped=2 busy=0 unresolved=1 stepped:spira,svc unresolved:broken\n"), "{out}");
}

#[test]
fn step_all_does_not_propagate_one_repos_failed_step() {
    let t = T::new(LandMode::Queue);
    // queue.local with no local base: the publish inside the step refuses (status 1)
    t.repo("spira", LandMode::QueueLocal);
    t.repo("svc", LandMode::Queue);
    *t.lib.repos.borrow_mut() = Ok(vec!["spira".into(), "svc".into()]);
    // positive control in its own world (its own lock files): the single step does fail
    let c = T::new(LandMode::Queue);
    c.repo("spira", LandMode::QueueLocal);
    assert_ne!(c.run(&["step", "spira"]), 0);
    assert_eq!(t.run(&["step", "--all"]), 0);
    assert_eq!(t.verdicts(), vec!["verdict svc"]);
}

#[test]
fn step_all_without_a_repository_list_is_a_failure_not_an_empty_pass() {
    let t = T::new(LandMode::Queue);
    *t.lib.repos.borrow_mut() = Err("lib.sh repos seam exited 1 with 0 name(s)".into());
    assert_eq!(t.run(&["step", "--all"]), 1);
    assert!(t.err().contains("queue.sh step --all: cannot list repositories: lib.sh repos seam exited 1"));
    assert!(t.verdicts().is_empty());
}

#[test]
fn step_all_skips_a_repo_another_stepper_holds_and_steps_the_others() {
    let t = T::new(LandMode::Queue);
    t.repo("spira", LandMode::Queue);
    t.repo("svc", LandMode::Queue);
    *t.lib.repos.borrow_mut() = Ok(vec!["spira".into(), "svc".into()]);
    let held = crate::lock::try_step_lock(&t.lib.s.queue_dir, "svc");
    assert!(matches!(held, crate::lock::Acquire::Held(_)));
    assert_eq!(t.run(&["step", "--all"]), 0);
    assert_eq!(t.verdicts(), vec!["verdict spira"]);
    assert!(t.err().contains("queue.sh step: another step holds the step lock for svc — skipped"));
    assert!(t.out().ends_with("stepped=1 busy=1 unresolved=0 stepped:spira busy:svc\n"));
    drop(held);
    t.forge.calls.borrow_mut().clear();
    assert_eq!(t.run(&["step", "--all"]), 0);
    assert_eq!(t.verdicts(), vec!["verdict spira", "verdict svc"], "{}", t.err());
}

#[test]
fn a_single_step_under_a_held_step_lock_is_skipped_with_status_zero() {
    let t = T::new(LandMode::Queue);
    let _held = crate::lock::try_step_lock(&t.lib.s.queue_dir, "spira");
    assert_eq!(t.run(&["step", "spira"]), 0);
    assert!(t.verdicts().is_empty());
    assert!(t.err().contains("another step holds the step lock for spira — skipped"));
}



#[test]
fn step_all_probes_spira_lc_and_refuses_loudly_when_unreachable() {
    let t = T::new(LandMode::Queue);
    t.repo("spira", LandMode::Queue);
    *t.lib.repos.borrow_mut() = Ok(vec!["spira".into()]);
    assert_eq!(t.run(&["step", "--all"]), 0);
    assert!(t.lc.calls.borrow().contains(&"probe".to_string()));
    for args in [&["step", "--all"][..], &["step", "spira"][..], &["flush"][..]] {
        let t = T::new(LandMode::Queue);
        *t.lc.rows.borrow_mut() = Err("dolt down".into());
        t.repo("spira", LandMode::Queue);
        *t.lib.repos.borrow_mut() = Ok(vec!["spira".into()]);
        assert_eq!(t.run(args), 1, "{args:?}");
        assert!(t.err().contains("spira-lc is unreachable (dolt down) — refused, nothing changed"), "{}", t.err());
        assert!(t.scripts.calls.borrow().is_empty(), "{args:?} ran a child");
    }
}

#[test]
fn a_step_never_holds_the_queue_lock_verdict_and_batch_take() {
    let t = T::new(LandMode::Queue);
    t.repo("spira", LandMode::Queue);
    // the verdict and batch.sh take <repo>/lock themselves; the step must leave it free
    let _q = crate::lock::try_lock(&t.lib.s.queue_dir, "spira");
    assert!(matches!(_q, crate::lock::Acquire::Held(_)));
    assert_eq!(t.run(&["step", "spira"]), 0);
    // the verdict waited SPIRA_QUEUE_LOCK_WAIT for the lock, then skipped its turn
    assert!(t.out().contains("verdict spira: another queue operation holds the lock"), "{}", t.out());
    assert!(t.verdicts().is_empty());
    drop(_q);
    assert_eq!(t.run(&["step", "spira"]), 0);
    assert_eq!(t.verdicts(), vec!["verdict spira"]);
}

// ------------------------------------------------------------------------------- eject

fn open_batch_record(t: &T) {
    t.open_record("pr=12\nhead=h1\nbase=b0\nmembers=sp-a:ta sp-b:tb\nopened=5\nbranch=spira/queue/x\nowner=batcher\n");
}


#[test]
fn eject_member_records_the_harness_cause_and_returns_it_to_rework_on_spira_lc() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    assert_eq!(t.run(&["eject", "sp-a", "--reason", "needs rebase"]), 0);
    assert!(t.lib.has("cause_event sp-a eject"));
    let calls = t.lc.calls.borrow().clone();
    let d = calls.iter().position(|c| c == "event bead sp-a CERTIFIED 3 \"Deliver\"").unwrap_or_else(|| panic!("no Deliver: {calls:?}"));
    let r = calls.iter().position(|c| c == "event bead sp-a IN_DELIVERY 4 {\"Returned\":{\"reason\":\"batch-ejected\"}}").unwrap_or_else(|| panic!("no Returned: {calls:?}"));
    assert!(d < r);
    assert!(t.lib.has("release_claim sp-a"));
    assert!(!t.lib.has("bead_reopen"));
    assert!(t.forge.calls.borrow().contains(&"pr-close 12".to_string()));
    assert!(!t.qfile("open").exists());
    assert!(t.lib.has("notify spira sp-a ejected (queue.sh eject)"));
    assert!(t.out().contains("queue.sh eject: ejected sp-a from spira batch"));
}

#[test]
fn eject_red_records_eject_red() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    assert_eq!(t.run(&["eject", "sp-a", "--suites", "test-x.sh"]), 0);
    assert!(t.lib.has("cause_event sp-a eject-red"));
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    assert_eq!(t.run(&["eject", "sp-a", "--red"]), 0);
    assert!(t.lib.has("cause_event sp-a eject-red"));
}

#[test]
fn eject_of_a_certified_unbatched_bead_reopens_with_the_cause() {
    let t = T::new(LandMode::Queue);
    t.lc_row("sp-c", "CERTIFIED", "tc", 5);
    assert_eq!(t.run(&["eject", "sp-c", "--red", "--reason-file", "-"]), 0);
    assert!(t.lib.has("bead_reopen sp-c eject-red "));
    assert!(t.out().contains("ejected sp-c (certified, not yet batched) for spira (withdrawn)"));
}

#[test]
fn eject_takes_a_comma_separated_suites_list_and_still_refuses_a_bad_name_in_it() {
    let t = T::new(LandMode::Queue);
    t.lc_row("sp-c", "CERTIFIED", "tc", 5);
    assert_eq!(t.run(&["eject", "sp-c", "--suites", "test-x.sh,test-y.sh"]), 0);
    assert!(t.lib.has("bead_reopen sp-c eject-red test-x.sh,test-y.sh"));
    let t = T::new(LandMode::Queue);
    t.lc_row("sp-c", "CERTIFIED", "tc", 5);
    assert_ne!(t.run(&["eject", "sp-c", "--suites", "test-x.sh,bad name.sh"]), 0);
    assert!(!t.lib.has("bead_reopen"));
    let t = T::new(LandMode::Queue);
    t.lc_row("sp-c", "CERTIFIED", "tc", 5);
    assert_ne!(t.run(&["eject", "sp-c", "--suites", "test-x.sh,"]), 0, "an empty name in the list is refused");
}

#[test]
fn eject_of_a_stranger_names_the_members_and_changes_nothing() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    assert_eq!(t.run(&["eject", "sp-z"]), 1);
    assert!(t.err().contains("sp-z is not a member of the open batch for spira and is not CERTIFIED"));
    assert!(t.err().contains("batch members: sp-a sp-b"));
    assert!(t.qfile("open").exists());
    assert!(t.lib.calls.borrow().is_empty());
}

#[test]
fn eject_refuses_a_concierge_owned_batch_unless_overridden() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=12\nmembers=sp-a:ta\nowner=concierge\n");
    assert_eq!(t.run(&["eject", "sp-a"]), 1);
    assert!(t.err().contains("queue.sh eject: refused — this batch is claimed by concierge; override with SPIRA_QUEUE_OWNER_OVERRIDE=1"));
    t.var("SPIRA_QUEUE_OWNER_OVERRIDE", "1");
    assert_eq!(t.run(&["eject", "sp-a"]), 0);
}


#[test]
fn eject_with_a_batch_id_ejects_on_spira_lc_too() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=12\nmembers=sp-a:ta\nbatch_id=spira-1\nversion=3\n");
    assert_eq!(t.run(&["eject", "sp-a", "--reason", "multi\nline"]), 0);
    assert!(t.lc.calls.borrow().contains(&"eject-member spira-1 sp-a CI_RUNNING 4 multi line".to_string()));
}

#[test]
fn spira_lc_unreachable_refuses_loudly_and_changes_nothing() {
    for args in [&["eject", "sp-a"][..], &["abandon", "--reason", "x"][..], &["open-batch", "--skip-pregate"][..]] {
        let t = T::new(LandMode::Queue);
        *t.lc.rows.borrow_mut() = Err("Access denied".into());
        open_batch_record(&t);
        assert_eq!(t.run(args), 1, "{args:?}");
        assert!(t.err().contains("spira-lc is unreachable (Access denied) — refused, nothing changed"), "{}", t.err());
        assert!(t.lib.calls.borrow().is_empty(), "{args:?} changed something");
        assert!(t.qfile("open").exists());
    }
    // no binary at all is the same refusal
    let t = T::new(LandMode::Queue);
    t.lc.available.set(false);
    open_batch_record(&t);
    assert_eq!(t.run(&["eject", "sp-a"]), 1);
    assert!(t.err().contains("no spira-lc program"));
}


#[test]
fn eject_of_a_member_already_in_delivery_returns_without_a_second_deliver() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    t.lc.bead_rows.borrow_mut().insert("sp-a".into(), ("IN_DELIVERY".into(), "7".into()));
    assert_eq!(t.run(&["eject", "sp-a"]), 0);
    let calls = t.lc.calls.borrow().join("\n");
    assert!(!calls.contains("\"Deliver\""), "{calls}");
    assert!(calls.contains("event bead sp-a IN_DELIVERY 7 {\"Returned\":{\"reason\":\"batch-ejected\"}}"), "{calls}");
}

#[test]
fn eject_of_a_certified_unbatched_bead_delivers_then_returns_on_spira_lc() {
    let t = T::new(LandMode::Queue);
    t.lc_row("sp-c", "CERTIFIED", "tc", 1);
    assert_eq!(t.run(&["eject", "sp-c"]), 0, "{}", t.err());
    let calls = t.lc.calls.borrow().clone();
    assert!(calls.contains(&"event bead sp-c CERTIFIED 3 \"Deliver\"".to_string()), "{calls:?}");
    assert!(calls.contains(&"event bead sp-c IN_DELIVERY 4 {\"Returned\":{\"reason\":\"batch-ejected\"}}".to_string()), "{calls:?}");
}

#[test]
fn eject_refuses_while_another_queue_operation_holds_the_lock() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    let _g = t.hold_lock();
    assert_eq!(t.run(&["eject", "sp-a"]), 1);
    assert!(t.err().contains("another queue operation holds the lock for spira"));
}

#[test]
fn eject_refuses_an_id_that_is_not_an_identifier() {
    let t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["eject", "sp-a;rm"]), 1);
    assert!(t.err().contains("refused"));
}

#[test]
fn czar_fence_refusal_stops_eject_before_anything() {
    let t = T::new(LandMode::Queue);
    t.var("SPIRA_FAYTH", "czar");
    t.var("SPIRA_CZAR_CLASS", "deadlock");
    t.scripts.fence_ok.set(false);
    open_batch_record(&t);
    assert_eq!(t.run(&["eject", "sp-a"]), 1);
    assert_eq!(t.scripts.calls.borrow()[0], "czar deadlock");
    assert!(t.qfile("open").exists());
}

// ----------------------------------------------------------------------------- abandon

#[test]
fn abandon_requires_a_reason() {
    let t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["abandon"]), 2);
    assert!(t.err().contains("--reason is required"));
}


#[test]
fn abandon_with_lifecycle_on_abandons_the_batch_on_spira_lc() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=12\nmembers=sp-a:ta\nbatch_id=spira-1\nversion=3\n");
    assert_eq!(t.run(&["abandon", "--reason", "dirty"]), 0);
    assert!(t.lc.calls.borrow().contains(&"abandon-batch spira-1 CI_RUNNING 4 dirty".to_string()));
    assert!(t.out().contains("spira-1 abandoned on spira-lc"));
}

#[test]
fn abandon_dry_run_prints_the_audit_line_and_changes_nothing() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    assert_eq!(t.run(&["abandon", "--reason=why", "--dry-run"]), 0);
    assert!(t.out().contains("dry-run: audit line (landing.log): QUEUE ABANDON"));
    assert!(t.qfile("open").exists());
    assert!(t.landing_log().is_empty());
}

// ------------------------------------------------------------------------ claim/release

#[test]
fn claim_and_release_hand_the_batch_back() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    assert_eq!(t.run(&["claim", "--reason", "hand edit"]), 0);
    let rec = fs::read_to_string(t.qfile("open")).unwrap();
    assert!(rec.ends_with("owner=concierge\npre_claim_owner=batcher\nclaim_reason=hand edit\n"), "{rec}");
    assert_eq!(t.run(&["claim", "--reason", "again"]), 1);
    assert!(t.err().contains("already claimed by concierge — pass --force to reclaim"));
    assert_eq!(t.run(&["release"]), 0);
    assert!(t.out().contains("released back to batcher"));
    assert!(fs::read_to_string(t.qfile("open")).unwrap().ends_with("owner=batcher\n"));
    assert_eq!(t.run(&["release"]), 1);
    assert!(t.err().contains("is not claimed by concierge"));
}

#[test]
fn claim_without_reason_is_usage() {
    let t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["claim"]), 2);
}

// -------------------------------------------------------------------------- open-batch


#[test]
fn open_batch_with_lifecycle_on_cuts_on_spira_lc_and_records_the_batch_id() {
    let t = T::new(LandMode::Queue);
    t.git.set("origin/main", "b0");
    *t.git.branches.borrow_mut() = vec![("spira/sp-a".into(), "ta".into())];
    t.lc_row("sp-a", "CERTIFIED", "ta", 1);
    t.bd.rows.borrow_mut().push(BeadRow { id: "sp-a".into(), status: Some("closed".into()), ..Default::default() });
    assert_eq!(t.run(&["open-batch", "--skip-pregate"]), 0, "{}", t.err());
    let calls = t.lc.calls.borrow().clone();
    assert!(calls.contains(&"create-bead sp-a".to_string()));
    assert!(calls.contains(&"cut spira-20260929T010203Z sp-a:ta".to_string()));
    assert!(fs::read_to_string(t.qfile("open")).unwrap().ends_with("batch_id=spira-20260929T010203Z\nversion=1\n"));
}

#[test]
fn open_batch_refuses_while_a_batch_is_open() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=1\n");
    assert_eq!(t.run(&["open-batch"]), 1);
    assert!(t.err().contains("a batch is already open for spira (owner=batcher)"));
}

#[test]
fn open_batch_does_not_admit_when_the_lifecycle_row_cannot_be_read() {
    let t = T::new(LandMode::Queue);
    t.git.set("origin/main", "b0");
    *t.git.branches.borrow_mut() = vec![("spira/sp-a".into(), "ta".into())];
    t.lc_row("sp-a", "CERTIFIED", "ta", 1);
    t.lc.row_fails.set(true);
    assert_eq!(t.run(&["open-batch", "--members", "sp-a"]), 1);
    assert!(t.out().contains("sp-a: no lifecycle row (spira-lc could not say) — not admitted"), "{}", t.out());
    assert!(t.out().contains("no cut — nothing admissible for spira"));
}

#[test]
fn open_batch_red_pregate_deletes_the_branch() {
    let t = T::new(LandMode::Queue);
    t.git.set("origin/main", "b0");
    *t.git.branches.borrow_mut() = vec![("spira/sp-a".into(), "ta".into())];
    t.lc_row("sp-a", "CERTIFIED", "ta", 1);
    t.bd.rows.borrow_mut().push(BeadRow { id: "sp-a".into(), status: Some("closed".into()), ..Default::default() });
    t.lib.pf_rc.set(1);
    assert_eq!(t.run(&["open-batch", "--members", "sp-a"]), 1);
    assert!(t.err().contains("local pre-flight gate failed"));
    assert!(t.git.calls.borrow().iter().any(|c| c == "branch -D spira/queue/20260929T010203Z"));
    assert!(!t.qfile("open").exists());
}

// -------------------------------------------------------------------------- land-local

/// h1's tree, and a tree nothing certified (the pre-merge branch's, in the incident).
const T1: &str = "1111111111111111111111111111111111111111";
const T_BRANCH: &str = "2222222222222222222222222222222222222222";

fn local_repo(t: &T) {
    uncertified_local_repo(t);
    t.certify("spira", T1, gate::cert::Source::Round, "-");
}

/// local/main at b0, h1 fast-forwards from it with tree T1 — and no certificate anywhere.
fn uncertified_local_repo(t: &T) {
    t.git.set("refs/heads/local/main", "b0");
    t.git.ancestor("b0", "h1");
    for tip in ["ta", "tb", "tc"] {
        t.git.ancestor(tip, "h1");
    }
    t.git.set("refs/remotes/origin/main", "f0");
    t.git.trees.borrow_mut().insert("h1".into(), T1.into());
}

impl T {
    /// A tree certificate as the gate or batcher-cut writes it (gate::cert, §8 D12).
    fn certify(&self, repo: &str, tree: &str, source: gate::cert::Source, harness: &str) {
        let c = gate::cert::Cert {
            source,
            tree: tree.into(),
            repo: repo.into(),
            rev: "judged-rev".into(),
            branch: "spira/sp-a".into(),
            by: "test".into(),
            when: "2026-09-29T18:00:00Z".into(),
            at: 1,
            harness: harness.into(),
            suites: "-".into(),
        };
        gate::cert::write(&self.s().run.join("verdicts"), &c).unwrap();
    }
    fn landed_ref(&self) -> Option<String> {
        self.git.get("refs/heads/local/main")
    }
}

#[test]
fn land_local_refuses_a_tree_no_gate_or_round_certified() {
    let t = T::new(LandMode::QueueLocal);
    uncertified_local_repo(&t);
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]), 1);
    let e = t.err();
    assert!(e.contains(&format!("no round GREEN for h1's tree {T1} in spira")), "{e}");
    assert!(e.contains("SPIRA_LAND_UNGATED=<reason>") && e.contains("refused, nothing changed"), "{e}");
    assert_eq!(t.landed_ref().as_deref(), Some("b0"), "nothing moved");
    assert!(!t.lc.has("certify") && !t.lc.has("event"));
    assert!(!t.qfile("round-seq").exists());
    assert!(t.git.get("refs/archive/rounds/1").is_none());
}

#[test]
fn land_local_refuses_a_budgeted_gate_pass_for_the_head_tree() {
    let t = T::new(LandMode::QueueLocal);
    uncertified_local_repo(&t);
    t.certify("spira", T1, gate::cert::Source::Gate, "harness-now");
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]), 1);
    let e = t.err();
    assert!(e.contains("only a budgeted gate PASS exists") && e.contains("no round GREEN"), "{e}");
    assert_eq!(t.landed_ref().as_deref(), Some("b0"), "nothing moved");
    assert!(!t.lib.has("land_mark"));
}

#[test]
fn land_local_accepts_a_round_green_for_the_head_tree() {
    let t = T::new(LandMode::QueueLocal);
    uncertified_local_repo(&t);
    t.certify("spira", T1, gate::cert::Source::Round, "-");
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("h1"));
    assert!(t.err().contains(&format!("tree {T1} certified by round GREEN")), "{}", t.err());
}

#[test]
fn land_local_ignores_a_pass_for_another_tree_or_repo() {
    // The incident: the branch passed alone (its own tree); the merge that landed is h1.
    let t = T::new(LandMode::QueueLocal);
    uncertified_local_repo(&t);
    t.certify("spira", T_BRANCH, gate::cert::Source::Gate, "harness-now");
    t.certify("other", T1, gate::cert::Source::Gate, "harness-now");
    // A file at the right path that claims another tree is not a certificate for this one.
    let p = gate::cert::path(&t.s().run.join("verdicts"), "spira", T1).unwrap();
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    let forged = fs::read_to_string(gate::cert::path(&t.s().run.join("verdicts"), "spira", T_BRANCH).unwrap()).unwrap();
    fs::write(&p, forged).unwrap();
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]), 1);
    assert!(t.err().contains("no round GREEN"), "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
    // A red verdict is not a certificate either.
    fs::write(&p, fs::read_to_string(&p).unwrap().replace(T_BRANCH, T1).replace("verdict=PASS", "verdict=FAIL")).unwrap();
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]), 1);
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
}

#[test]
fn land_local_honours_spira_verdicts_as_the_gate_does() {
    let t = T::new(LandMode::QueueLocal);
    uncertified_local_repo(&t);
    let v = t.dir.join("elsewhere");
    t.var("SPIRA_VERDICTS", &v.display().to_string());
    let c = gate::cert::Cert { source: gate::cert::Source::Round, tree: T1.into(), repo: "spira".into(), rev: "r".into(), branch: "b".into(), by: "x".into(), when: "w".into(), at: 1, harness: "-".into(), suites: "-".into() };
    gate::cert::write(&v, &c).unwrap();
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]), 0, "{}", t.err());
}

#[test]
fn land_local_ungated_override_lands_and_logs_the_reason() {
    let t = T::new(LandMode::QueueLocal);
    uncertified_local_repo(&t);
    t.var("SPIRA_LAND_UNGATED", "");
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]), 1, "an empty reason is no override");
    t.var("SPIRA_LAND_UNGATED", "operator: gate host down\nsecond line");
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta,sp-b"]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("h1"));
    let reason = "operator: gate host down second line";
    assert!(t.err().contains(&format!("UNGATED LANDING of h1 (tree {T1}) onto local/main")), "{}", t.err());
    assert!(t.err().contains(&format!("SPIRA_LAND_UNGATED={reason}")), "{}", t.err());
    assert!(t.landing_log().contains(&format!("QUEUE UNGATED 1000 repo=spira head=h1 tree={T1} reason={reason}")), "{}", t.landing_log());
}


#[test]
fn land_local_delivers_each_member_at_its_version() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta,sp-b"]), 0, "{}", t.err());
    let calls = t.lc.calls.borrow().clone();
    let ev = |c: &str| calls.iter().position(|x| x == c).unwrap_or_else(|| panic!("missing {c}: {calls:?}"));
    let a = (
        ev("event bead sp-a CERTIFIED 3 \"Deliver\""),
        ev("event bead sp-a IN_DELIVERY 4 {\"Delivered\":{\"merge_sha\":\"ta\",\"proof\":\"ancestry\"}}"),
    );
    assert!(a.0 < a.1);
    ev("event bead sp-b CERTIFIED 3 \"Deliver\"");
    ev("event bead sp-b IN_DELIVERY 4 {\"Delivered\":{\"merge_sha\":\"h1\",\"proof\":\"ancestry\"}}");
}

#[test]
fn land_local_with_lifecycle_on_resumes_a_member_already_in_delivery() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    t.git.ancestor("ta", "h1");
    t.lc.bead_rows.borrow_mut().insert("sp-a".into(), ("IN_DELIVERY".into(), "7".into()));
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]), 0, "{}", t.err());
    let calls = t.lc.calls.borrow().join("\n");
    assert!(!calls.contains("\"Deliver\""), "{calls}");
    assert!(calls.contains("event bead sp-a IN_DELIVERY 7 {\"Delivered\""), "{calls}");
}

#[test]
fn land_local_with_lifecycle_on_does_not_deliver_a_tip_that_is_not_in_the_head() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:tz"]), 1);
    assert!(t.err().contains("sp-a: tip tz is not an ancestor of h1"), "{}", t.err());
    assert!(!t.lc.calls.borrow().iter().any(|c| c.starts_with("event bead")));
}

#[test]
fn land_local_with_spira_lc_unreachable_lands_nothing() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    t.lc.available.set(false);
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]), 1);
    assert!(t.err().contains("spira-lc is unreachable (no spira-lc program) — refused, nothing changed"), "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
    assert!(!t.lib.has("bead_close"));
}


#[test]
fn land_local_refuses_a_head_that_does_not_fast_forward() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    t.git.set("h9", "h9");
    assert_eq!(t.run(&["land-local", "--head", "h9", "--members", "sp-a:ta"]), 1);
    assert!(t.err().contains("does not fast-forward from local/main (b0) — refused, nothing changed"));
    assert_eq!(t.git.get("refs/heads/local/main").as_deref(), Some("b0"));
}

#[test]
fn land_local_refuses_a_remote_tracking_base_and_other_modes() {
    let t = T::new(LandMode::QueueLocal);
    t.lib.r.borrow_mut().landref = Some("origin/main".into());
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "a:b"]), 1);
    assert!(t.err().contains("resolves to a remote-tracking ref (origin/main) — not a queue.local base"));
    let t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "a:b"]), 1);
    assert!(t.err().contains("repo is not in queue.local mode (mode=queue)"));
}

fn release_in_force(t: &T) {
    let rel = t.s().releases.clone().unwrap();
    fs::create_dir_all(rel.join("r1")).unwrap();
    std::os::unix::fs::symlink(rel.join("r1"), rel.join("current")).unwrap();
}

/// A round worktree checked out at `tree`, its target/release holding the round's own
/// tested binaries — including `release` itself (§8 D14, sp-ktgll): a real round build of
/// the harness carries the release crate too, so land-local's own deploy must run that
/// binary, never production's.
fn round_worktree(t: &T, tree: &str) -> PathBuf {
    let wt = t.dir.join("round-wt");
    let bins = wt.join("target/release");
    fs::create_dir_all(&bins).unwrap();
    testkit::write_exe(bins.join("spira-config"), "#!/bin/sh\n");
    testkit::write_exe(bins.join("release"), "#!/bin/sh\n");
    t.git.refs.borrow_mut().insert("HEAD".into(), "wt-head".into());
    t.git.trees.borrow_mut().insert("wt-head".into(), tree.into());
    t.git.trees.borrow_mut().insert("h1".into(), T1.into());
    wt
}

#[test]
fn land_local_refuses_a_worktree_at_another_tree() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    let wt = round_worktree(&t, "OTHER");
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta", "--worktree", &wt.display().to_string()]), 1);
    assert!(t.err().contains("is not at h1's tree"));
    assert_eq!(t.git.get("refs/heads/local/main").as_deref(), Some("b0"));
}

#[test]
fn land_local_of_another_repository_publishes_no_release() {
    // The fixture's harness is "harness"; landing "spira" is some other queue.local repo.
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    release_in_force(&t);
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("h1"));
    assert!(t.scripts.release_calls.borrow().is_empty(), "no release step for a repository that is not the harness");
}
#[test]
fn land_local_skips_its_lock_only_when_the_caller_holds_it() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    let _g = t.hold_lock();
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]), 1);
    assert!(t.err().contains("another queue operation holds the lock"));
    t.var("SPIRA_QUEUE_LOCK_HELD", "1");
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]), 0);
}

#[test]
fn land_local_reads_members_from_stdin() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    *t.env.stdin.borrow_mut() = "sp-a:ta\nsp-b:tb\n".into();
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members-file", "-"]), 0);
    assert!(t.lc.has("event bead sp-b CERTIFIED 3 \"Deliver\""));
}


// ------------------------------------------ land-local: the landing publishes a release (§8 D13)

/// The landing repository IS the harness (`spira.home_repo`), with the round's worktree at
/// h1's tree. Returns the worktree path.
fn harness_world(t: &mut T) -> String {
    t.lib.s.home_repo = "spira".into();
    local_repo(t);
    round_worktree(t, T1).display().to_string()
}

fn land_harness(t: &T, wts: &str) -> i32 {
    t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta,sp-b", "--worktree", wts])
}

fn release_argv(t: &T) -> Vec<String> {
    t.scripts.release_calls.borrow().iter().map(|(_, a, _)| a.clone()).collect()
}

/// The `bin` each `release` call was actually run as (§8 D14, sp-ktgll).
fn release_bins(t: &T) -> Vec<String> {
    t.scripts.release_calls.borrow().iter().map(|(bin, _, _)| bin.clone()).collect()
}

#[test]
fn land_local_publishes_the_rounds_tested_build_as_a_release_and_activates_it() {
    let mut t = T::new(LandMode::QueueLocal);
    t.submitted(&["sp-a", "sp-b"]);
    let wts = harness_world(&mut t);
    release_in_force(&t);
    assert_eq!(land_harness(&t, &wts), 0, "{}", t.err());
    let rel = t.s().releases.clone().unwrap().display().to_string();
    let run = t.s().run.display().to_string();
    let common = format!("--releases {rel} --run {run}");
    assert_eq!(
        release_argv(&t),
        vec![
            format!("build h1 --repo {REPO} --bin-dir {wts}/target/release {common}"),
            format!("verify h1 {common}"),
            format!("activate h1 --repo {REPO} --landed-ref local/main --drain-wait {} {common}", crate::ops::deploy::LAND_DRAIN_WAIT_SECS),
        ]
    );
    assert!(t.scripts.release_calls.borrow().iter().all(|(_, _, db)| db == "/db"), "every release child gets SPIRA_DB");
    // §8 D14 (sp-ktgll): build, verify and activate all run the round's OWN release
    // binary, from the --bin-dir it is about to ship — never production's by name on PATH.
    let want_bin = format!("{wts}/target/release/release");
    assert_eq!(release_bins(&t), vec![want_bin.clone(), want_bin.clone(), want_bin], "the builder and the build must be the same commit");
    assert!(!t.err().contains("running PATH's release instead"), "{}", t.err());
    assert!(t.out().contains("queue.sh land-local: activated release h1"), "{}", t.out());
    assert_eq!(t.landed_ref().as_deref(), Some("h1"));
    // The landing is recorded before the (slow) release step starts.
    let calls = t.lib.calls.borrow().clone();
    assert!(t.lc.has("event bead sp-a CERTIFIED 3 \"Deliver\""));
    assert!(calls.iter().any(|c| c.starts_with("bead_close sp-b h1")));
    assert!(!t.err().contains("LAND DEPLOY FAILED"));
}

#[test]
fn land_drain_wait_is_explicit_and_below_the_round_cap() {
    assert!(crate::ops::deploy::LAND_DRAIN_WAIT_SECS > 0 && crate::ops::deploy::LAND_DRAIN_WAIT_SECS < 15 * 60);
}

#[test]
fn land_local_falls_back_to_paths_release_when_the_bin_dir_has_none_and_says_so() {
    // Before §8 D14, or whenever a round's own build happens not to carry a `release`
    // binary: land-local still runs (never a new refusal), but every step falls back to
    // bare `release` (the launcher's PATH), and that fallback is said aloud rather than
    // taken silently.
    let mut t = T::new(LandMode::QueueLocal);
    t.lib.s.home_repo = "spira".into();
    local_repo(&t);
    let wt = t.dir.join("round-wt-no-release");
    let bins = wt.join("target/release");
    fs::create_dir_all(&bins).unwrap();
    testkit::write_exe(bins.join("spira-config"), "#!/bin/sh\n"); // no `release` binary here
    t.git.refs.borrow_mut().insert("HEAD".into(), "wt-head".into());
    t.git.trees.borrow_mut().insert("wt-head".into(), T1.into());
    t.git.trees.borrow_mut().insert("h1".into(), T1.into());
    release_in_force(&t);
    assert_eq!(land_harness(&t, &wt.display().to_string()), 0, "{}", t.err());
    assert_eq!(release_bins(&t), vec!["release".to_string(), "release".to_string(), "release".to_string()]);
    let want = format!("no release binary at {}/release — running PATH's release instead, not this round's own build", bins.display());
    assert!(t.err().contains(&want), "{}", t.err());
}

#[test]
fn land_local_build_or_verify_failure_refuses_and_leaves_the_ref_where_it_was() {
    for (sub, calls) in [("build", 1), ("verify", 2)] {
        let mut t = T::new(LandMode::QueueLocal);
        t.submitted(&["sp-a", "sp-b"]);
        let wts = harness_world(&mut t);
        release_in_force(&t);
        t.scripts.release_rc.borrow_mut().insert(sub.into(), (1, format!("release: {sub} said no\n")));
        assert_eq!(land_harness(&t, &wts), 1, "{sub}");
        let e = t.err();
        assert!(e.contains(&format!("release h1 failed to build or verify: release {sub} exited 1: release: {sub} said no")), "{e}");
        assert!(e.contains("refused, local/main left at b0"), "{e}");
        assert_eq!(release_argv(&t).len(), calls, "{sub}");
        assert_eq!(t.landed_ref().as_deref(), Some("b0"), "{sub}: the ref never moved");
        assert!(!t.lib.has("bead_close sp-a h1") && t.git.get("refs/archive/rounds/1").is_none(), "{sub}");
    }
}

#[test]
fn land_local_activate_failure_is_a_deploy_fault_and_keeps_the_landing() {
    let mut t = T::new(LandMode::QueueLocal);
    t.submitted(&["sp-a", "sp-b"]);
    let wts = harness_world(&mut t);
    release_in_force(&t);
    t.scripts.release_rc.borrow_mut().insert("activate".into(), (1, "release: activate said no\n".into()));
    assert_eq!(land_harness(&t, &wts), crate::ops::DEPLOY_FAULT);
    let e = t.err();
    assert!(e.contains("LAND DEPLOY FAILED for h1: release activate exited 1: release: activate said no"), "{e}");
    assert!(e.contains("current is untouched (still ") && e.contains("/r1)"), "names what current still is: {e}");
    assert_eq!(release_argv(&t).len(), 3);
    assert_eq!(t.landed_ref().as_deref(), Some("h1"));
    assert!(t.lc.has("event bead sp-a CERTIFIED 3 \"Deliver\"") && t.lib.has("bead_close sp-a h1"));
}

#[test]
fn land_local_over_a_standing_hotfix_is_refused_by_release_activate_and_reported() {
    // The supersede rule lives in the release crate; land-local hands it the landing ref.
    let mut t = T::new(LandMode::QueueLocal);
    let wts = harness_world(&mut t);
    release_in_force(&t);
    t.scripts.release_rc.borrow_mut().insert(
        "activate".into(),
        (1, "release: a hotfix is running (abc: fix) and it is not in both h1 and local/main — land it or roll it back first\n".into()),
    );
    assert_eq!(land_harness(&t, &wts), crate::ops::DEPLOY_FAULT);
    assert!(release_argv(&t)[2].contains("--landed-ref local/main"));
    assert!(t.err().contains("LAND DEPLOY FAILED for h1: release activate exited 1: release: a hotfix is running"), "{}", t.err());
}

#[test]
fn land_local_answer_for_another_commit_is_a_fault() {
    let mut t = T::new(LandMode::QueueLocal);
    let wts = harness_world(&mut t);
    release_in_force(&t);
    *t.scripts.build_answers.borrow_mut() = Some("h0".into());
    assert_eq!(land_harness(&t, &wts), 1);
    assert!(t.err().contains("release build answered \"h0\" for h1 — not the landed commit"), "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
    assert_eq!(release_argv(&t).len(), 1);
}

#[test]
fn land_local_with_no_release_in_force_skips_the_release_step_and_needs_no_worktree() {
    // Before the cutover: the landing lands; nothing is built or activated, loudly.
    let mut t = T::new(LandMode::QueueLocal);
    harness_world(&mut t);
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("h1"));
    assert!(t.scripts.release_calls.borrow().is_empty());
    assert!(t.err().contains("release step skipped: no release is in force"), "{}", t.err());
    assert!(t.err().contains("production does not run h1 until a release is activated"), "{}", t.err());
}

#[test]
fn land_local_of_the_harness_requires_the_round_worktree_while_a_release_is_in_force() {
    let mut t = T::new(LandMode::QueueLocal);
    harness_world(&mut t);
    release_in_force(&t);
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]), 1);
    assert!(t.err().contains("--worktree <round worktree> is required to land spira"), "{}", t.err());
    assert!(t.err().contains("law-deploy-the-tested-artifacts") && t.err().contains("refused, nothing changed"));
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
    assert!(t.scripts.release_calls.borrow().is_empty());
}

// ----------------------------------------------------------------------------- publish

fn full_suite_passed(t: &T, sha: &str) {
    spira_config::local_pass::record(&t.lib.s.run, spira_config::local_pass::Kind::FullSuite, sha, "test", "0").unwrap();
}

fn publishable(t: &T) {
    full_suite_passed(t, "m3");
    t.git.set("refs/heads/local/main", "m3");
    t.git.set("refs/remotes/origin/main", "f0");
    t.git.ancestor("f0", "m3");
    for (tip, id) in [("t1", "sp-a"), ("t2", "sp-b")] {
        t.git.ancestor(tip, "m3");
        let _ = id;
    }
    *t.git.log.borrow_mut() = vec![
        RangeCommit { sha: "m3".into(), parents: vec!["m2".into(), "t2".into()], subject: "spira: land sp-b — B".into() },
        RangeCommit { sha: "r".into(), parents: vec!["m1".into()], subject: "round 9: fix".into() },
        RangeCommit { sha: "m1".into(), parents: vec!["f0".into(), "t1".into()], subject: "spira: land sp-a".into() },
    ];
}


#[test]
fn publish_prefers_the_lifecycle_row_tip_when_it_is_in_range() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    t.git.ancestor("tx", "m3");
    t.lc_row("sp-a", "LANDED", "tx", 5);
    assert_eq!(t.run(&["publish"]), 0);
    assert!(fs::read_to_string(t.qfile("publish")).unwrap().contains("members=sp-a:tx sp-b:t2"));
}

#[test]
fn publish_with_nothing_new_is_success() {
    let t = T::new(LandMode::QueueLocal);
    t.git.set("refs/heads/local/main", "f0");
    t.git.set("refs/remotes/origin/main", "f0");
    assert_eq!(t.run(&["publish"]), 0);
    assert!(t.out().contains("nothing to publish for spira (origin/main already at f0)"));
    assert!(!t.qfile("publish").exists());
}

#[test]
fn publish_refuses_a_divergence_an_open_publish_and_a_range_without_land_commits() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    t.lib.diverged.set(true);
    assert_eq!(t.run(&["publish"]), 1);
    assert!(t.err().contains("is not an ancestor of local/main — refusing to publish"));

    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    t.lib.cannot_check.set(true);
    assert_eq!(t.run(&["publish"]), 1);
    assert!(t.err().contains("cannot check whether origin/main is an ancestor of local/main"), "{}", t.err());
    assert!(t.err().contains("no queue dir"));
    assert!(!t.err().contains("something else moved the forge"));

    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    fs::write(t.qfile("publish"), "pr=9\n").unwrap();
    assert_eq!(t.run(&["publish"]), 1);
    assert!(t.err().contains("a publish PR is already open for spira (pr=9) — settle it first"));

    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    t.git.log.borrow_mut().retain(|c| !c.subject.starts_with("spira: land"));
    assert_eq!(t.run(&["publish"]), 1);
    assert!(t.err().contains("no land commit (spira: land <id>) in range — refusing"));
    assert!(!t.lib.has("push"));
}

#[test]
fn publish_waits_on_an_unmoved_red_head_then_republishes_once_local_main_moves() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    fs::write(t.qfile("publish-red"), "head=m3\nfix_forward=sp-fix\n").unwrap();
    assert_eq!(t.run(&["publish"]), 0, "{}", t.err());
    assert!(t.out().contains("waiting on fix-forward sp-fix for spira"));
    assert!(t.landing_log().contains("QUEUE PUBLISH_WAIT"));
    assert!(t.landing_log().contains("fix_forward=sp-fix"));
    assert!(!t.lib.has("push"));
    assert!(t.forge.calls.borrow().is_empty());
    assert!(!t.qfile("publish").exists());
    // the marker survives — the head still has not moved
    assert!(t.qfile("publish-red").exists());

    // local/main moves past the red head: the marker no longer matches and is cleared,
    // and this publish proceeds normally.
    full_suite_passed(&t, "m4");
    t.git.set("refs/heads/local/main", "m4");
    t.git.ancestor("m4", "m4");
    t.git.ancestor("f0", "m4");
    *t.git.log.borrow_mut() = vec![RangeCommit { sha: "m4".into(), parents: vec!["m3".into()], subject: "spira: land sp-c".into() }];
    assert_eq!(t.run(&["publish"]), 0, "{}", t.err());
    assert!(t.qfile("publish").exists());
    assert!(!t.qfile("publish-red").exists());
}

// -------------------------------------------------------------------------- transitions

fn forgeable(t: &mut T) {
    publishable(t);
    t.lib.toml = Some(t.dir.join("cfg"));
    t.config.rows.borrow_mut().insert("spira".into(), ("queue.local".into(), "local/main".into()));
    let map = t.dir.join("legacy-map");
    fs::write(&map, "x").unwrap();
    t.lib.s.repo_map = Some(map);
}

#[test]
fn to_forge_waits_for_the_final_publish_then_refuses_if_the_forge_did_not_catch_up() {
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    // pending, then green (the in-process settle fast-forwards and removes the record)...
    t.forge.status.borrow_mut().extend([Some("pending\n".into()), Some("green\n".into())]);
    // ...but the forge ref this fake fetches never moved to local/main's m3.
    assert_eq!(t.run(&["to-forge"]), 1);
    assert!(t.out().contains("running the final publish for spira"));
    assert!(t.out().contains("queue.sh publish: PR 77 opened"), "the final publish's output is on stdout");
    assert!(t.out().contains("waiting for publish PR 77 to settle green"));
    assert_eq!(t.forge.calls.borrow().iter().filter(|c| c.starts_with("check-status")).count(), 2);
    assert!(t.out().contains("verdict spira: publish PR 77 green — origin/main fast-forwarded to m3"), "{}", t.out());
    assert!(t.err().contains("origin/main (f0) and local/main (m3) differ — refused, nothing changed"));
    assert!(t.config.legacy.borrow().is_empty());
    assert_eq!(t.config.rows.borrow()["spira"].0, "queue.local");
}

#[test]
fn to_forge_happy_path_flips_archives_and_deletes_the_local_branch() {
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    // nothing to publish: forge already at local/main
    t.git.set("refs/remotes/origin/main", "m3");
    *t.lib.readback.borrow_mut() = ("queue".into(), "origin/main".into());
    assert_eq!(t.run(&["to-forge"]), 0, "{}", t.err());
    assert_eq!(t.config.rows.borrow()["spira"], ("queue.forge".to_string(), "origin/main".to_string()));
    assert_eq!(t.config.legacy.borrow()["spira"], ("queue.forge".to_string(), "origin/main".to_string()));
    assert_eq!(t.git.get("refs/archive/local/main").as_deref(), Some("m3"));
    assert!(t.git.get("refs/heads/local/main").is_none());
    assert!(t.out().contains("queue.sh to-forge: spira is now queue.forge (base=origin/main); local/main archived at refs/archive/local/main"));
}

#[test]
fn to_forge_refuses_on_a_red_final_publish() {
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    t.forge.status.borrow_mut().push(Some("red\nred-suite: test-a.sh\n".into()));
    assert_eq!(t.run(&["to-forge"]), 1);
    // red is refused, not settled: the record stays for the normal fix-forward recovery
    assert!(t.qfile("publish").exists());
    assert!(t.err().contains("the final publish (PR 77) is red — refused, nothing changed"));
    assert_eq!(t.config.rows.borrow()["spira"].0, "queue.local");
}

#[test]
fn to_forge_times_out_on_a_pending_publish() {
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    assert_eq!(t.run(&["to-forge"]), 1);
    assert!(t.err().contains("timed out waiting for publish PR 77 to settle — refused, nothing changed"));
}


#[test]
fn transitions_refuse_when_the_two_stores_already_disagree() {
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    t.config.rows.borrow_mut().insert("spira".into(), ("queue".into(), "origin/main".into()));
    assert_eq!(t.run(&["to-forge"]), 1);
    assert!(t.err().contains("already disagree about spira"));
    assert!(t.err().contains("queue.local|local/main"));
    assert!(t.err().contains("queue|origin/main"));
}

#[test]
fn to_local_derives_the_local_branch_and_restores_the_config_if_the_legacy_map_fails() {
    let mut t = T::new(LandMode::Queue);
    t.lib.toml = Some(t.dir.join("cfg"));
    t.config.rows.borrow_mut().insert("spira".into(), ("queue.forge".into(), "origin/main".into()));
    let map = t.dir.join("legacy-map");
    fs::write(&map, "x").unwrap();
    t.lib.s.repo_map = Some(map);
    t.git.set("refs/remotes/origin/main", "f5");
    t.config.legacy_fail.set(true);
    assert_eq!(t.run(&["to-local"]), 1);
    assert!(t.err().contains("legacy map write failed"));
    assert_eq!(t.config.rows.borrow()["spira"], ("queue.forge".to_string(), "origin/main".to_string()), "restored");
    assert!(t.git.get("refs/heads/local/main").is_none(), "the new branch is deleted again");

    t.config.legacy_fail.set(false);
    *t.lib.readback.borrow_mut() = ("queue.local".into(), "local/main".into());
    assert_eq!(t.run(&["to-local"]), 0, "{}", t.err());
    assert_eq!(t.git.get("refs/heads/local/main").as_deref(), Some("f5"));
    assert_eq!(t.config.rows.borrow()["spira"], ("queue.local".to_string(), "local/main".to_string()));
    assert!(t.out().contains("spira is now queue.local (base=local/main, synced to origin/main)"));
}

#[test]
fn to_local_refuses_an_archive_the_forge_does_not_descend_from() {
    let t = T::new(LandMode::Queue);
    t.git.set("refs/remotes/origin/main", "f5");
    t.git.set("refs/archive/local/main", "old");
    assert_eq!(t.run(&["to-local"]), 1);
    assert!(t.err().contains("is not an ancestor of origin/main (f5) — refs differ, refused"));
}

// ---------------------------------------------------------------------- rollback-local

#[test]
fn rollback_local_reactivates_the_previous_rounds_release_without_rebuilding() {
    let t = T::new(LandMode::QueueLocal);
    assert_eq!(t.run(&["rollback-local"]), 1);
    assert!(t.err().contains("round-seq is 0 — no landed round to roll back to"));
    fs::write(t.qfile("round-seq"), "2\n").unwrap();
    t.git.set("refs/archive/rounds/1", "p1");
    t.git.set("refs/heads/local/main", "h2");
    assert_eq!(t.run(&["rollback-local"]), 1);
    assert!(t.err().contains("no release is in force"), "{}", t.err());
    release_in_force(&t);
    assert_eq!(t.run(&["rollback-local"]), 1);
    assert!(t.err().contains("no release p1 in"), "{}", t.err());
    fs::create_dir_all(t.s().releases.clone().unwrap().join("p1")).unwrap();
    t.scripts.release_rc.borrow_mut().insert("verify".into(), (1, "release: p1 FAILS verify\n".into()));
    assert_eq!(t.run(&["rollback-local"]), 1);
    assert!(t.err().contains("could not re-activate release p1; nothing changed"), "{}", t.err());
    assert_eq!(t.git.get("refs/heads/local/main").as_deref(), Some("h2"));
    t.scripts.release_rc.borrow_mut().clear();
    t.scripts.release_calls.borrow_mut().clear();
    assert_eq!(t.run(&["rollback-local"]), 0, "{}", t.err());
    let argv = release_argv(&t);
    assert_eq!(argv.len(), 2);
    assert!(argv[0].starts_with("verify p1 ") && argv[1].starts_with(&format!("activate p1 --repo {REPO} --landed-ref local/main")), "{argv:?}");
    assert_eq!(t.git.get("refs/heads/local/main").as_deref(), Some("p1"));
    assert!(t.out().contains("queue.sh rollback-local: activated release p1, local/main reset to p1"));
}

// ------------------------------------------------------------------------------- misc

#[test]
fn stats_reads_the_landing_log() {
    let t = T::new(LandMode::Queue);
    fs::write(t.s().run.join("landing.log"), "QUEUE CAUGHT 1 branch=x\n").unwrap();
    assert_eq!(t.run(&["stats"]), 0);
    assert!(t.out().starts_with("caught:          1\n"));
}

#[test]
fn usage_errors_exit_two() {
    let t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["bogus"]), 2);
    assert_eq!(t.run(&["abandon", "--nope"]), 2);
    assert!(matches!(cli::parse(&["stats".to_string()]), Ok(Cmd::Stats)));
}

// ------------------------------------------------------------------- publish-settle

use crate::ops::publish::{ci_of, decide, Ci, Pr, Settle};

#[test]
fn settle_decision_table() {
    let open = |ci, behind| Some(Pr::Open { ci, behind });
    assert_eq!(decide(None), Settle::Open);
    assert_eq!(decide(open(Ci::Green, true)), Settle::Wait);
    assert_eq!(decide(open(Ci::Green, false)), Settle::Wait);
    assert_eq!(decide(open(Ci::Pending, true)), Settle::Wait);
    assert_eq!(decide(open(Ci::Red, true)), Settle::Supersede);
    assert_eq!(decide(open(Ci::Untested, true)), Settle::Supersede);
    assert_eq!(decide(open(Ci::Red, false)), Settle::Wait);
    assert_eq!(decide(Some(Pr::Gone)), Settle::Retire);
    assert_eq!(decide(Some(Pr::Unknown)), Settle::Wait);
    assert_eq!(ci_of("provision_fault\n"), Ci::Untested);
    assert_eq!(ci_of("harness_fault"), Ci::Untested);
    assert_eq!(ci_of("red\nred-suite: a"), Ci::Red);
    assert_eq!(ci_of(""), Ci::Pending);
}

fn stale_publish(t: &T, status: &str) {
    publishable(t);
    full_suite_passed(t, "m4");
    t.git.set("refs/heads/local/main", "m4");
    t.git.ancestor("f0", "m4");
    t.git.ancestor("t1", "m4");
    t.git.ancestor("t2", "m4");
    fs::write(
        t.qfile("publish"),
        "pr=77\nhead=m3\nbase=f0\nmembers=sp-a:t1\nopened=900\nbranch=spira/publish/OLD\nremote=origin\nforge_branch=main\n",
    )
    .unwrap();
    t.forge.status.borrow_mut().push(Some(status.into()));
    *t.forge.pr.borrow_mut() = Some("78".into());
}

#[test]
fn settle_supersedes_a_red_or_untested_publish_that_local_has_moved_past() {
    for status in ["red\n", "provision_fault\n"] {
        let t = T::new(LandMode::QueueLocal);
        stale_publish(&t, status);
        assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
        let calls = t.forge.calls.borrow().clone();
        assert!(calls.contains(&"pr-close 77".to_string()), "{calls:?}");
        assert!(calls.iter().any(|c| c.starts_with("pr-create spira/publish/20260929T010203Z main")), "{calls:?}");
        assert!(fs::read_to_string(t.qfile("publish")).unwrap().starts_with("pr=78\nhead=m4\n"));
    }
}

#[test]
fn settle_waits_on_a_green_or_pending_publish_and_on_an_unmoved_red_one() {
    for (status, moved) in [("green\n", true), ("pending\n", true), ("red\n", false)] {
        let t = T::new(LandMode::QueueLocal);
        stale_publish(&t, status);
        if !moved {
            t.git.set("refs/heads/local/main", "m3");
        }
        assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
        let calls = t.forge.calls.borrow().clone();
        assert!(!calls.iter().any(|c| c.starts_with("pr-close") || c.starts_with("pr-create")), "{calls:?}");
        assert!(fs::read_to_string(t.qfile("publish")).unwrap().starts_with("pr=77\n"));
    }
}

#[test]
fn settle_opens_a_publish_when_none_is_open_and_skips_other_modes() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    *t.forge.pr.borrow_mut() = Some("78".into());
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert!(fs::read_to_string(t.qfile("publish")).unwrap().starts_with("pr=78\nhead=m3\n"));
    let t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["publish-settle"]), 0);
    assert!(t.forge.calls.borrow().is_empty());
}

#[test]
fn settle_exits_zero_and_logs_when_the_queue_lock_is_held() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    let _q = crate::lock::try_lock(&t.lib.s.queue_dir, "spira");
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert!(t.out().contains("publish-settle spira: another queue operation holds the lock"), "{}", t.out());
    assert!(t.forge.calls.borrow().is_empty());
}

#[test]
fn submit_green_in_a_queue_mode_certifies_on_spira_lc_and_keeps_no_record_of_its_own() {
    let mut t = T::new(LandMode::Queue);
    t.git.set("refs/heads/spira/sp-a", "t1");
    t.lib.s.certify_suites = "off".into();
    assert_eq!(t.run(&["submit", "spira/sp-a"]), 0);
    assert_eq!(t.scripts.calls.borrow()[0], "gate spira/sp-a spira bead=sp-a suites=off");
    assert!(t.lc.has("certify sp-a t1"));
    assert!(!t.s().queue_dir.join("sp-a").exists(), "the lifecycle row is the only certification record");
    assert!(t.landing_log().contains("QUEUE GATE_COST 1000 branch=sp-a seconds=0"));
    assert!(t.out().contains("queue.sh submit: certified spira/sp-a"));
}

#[test]
fn submit_does_not_certify_when_spira_lc_refuses() {
    let t = T::new(LandMode::Queue);
    t.git.set("refs/heads/spira/sp-a", "t1");
    t.lc.certify_refused.set(true);
    assert_eq!(t.run(&["submit", "spira/sp-a"]), 1);
    assert!(t.err().contains("spira-lc certify refused for sp-a (rc=3)"));
    assert!(!t.s().queue_dir.join("sp-a").exists());
}

#[test]
fn eject_dry_run_writes_nothing() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    t.bd.rows.borrow_mut().push(BeadRow { id: "sp-a".into(), ..Default::default() });
    assert_eq!(t.run(&["eject", "sp-a", "--dry-run"]), 0);
    assert!(t.out().contains("dry-run: would record cause eject"));
    assert!(t.out().contains("dry-run: would return bead sp-a to spira-lc via a Returned event"));
    assert!(!t.lc.has("event") && !t.lc.has("eject-member"));
    assert!(t.qfile("open").exists());
    assert!(t.lib.calls.borrow().is_empty());
}

#[test]
fn abandon_keeps_reworked_members_archives_and_audits() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    t.lc_row("sp-a", "REWORK", "ta", 5);
    t.var("SPIRA_QUEUE_ACTOR", "op");
    assert_eq!(t.run(&["abandon", "--reason", "dirty\nPR"]), 0);
    assert!(t.out().contains("queue.sh abandon: sp-a: left as REWORK"));
    assert!(t.out().contains("queue.sh abandon: sp-b: returned to CERTIFIED"));
    assert!(t.lib.has("cancel_runs spira/queue/x"));
    assert!(t.forge.calls.borrow().iter().any(|c| c == "pr-comment 12 Batch abandoned. Reason: dirty PR"));
    let archive = t.qfile("closed-pr12-20260929T010203Z");
    let a = fs::read_to_string(&archive).unwrap();
    assert!(a.ends_with("reason=dirty PR\nactor=op\n"), "{a}");
    assert!(!t.qfile("open").exists());
    assert!(t.landing_log().contains("QUEUE ABANDON 1000 repo=spira pr=12 actor=op members=sp-a:REWORK,sp-b:CERTIFIED reason=dirty PR"));
    assert!(t.lib.has("event queue.abandoned abandoned PR 12 for spira (actor=op)"));
}

/// sp-mve9i: admission reads the member's lifecycle row, never bd's status (design §3.4 —
/// bd status is inert for work beads). A CERTIFIED bead whose bd row still reads in_progress
/// (the work verbs never move it) is admitted; a bd-closed bead whose row has gone back to
/// REWORK is not.
#[test]
fn open_batch_admits_on_the_lifecycle_row_not_bd_status() {
    let t = T::new(LandMode::Queue);
    t.git.set("origin/main", "b0");
    *t.git.branches.borrow_mut() = vec![("spira/sp-a".into(), "ta".into()), ("spira/sp-r".into(), "tr".into())];
    t.lc_row("sp-a", "CERTIFIED", "ta", 1);
    t.lc_row("sp-r", "REWORK", "tr", 2);
    t.lc_row("sp-r", "CERTIFIED", "tr", 2);
    t.bd.rows.borrow_mut().extend([
        BeadRow { id: "sp-a".into(), status: Some("in_progress".into()), title: Some("A".into()), ..Default::default() },
        BeadRow { id: "sp-r".into(), status: Some("closed".into()), title: Some("R".into()), ..Default::default() },
    ]);
    assert_eq!(t.run(&["open-batch", "--skip-pregate"]), 0, "{}", t.err());
    let out = t.out();
    assert!(out.contains("skip — sp-r: lifecycle state=REWORK (no longer CERTIFIED) — not admitted"), "{out}");
    let rec = fs::read_to_string(t.qfile("open")).unwrap();
    assert!(rec.contains("members=sp-a:ta\n"), "{rec}");
}

#[test]
fn open_batch_assembles_admits_opens_and_cuts_on_spira_lc() {
    let t = T::new(LandMode::Queue);
    t.git.set("origin/main", "b0");
    *t.git.branches.borrow_mut() = vec![("spira/sp-a".into(), "ta".into()), ("spira/sp-b".into(), "tb".into()), ("spira/sp-c".into(), "tc".into())];
    t.lc_row("sp-a", "CERTIFIED", "ta", 1);
    t.lc_row("sp-b", "CERTIFIED", "tb", 2);
    // sp-c was CERTIFIED when the candidates were read and is REWORK by admission (the
    // fake's fresh `show` finds the first row).
    t.lc_row("sp-c", "REWORK", "tc", 3);
    t.lc_row("sp-c", "CERTIFIED", "tc", 3);
    t.bd.rows.borrow_mut().extend([
        BeadRow { id: "sp-a".into(), status: Some("closed".into()), title: Some("A".into()), ..Default::default() },
        BeadRow { id: "sp-b".into(), status: Some("open".into()), labels: vec!["spira-submitted".into()], ..Default::default() }, // literal-ok: fixture vocabulary
        BeadRow { id: "sp-c".into(), status: Some("open".into()), ..Default::default() },
    ]);
    t.lib.conflict_with_base.borrow_mut().insert("tb".into());
    t.git.merge_fail.borrow_mut().insert("tb".into());
    assert_eq!(t.run(&["open-batch", "--skip-pregate"]), 0, "{}", t.err());
    let out = t.out();
    assert!(out.contains("skip — sp-c: lifecycle state=REWORK (no longer CERTIFIED) — not admitted"), "{out}");
    assert!(out.contains("skip — sp-b: conflicts with base"));
    assert!(out.contains("PR 77 opened — 1 branches (spira/queue/20260929T010203Z)"));
    let rec = fs::read_to_string(t.qfile("open")).unwrap();
    assert!(rec.starts_with("pr=77\nhead=merged-ta\nbase=b0\nmembers=sp-a:ta\nopened=1000\nbranch=spira/queue/20260929T010203Z\nowner=operator\n"), "{rec}");
    assert!(rec.contains("batch_id=spira-20260929T010203Z\nversion=1\n"), "{rec}");
    assert!(t.lc.has("create-bead sp-a") && t.lc.has("cut spira-20260929T010203Z sp-a:ta"));
    assert!(t.forge.calls.borrow()[0].contains("- sp-a — A"));
    assert!(t.landing_log().contains("members=1 gate_seconds=0 verdict=green source=open-batch"));
}

#[test]
fn land_local_lands_and_archives_and_delivers_on_spira_lc() {
    let t = T::new(LandMode::QueueLocal);
    t.submitted(&["sp-a", "sp-b"]);
    local_repo(&t);
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta,sp-b"]), 0, "{}", t.err());
    assert_eq!(t.git.get("refs/heads/local/main").as_deref(), Some("h1"));
    assert_eq!(t.git.get("refs/archive/rounds/1").as_deref(), Some("h1"));
    assert_eq!(fs::read_to_string(t.qfile("round-seq")).unwrap(), "1\n");
    assert!(t.lib.has("divergence f0 b0"), "the cached forge ref is checked, never fetched");
    assert!(!t.git.calls.borrow().iter().any(|c| c.starts_with("fetch")));
    assert!(t.lc.has("event bead sp-a CERTIFIED 3 \"Deliver\""));
    assert!(t.lib.has("bead_close sp-b h1"));
    assert!(t.out().contains("queue.sh land-local: local/main fast-forwarded to h1 (round 1, archived at refs/archive/rounds/1)"));
}

#[test]
fn publish_members_come_from_land_commits_even_when_lifecycle_rows_are_missing() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    t.bd.rows.borrow_mut().push(BeadRow { id: "sp-b".into(), title: Some("B".into()), ..Default::default() });
    assert_eq!(t.run(&["publish"]), 0, "{}", t.err());
    assert!(t.git.calls.borrow().contains(&"log f0..m3".to_string()));
    let rec = fs::read_to_string(t.qfile("publish")).unwrap();
    assert_eq!(
        rec,
        "pr=77\nhead=m3\nbase=f0\nmembers=sp-a:t1 sp-b:t2\nopened=1000\nbranch=spira/publish/20260929T010203Z\nremote=origin\nforge_branch=main\n"
    );
    assert!(t.lib.has("push origin m3:refs/heads/spira/publish/20260929T010203Z"));
    let pr = t.forge.calls.borrow()[0].clone();
    assert!(pr.starts_with("pr-create spira/publish/20260929T010203Z main publish: 2 bead(s) for spira"));
    assert!(pr.contains("- sp-a — (title unavailable)") && pr.contains("- sp-b — B"));
    assert!(t.landing_log().contains("QUEUE PUBLISH 1000 repo=spira pr=77 members=2"));
    assert!(t.out().contains("queue.sh publish: PR 77 opened — 2 bead(s) since last publish"));
}

#[test]
fn transitions_refuse_while_work_is_in_delivery() {
    // an open batch record
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    t.open_record("pr=5\n");
    assert_eq!(t.run(&["to-forge"]), 1);
    assert!(t.err().contains("has work in delivery (open batch PR 5) — refused, nothing changed"));
    assert!(!t.lib.has("push"), "no final publish");

    // spira-lc says IN_DELIVERY
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    t.lc_row("sp-r", "IN_DELIVERY", "t2", 1);
    assert_eq!(t.run(&["to-forge"]), 1);
    assert!(t.err().contains("(sp-r)"));

    // spira-lc unreachable: loud refusal
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    *t.lc.rows.borrow_mut() = Err("dolt down".into());
    assert_eq!(t.run(&["to-forge"]), 1);
    assert!(t.err().contains("spira-lc is unreachable (dolt down)"));

    // another repository's IN_DELIVERY bead does not block
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    t.git.set("refs/remotes/origin/main", "m3");
    *t.lib.readback.borrow_mut() = ("queue".into(), "origin/main".into());
    t.lc_row("xx-1", "IN_DELIVERY", "elsewhere", 1);
    assert_eq!(t.run(&["to-forge"]), 0, "{}", t.err());
}

// ------------------------------------------------------------------ close on land (sp-du6dl)
// The queue closes a landed member itself, in-process: no landing-pass oracle and no second
// ledger — LANDED lives on spira-lc, written by the queue's own delivery.

#[test]
fn a_landed_submitted_member_is_closed_citing_the_sha_and_its_branch_reaped() {
    let t = T::new(LandMode::Push);
    t.submitted(&["sp-a"]);
    t.git.set("refs/heads/spira/sp-a", "t1");
    assert_eq!(t.run(&["submit", "spira/sp-a"]), 0, "{}", t.err());
    assert!(t.lib.has("bead_close sp-a t1"));
    assert!(t.lib.has("reap sp-a spira spira/sp-a landed at t1"));
    assert!(t.out().contains("land-close sp-a: closed at t1 (submitted -> landed)"));
    assert!(t.out().contains("land-close sp-a: reaped branch spira/sp-a"));
}

#[test]
fn close_on_land_leaves_a_never_submitted_bead_alone_whatever_bd_status_says() {
    for row in [
        BeadRow { id: "sp-a".into(), status: Some("closed".into()), labels: vec!["repo:spira".into()], ..Default::default() }, // literal-ok: fixture vocabulary
        BeadRow { id: "sp-a".into(), status: Some("open".into()), labels: vec!["repo:spira".into()], ..Default::default() },
    ] {
        let t = T::new(LandMode::Push);
        t.bd.rows.borrow_mut().push(row);
        t.git.set("refs/heads/spira/sp-a", "t1");
        assert_eq!(t.run(&["submit", "spira/sp-a"]), 0, "{}", t.err());
        assert!(!t.lib.has("bead_close"), "{:?}", t.lib.calls.borrow());
        assert!(!t.lib.has("reap"));
    }
}

#[test]
fn a_refused_close_is_left_submitted_and_reaps_nothing() {
    let t = T::new(LandMode::Push);
    t.submitted(&["sp-a"]);
    t.lib.close_fail.set(true);
    t.git.set("refs/heads/spira/sp-a", "t1");
    assert_eq!(t.run(&["submit", "spira/sp-a"]), 0, "{}", t.err());
    assert!(t.lib.has("bead_close sp-a t1"));
    assert!(!t.lib.has("reap"));
    assert!(t.out().contains("land-close sp-a: bd close failed — left submitted (LANDED is on the lifecycle record)"));
}

#[test]
fn the_land_close_reason_cites_the_sha_or_unknown() {
    assert!(crate::ops::helpers::land_close_reason("abc").contains("work landed at abc (law-closed-is-not-landed)"));
    assert!(crate::ops::helpers::land_close_reason("").contains("work landed at unknown"));
}

#[test]
fn publish_refuses_a_head_with_no_full_suite_local_pass_naming_the_record_and_command() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    fs::remove_file(t.lib.s.run.join("local-pass/full-suite/m3")).unwrap();
    *t.forge.pr.borrow_mut() = Some("78".into());
    for cmd in ["publish", "publish-settle"] {
        assert_eq!(t.run(&[cmd]), 1, "{cmd}: {}", t.out());
        assert!(t.err().contains("full-suite local-pass record for m3") && t.err().contains("testenv --suites"), "{}", t.err());
    }
    assert!(t.forge.calls.borrow().is_empty());
    assert!(!t.qfile("publish").exists());
}

#[test]
fn publish_goes_ahead_on_a_named_override_and_logs_it() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    fs::remove_file(t.lib.s.run.join("local-pass/full-suite/m3")).unwrap();
    *t.forge.pr.borrow_mut() = Some("78".into());
    t.env.vars.borrow_mut().insert(spira_config::local_pass::OVERRIDE_ENV.into(), "local round impossible, box is down".into());
    assert_eq!(t.run(&["publish"]), 0, "{}", t.err());
    assert!(t.qfile("publish").exists());
    let log = fs::read_to_string(t.lib.s.run.join("local-pass/overrides.log")).unwrap();
    assert!(log.contains("sha=m3") && log.contains("box is down"), "{log}");
}
