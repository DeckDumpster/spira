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

mod verdict;

// ------------------------------------------------------------------------------- fakes

#[derive(Default)]
struct FGit {
    refs: RefCell<BTreeMap<String, String>>,
    anc: RefCell<BTreeSet<(String, String)>>,
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
    fn merge_no_ff(&self, wt: &Path, msg: &str, tip: &str) -> bool {
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
    pf_rc: Cell<i32>,
    conflict_with_base: RefCell<BTreeSet<String>>,
    /// R22's answer (step --all).
    repos: RefCell<Result<Vec<String>, String>>,
    /// Per-name contexts; a name not here resolves to `r`.
    by_name: RefCell<BTreeMap<String, Result<RepoCtx, String>>>,
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
    fn land_mark(&self, id: &str, st: &str, tip: &str, reason: &str) {
        self.log(format!("land_mark {id} {st} {tip} {reason}"));
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
    fn bead_close_on_land(&self, id: &str, sha: &str) {
        self.log(format!("close_on_land {id} {sha}"));
    }
    fn gh_issue_closeout(&self, id: &str, sha: &str, _: &Path) {
        self.log(format!("gh_closeout {id} {sha}"));
    }
    fn comment(&self, id: &str, text: &str) {
        self.log(format!("comment {id} {text}"));
    }
    fn notify(&self, repo: &str, subject: &str, _body: &str) {
        self.log(format!("notify {repo} {subject}"));
    }
    fn event(&self, kind: &str, title: &str, detail: &str) {
        self.log(format!("event {kind} {title} {detail}"));
    }
    fn divergence(&self, _: &Path, _: &str, _: &Path, forge: &str, local: &str) -> Divergence {
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
    fn sort_rows(&self, _: &Path, _: &str, prio: &str, rows: &str) -> Vec<(String, String)> {
        self.log(format!("sort_rows {prio}"));
        rows.lines().filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.to_string(), it.next()?.to_string()))
        }).collect()
    }
    fn cancel_runs(&self, _: &Path, _: &Path, br: &str) {
        self.log(format!("cancel_runs {br}"));
    }
    fn lc_returned(&self, id: &str, reason: &str) {
        self.log(format!("lc_returned {id} {reason}"));
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
    /// The lc_off each step child was run with.
    lc_off: RefCell<Vec<bool>>,
    /// What `batcher judgement-ci` answers.
    judgement: RefCell<RunOut>,
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
    fn batcher_cut(&self, _: &Path, repo: &str, wz: bool, lc_off: bool) -> i32 {
        self.calls.borrow_mut().push(format!("cut {repo} wait0={wz}"));
        self.lc_off.borrow_mut().push(lc_off);
        0
    }
    fn czar_fence(&self, class: &str) -> bool {
        self.calls.borrow_mut().push(format!("czar {class}"));
        self.fence_ok.get()
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
    in_delivery: RefCell<Result<Vec<LcBeadRow>, String>>,
    calls: RefCell<Vec<String>>,
}

impl Default for FLc {
    fn default() -> Self {
        FLc { available: Cell::new(false), in_delivery: RefCell::new(Ok(Vec::new())), calls: RefCell::default() }
    }
}

impl Lc for FLc {
    fn available(&self) -> bool {
        self.calls.borrow_mut().push("available".into());
        self.available.get()
    }
    fn probe(&self) -> Result<(), String> {
        self.calls.borrow_mut().push("probe".into());
        self.in_delivery.borrow().clone().map(|_| ())
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
    fn land_batch(&self, id: &str, v: &str, _: &str, sha: &str) -> Result<(), (i32, String)> {
        self.calls.borrow_mut().push(format!("land {id} {v} {sha}"));
        Ok(())
    }
    fn in_delivery(&self) -> Result<Vec<LcBeadRow>, String> {
        self.calls.borrow_mut().push("list IN_DELIVERY".into());
        self.in_delivery.borrow().clone()
    }
}

#[derive(Default)]
struct FConfig {
    rows: RefCell<BTreeMap<String, (String, String)>>,
    legacy: RefCell<BTreeMap<String, (String, String)>>,
    legacy_fail: Cell<bool>,
    lifecycle: Cell<bool>,
}

impl ConfigStore for FConfig {
    fn repo_row(&self, _: &Path, name: &str) -> Result<(String, String), String> {
        Ok(self.rows.borrow().get(name).cloned().unwrap_or_default())
    }
    fn set_repo_row(&self, _: &Path, name: &str, m: &str, b: &str) -> Result<(), String> {
        self.rows.borrow_mut().insert(name.into(), (m.into(), b.into()));
        Ok(())
    }
    fn lifecycle_enforce(&self, _: Option<&Path>) -> bool {
        self.lifecycle.get()
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
            landstate: dir.join("run/landstate"),
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
        };
        fs::create_dir_all(&s.landstate).unwrap();
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
            pf_rc: Cell::new(0),
            conflict_with_base: RefCell::default(),
            repos: RefCell::new(Ok(vec!["spira".into()])),
            by_name: RefCell::default(),
        };
        let scripts = FScripts::default();
        scripts.fence_ok.set(true);
        let forge = FForge::default();
        *forge.pr.borrow_mut() = Some("77".into());
        let lc = FLc::default();
        *lc.in_delivery.borrow_mut() = Ok(Vec::new());
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
    fn landstate(&self, id: &str, line: &str) {
        fs::write(self.s().landstate.join(id), line).unwrap();
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
    /// lifecycle_enforce ON with a reachable spira-lc.
    fn lifecycle_on(&self) {
        self.config.lifecycle.set(true);
        self.lc.available.set(true);
    }
    /// The OFF contract: spira-lc was never invoked, not even probed.
    fn assert_lc_untouched(&self) {
        assert!(self.lc.calls.borrow().is_empty(), "spira-lc invoked with lifecycle_enforce off: {:?}", self.lc.calls.borrow());
        assert!(!self.lib.has("lc_returned"), "lc_returned with lifecycle_enforce off");
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
    assert!(t.err().contains("queue mode requires a branch under spira/ or spira-suite-state/"));
    assert!(t.scripts.calls.borrow().is_empty());
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
    assert!(!t.lib.has("land_mark"));
}

#[test]
fn submit_green_in_a_queue_mode_certifies_and_writes_the_entry() {
    let t = T::new(LandMode::Queue);
    t.git.set("refs/heads/spira/sp-a", "t1");
    t.var("SPIRA_CERTIFY_SUITES", "off");
    assert_eq!(t.run(&["submit", "spira/sp-a"]), 0);
    assert_eq!(t.scripts.calls.borrow()[0], "gate spira/sp-a spira bead=sp-a suites=off");
    assert!(t.lib.has("land_mark sp-a CERTIFIED t1"));
    assert_eq!(fs::read_to_string(t.s().queue_dir.join("sp-a")).unwrap(), "CERTIFIED t1 1000\n");
    assert!(t.landing_log().contains("QUEUE GATE_COST 1000 branch=sp-a seconds=0"));
    assert!(t.out().contains("queue.sh submit: certified spira/sp-a"));
    t.assert_lc_untouched();
}

#[test]
fn submit_push_mode_rebases_pushes_and_closes() {
    let t = T::new(LandMode::Push);
    t.git.set("refs/heads/spira/sp-a", "t1");
    assert_eq!(t.run(&["submit", "spira/sp-a"]), 0);
    assert!(t.lib.has("rebase spira/sp-a origin/main"));
    assert!(t.lib.has("push origin spira/sp-a:main"));
    assert!(t.lib.has("land_mark sp-a LANDED t1"));
    assert!(t.lib.has("close_on_land sp-a t1"));
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
fn step_all_with_lifecycle_off_pins_every_child_off_and_never_touches_spira_lc() {
    let mut t = T::new(LandMode::Queue);
    t.repo("spira", LandMode::Queue);
    t.repo("svc", LandMode::Queue);
    *t.lib.repos.borrow_mut() = Ok(vec!["spira".into(), "svc".into()]);
    t.lc.available.set(true); // a binary is present: presence is never the switch
    // The verdict runs in process now (no lc_off to pin on a child of its own); the
    // batcher is the only remaining out-of-process, lc_off-taking call per repo.
    let bin = t.dir.join("batcher");
    testkit::write_exe(&bin, "#!/bin/sh\n");
    t.lib.s.batcher_bin = Some(bin);
    assert_eq!(t.run(&["step", "--all"]), 0);
    assert_eq!(t.verdicts(), vec!["verdict spira", "verdict svc"]);
    let modes = t.scripts.lc_off.borrow().clone();
    assert!(!modes.is_empty() && modes.iter().all(|off| *off), "{modes:?}");
    t.assert_lc_untouched();
}

#[test]
fn lifecycle_env_switch_wins_over_the_config_both_ways() {
    let t = T::new(LandMode::Queue);
    t.lifecycle_on();
    t.var("SPIRA_LIFECYCLE_ENFORCE", "0");
    assert_eq!(t.run(&["step", "spira"]), 0);
    assert!(t.scripts.lc_off.borrow().iter().all(|off| *off));
    t.assert_lc_untouched();
    let t = T::new(LandMode::Queue);
    t.lc.available.set(true);
    t.var("SPIRA_LIFECYCLE_ENFORCE", "true");
    assert_eq!(t.run(&["step", "spira"]), 0);
    assert!(t.scripts.lc_off.borrow().iter().all(|off| !*off));
}

#[test]
fn step_all_with_lifecycle_on_runs_children_on_and_refuses_loudly_when_unreachable() {
    let t = T::new(LandMode::Queue);
    t.lifecycle_on();
    t.repo("spira", LandMode::Queue);
    *t.lib.repos.borrow_mut() = Ok(vec!["spira".into()]);
    assert_eq!(t.run(&["step", "--all"]), 0);
    assert!(t.scripts.lc_off.borrow().iter().all(|off| !*off));
    assert!(t.lc.calls.borrow().contains(&"probe".to_string()));
    for args in [&["step", "--all"][..], &["step", "spira"][..], &["flush"][..]] {
        let t = T::new(LandMode::Queue);
        t.lifecycle_on();
        *t.lc.in_delivery.borrow_mut() = Err("dolt down".into());
        t.repo("spira", LandMode::Queue);
        *t.lib.repos.borrow_mut() = Ok(vec!["spira".into()]);
        assert_eq!(t.run(args), 1, "{args:?}");
        assert!(t.err().contains("lifecycle_enforce is on and spira-lc is unreachable (dolt down) — refused, nothing changed"), "{}", t.err());
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
fn eject_member_with_lifecycle_off_reopens_through_bead_reopen_and_never_touches_spira_lc() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    // a batch_id on the record does not summon spira-lc while the switch is off
    t.open_record("pr=12\nmembers=sp-a:ta sp-b:tb\nbatch_id=spira-1\nversion=3\n");
    t.lc.available.set(true);
    assert_eq!(t.run(&["eject", "sp-a", "--suites", "test-x.sh", "--reason", "red"]), 0);
    assert!(t.lib.has("land_mark sp-a RED ta red"));
    assert!(t.lib.has("bead_reopen sp-a eject-red test-x.sh"));
    assert!(!t.lib.has("cause_event"), "bead_reopen writes the cause row itself");
    assert!(t.lib.has("land_mark sp-b CERTIFIED tb"));
    t.assert_lc_untouched();
}

#[test]
fn eject_member_marks_red_records_the_harness_cause_and_returns_survivors() {
    let t = T::new(LandMode::Queue);
    t.lifecycle_on();
    open_batch_record(&t);
    assert_eq!(t.run(&["eject", "sp-a", "--reason", "needs rebase"]), 0);
    assert!(t.lib.has("land_mark sp-a RED ta needs rebase"));
    assert!(t.lib.has("cause_event sp-a eject"));
    assert!(t.lib.has("lc_returned sp-a needs rebase"));
    assert!(t.lib.has("release_claim sp-a"));
    assert!(!t.lib.has("bead_reopen"));
    assert!(t.lib.has("land_mark sp-b CERTIFIED tb"));
    assert!(t.forge.calls.borrow().contains(&"pr-close 12".to_string()));
    assert!(!t.qfile("open").exists());
    assert!(t.lib.has("notify spira sp-a ejected (queue.sh eject)"));
    assert!(t.out().contains("queue.sh eject: ejected sp-a from spira batch (landstate=RED)"));
}

#[test]
fn eject_red_records_eject_red_and_writes_the_suites_sidecar() {
    let t = T::new(LandMode::Queue);
    t.lifecycle_on();
    open_batch_record(&t);
    assert_eq!(t.run(&["eject", "sp-a", "--suites", "test-x.sh"]), 0);
    assert!(t.lib.has("cause_event sp-a eject-red"));
    assert_eq!(fs::read_to_string(t.s().landstate.join("sp-a.ejected")).unwrap(), "test-x.sh");
    let t = T::new(LandMode::Queue);
    t.lifecycle_on();
    open_batch_record(&t);
    assert_eq!(t.run(&["eject", "sp-a", "--red"]), 0);
    assert!(t.lib.has("cause_event sp-a eject-red"));
}

#[test]
fn eject_of_a_certified_unbatched_bead_reopens_with_the_cause() {
    let t = T::new(LandMode::Queue);
    t.landstate("sp-c", "CERTIFIED tc 5 ");
    assert_eq!(t.run(&["eject", "sp-c", "--red", "--reason-file", "-"]), 0);
    assert!(t.lib.has("bead_reopen sp-c eject-red "));
    assert!(t.out().contains("ejected sp-c (certified, not yet batched) for spira (landstate=WITHDRAWN)"));
}

#[test]
fn eject_takes_a_comma_separated_suites_list_and_still_refuses_a_bad_name_in_it() {
    let t = T::new(LandMode::Queue);
    t.landstate("sp-c", "CERTIFIED tc 5 ");
    assert_eq!(t.run(&["eject", "sp-c", "--suites", "test-x.sh,test-y.sh"]), 0);
    assert!(t.lib.has("bead_reopen sp-c eject-red test-x.sh,test-y.sh"));
    let t = T::new(LandMode::Queue);
    t.landstate("sp-c", "CERTIFIED tc 5 ");
    assert_ne!(t.run(&["eject", "sp-c", "--suites", "test-x.sh,bad name.sh"]), 0);
    assert!(!t.lib.has("bead_reopen"));
    let t = T::new(LandMode::Queue);
    t.landstate("sp-c", "CERTIFIED tc 5 ");
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
fn eject_dry_run_writes_nothing() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    t.bd.rows.borrow_mut().push(BeadRow { id: "sp-a".into(), ..Default::default() });
    assert_eq!(t.run(&["eject", "sp-a", "--dry-run"]), 0);
    assert!(t.out().contains("dry-run: would record cause eject"));
    assert!(t.out().contains("dry-run: would reopen bead sp-a and clear assignee"));
    t.assert_lc_untouched();
    assert!(t.qfile("open").exists());
    assert!(t.lib.calls.borrow().is_empty());
}

#[test]
fn eject_with_a_batch_id_ejects_on_spira_lc_too() {
    let t = T::new(LandMode::Queue);
    t.lifecycle_on();
    t.open_record("pr=12\nmembers=sp-a:ta\nbatch_id=spira-1\nversion=3\n");
    assert_eq!(t.run(&["eject", "sp-a", "--reason", "multi\nline"]), 0);
    assert!(t.lc.calls.borrow().contains(&"eject-member spira-1 sp-a CI_RUNNING 4 multi line".to_string()));
}

#[test]
fn lifecycle_on_with_spira_lc_unreachable_refuses_loudly_and_changes_nothing() {
    for args in [&["eject", "sp-a"][..], &["abandon", "--reason", "x"][..], &["open-batch", "--skip-pregate"][..]] {
        let t = T::new(LandMode::Queue);
        t.config.lifecycle.set(true);
        t.lc.available.set(true);
        *t.lc.in_delivery.borrow_mut() = Err("Access denied".into());
        open_batch_record(&t);
        assert_eq!(t.run(args), 1, "{args:?}");
        assert!(t.err().contains("lifecycle_enforce is on and spira-lc is unreachable (Access denied) — refused, nothing changed"), "{}", t.err());
        assert!(t.lib.calls.borrow().is_empty(), "{args:?} changed something");
        assert!(t.qfile("open").exists());
    }
    // no binary at all is the same refusal
    let t = T::new(LandMode::Queue);
    t.config.lifecycle.set(true);
    open_batch_record(&t);
    assert_eq!(t.run(&["eject", "sp-a"]), 1);
    assert!(t.err().contains("no spira-lc program"));
}

#[test]
fn the_environment_pins_the_switch_over_the_config() {
    let t = T::new(LandMode::Queue);
    t.config.lifecycle.set(true);
    t.var("SPIRA_LIFECYCLE_ENFORCE", "0");
    open_batch_record(&t);
    assert_eq!(t.run(&["eject", "sp-a"]), 0);
    t.assert_lc_untouched();
    let t = T::new(LandMode::Queue);
    t.var("SPIRA_LIFECYCLE_ENFORCE", "1");
    t.lc.available.set(true);
    open_batch_record(&t);
    assert_eq!(t.run(&["eject", "sp-a"]), 0);
    assert!(t.lib.has("lc_returned sp-a"));
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
fn abandon_keeps_red_members_archives_and_audits() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    t.landstate("sp-a", "RED ta 5 x");
    t.var("SPIRA_QUEUE_ACTOR", "op");
    assert_eq!(t.run(&["abandon", "--reason", "dirty\nPR"]), 0);
    assert!(!t.lib.has("land_mark sp-a"));
    assert!(t.lib.has("land_mark sp-b CERTIFIED tb"));
    assert!(t.lib.has("cancel_runs spira/queue/x"));
    assert!(t.forge.calls.borrow().iter().any(|c| c == "pr-comment 12 Batch abandoned. Reason: dirty PR"));
    let archive = t.qfile("closed-pr12-20260929T010203Z");
    let a = fs::read_to_string(&archive).unwrap();
    assert!(a.ends_with("reason=dirty PR\nactor=op\n"), "{a}");
    assert!(!t.qfile("open").exists());
    assert!(t.landing_log().contains("QUEUE ABANDON 1000 repo=spira pr=12 actor=op members=sp-a:RED,sp-b:CERTIFIED reason=dirty PR"));
    assert!(t.lib.has("event queue.abandoned abandoned PR 12 for spira (actor=op)"));
    t.assert_lc_untouched();
}

#[test]
fn abandon_with_lifecycle_on_abandons_the_batch_on_spira_lc() {
    let t = T::new(LandMode::Queue);
    t.lifecycle_on();
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
fn open_batch_assembles_admits_opens_and_marks_batched() {
    let t = T::new(LandMode::Queue);
    t.git.set("origin/main", "b0");
    *t.git.branches.borrow_mut() = vec![("spira/sp-a".into(), "ta".into()), ("spira/sp-b".into(), "tb".into()), ("spira/sp-c".into(), "tc".into())];
    t.landstate("sp-a", "CERTIFIED ta 1 ");
    t.landstate("sp-b", "CERTIFIED tb 2 ");
    t.landstate("sp-c", "CERTIFIED tc 3 ");
    t.bd.rows.borrow_mut().extend([
        BeadRow { id: "sp-a".into(), status: Some("closed".into()), title: Some("A".into()), ..Default::default() },
        BeadRow { id: "sp-b".into(), status: Some("open".into()), labels: vec!["spira-submitted".into()], ..Default::default() }, // literal-ok: fixture
        BeadRow { id: "sp-c".into(), status: Some("open".into()), ..Default::default() },
    ]);
    t.lib.conflict_with_base.borrow_mut().insert("tb".into());
    t.git.merge_fail.borrow_mut().insert("tb".into());
    assert_eq!(t.run(&["open-batch", "--skip-pregate"]), 0, "{}", t.err());
    let out = t.out();
    assert!(out.contains("skip — sp-c: bead status=open (not closed, not submitted)"));
    assert!(out.contains("skip — sp-b: conflicts with base"));
    assert!(out.contains("PR 77 opened — 1 branches (spira/queue/20260929T010203Z)"));
    let rec = fs::read_to_string(t.qfile("open")).unwrap();
    assert!(rec.starts_with("pr=77\nhead=merged-ta\nbase=b0\nmembers=sp-a:ta\nopened=1000\nbranch=spira/queue/20260929T010203Z\nowner=operator\n"), "{rec}");
    assert!(t.lib.has("land_mark sp-a BATCHED ta"));
    assert!(t.forge.calls.borrow()[0].contains("- sp-a — A"));
    assert!(t.landing_log().contains("members=1 gate_seconds=0 verdict=green source=open-batch"));
    assert!(!rec.contains("batch_id="), "no lifecycle batch row with the switch off");
    t.assert_lc_untouched();
}

#[test]
fn open_batch_with_lifecycle_on_cuts_on_spira_lc_and_records_the_batch_id() {
    let t = T::new(LandMode::Queue);
    t.lifecycle_on();
    t.git.set("origin/main", "b0");
    *t.git.branches.borrow_mut() = vec![("spira/sp-a".into(), "ta".into())];
    t.landstate("sp-a", "CERTIFIED ta 1 ");
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
fn open_batch_does_not_admit_when_bd_cannot_answer() {
    let t = T::new(LandMode::Queue);
    t.git.set("origin/main", "b0");
    *t.git.branches.borrow_mut() = vec![("spira/sp-a".into(), "ta".into())];
    t.landstate("sp-a", "CERTIFIED ta 1 ");
    t.bd.fail.set(true);
    assert_eq!(t.run(&["open-batch", "--members", "sp-a"]), 1);
    assert!(t.out().contains("no cut — nothing admissible for spira"));
}

#[test]
fn open_batch_red_pregate_deletes_the_branch() {
    let t = T::new(LandMode::Queue);
    t.git.set("origin/main", "b0");
    *t.git.branches.borrow_mut() = vec![("spira/sp-a".into(), "ta".into())];
    t.landstate("sp-a", "CERTIFIED ta 1 ");
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
    t.certify("spira", T1, gate::cert::Source::Gate, "harness-now");
}

/// local/main at b0, h1 fast-forwards from it with tree T1 — and no certificate anywhere.
fn uncertified_local_repo(t: &T) {
    t.git.set("refs/heads/local/main", "b0");
    t.git.ancestor("b0", "h1");
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
    assert!(e.contains(&format!("no gate PASS or round GREEN for h1's tree {T1} in spira")), "{e}");
    assert!(e.contains(&format!("run: bash {}/gate.sh h1 spira", t.s().home.display())), "names the exact gate command: {e}");
    assert!(e.contains("SPIRA_LAND_UNGATED=<reason>") && e.contains("refused, nothing changed"), "{e}");
    assert_eq!(t.landed_ref().as_deref(), Some("b0"), "nothing moved");
    assert!(!t.lib.has("land_mark"));
    assert!(!t.qfile("round-seq").exists());
    assert!(t.git.get("refs/archive/rounds/1").is_none());
}

#[test]
fn land_local_accepts_a_gate_pass_for_the_head_tree() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("h1"));
    assert!(t.err().contains(&format!("tree {T1} certified by gate PASS")), "{}", t.err());
    assert!(t.lib.has("land_mark sp-a LANDED ta"));
    assert!(!t.err().contains("UNGATED"));
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
    assert!(t.err().contains("no gate PASS or round GREEN"), "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
    // A red verdict is not a certificate either.
    fs::write(&p, fs::read_to_string(&p).unwrap().replace(T_BRANCH, T1).replace("verdict=PASS", "verdict=FAIL")).unwrap();
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]), 1);
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
}

#[test]
fn land_local_a_pass_from_an_older_gate_binary_still_counts() {
    let t = T::new(LandMode::QueueLocal);
    uncertified_local_repo(&t);
    t.certify("spira", T1, gate::cert::Source::Gate, "a-harness-hash-from-an-older-gate");
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]), 0, "{}", t.err());
}

