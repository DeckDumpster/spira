//! The Sending against REAL git repositories (every disposition is a git question) and a
//! recording world for lib.sh's chokepoints, bd, the forge and the lifecycle machine. The
//! fixture is test-sending.sh's, branch for branch; the chokepoints themselves (salvage,
//! the verified deletion, the content fence) are lib.sh's and stay proven by the suites
//! that drive the binary end to end.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use crate::git::Git;
use crate::ports::{Base, Repo, Sent, World};
use crate::sweep::{disposition, Ctx, Disp, Opts, Scope, Sweep};

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

fn commit_file(r: &Path, file: &str, body: &str, msg: &str) {
    std::fs::write(r.join(file), body).unwrap();
    git(r, &["add", file]);
    git(r, &["commit", "-q", "-m", msg]);
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
    enforce: bool,
    base: Option<Base>,
    wts: PathBuf,
    destroy_fails: bool,
    out: RefCell<Vec<String>>,
    calls: RefCell<Vec<String>>,
}

impl Fake {
    fn put(&mut self, id: &str, v: Value) {
        self.beads.insert(id.into(), v);
    }
    fn call(&self, s: String) {
        self.calls.borrow_mut().push(s);
    }
    fn out(&self) -> String {
        self.out.borrow().join("\n")
    }
    fn called(&self, prefix: &str) -> bool {
        self.calls.borrow().iter().any(|c| c.starts_with(prefix))
    }
}

