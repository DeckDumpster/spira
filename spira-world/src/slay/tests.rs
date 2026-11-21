use super::*;
use spira_config::admission::Procs;
use std::cell::RefCell;

fn argv(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

fn parsed(a: &[&str]) -> Args {
    match parse_args(&argv(a)) {
        Ok(Parsed::Run(args)) => args,
        other => panic!("expected a run, got {other:?}"),
    }
}

fn refused(a: &[&str]) -> ArgError {
    parse_args(&argv(a)).expect_err("expected a refusal")
}

// ---- argument parsing: no store is in reach, every refusal is exit 2 --------------------

// covers: UC-dwarf-execution-26
#[test_id(tg7pkjg)]
#[test]
fn an_unknown_flag_is_refused_by_name() {
    let e = refused(&["--bogus-flag"]);
    assert_eq!(e, ArgError::Usage("unknown flag --bogus-flag".into()));
    assert!(e.message().contains("unknown flag"));
}

// covers: UC-dwarf-execution-26
#[test_id(tg3x9az)]
#[test]
fn a_positional_beside_the_bead_is_refused_and_points_at_the_named_flags() {
    let e = refused(&["--bead", "sp-x", "a positional sentence"]);
    assert_eq!(e, ArgError::Positional("a positional sentence".into()));
    let m = e.message();
    assert!(m.contains("unexpected argument") && m.contains("--bead") && m.contains("--why"));
}

// covers: UC-dwarf-execution-26
#[test_id(tr3ktbv)]
#[test]
fn two_bead_values_are_refused_naming_both() {
    let e = refused(&["--bead", "sp-x", "--bead", "sp-y"]);
    assert_eq!(e, ArgError::Usage("--bead given twice (sp-x, sp-y)".into()));
}

// covers: UC-dwarf-execution-26
#[test_id(ta2d2zu)]
#[test]
fn no_bead_at_all_is_refused_as_required() {
    assert!(refused(&[]).message().contains("--bead is required"));
}

// covers: UC-dwarf-execution-26
#[test_id(toog7v8)]
#[test]
fn every_flag_that_takes_a_value_refuses_to_run_without_one() {
    for (flag, said) in [("--close", "--close needs a reason"), ("--why", "--why needs text")] {
        let e = refused(&["--bead", "sp-x", flag]);
        assert!(e.message().contains(said), "{flag}: {}", e.message());
    }
    assert_eq!(refused(&["--bead"]), ArgError::Usage("--bead needs a bead id".into()));
}

// covers: UC-dwarf-execution-26
#[test_id(tg0xqdc)]
#[test]
fn help_is_not_a_refusal_and_documents_every_flag_and_the_exit_codes() {
    assert_eq!(parse_args(&argv(&["-h"])), Ok(Parsed::Help));
    assert_eq!(parse_args(&argv(&["--help"])), Ok(Parsed::Help));
    for f in ["--bead", "--why", "--keep-work", "--close", "--reopen", "EXIT"] {
        assert!(usage_text().contains(f), "-h must document {f}");
    }
}

// covers: UC-dwarf-execution-26
#[test_id(twg5pd2)]
#[test]
fn the_default_mode_reopens_with_the_default_reason_and_removes_the_work() {
    let a = parsed(&["--bead", "sp-s1"]);
    assert_eq!(a, Args { id: "sp-s1".into(), mode: Mode::Reopen, keep: false, why: DEFAULT_WHY.into() });
}

// covers: UC-dwarf-execution-26
#[test_id(tqgrabw)]
#[test]
fn close_keep_work_and_why_are_each_read() {
    let a = parsed(&["--bead", "sp-s2", "--close", "drop it", "--keep-work", "--why", "halted"]);
    assert_eq!(a, Args { id: "sp-s2".into(), mode: Mode::Close("drop it".into()), keep: true, why: "halted".into() });
}

// covers: UC-dwarf-execution-26
#[test_id(t46qesg)]
#[test]
fn a_later_reopen_overrides_an_earlier_close() {
    assert_eq!(parsed(&["--bead", "sp-s2", "--close", "r", "--reopen"]).mode, Mode::Reopen);
}

// ---- the pipeline, against a fake world ---------------------------------------------------

struct Fake {
    answers_id: String,
    state: RefCell<String>,
    labels: RefCell<Vec<String>>,
    repo: Option<PathBuf>,
    branch: RefCell<bool>,
    base: Option<String>,
    ahead: u64,
    landed: bool,
    own_commit: bool,
    worktree: bool,
    salvage: Result<Option<PathBuf>, SalvageFailed>,
    commit_ok: bool,
    park_ok: bool,
    destroy_wt_ok: bool,
    destroy_branch: Result<(), String>,
    branch_survives_destroy: bool,
    stopped: Stopped,
    pid_alive: bool,
    pidfile: bool,
    holder: Option<String>,
    procs: RefCell<Vec<(u32, u64)>>,
    survives_term: bool,
    calls: RefCell<Vec<String>>,
}

impl Fake {
    /// A bead with one unlanded commit on its branch, a worktree, a clean tree.
    fn new() -> Fake {
        Fake {
            answers_id: "sp-s1".into(),
            state: RefCell::new("WORKING".into()),
            labels: RefCell::new(vec!["spira".into()]),
            repo: Some(PathBuf::from("repo")),
            branch: RefCell::new(true),
            base: Some("origin/main".into()),
            ahead: 1,
            landed: false,
            own_commit: true,
            worktree: true,
            salvage: Ok(None),
            commit_ok: true,
            park_ok: true,
            destroy_wt_ok: true,
            destroy_branch: Ok(()),
            branch_survives_destroy: false,
            stopped: Stopped { name: "dwarf-test".into(), pid: "4242".into(), unit: "spira-dwarf.service".into(), fail: false },
            pid_alive: false,
            pidfile: false,
            holder: None,
            procs: RefCell::new(vec![]),
            survives_term: false,
            calls: RefCell::new(vec![]),
        }
    }

    fn log(&self, s: String) {
        self.calls.borrow_mut().push(s);
    }
    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
    fn called(&self, prefix: &str) -> bool {
        self.calls().iter().any(|c| c.starts_with(prefix))
    }
    fn call(&self, prefix: &str) -> String {
        self.calls().into_iter().find(|c| c.starts_with(prefix)).unwrap_or_else(|| panic!("no call {prefix}: {:?}", self.calls()))
    }
}

impl Beads for Fake {
    fn view(&self, id: &str) -> Option<BeadView> {
        self.log(format!("view {id}"));
        if self.answers_id.is_empty() {
            return None;
        }
        Some(BeadView { id: self.answers_id.clone(), labels: self.labels.borrow().clone() })
    }
    fn clear_assignee(&self, id: &str) {
        self.log(format!("clear_assignee {id}"));
    }
    fn label_add(&self, id: &str, label: &str) {
        self.log(format!("label_add {id} {label}"));
        self.labels.borrow_mut().push(label.into());
    }
    fn label_remove(&self, id: &str, label: &str) {
        self.log(format!("label_remove {id} {label}"));
        self.labels.borrow_mut().retain(|l| l != label);
    }
    fn note(&self, id: &str, text: &str) {
        self.log(format!("note {id} {text}"));
    }
    fn repo_root_of(&self, _id: &str) -> Option<PathBuf> {
        self.repo.clone()
    }
}

impl Lifecycle for Fake {
    fn state(&self, _id: &str) -> Option<String> {
        Some(self.state.borrow().clone())
    }
    fn holder(&self, _id: &str) -> Option<String> {
        self.holder.clone()
    }
    fn disposition(&self, id: &str, why: &str) {
        self.log(format!("disposition {id} slain {why}"));
    }
    fn holder_dead(&self, id: &str) {
        self.log(format!("holder_dead {id}"));
        *self.state.borrow_mut() = "READY".into();
    }
    fn drop_bead(&self, id: &str, text: &str) {
        self.log(format!("drop {id} {text}"));
        *self.state.borrow_mut() = "DROPPED".into();
    }
}

impl Git for Fake {
    fn is_dir(&self, _p: &Path) -> bool {
        self.worktree
    }
    fn branch_exists(&self, _repo: &Path, _br: &str) -> bool {
        *self.branch.borrow()
    }
    fn short_tip(&self, _repo: &Path, _br: &str) -> Option<String> {
        self.branch.borrow().then(|| "abc1234".to_string())
    }
    fn landref(&self, _repo: &Path) -> Option<String> {
        self.base.clone()
    }
    fn names_bead_between(&self, _repo: &Path, _base: &str, _br: &str, _id: &str) -> bool {
        self.own_commit
    }
    fn ahead(&self, _repo: &Path, _base: &str, _br: &str) -> u64 {
        self.ahead
    }
    fn content_landed(&self, _repo: &Path, _br: &str, _base: &str) -> bool {
        self.landed
    }
    fn abort_inflight(&self, wt: &Path) {
        self.log(format!("abort_inflight {}", wt.display()));
    }
    fn commit_all(&self, _wt: &Path, msg: &str) -> bool {
        self.log(format!("commit_all {msg}"));
        self.commit_ok
    }
    fn park(&self, _repo: &Path, refname: &str, br: &str) -> bool {
        self.log(format!("park {refname} {br}"));
        self.park_ok
    }
    fn salvage(&self, id: &str, _wt: &Path) -> Result<Option<PathBuf>, SalvageFailed> {
        self.log(format!("salvage {id}"));
        self.salvage.clone()
    }
    fn destroy_worktree(&self, id: &str, wt: &Path, _repo: &Path, why: &str) -> bool {
        self.log(format!("destroy_worktree {id} {} {why}", wt.display()));
        self.destroy_wt_ok
    }
    fn destroy_branch(&self, id: &str, br: &str, _repo: &Path, why: &str) -> Result<(), String> {
        self.log(format!("destroy_branch {id} {br} {why}"));
        if self.destroy_branch.is_ok() && !self.branch_survives_destroy {
            *self.branch.borrow_mut() = false;
        }
        self.destroy_branch.clone()
    }
}

impl Dwarf for Fake {
    fn stop(&self, id: &str, holder: Option<&str>, say: &mut Vec<String>) -> Stopped {
        self.log(format!("stop {id}"));
        if self.stopped.pid.is_empty() {
            if let Some(pid) = holder.and_then(|h| live_holder_pid(h, self)) {
                self.log(format!("term {pid}"));
                if !self.survives_term {
                    self.procs.borrow_mut().retain(|(p, _)| *p != pid);
                }
            } else {
                say.push(format!("dwarf: none running for {id} (no live pid file)"));
                return self.stopped.clone();
            }
        }
        say.push("dwarf: stopped (1s)".into());
        self.stopped.clone()
    }
    fn alive(&self, _pid: &str) -> bool {
        self.pid_alive
    }
    fn pidfile_remains(&self, _id: &str) -> bool {
        self.pidfile
    }
}

impl Procs for Fake {
    fn start_of(&self, pid: u32) -> Option<u64> {
        self.procs.borrow().iter().find(|(p, _)| *p == pid).map(|(_, s)| *s)
    }
    fn ppid_of(&self, _pid: u32) -> Option<u32> {
        None
    }
}

/// No pid file; the row's holder is `dwarf-x@7.99` and pid 7 runs with that start time.
fn holder_only(procs: &[(u32, u64)]) -> Fake {
    let mut f = Fake::new();
    f.stopped = Stopped::default();
    f.holder = Some("dwarf-x@7.99".into());
    f.procs = RefCell::new(procs.to_vec());
    f
}

fn run(f: &Fake, a: &[&str]) -> Report {
    let ports = Ports { git: f, beads: f, lc: f, dwarf: f, procs: f };
    let ctx = Ctx { run: Path::new("run"), submitted_label: "custom-submitted", store: "the-store" };
    let mut v = vec!["--bead", "sp-s1"];
    v.extend_from_slice(a);
    slay(&ctx, &ports, &parsed(&v))
}

fn said(r: &Report, needle: &str) -> bool {
    r.out.iter().any(|l| l.contains(needle))
}

// covers: UC-dwarf-execution-26
#[test_id(tu9jy38)]
#[test]
fn the_slain_disposition_is_the_first_action_and_precedes_the_stop() {
    let f = Fake::new();
    run(&f, &[]);
    let calls = f.calls();
    let first_write = calls.iter().find(|c| !c.starts_with("view")).unwrap();
    assert_eq!(first_write, "disposition sp-s1 slain slain by the operator");
    let at = |p: &str| calls.iter().position(|c| c.starts_with(p)).unwrap();
    assert!(at("disposition") < at("stop"), "{calls:?}");
    assert!(at("stop") < at("holder_dead"), "{calls:?}");
}

// covers: UC-dwarf-execution-26
#[test_id(trd6ga3)]
#[test]
fn the_why_is_what_the_disposition_records() {
    let f = Fake::new();
    run(&f, &["--why", "operator halted it"]);
    assert_eq!(f.call("disposition"), "disposition sp-s1 slain operator halted it");
}

// covers: UC-dwarf-execution-26
#[test_id(t24uiwj)]
#[test]
fn default_slay_releases_the_claim_unassigns_and_removes_the_worktree() {
    let f = Fake::new();
    let r = run(&f, &[]);
    assert_eq!(r.code, 0, "{r:?}");
    assert!(said(&r, "bead: sp-s1 state=READY labels=spira"));
    assert!(said(&r, "slain: sp-s1"));
    assert!(f.called("holder_dead sp-s1"));
    assert!(f.called("clear_assignee sp-s1"));
    assert!(f.call("destroy_worktree").contains("run/worktree/sp-s1"));
    assert!(f.call("destroy_worktree").ends_with("slain: slain by the operator"));
    assert!(f.called("abort_inflight"));
    assert!(!f.called("drop"), "reopen must not drop the bead");
}

// covers: UC-dwarf-execution-26 UC-landed-audit-reaping-23
#[test_id(ttq6x1o)]
#[test]
fn default_slay_keeps_a_branch_with_unlanded_commits_and_does_not_park_or_delete_it() {
    let f = Fake::new();
    let r = run(&f, &[]);
    assert!(said(&r, "kept in refs/heads for the next builder"));
    assert!(!f.called("destroy_branch") && !f.called("park") && !f.called("label_remove"));
    assert!(f.call("note").contains("branch spira/sp-s1 kept in refs/heads with its commits"));
    assert!(!said(&r, "parked"));
}

// covers: UC-dwarf-execution-26
#[test_id(tee8wsp)]
#[test]
fn slay_with_no_base_to_compare_keeps_the_branch_on_reopen() {
    let mut f = Fake::new();
    f.base = None;
    let r = run(&f, &[]);
    assert_eq!(r.code, 0);
    assert!(said(&r, "no base to compare spira/sp-s1 against — kept in refs/heads"));
    assert!(!f.called("destroy_branch"));
}

// covers: UC-dwarf-execution-26
#[test_id(t4wd7ra)]
#[test]
fn close_drops_the_bead_with_the_reason_and_the_note() {
    let f = Fake::new();
    let r = run(&f, &["--close", "operator decided to drop this"]);
    assert_eq!(r.code, 0, "{r:?}");
    let drop = f.call("drop");
    let note = f.call("note");
    assert!(drop.starts_with("drop sp-s1 operator decided to drop this — Slain by the operator: slain by the operator."), "{drop}");
    assert_eq!(drop.strip_prefix("drop sp-s1 "), note.strip_prefix("note sp-s1 "));
}

// covers: UC-dwarf-execution-26 UC-landed-audit-reaping-23
#[test_id(tfw3pbx)]
#[test]
fn close_parks_unlanded_work_at_refs_slain_then_deletes_the_branch_and_its_label() {
    let f = Fake::new();
    let r = run(&f, &["--close", "drop"]);
    assert_eq!(r.code, 0, "{r:?}");
    assert_eq!(f.call("park"), "park refs/slain/sp-s1 spira/sp-s1");
    assert!(said(&r, "parked at refs/slain/sp-s1"));
    assert!(said(&r, "branch spira/sp-s1 deleted at abc1234, kept at refs/slain/sp-s1"));
    let calls = f.calls();
    let at = |p: &str| calls.iter().position(|c| c.starts_with(p)).unwrap();
    assert!(at("park") < at("destroy_branch") && at("destroy_branch") < at("label_remove"), "{calls:?}");
    assert_eq!(f.call("label_remove"), "label_remove sp-s1 branch:spira/sp-s1");
}

// covers: UC-dwarf-execution-26
#[test_id(tip83sh)]
#[test]
fn close_refuses_to_delete_a_branch_it_could_not_park() {
    let mut f = Fake::new();
    f.park_ok = false;
    let r = run(&f, &["--close", "drop"]);
    assert_eq!(r.code, 1);
    assert!(said(&r, "could not park spira/sp-s1 at refs/slain/sp-s1 — REFUSING to delete it"));
    assert!(!f.called("destroy_branch"));
    assert!(said(&r, "slay: INCOMPLETE for sp-s1"));
}

// covers: UC-dwarf-execution-26 UC-landed-audit-reaping-23
#[test_id(tih3u49)]
#[test]
fn a_landed_branch_is_deleted_without_being_parked() {
    let mut f = Fake::new();
    f.landed = true;
    let r = run(&f, &["--close", "drop"]);
    assert_eq!(r.code, 0);
    assert!(!f.called("park") && !said(&r, "parked"));
    assert!(f.called("destroy_branch sp-s1 spira/sp-s1 slain: slain by the operator"));
}

// covers: UC-dwarf-execution-26
#[test_id(tqmvlfh)]
#[test]
fn a_branch_with_nothing_the_base_lacks_is_deleted_not_parked_even_on_reopen() {
    let mut f = Fake::new();
    f.ahead = 0;
    let r = run(&f, &[]);
    assert_eq!(r.code, 0);
    assert!(!f.called("park") && !said(&r, "parked") && !said(&r, "kept in refs/heads"));
    assert!(f.called("destroy_branch"));
    assert!(f.called("label_remove sp-s1 branch:spira/sp-s1"));
}

// covers: UC-dwarf-execution-26
#[test_id(t81ux1w)]
#[test]
fn keep_work_touches_neither_branch_nor_worktree_but_still_releases_the_claim() {
    let f = Fake::new();
    let r = run(&f, &["--keep-work"]);
    assert_eq!(r.code, 0, "{r:?}");
    assert!(said(&r, "work: kept — branch spira/sp-s1 at abc1234, worktree run/worktree/sp-s1"));
    for c in ["destroy_worktree", "destroy_branch", "salvage", "park", "abort_inflight"] {
        assert!(!f.called(c), "{c} must not run with --keep-work");
    }
    assert!(f.called("holder_dead") && f.called("clear_assignee"));
}

// covers: UC-dwarf-execution-26
#[test_id(t3e3e9t)]
#[test]
fn a_kept_branch_carrying_the_beads_commit_reopens_as_submitted_with_the_configured_label() {
    let f = Fake::new();
    let r = run(&f, &["--keep-work"]);
    assert_eq!(f.call("label_add"), "label_add sp-s1 custom-submitted");
    assert!(said(&r, "already carries a commit of sp-s1 — reopened as submitted"));
}

// covers: UC-dwarf-execution-26
#[test_id(txb21y5)]
#[test]
fn a_kept_branch_with_no_commit_of_its_own_gets_no_submitted_label() {
    let mut f = Fake::new();
    f.own_commit = false;
    run(&f, &["--keep-work"]);
    assert!(!f.called("label_add"));
}

// covers: UC-dwarf-execution-26
#[test_id(to73p5b)]
#[test]
fn the_submitted_label_is_only_for_a_kept_reopen() {
    let f = Fake::new();
    run(&f, &[]);
    assert!(!f.called("label_add"));
    let f = Fake::new();
    run(&f, &["--keep-work", "--close", "r"]);
    assert!(!f.called("label_add"));
}

// covers: UC-dwarf-execution-26
#[test_id(tcgis8b)]
#[test]
fn a_dirty_worktree_is_salvaged_wip_committed_and_the_branch_stays_in_refs_heads() {
    let mut f = Fake::new();
    f.salvage = Ok(Some(PathBuf::from("run/reaped/sp-s1.1.patch")));
    let r = run(&f, &["--why", "operator halted it", "--close", "drop"]);
    assert_eq!(r.code, 0, "{r:?}");
    let msg = f.call("commit_all");
    assert!(msg.contains("wip — salvaged at slay") && msg.contains("sp-s1") && msg.contains("operator halted it"), "{msg}");
    assert!(said(&r, "uncommitted changes salvaged to run/reaped/sp-s1.1.patch"));
    assert!(said(&r, "wip committed to spira/sp-s1 at abc1234"));
    assert!(!f.called("park") && !f.called("destroy_branch"), "a wip branch is neither parked nor deleted");
    assert!(f.call("note").contains("wip committed to spira/sp-s1; branch kept in refs/heads; patch at run/reaped/sp-s1.1.patch"));
}

// covers: UC-dwarf-execution-26
#[test_id(tptd6bw)]
#[test]
fn salvage_runs_before_the_worktree_is_destroyed() {
    let mut f = Fake::new();
    f.salvage = Ok(Some(PathBuf::from("run/reaped/p.patch")));
    run(&f, &[]);
    let calls = f.calls();
    let at = |p: &str| calls.iter().position(|c| c.starts_with(p)).unwrap();
    assert!(at("abort_inflight") < at("salvage") && at("salvage") < at("commit_all") && at("commit_all") < at("destroy_worktree"), "{calls:?}");
}

// covers: UC-dwarf-execution-26
#[test_id(tjtkuwa)]
#[test]
fn a_clean_worktree_makes_no_wip_commit() {
    let f = Fake::new();
    let r = run(&f, &["--close", "drop"]);
    assert!(!f.called("commit_all") && !said(&r, "wip committed"));
}

// covers: UC-dwarf-execution-26
#[test_id(t9i20zc)]
#[test]
fn a_failed_wip_commit_says_the_patch_is_the_only_copy_and_carries_on() {
    let mut f = Fake::new();
    f.salvage = Ok(Some(PathBuf::from("run/reaped/p.patch")));
    f.commit_ok = false;
    let r = run(&f, &[]);
    assert!(said(&r, "could not make wip commit — patch is the only copy"));
    assert!(f.called("destroy_worktree"));
}

// covers: UC-dwarf-execution-26
#[test_id(tgat5gu)]
#[test]
fn a_failed_salvage_leaves_the_worktree_and_branch_alone_and_fails() {
    let mut f = Fake::new();
    f.salvage = Err(SalvageFailed);
    let r = run(&f, &["--close", "drop"]);
    assert_eq!(r.code, 1);
    assert!(said(&r, "could not salvage run/worktree/sp-s1 — leaving it in place"));
    assert!(!f.called("destroy_worktree") && !f.called("destroy_branch") && !f.called("park"));
    assert!(f.called("drop"), "the bead is still told what happened");
}

// covers: UC-dwarf-execution-26
#[test_id(tzbagsa)]
#[test]
fn a_worktree_that_will_not_go_fails_and_keeps_the_branch() {
    let mut f = Fake::new();
    f.destroy_wt_ok = false;
    let r = run(&f, &["--close", "drop"]);
    assert_eq!(r.code, 1);
    assert!(said(&r, "could not remove run/worktree/sp-s1 — see run/reap.log"));
    assert!(!f.called("destroy_branch") && !f.called("park"));
}

// covers: UC-dwarf-execution-26
#[test_id(t2cbfjw)]
#[test]
fn a_branch_whose_deletion_is_refused_names_the_refusal_and_fails() {
    let mut f = Fake::new();
    f.ahead = 0;
    f.destroy_branch = Err("certified-queued".into());
    let r = run(&f, &[]);
    assert_eq!(r.code, 1);
    assert!(said(&r, "could not delete spira/sp-s1 — certified-queued"));
    assert!(!f.called("label_remove"), "a branch that was not deleted keeps its label");
}

// covers: UC-dwarf-execution-26
#[test_id(tketzmz)]
#[test]
fn a_branch_that_survives_its_deletion_is_reported_and_fails() {
    let mut f = Fake::new();
    f.ahead = 0;
    f.branch_survives_destroy = true;
    let r = run(&f, &[]);
    assert_eq!(r.code, 1);
    assert!(said(&r, "verify: spira/sp-s1 still exists"));
}

// covers: UC-dwarf-execution-26
#[test_id(t348p1m)]
#[test]
fn a_bead_that_names_no_repository_has_no_work_to_remove_but_is_still_released() {
    let mut f = Fake::new();
    f.repo = None;
    let r = run(&f, &["--close", "drop"]);
    assert_eq!(r.code, 0, "{r:?}");
    assert!(said(&r, "bead names no resolvable repository — nothing to remove"));
    assert!(!f.called("destroy_worktree") && !f.called("destroy_branch"));
    assert!(f.called("drop"));
}

// covers: UC-dwarf-execution-26
#[test_id(t81ijhy)]
#[test]
fn closing_an_already_closed_bead_adds_a_note_and_releases_no_holder() {
    let f = Fake::new();
    *f.state.borrow_mut() = "DROPPED".into();
    let r = run(&f, &["--close", "second close"]);
    assert_eq!(r.code, 0, "{r:?}");
    assert!(!f.called("holder_dead"));
    assert!(f.call("note").contains("second close — Slain by the operator"));
    assert!(said(&r, "bead: sp-s1 state=DROPPED labels=spira"));
}

// covers: UC-dwarf-execution-26
#[test_id(tn3ep3f)]
#[test]
fn the_note_names_the_dwarf_its_pid_and_unit_and_says_no_attempt_was_charged() {
    let f = Fake::new();
    run(&f, &["--why", "stuck"]);
    let n = f.call("note");
    assert!(n.contains("Slain by the operator: stuck. Dwarf dwarf-test (pid 4242) stopped via spira-dwarf.service."), "{n}");
    assert!(n.ends_with("No attempt charged."));
}

// covers: UC-dwarf-execution-26
#[test_id(tq6yb34)]
#[test]
fn the_note_says_so_when_no_dwarf_was_running() {
    let mut f = Fake::new();
    f.stopped = Stopped::default();
    run(&f, &[]);
    assert!(f.call("note").contains("Dwarf ? stopped. "), "{}", f.call("note"));
}

// ---- refusals: a destructive tool fails closed on a target it never found ---------------

// covers: UC-dwarf-execution-26
#[test_id(tib3852)]
#[test]
fn a_bead_id_no_bead_carries_exits_2_and_touches_nothing() {
    let mut f = Fake::new();
    f.answers_id = String::new();
    let r = run(&f, &["--why", "probe"]);
    assert_eq!(r.code, 2);
    assert_eq!(r.err, vec!["slay.sh: no bead sp-s1 in the-store — refusing to act".to_string()]);
    assert!(!r.out.iter().any(|l| l.starts_with("slain:")));
    assert!(f.calls().iter().all(|c| c.starts_with("view")), "{:?}", f.calls());
}

// covers: UC-dwarf-execution-26
#[test_id(tukmysh)]
#[test]
fn a_store_that_answers_with_a_prefix_matched_child_is_refused_and_says_so() {
    let mut f = Fake::new();
    f.answers_id = "sp-s1.1".into();
    let r = run(&f, &[]);
    assert_eq!(r.code, 2);
    assert!(r.err.iter().any(|l| l.contains("the store answered with sp-s1.1 instead (prefix match)")));
    assert!(f.calls().iter().all(|c| c.starts_with("view")));
}

// covers: UC-dwarf-execution-26
#[test_id(tj34ivt)]
#[test]
fn positive_control_a_real_id_with_a_why_still_slays() {
    let f = Fake::new();
    let r = run(&f, &["--keep-work", "--why", "positive control"]);
    assert_eq!(r.code, 0);
    assert!(said(&r, "slain: sp-s1") && f.called("holder_dead"));
}

// ---- the process side --------------------------------------------------------------------

// covers: UC-dwarf-execution-26
#[test_id(t2r6exu)]
#[test]
fn a_dwarf_that_will_not_stop_fails_the_slaying_but_the_bead_is_still_made_true() {
    let mut f = Fake::new();
    f.stopped.fail = true;
    let r = run(&f, &[]);
    assert_eq!(r.code, 1);
    assert!(said(&r, "slay: INCOMPLETE for sp-s1") && !said(&r, "slain: sp-s1"));
    assert!(f.called("note"));
}

// covers: UC-dwarf-execution-26
#[test_id(tzdbu3i)]
#[test]
fn a_pid_still_alive_after_the_finish_fails_the_slaying() {
    let mut f = Fake::new();
    f.pid_alive = true;
    assert_eq!(run(&f, &[]).code, 1);
}

// covers: UC-dwarf-execution-26
#[test_id(ti6tc80)]
#[test]
fn a_remaining_pidfile_is_reported_and_fails() {
    let mut f = Fake::new();
    f.pidfile = true;
    let r = run(&f, &[]);
    assert_eq!(r.code, 1);
    assert!(said(&r, "verify: a pid file for sp-s1 remains"));
}

// covers: UC-dwarf-execution-26
#[test_id(t9o0hci)]
#[test]
fn a_holder_with_no_pid_file_is_stopped_by_its_session_before_the_claim_is_released() {
    let f = holder_only(&[(7, 99)]);
    let r = run(&f, &["--keep-work"]);
    assert_eq!(r.code, 0, "{:?}", r.out);
    assert!(f.called("term 7") && f.called("holder_dead sp-s1"), "{:?}", f.calls());
    assert!(said(&r, "slain: sp-s1"));
}

// covers: UC-dwarf-execution-26
#[test_id(tpj4bmk)]
#[test]
fn a_holder_that_survives_the_stop_keeps_its_claim_and_is_named() {
    let mut f = holder_only(&[(7, 99)]);
    f.survives_term = true;
    let r = run(&f, &["--keep-work"]);
    assert_eq!(r.code, 1);
    assert!(!f.called("holder_dead") && !f.called("note") && !f.called("clear_assignee"), "{:?}", f.calls());
    assert!(said(&r, "dwarf-x@7.99"), "{:?}", r.out);
}

// covers: UC-dwarf-execution-26
#[test_id(tq34noc)]
#[test]
fn a_reused_pid_with_another_start_time_is_not_signalled() {
    let f = holder_only(&[(7, 12345)]);
    let r = run(&f, &["--keep-work"]);
    assert!(!f.called("term"), "{:?}", f.calls());
    assert_eq!(r.code, 0, "{:?}", r.out);
    assert!(f.called("holder_dead sp-s1"));
}