#[test]
fn land_local_honours_spira_verdicts_as_the_gate_does() {
    let t = T::new(LandMode::QueueLocal);
    uncertified_local_repo(&t);
    let v = t.dir.join("elsewhere");
    t.var("SPIRA_VERDICTS", &v.display().to_string());
    let c = gate::cert::Cert { source: gate::cert::Source::Gate, tree: T1.into(), repo: "spira".into(), rev: "r".into(), branch: "b".into(), by: "x".into(), when: "w".into(), at: 1, harness: "h".into(), suites: "-".into() };
    gate::cert::write(&v, &c).unwrap();
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]), 0, "{}", t.err());
}

#[test]
fn land_local_ungated_override_lands_and_records_the_reason() {
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
    assert!(t.lib.has(&format!("land_mark sp-a LANDED ta ungated: {reason}")), "the landstate record carries it");
    assert!(t.lib.has(&format!("land_mark sp-b LANDED h1 ungated: {reason}")));
    let last_mark = t.lib.calls.borrow().iter().rev().find(|c| c.starts_with("land_mark sp-a ")).cloned();
    assert_eq!(last_mark, Some(format!("land_mark sp-a LANDED ta ungated: {reason}")), "re-marked after bead_close_on_land's own LANDED write");
    let closes = t.lib.calls.borrow().iter().position(|c| c.starts_with("close_on_land sp-a"));
    let remark = t.lib.calls.borrow().iter().rposition(|c| c.starts_with("land_mark sp-a "));
    assert!(closes < remark, "the ungated mark is written after the close");
    assert!(t.landing_log().contains(&format!("QUEUE UNGATED 1000 repo=spira head=h1 tree={T1} reason={reason}")), "{}", t.landing_log());
    // When bead_close_on_land re-marked the record at the head, the re-mark keeps that tip
    // and changes only the reason, so publish (§8 D4) reads the tip it always did.
    let t = T::new(LandMode::QueueLocal);
    uncertified_local_repo(&t);
    t.landstate("sp-c", "LANDED h1 5 Closed by landing pass");
    t.var("SPIRA_LAND_UNGATED", "why");
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-c:tc"]), 0, "{}", t.err());
    let last_mark = t.lib.calls.borrow().iter().rev().find(|c| c.starts_with("land_mark sp-c ")).cloned();
    assert_eq!(last_mark.as_deref(), Some("land_mark sp-c LANDED h1 ungated: why"));
}