impl World for Fake {
    fn prefetch(&self) {
        self.call("prefetch".into());
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
    fn prune(&self, _repo: &Path) {
        self.call("prune".into());
    }
    fn label_add(&self, id: &str, label: &str) {
        self.call(format!("label {id} {label}"));
    }
    fn content_on_base(&self, id: &str, proof: &str) {
        self.call(format!("lc {id} {proof}"));
    }
    fn pr_merged_tip(&self, _repo: &Path, br: &str) -> Option<String> {
        self.call(format!("gh {br}"));
        self.prs.get(br).cloned()
    }
    fn enforce(&self) -> bool {
        self.enforce
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

/// test-sending.sh's fixture: one repository whose base is `main` (no remote), a branch per
/// disposition.
struct Fx {
    _t: testkit::TempDir,
    repo: PathBuf,
    run: PathBuf,
    noone_tip: String,
    sq_tip: String,
}

fn fixture() -> (Fx, Fake) {
    let t = testkit::TempDir::new("sending-fx");
    let r = t.path().join("repo");
    let run = t.path().join("run");
    std::fs::create_dir_all(run.join("worktree")).unwrap();
    std::fs::create_dir_all(&r).unwrap();
    git(&r, &["init", "-q", "-b", "main"]);
    git(&r, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let mut f = Fake { wts: run.join("worktree"), ..Default::default() };
    let b = |status: &str| json!({"status": status, "labels": [], "dependencies": []});
    let sup = |id: &str| json!({"status": "closed", "labels": [], "dependencies": [{"issue_id": id, "depends_on_id": "sp-succ", "type": "supersedes"}]});

    // sp-cl0: ancestor shortcut, ahead 0. sp-cl1: an empty commit, merge-tree equal to base.
    git(&r, &["branch", "spira/sp-cl0", "main"]);
    f.put("sp-cl0", b("open"));
    for id in ["sp-cl1", "sp-clnoassert"] {
        git(&r, &["checkout", "-q", "-b", &format!("spira/{id}"), "main"]);
        git(&r, &["commit", "-q", "--allow-empty", "-m", &format!("{id}: review only")]);
        git(&r, &["checkout", "-q", "main"]);
        f.put(id, b("closed"));
    }
    git(&r, &["branch", "spira/round-54", "main"]);
    // sp-supsafe: superseded, conflicts with the base. sp-supunsafe: adds content cleanly.
    git(&r, &["checkout", "-q", "-b", "spira/sp-supsafe", "main"]);
    commit_file(&r, "shared-sup.txt", "branch version\n", "sp-supsafe: work");
    git(&r, &["checkout", "-q", "main"]);
    commit_file(&r, "shared-sup.txt", "base version\n", "base: conflicting change");
    f.put("sp-supsafe", sup("sp-supsafe"));
    git(&r, &["checkout", "-q", "-b", "spira/sp-supunsafe", "main"]);
    commit_file(&r, "sp-supunsafe.txt", "unique\n", "sp-supunsafe: unique work");
    git(&r, &["checkout", "-q", "main"]);
    f.put("sp-supunsafe", sup("sp-supunsafe"));
    git(&r, &["worktree", "add", "-q", run.join("worktree/sp-supunsafe").to_str().unwrap(), "spira/sp-supunsafe"]);
    // sp-sq: squash-merged, base moved past the squash.
    git(&r, &["checkout", "-q", "-b", "spira/sp-sq", "main"]);
    commit_file(&r, "shared-sq.txt", "line1\n", "sp-sq: commit A");
    commit_file(&r, "shared-sq.txt", "line1\nline2\n", "sp-sq: commit B");
    let sq_tip = git(&r, &["rev-parse", "spira/sp-sq"]);
    git(&r, &["checkout", "-q", "main"]);
    commit_file(&r, "shared-sq.txt", "line1\nline2\n", "sp-sq: squash-merge (#1)");
    commit_file(&r, "shared-sq.txt", "line1\nline2\nline3\n", "unrelated: advance shared-sq.txt");
    f.put("sp-sq", b("closed"));
    f.prs.insert("spira/sp-sq".into(), sq_tip.clone());
    // sp-otherpr: a landing record names it, every commit patch-equivalent upstream.
    // sp-cherry: the record is about the past; a later commit is unapplied.
    for id in ["sp-otherpr", "sp-cherry"] {
        let file = format!("shared-{id}.txt");
        git(&r, &["checkout", "-q", "-b", &format!("spira/{id}"), "main"]);
        commit_file(&r, &file, "v1\n", &format!("{id}: add content"));
        git(&r, &["checkout", "-q", "main"]);
        commit_file(&r, &file, "v1\n", &format!("spira: land {id}"));
        commit_file(&r, &file, "v2\n", "unrelated: advance further");
        f.put(id, b("closed"));
    }
    git(&r, &["checkout", "-q", "spira/sp-cherry"]);
    commit_file(&r, "sp-cherry-extra.txt", "never landed\n", "sp-cherry: one more commit, after landing");
    git(&r, &["checkout", "-q", "main"]);
    // sp-unlanded: real content, a bead, no record.
    git(&r, &["checkout", "-q", "-b", "spira/sp-unlanded", "main"]);
    commit_file(&r, "sp-unlanded.txt", "unlanded\n", "sp-unlanded: real work");
    git(&r, &["checkout", "-q", "main"]);
    f.put("sp-unlanded", b("open"));
    // sp-noone: real content, no bead at all — ORPHAN, archived.
    git(&r, &["checkout", "-q", "-b", "spira/sp-noone", "main"]);
    commit_file(&r, "sp-noone.txt", "orphaned content\n", "sp-noone: no bead names this");
    git(&r, &["checkout", "-q", "main"]);
    let noone_tip = git(&r, &["rev-parse", "spira/sp-noone"]);
    // sp-stray: no bead, an ancestor of main.
    git(&r, &["branch", "spira/sp-stray", "main"]);
    // sp-held: a live claim.
    git(&r, &["checkout", "-q", "-b", "spira/sp-held", "main"]);
    commit_file(&r, "sp-held.txt", "in flight\n", "sp-held: an aeon is still here");
    git(&r, &["checkout", "-q", "main"]);
    f.put("sp-held", b("in_progress"));
    f.held.insert("sp-held".into());
    // sp-orphan: a registered worktree whose branch ref is gone (PASS 2).
    git(&r, &["worktree", "add", "-q", "-b", "spira/sp-orphan", run.join("worktree/sp-orphan").to_str().unwrap(), "main"]);
    git(&r, &["update-ref", "-d", "refs/heads/spira/sp-orphan"]);
    // A harness tree (leading dot): never judged.
    git(&r, &["worktree", "add", "-q", "--detach", run.join("worktree/.landing.repo").to_str().unwrap(), "main"]);

    f.base = Some(Base { landref: "main".into(), landrefs: vec!["main".into()], remote: None });
    (Fx { _t: t, repo: r, run, noone_tip, sq_tip }, f)
}

fn opts() -> Opts {
    Opts { dry: false, fetch: false, only: None, scope: Scope::All }
}

fn repo(fx: &Fx) -> Repo {
    Repo { name: "home".into(), root: Some(fx.repo.clone()), queued: false }
}

fn sweep(f: &Fake, o: Opts, repos: &[Repo]) -> i32 {
    let mut s = Sweep { w: f, opts: o, submitted_label: "spira-submitted".into(), tally: Default::default() };
    s.run(repos)
}

fn exists(fx: &Fx, br: &str) -> bool {
    Git(&fx.repo).branch_exists(br)
}

#[test]
fn every_branch_gets_the_shells_disposition() {
    let (fx, f) = fixture();
    let base = f.base.clone().unwrap();
    let c = Ctx { w: &f, repo: &fx.repo, name: "home", base: &base, submitted_label: "spira-submitted" };
    for (id, want) in [
        ("sp-cl0", Disp::SendContentLanded),
        ("sp-cl1", Disp::SendContentLanded),
        ("sp-supsafe", Disp::ReapSupersededSafe),
        ("sp-supunsafe", Disp::KeepSupersededUnsafe),
        ("sp-sq", Disp::ReapSquashMerged),
        ("sp-otherpr", Disp::SendOtherPr),
        ("sp-cherry", Disp::KeepCherryUnapplied),
        ("sp-unlanded", Disp::KeepUnlanded),
        ("sp-noone", Disp::OrphanNoBead),
        ("sp-stray", Disp::SendContentLanded),
    ] {
        assert_eq!(disposition(&c, id, &format!("spira/{id}")), want, "{id} ({})", want.code());
    }
    // Deciding touched nothing: every branch still stands, nothing was sent or labelled.
    for id in ["sp-cl0", "sp-cl1", "sp-supsafe", "sp-sq", "sp-otherpr", "sp-noone", "sp-stray"] {
        assert!(exists(&fx, &format!("spira/{id}")), "{id}");
    }
    assert!(f.calls.borrow().iter().all(|c| c.starts_with("bead ") || c.starts_with("gh ")), "{:?}", f.calls.borrow());
    // The forge is asked only after content_landed and the supersede check both said no,
    // and only for a closed-or-submitted bead.
    assert!(f.called("gh spira/sp-sq") && !f.called("gh spira/sp-cl1") && !f.called("gh spira/sp-supsafe") && !f.called("gh spira/sp-unlanded"));
    assert_eq!(fx.sq_tip, f.prs["spira/sp-sq"]);
}

#[test]
fn one_pass_sends_reaps_keeps_archives_and_holds() {
    let (fx, f) = fixture();
    let rc = sweep(&f, opts(), &[repo(&fx)]);
    let out = f.out();
    assert_eq!(rc, 0, "{out}");
    for (line, gone) in [
        ("SENT sp-cl0  home spira/sp-cl0", true),
        ("SENT sp-cl1  home spira/sp-cl1", true),
        ("SENT sp-clnoassert  home spira/sp-clnoassert", true),
        ("REAPED sp-supsafe  home spira/sp-supsafe", true),
        ("REAPED sp-sq  home spira/sp-sq", true),
        ("SENT sp-otherpr  home spira/sp-otherpr", true),
        ("ARCHIVED sp-noone  home spira/sp-noone", true),
        ("SENT sp-stray  home spira/sp-stray", true),
    ] {
        assert!(out.lines().any(|l| l == line), "missing {line:?} in\n{out}");
        let br = line.split_whitespace().nth(3).unwrap();
        assert_eq!(!exists(&fx, br), gone, "{br}");
    }
    assert!(out.contains("SKIP   round-54  round branch, not a bead"));
    assert!(exists(&fx, "spira/round-54"));
    // `diff --name-only <base> <br>` (two-dot, as the shell had it), at most five paths.
    assert!(out.contains("KEEP   sp-supunsafe  superseded but 1 unlanded commit(s) add content absent from main: "), "{out}");
    assert!(out.contains("\nsp-supunsafe.txt\n"), "{out}");
    assert!(exists(&fx, "spira/sp-supunsafe"));
    assert!(f.called("destroy sp-supunsafe"), "the kept branch's worktree is freed");
    assert!(out.contains("KEEP   sp-cherry  2 commit(s) not in main; landed() names it but git cherry finds unapplied commits"), "{out}");
    assert!(out.contains("KEEP   sp-unlanded  unlanded — 1 commit(s) not in main"));
    assert!(out.contains("HELD   sp-held  in_progress"));
    assert!(exists(&fx, "spira/sp-held") && exists(&fx, "spira/sp-cherry") && exists(&fx, "spira/sp-unlanded"));
    // The orphan's commits stay reachable at refs/archive, written before the reap and
    // reaped with the "archived" caller exception.
    assert_eq!(Git(&fx.repo).verify("refs/archive/spira/sp-noone").as_deref(), Some(fx.noone_tip.as_str()));
    assert!(f.called("send sp-noone spira/sp-noone landed in main archived"));
    assert!(Git(&fx.repo).verify("refs/archive/spira/sp-stray").is_none(), "an ancestor has nothing to preserve");
    // Landed arms close a submitted bead at the land ref's sha; reaps of work that did not
    // land through this branch (superseded) do not.
    let main = git(&fx.repo, &["rev-parse", "main"]);
    assert!(f.called(&format!("close sp-cl1 {main}")) && f.called("close sp-sq ") && f.called("close sp-otherpr "));
    assert!(!f.called("close sp-supsafe"));
    // PASS 2: the orphaned worktree; the harness's own dot-tree is never judged.
    assert!(out.contains("SENT sp-orphan  home spira/sp-orphan  orphaned worktree (branch was already gone)"), "{out}");
    assert!(!f.called("destroy .landing.repo"));
    assert!(f.called("prune"));
    assert!(out.ends_with("LOG sending: 9 sent, 0 failed"), "{out}");
}

#[test]
fn content_landed_evidence_is_a_machine_event_on_and_the_label_off() {
    // OFF (production today): the `content-landed` label CHECK 5's exemption reads — the
    // gap this port closes. Only for a branch that carried commits (sp-cl1), never for a
    // zero-ahead one (sp-cl0), whose own merge commit is the evidence.
    let (fx, f) = fixture();
    sweep(&f, opts(), &[repo(&fx)]);
    assert!(f.called("label sp-cl1 content-landed") && f.called("label sp-clnoassert content-landed"));
    assert!(!f.called("label sp-cl0") && !f.called("label sp-stray") && !f.called("lc "));
    // ON: a ContentOnBase event whose proof names the base tip, and no label.
    let (fx, mut f) = fixture();
    f.enforce = true;
    let main = git(&fx.repo, &["rev-parse", "main"]);
    sweep(&f, opts(), &[repo(&fx)]);
    assert!(f.called(&format!("lc sp-cl1 merge-tree:{main}")), "{:?}", f.calls.borrow());
    assert!(!f.called("lc sp-cl0") && !f.called("label "));
}

#[test]
fn dry_run_says_would_and_changes_nothing() {
    let (fx, f) = fixture();
    let rc = sweep(&f, Opts { dry: true, ..opts() }, &[repo(&fx)]);
    let out = f.out();
    assert_eq!(rc, 0);
    assert!(out.contains("WOULD  sp-cl1  send branch spira/sp-cl1") && !out.contains("SENT sp-cl1"), "{out}");
    assert!(out.contains("WOULD  sp-supunsafe  free worktree"));
    assert!(out.contains("WOULD  sp-noone  archive orphan branch spira/sp-noone (1 commit(s) not in main) to refs/archive/spira/sp-noone"));
    assert!(out.contains("WOULD  sp-sq  reap squash-merged branch spira/sp-sq (PR merged at this tip)"));
    assert!(out.contains("WOULD  sp-orphan  remove orphaned worktree"));
    assert!(!out.contains("LOG sending:"), "no tally line on a dry run");
    for c in f.calls.borrow().iter() {
        assert!(c == "prefetch" || c.starts_with("bead ") || c.starts_with("gh "), "a dry run only reads: {c}");
    }
    assert!(exists(&fx, "spira/sp-cl1") && Git(&fx.repo).verify("refs/archive/spira/sp-noone").is_none());
}

#[test]
fn mid_send_hold_queue_and_failure() {
    let (fx, mut f) = fixture();
    f.held_mid.insert("sp-cl1".into());
    f.queued.insert("sp-otherpr".into());
    f.fail.insert("sp-supsafe".into(), "worktree /x was not removed — see /run/reap.log".into());
    f.fail.insert("sp-stray".into(), String::new());
    let rc = sweep(&f, opts(), &[repo(&fx)]);
    let out = f.out();
    assert!(out.contains("HELD   sp-cl1  in_progress — the lease has not been released (mid-send)"), "{out}");
    assert!(exists(&fx, "spira/sp-cl1"));
    assert!(out.contains("SKIP   sp-otherpr  CERTIFIED/BATCHED — waiting for verdict"));
    assert!(out.contains("FAILED sp-supsafe  worktree /x was not removed — see /run/reap.log"));
    assert!(out.contains("FAILED sp-stray  refused, see /run/reap.log"), "an empty reap error names the reap log");
    assert_eq!(rc, 1, "a failed deletion fails the pass");
    assert!(out.ends_with("failed"), "{out}");
    assert!(out.contains(", 2 failed"));
}

#[test]
fn an_orphaned_worktree_that_will_not_go_is_a_failure_and_a_held_one_is_kept() {
    let (fx, mut f) = fixture();
    f.destroy_fails = true;
    assert_eq!(sweep(&f, Opts { only: None, ..opts() }, &[repo(&fx)]), 1);
    assert!(f.out().contains(&format!("FAILED sp-orphan  orphaned worktree {} was not removed — see /run/reap.log", fx.run.join("worktree/sp-orphan").display())));
    let (fx, mut f) = fixture();
    f.held.insert("sp-orphan".into());
    sweep(&f, opts(), &[repo(&fx)]);
    assert!(f.out().contains("HELD   sp-orphan  orphaned worktree kept — in_progress"));
}

#[test]
fn one_bead_sweeps_one_branch_and_skips_pass_two() {
    let (fx, f) = fixture();
    sweep(&f, Opts { only: Some("spira/sp-cl1".into()), ..opts() }, &[repo(&fx)]);
    let out = f.out();
    assert!(out.contains("SENT sp-cl1"), "{out}");
    assert!(!out.contains("sp-cl0") && !out.contains("sp-orphan") && !f.called("prune"), "{out}");
    let (fx, f) = fixture();
    sweep(&f, Opts { only: Some("sp-stray".into()), ..opts() }, &[repo(&fx)]);
    assert!(f.out().contains("SENT sp-stray") && exists(&fx, "spira/sp-cl1"));
}

#[test]
fn repositories_that_cannot_be_judged_are_skipped_loudly() {
    let (fx, mut f) = fixture();
    let t = testkit::TempDir::new("sending-other");
    let plain = t.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let repos = [
        Repo { name: "nopath".into(), root: None, queued: false },
        Repo { name: "plain".into(), root: Some(plain.clone()), queued: false },
    ];
    sweep(&f, opts(), &repos);
    let out = f.out();
    assert!(out.contains("SKIP   nopath  no path is configured for it"));
    assert!(out.contains(&format!("SKIP   plain  {} is not a git checkout", plain.display())));
    f.base = None;
    f.out.borrow_mut().clear();
    sweep(&f, opts(), &[repo(&fx)]);
    assert!(f.out().contains("SKIP   home  cannot resolve the ref it lands on — configure its `base`"));
    assert!(exists(&fx, "spira/sp-cl1"), "an unresolvable base deletes nothing");
}

#[test]
fn skip_queue_and_queue_only_partition_the_repositories() {
    let (fx, f) = fixture();
    let q = Repo { name: "home".into(), root: Some(fx.repo.clone()), queued: true };
    sweep(&f, Opts { scope: Scope::SkipQueue, ..opts() }, std::slice::from_ref(&q));
    assert!(!f.out().contains("SENT"), "--skip-queue leaves a queue repo alone");
    assert!(exists(&fx, "spira/sp-cl1"));
    sweep(&f, Opts { scope: Scope::QueueOnly, ..opts() }, &[q]);
    assert!(f.out().contains("SENT sp-cl1"), "--queue-only sweeps it");
    let (fx, f) = fixture();
    sweep(&f, Opts { scope: Scope::QueueOnly, ..opts() }, &[repo(&fx)]);
    assert!(!f.out().contains("SENT") && exists(&fx, "spira/sp-cl1"), "--queue-only leaves a push repo alone");
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
fn harness_home_is_env_first_then_the_release_layout() {
    let t = testkit::TempDir::new("sending-home");
    let rel = t.path().join("rel");
    std::fs::create_dir_all(rel.join("bin")).unwrap();
    std::fs::create_dir_all(rel.join("spira")).unwrap();
    std::fs::write(rel.join("spira/lib.sh"), "").unwrap();
    let exe = rel.join("bin/sending");
    assert_eq!(crate::locate_home(Some("/h"), &exe), Some(PathBuf::from("/h")));
    assert_eq!(crate::locate_home(None, &exe), Some(rel.join("spira").canonicalize().unwrap()));
    assert_eq!(crate::locate_home(None, &t.path().join("x/y")), None);
}

#[test]
fn the_store_is_read_in_one_batch_however_many_branches_there_are() {
    let (fx, f) = fixture();
    sweep(&f, opts(), &[repo(&fx)]);
    let n = Git(&fx.repo).spira_branches().len();
    let prefetches = f.calls.borrow().iter().filter(|c| c.as_str() == "prefetch").count();
    assert_eq!(prefetches, 1, "one store read per repository, not per branch");
    assert!(n > 1);
}
