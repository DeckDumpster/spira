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
    push_ok: Cell<bool>,
    /// settle_publish answers, in order; the file is removed on a 0 answer marked "green".
    settle: RefCell<Vec<(i32, bool)>>,
    pf_rc: Cell<i32>,
    conflict_with_base: RefCell<BTreeSet<String>>,
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
    fn context(&self, _repo: Option<&str>) -> Result<(Settings, RepoCtx), String> {
        Ok((self.s.clone(), self.r.borrow().clone()))
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
    fn divergence(&self, _: &str, _: &Path, forge: &str, local: &str) -> bool {
        self.log(format!("divergence {forge} {local}"));
        !self.diverged.get()
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
    fn settle_publish(&self, name: &str, _: &Path) -> i32 {
        self.log(format!("settle {name}"));
        let (rc, green) = if self.settle.borrow().is_empty() { (0, false) } else { self.settle.borrow_mut().remove(0) };
        if green {
            let _ = fs::remove_file(self.s.queue_dir.join(name).join("publish"));
        }
        rc
    }
}

#[derive(Default)]
struct FScripts {
    gate_rc: Cell<i32>,
    activate_rc: Cell<i32>,
    fence_ok: Cell<bool>,
    calls: RefCell<Vec<String>>,
}

impl Scripts for FScripts {
    fn gate(&self, br: &str, repo: &str, bead: &str, suites: &str) -> (i32, String) {
        self.calls.borrow_mut().push(format!("gate {br} {repo} bead={bead} suites={suites}"));
        (self.gate_rc.get(), "gate says".into())
    }
    fn batch_sweep(&self, repo: &str, wz: bool) -> i32 {
        self.calls.borrow_mut().push(format!("sweep {repo} wait0={wz}"));
        0
    }
    fn verdict(&self, repo: &str) -> i32 {
        self.calls.borrow_mut().push(format!("verdict {repo}"));
        0
    }
    fn batcher_cut(&self, _: &Path, repo: &str, wz: bool) -> i32 {
        self.calls.borrow_mut().push(format!("cut {repo} wait0={wz}"));
        0
    }
    fn czar_fence(&self, class: &str) -> bool {
        self.calls.borrow_mut().push(format!("czar {class}"));
        self.fence_ok.get()
    }
    fn build_tarball(&self, bins: &Path, _: &str, name: &str, out: &Path, _: &str, _: &Path) -> Option<PathBuf> {
        self.calls.borrow_mut().push(format!("tarball {} {name}", bins.display()));
        let p = out.join(format!("{name}.tar.gz"));
        fs::write(&p, "x").ok()?;
        Some(p)
    }
    fn activate(&self, t: &Path, ll: bool) -> (i32, String) {
        self.calls.borrow_mut().push(format!("activate {} land_local={ll}", t.file_name().unwrap().to_string_lossy()));
        (self.activate_rc.get(), String::new())
    }
}

#[derive(Default)]
struct FForge {
    pr: RefCell<Option<String>>,
    calls: RefCell<Vec<String>>,
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
    dir: PathBuf,
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
            lc_bin: None,
            submitted_label: "spira-submitted".into(), // literal-ok: fixture vocabulary
            home_repo: "spira".into(),
            db: "/db".into(),
            bd: "bd".into(),
            transition_pollsec: 5,
            transition_maxsec: 30,
            preflight_wall_secs: 240,
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
            push_ok: Cell::new(true),
            settle: RefCell::default(),
            pf_rc: Cell::new(0),
            conflict_with_base: RefCell::default(),
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

impl Drop for T {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
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
    assert!(t.err().contains("SPIRA_BATCHER_BIN not available"));
    let bin = t.dir.join("batcher");
    fs::write(&bin, "#!/bin/sh\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    t.lib.s.batcher_bin = Some(bin);
    assert_eq!(t.run(&["flush"]), 0);
    let calls = t.scripts.calls.borrow().clone();
    assert!(calls.contains(&"sweep spira wait0=true".to_string()) && calls.contains(&"cut spira wait0=true".to_string()));
}

#[test]
fn step_queue_local_publishes_with_stderr_folded_into_stdout() {
    let t = T::new(LandMode::QueueLocal);
    t.git.set("refs/remotes/origin/main", "f0");
    t.git.set("refs/heads/local/main", "f0");
    let rc = t.run(&["step", "spira"]);
    assert_eq!(rc, 0);
    assert_eq!(t.scripts.calls.borrow()[0], "verdict spira");
    // the missing batcher's refusal is on stderr (it is _batch_cut's own), the publish's on stdout
    assert!(t.err().contains("SPIRA_BATCHER_BIN not available"));
    assert!(t.out().contains("nothing to publish for spira"));
}

#[test]
fn step_on_queue_forge_is_status_zero_whatever_the_cut_did() {
    let t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["step", "spira"]), 0);
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
    assert!(t.err().contains("SPIRA_LC_BIN is not an executable"));
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

fn local_repo(t: &T) {
    t.git.set("refs/heads/local/main", "b0");
    t.git.ancestor("b0", "h1");
    t.git.set("refs/remotes/origin/main", "f0");
}

#[test]
fn land_local_without_a_release_in_force_lands_marks_and_archives() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta,sp-b"]), 0, "{}", t.err());
    assert_eq!(t.git.get("refs/heads/local/main").as_deref(), Some("h1"));
    assert_eq!(t.git.get("refs/archive/rounds/1").as_deref(), Some("h1"));
    assert_eq!(fs::read_to_string(t.qfile("round-seq")).unwrap(), "1\n");
    assert!(t.err().contains("release step skipped: production runs a checkout"));
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

fn round_worktree(t: &T, tree: &str) -> PathBuf {
    let wt = t.dir.join("round-wt");
    let bins = wt.join("target/release");
    fs::create_dir_all(&bins).unwrap();
    let exe = bins.join("spira-config");
    fs::write(&exe, "#!/bin/sh\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
    t.git.refs.borrow_mut().insert("HEAD".into(), "wt-head".into());
    t.git.trees.borrow_mut().insert("wt-head".into(), tree.into());
    t.git.trees.borrow_mut().insert("h1".into(), "T1".into());
    wt
}

#[test]
fn land_local_with_a_release_in_force_requires_the_round_worktree() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    release_in_force(&t);
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]), 1);
    assert!(t.err().contains("--worktree <round worktree> is required while production runs a release"));
    assert_eq!(t.git.get("refs/heads/local/main").as_deref(), Some("b0"));
}