#[test]
fn land_local_lands_marks_and_archives() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta,sp-b"]), 0, "{}", t.err());
    assert_eq!(t.git.get("refs/heads/local/main").as_deref(), Some("h1"));
    assert_eq!(t.git.get("refs/archive/rounds/1").as_deref(), Some("h1"));
    assert_eq!(fs::read_to_string(t.qfile("round-seq")).unwrap(), "1\n");
    assert!(t.lib.has("divergence f0 b0"), "the cached forge ref is checked, never fetched");
    assert!(!t.git.calls.borrow().iter().any(|c| c.starts_with("fetch")));
    assert!(t.lib.has("land_mark sp-a LANDED ta"));
    assert!(t.lib.has("land_mark sp-b LANDED h1"), "a bare id landed at the head");
    assert!(t.lib.has("close_on_land sp-b h1"));
    assert!(t.out().contains("queue.sh land-local: local/main fast-forwarded to h1 (round 1, archived at refs/archive/rounds/1)"));
    t.assert_lc_untouched();
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
    assert!(t.lib.has("land_mark sp-b LANDED tb"));
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
            format!("activate h1 --repo {REPO} --landed-ref local/main {common}"),
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
    assert!(calls.iter().any(|c| c.starts_with("land_mark sp-a LANDED ta")));
    assert!(calls.iter().any(|c| c.starts_with("close_on_land sp-b h1")));
    assert!(!t.err().contains("LAND DEPLOY FAILED"));
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
fn land_local_build_failure_leaves_current_alone_keeps_the_landing_and_reports_a_deploy_fault() {
    let mut t = T::new(LandMode::QueueLocal);
    let wts = harness_world(&mut t);
    release_in_force(&t);
    t.scripts.release_rc.borrow_mut().insert("build".into(), (1, "release: the workspace declares queue but the build did not produce it\n".into()));
    assert_eq!(land_harness(&t, &wts), crate::ops::DEPLOY_FAULT);
    let e = t.err();
    assert!(e.contains("LAND DEPLOY FAILED for h1: release build exited 1: release: the workspace declares queue"), "{e}");
    assert!(e.contains("current is untouched (still ") && e.contains("/r1)"), "names what current still is: {e}");
    assert!(e.contains("local/main is at h1 and the landing stays recorded"), "{e}");
    assert_eq!(release_argv(&t).len(), 1, "no verify or activate after a failed build");
    assert_eq!(t.landed_ref().as_deref(), Some("h1"), "never reverted");
    assert!(t.lib.has("land_mark sp-a LANDED ta") && t.lib.has("close_on_land sp-a h1"));
    assert_eq!(t.git.get("refs/archive/rounds/1").as_deref(), Some("h1"));
}

#[test]
fn land_local_verify_or_activate_failure_is_a_deploy_fault_too() {
    for (sub, calls) in [("verify", 2), ("activate", 3)] {
        let mut t = T::new(LandMode::QueueLocal);
        let wts = harness_world(&mut t);
        release_in_force(&t);
        t.scripts.release_rc.borrow_mut().insert(sub.into(), (1, format!("release: {sub} said no\n")));
        assert_eq!(land_harness(&t, &wts), crate::ops::DEPLOY_FAULT, "{sub}");
        assert!(t.err().contains(&format!("LAND DEPLOY FAILED for h1: release {sub} exited 1: release: {sub} said no")), "{}", t.err());
        assert_eq!(release_argv(&t).len(), calls, "{sub}");
        assert_eq!(t.landed_ref().as_deref(), Some("h1"));
        assert!(t.lib.has("land_mark sp-a LANDED ta"));
    }
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
    assert_eq!(land_harness(&t, &wts), crate::ops::DEPLOY_FAULT);
    assert!(t.err().contains("release build answered \"h0\" for h1 — not the landed commit"), "{}", t.err());
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

fn publishable(t: &T) {
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
fn publish_members_come_from_land_commits_even_when_landstate_was_reaped() {
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
    t.assert_lc_untouched();
}

#[test]
fn publish_prefers_the_landstate_tip_when_it_is_in_range() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    t.git.ancestor("tx", "m3");
    t.landstate("sp-a", "LANDED tx 5 ");
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
fn transitions_refuse_while_work_is_in_delivery() {
    // an open batch record
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    t.open_record("pr=5\n");
    assert_eq!(t.run(&["to-forge"]), 1);
    assert!(t.err().contains("has work in delivery (open batch PR 5) — refused, nothing changed"));
    assert!(!t.lib.has("push"), "no final publish");

    // a BATCHED landstate whose tip is a commit here
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    t.landstate("sp-q", "BATCHED t1 5 ");
    assert_eq!(t.run(&["to-forge"]), 1);
    assert!(t.err().contains("(sp-q)"));

    // switch ON: spira-lc says IN_DELIVERY
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    t.lifecycle_on();
    *t.lc.in_delivery.borrow_mut() = Ok(vec![LcBeadRow { bead_id: "sp-r".into(), state: "IN_DELIVERY".into(), tip: Some("t2".into()) }]);
    assert_eq!(t.run(&["to-forge"]), 1);
    assert!(t.err().contains("(sp-r)"));

    // switch ON, spira-lc unreachable: loud refusal
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    t.lifecycle_on();
    *t.lc.in_delivery.borrow_mut() = Err("dolt down".into());
    assert_eq!(t.run(&["to-forge"]), 1);
    assert!(t.err().contains("lifecycle_enforce is on and spira-lc is unreachable (dolt down)"));

    // switch OFF, spira-lc unreachable and even holding an IN_DELIVERY row: never asked,
    // never blocks (production today: no lifecycle database at all)
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    t.git.set("refs/remotes/origin/main", "m3");
    *t.lib.readback.borrow_mut() = ("queue".into(), "origin/main".into());
    t.lc.available.set(true);
    *t.lc.in_delivery.borrow_mut() = Err("Access denied".into());
    assert_eq!(t.run(&["to-forge"]), 0, "{}", t.err());
    t.assert_lc_untouched();

    // switch ON: another repository's IN_DELIVERY bead does not block
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    t.git.set("refs/remotes/origin/main", "m3");
    *t.lib.readback.borrow_mut() = ("queue".into(), "origin/main".into());
    t.lifecycle_on();
    *t.lc.in_delivery.borrow_mut() = Ok(vec![LcBeadRow { bead_id: "xx-1".into(), state: "IN_DELIVERY".into(), tip: Some("elsewhere".into()) }]);
    assert_eq!(t.run(&["to-forge"]), 0, "{}", t.err());
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