#[test]
fn land_local_packages_the_round_worktrees_own_target_release() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    release_in_force(&t);
    let wt = round_worktree(&t, "T1");
    let wts = wt.display().to_string();
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta", "--worktree", &wts]), 0, "{}", t.err());
    let calls = t.scripts.calls.borrow().clone();
    assert_eq!(calls[0], format!("tarball {}/target/release spira-h1", wts));
    assert_eq!(calls[1], "activate spira-h1.tar.gz land_local=true");
    assert!(t.out().contains("queue.sh land-local: activated spira-h1"));
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
fn land_local_activation_failure_reverts_the_ref() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    release_in_force(&t);
    let wt = round_worktree(&t, "T1");
    t.scripts.activate_rc.set(1);
    assert_eq!(t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta", "--worktree", &wt.display().to_string()]), 1);
    assert!(t.err().contains("packaging/activation failed for h1 — reverted, nothing changed"));
    assert_eq!(t.git.get("refs/heads/local/main").as_deref(), Some("b0"));
    assert!(!t.lib.has("land_mark"));
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
    // pending, then green (the fake settle removes the publish record)...
    t.lib.settle.borrow_mut().extend([(0, false), (0, true)]);
    // ...but the forge ref this fake fetches never moved to local/main's m3.
    assert_eq!(t.run(&["to-forge"]), 1);
    assert!(t.out().contains("running the final publish for spira"));
    assert!(t.out().contains("queue.sh publish: PR 77 opened"), "the final publish's output is on stdout");
    assert!(t.out().contains("waiting for publish PR 77 to settle green"));
    assert_eq!(t.lib.calls.borrow().iter().filter(|c| c.starts_with("settle")).count(), 2);
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
    t.lib.settle.borrow_mut().push((3, false));
    assert_eq!(t.run(&["to-forge"]), 1);
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
fn rollback_local_needs_two_rounds_and_a_retained_tarball() {
    let t = T::new(LandMode::QueueLocal);
    assert_eq!(t.run(&["rollback-local"]), 1);
    assert!(t.err().contains("round-seq is 0 — no landed round to roll back to"));
    fs::write(t.qfile("round-seq"), "2\n").unwrap();
    t.git.set("refs/archive/rounds/1", "p1");
    t.git.set("refs/heads/local/main", "h2");
    assert_eq!(t.run(&["rollback-local"]), 1);
    assert!(t.err().contains("no retained tarball for the previous release"), "{}", t.err());
    let tb = t.s().releases.clone().unwrap().join(".tarballs");
    fs::create_dir_all(&tb).unwrap();
    fs::write(tb.join("spira-p1.tar.gz"), "x").unwrap();
    assert_eq!(t.run(&["rollback-local"]), 0);
    assert_eq!(t.git.get("refs/heads/local/main").as_deref(), Some("p1"));
    assert!(t.out().contains("queue.sh rollback-local: activated spira-p1, local/main reset to p1"));
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
