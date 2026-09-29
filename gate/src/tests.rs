//! The decision logic driven through a recording fake of every boundary (DESIGN.md "Boundaries").

use crate::compose::Changed;
use crate::engine::{Args, Trial, BASEFAIL, FAIL, NOVERDICT, PASS};
use crate::key;
use crate::ports::{Ctx, Merge, World};
use spira_config::GateMode;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const REPO: &str = "/r/spira";
const RUN: &str = "/run";
const BASE: &str = "local/main";
const BR: &str = "spira/sp-a";
const MERGE_SHA: &str = "m3rg3";

type UnitRuns = HashMap<(String, &'static str), (i32, String)>;

struct Fake {
    ctx: RefCell<Result<Ctx, String>>,
    readable: RefCell<HashSet<PathBuf>>,
    files: RefCell<HashMap<PathBuf, String>>,
    diff: RefCell<Result<String, String>>,
    merge: RefCell<Merge>,
    ancestor: Cell<bool>,
    blobs: RefCell<HashMap<String, Vec<u8>>>,
    bash_n_bad: RefCell<HashSet<Vec<u8>>>,
    beads: RefCell<String>,
    skew: Cell<i32>,
    admission_free: Cell<bool>,
    lock_free: Cell<bool>,
    worktree: Cell<bool>,
    /// (status, output) by the SPIRA_GATE_BRANCH the trial ran with.
    runs: RefCell<HashMap<String, (i32, String)>>,
    ran: RefCell<Vec<Vec<(String, String)>>>,
    checkouts: RefCell<Vec<String>>,
    written: RefCell<Vec<(PathBuf, String)>>,
    appended: RefCell<Vec<String>>,
    yields: RefCell<Vec<Vec<String>>>,
    lc: RefCell<Vec<Vec<String>>>,
    removed_trees: Cell<u32>,
    err: RefCell<Vec<String>>,
    clock: Cell<u64>,
    signal: Cell<bool>,
    // ---- composition (sp-2ghui)
    mode: RefCell<Result<GateMode, String>>,
    touched: RefCell<Result<Vec<Changed>, String>>,
    /// `cargo metadata` JSON by the revision the tree holds (the last checkout).
    metadata: RefCell<HashMap<String, Result<String, String>>>,
    /// (status, output) of a unit phase by (tree revision, the phase's first word after
    /// `cargo test --profile aeon -j N`): `--no-run` is the build, anything else the tests.
    unit_runs: RefCell<UnitRuns>,
    /// Every command run_gate was handed, in order.
    cmds: RefCell<Vec<String>>,
    /// Seconds each run_gate advances the clock by, per command kind.
    phase_secs: Cell<u64>,
}

fn ctx() -> Ctx {
    let mut vars = HashMap::new();
    for (k, v) in [
        ("SPIRA_REPO_MAP", "/cfg/repo-map"),
        ("SPIRA_RUN", RUN),
        ("SPIRA_VERDICT_TTL", "86400"),
        ("SPIRA_CERTIFY_PAR", "2"),
        ("HOME", "/home/u"),
        ("PATH", "/usr/bin"),
        ("LANDSTATE", "/run/landstate"),
    ] {
        vars.insert(k.to_string(), v.to_string());
    }
    Ctx {
        repo_name: "spira".into(),
        vars,
        repo_root: Some(REPO.into()),
        landref: Some(BASE.into()),
        gate_cmd: "bash spira/fence.sh && run-suites".into(),
        host_cores: "8".into(),
    }
}

impl Fake {
    fn new() -> Fake {
        let readable: HashSet<PathBuf> = [
            "/cfg/repo-map",
            "/h/exclude.sh",
            "/h/skew.sh",
            "/h/yield.sh",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        let mut runs = HashMap::new();
        runs.insert(
            MERGE_SHA.to_string(),
            (0, "test-a.sh ok\ntest-b.sh ok".to_string()),
        );
        runs.insert(BR.to_string(), (0, "test-a.sh ok".to_string()));
        runs.insert(BASE.to_string(), (0, "test-a.sh ok".to_string()));
        Fake {
            ctx: RefCell::new(Ok(ctx())),
            readable: RefCell::new(readable),
            files: RefCell::new(HashMap::new()),
            diff: RefCell::new(Ok("spira/x.sh\nsrc/a.rs".into())),
            merge: RefCell::new(Merge::Clean("mergedtree".into())),
            ancestor: Cell::new(false),
            blobs: RefCell::new(HashMap::new()),
            bash_n_bad: RefCell::new(HashSet::new()),
            beads: RefCell::new(String::new()),
            skew: Cell::new(0),
            admission_free: Cell::new(true),
            lock_free: Cell::new(true),
            worktree: Cell::new(true),
            runs: RefCell::new(runs),
            ran: RefCell::new(Vec::new()),
            checkouts: RefCell::new(Vec::new()),
            written: RefCell::new(Vec::new()),
            appended: RefCell::new(Vec::new()),
            yields: RefCell::new(Vec::new()),
            lc: RefCell::new(Vec::new()),
            removed_trees: Cell::new(0),
            err: RefCell::new(Vec::new()),
            clock: Cell::new(1_000_000),
            signal: Cell::new(false),
            mode: RefCell::new(Ok(GateMode::Suites)),
            touched: RefCell::new(Ok(Vec::new())),
            metadata: RefCell::new(HashMap::new()),
            unit_runs: RefCell::new(HashMap::new()),
            cmds: RefCell::new(Vec::new()),
            phase_secs: Cell::new(7),
        }
    }
    fn set_var(&self, k: &str, v: &str) {
        self.ctx
            .borrow_mut()
            .as_mut()
            .unwrap()
            .vars
            .insert(k.into(), v.into());
    }
    fn run(&self) -> i32 {
        Trial::new(
            self,
            Args {
                home: PathBuf::from("/h"),
                branch: BR.into(),
                repo: Some("spira".into()),
            },
        )
        .run()
    }
    fn stderr(&self) -> String {
        self.err.borrow().join("\n")
    }
    fn verdict_line(&self) -> String {
        self.err.borrow().last().cloned().unwrap_or_default()
    }
    fn env_of(&self, i: usize, k: &str) -> String {
        self.ran.borrow()[i]
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }
}

impl World for Fake {
    fn context(&self, _: Option<&str>) -> Result<Ctx, String> {
        self.ctx.borrow().clone()
    }
    fn readable(&self, p: &Path) -> bool {
        self.readable.borrow().contains(p) || self.files.borrow().contains_key(p)
    }
    fn exists(&self, p: &Path) -> bool {
        if p.ends_with(".git") {
            return self.worktree.get();
        }
        self.files.borrow().contains_key(p)
    }
    fn read(&self, p: &Path) -> Option<String> {
        self.files.borrow().get(p).cloned()
    }
    fn mkdir_p(&self, _: &Path) {}
    fn write_atomic(&self, dir: &Path, name: &str, content: &str) {
        self.written
            .borrow_mut()
            .push((dir.join(name), content.to_string()));
    }
    fn append(&self, _: &Path, line: &str) {
        self.appended.borrow_mut().push(line.to_string());
    }
    fn remove(&self, _: &Path) {}
    fn temp_file(&self, _: &str) -> Option<PathBuf> {
        Some(PathBuf::from("/tmp/files"))
    }
    fn rev_parse(&self, _: &Path, rev: &str) -> Option<String> {
        if rev.ends_with("^{tree}") {
            return Some(format!("tree-of-{}", rev.trim_end_matches("^{tree}")));
        }
        Some(rev.trim_end_matches("^{commit}").to_string())
    }
    fn diff_names(&self, _: &Path, _: &str) -> Result<String, String> {
        self.diff.borrow().clone()
    }
    fn diff_name_status(&self, _: &Path, _: &str) -> String {
        "M\tspira/x.sh\nA\tsrc/a.rs".into()
    }
    fn merge_tree(&self, _: &Path, _: &str, _: &str) -> Merge {
        self.merge.borrow().clone()
    }
    fn is_ancestor(&self, _: &Path, _: &str, _: &str) -> bool {
        self.ancestor.get()
    }
    fn commit_merge(&self, _: &Path, _: &str, _: &str, _: &str) -> Option<String> {
        Some(MERGE_SHA.into())
    }
    fn show_blob(&self, _: &Path, rev: &str, path: &str) -> Option<Vec<u8>> {
        self.blobs.borrow().get(&format!("{rev}:{path}")).cloned()
    }
    fn ls_tree_all(&self, _: &Path, rev: &str) -> String {
        format!("tree-of {rev}\n")
    }
    fn ls_tree_has(&self, _: &Path, _: &str, path: &str) -> bool {
        path != "spira/missing.sh"
    }
    fn bash_n(&self, content: &[u8]) -> Result<(), String> {
        if self.bash_n_bad.borrow().contains(content) {
            return Err("line 1: syntax error".into());
        }
        Ok(())
    }
    fn exclude_filter(&self, _: &Path, _: &str) -> String {
        self.beads.borrow().clone()
    }
    fn skew_foreign(&self, _: &Path, _: &Path, _: &str, _: &str) -> (i32, String) {
        (self.skew.get(), "skew says".into())
    }
    fn sweep(&self, _: &Path, _: &Path) {}
    fn yield_sh(&self, _: &Path, run: &str, args: &[&str]) {
        let mut v = vec![run.to_string()];
        v.extend(args.iter().map(|s| s.to_string()));
        self.yields.borrow_mut().push(v);
    }
    fn lc_certify(&self, bead: &str, tip: &str, outcome: &str, detail: &str) {
        self.lc.borrow_mut().push(
            [bead, tip, outcome, detail]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        );
    }
    fn harness_hash(&self) -> Option<String> {
        Some("harness".into())
    }
    fn nproc_all(&self) -> u64 {
        8
    }
    fn mem_avail_mib(&self) -> u64 {
        4000
    }
    fn admission_try(&self, _: &Path, _: u64) -> bool {
        self.admission_free.get()
    }
    fn tree_lock_open(&self, _: &Path) -> bool {
        true
    }
    fn tree_lock_try(&self) -> bool {
        self.lock_free.get()
    }
    fn write_holder(&self, _: &Path) {}
    fn checkout(&self, _: &Path, _: &Path, rev: &str, _: &str) -> Result<(), String> {
        self.checkouts.borrow_mut().push(rev.to_string());
        Ok(())
    }
    fn remove_worktree(&self, _: &Path, _: &Path) {
        self.removed_trees.set(self.removed_trees.get() + 1);
    }
    fn run_gate(&self, _: &Path, env: &[(String, String)], _: &str, cmd: &str) -> (i32, String) {
        self.ran.borrow_mut().push(env.to_vec());
        self.cmds.borrow_mut().push(cmd.to_string());
        let br = env
            .iter()
            .find(|(k, _)| k == "SPIRA_GATE_BRANCH")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        self.clock.set(self.clock.get() + self.phase_secs.get());
        if cmd.starts_with("cargo test") {
            let kind = if cmd.contains("--no-run") {
                "build"
            } else {
                "test"
            };
            let at = self.checkouts.borrow().last().cloned().unwrap_or_default();
            return self
                .unit_runs
                .borrow()
                .get(&(at, kind))
                .cloned()
                .unwrap_or((0, format!("{kind} ok")));
        }
        self.runs
            .borrow()
            .get(&br)
            .cloned()
            .unwrap_or((0, String::new()))
    }
    fn diff_raw(&self, _: &Path, _: &str, _: &str) -> Result<Vec<Changed>, String> {
        self.touched.borrow().clone()
    }
    fn gate_mode(&self, _: &str) -> Result<GateMode, String> {
        self.mode.borrow().clone()
    }
    fn cargo_metadata(&self, _: &Path, _: &str, _: &str) -> Result<String, String> {
        let at = self.checkouts.borrow().last().cloned().unwrap_or_default();
        self.metadata.borrow().get(&at).cloned().unwrap_or_else(|| {
            Ok(metadata_json(&[
                ("gate", &[]),
                ("spira-config", &[]),
                ("queue", &["spira-config"]),
            ]))
        })
    }
    fn now(&self) -> u64 {
        self.clock.get()
    }
    fn utc(&self) -> String {
        "2026-09-29T00:00:00Z".into()
    }
    fn sleep_ms(&self, ms: u64) {
        self.clock.set(self.clock.get() + ms.div_ceil(1000));
    }
    fn pid(&self) -> u32 {
        42
    }
    fn signalled(&self) -> bool {
        self.signal.get()
    }
    fn eprint(&self, s: &str) {
        self.err.borrow_mut().push(s.to_string());
    }
}

// ---------------------------------------------------------------------------- the merge

#[test]
fn a_branch_that_does_not_merge_is_stale_not_red() {
    let f = Fake::new();
    *f.merge.borrow_mut() = Merge::Conflict(vec!["spira/lib.sh".into(), "Cargo.lock".into()]);
    assert_eq!(f.run(), NOVERDICT);
    assert_eq!(
        f.verdict_line(),
        "gate: VERDICT=NO_VERDICT reason=conflict branch=spira/sp-a repo=spira suite=-"
    );
    let e = f.stderr();
    assert!(
        e.contains("does not merge onto local/main")
            && e.contains("gate:   spira/lib.sh")
            && e.contains("gate:   Cargo.lock"),
        "{e}"
    );
    assert!(f.ran.borrow().is_empty(), "no trial on a conflict");
    assert!(
        f.appended.borrow()[0].contains("rc=75 conflict"),
        "the conflict is metered"
    );
    assert_eq!(f.yields.borrow()[0][1..5], ["record", "spira", BR, "75"]);
}

#[test]
fn a_merge_that_cannot_be_computed_is_no_verdict() {
    let f = Fake::new();
    *f.merge.borrow_mut() = Merge::Failed("boom".into());
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=merge-failed"));
}

#[test]
fn a_stale_branch_is_judged_as_the_merge_commit() {
    let f = Fake::new();
    assert_eq!(f.run(), PASS);
    assert_eq!(
        f.checkouts.borrow()[0],
        MERGE_SHA,
        "the gate tree is the merge"
    );
    assert_eq!(
        f.env_of(0, "SPIRA_GATE_BRANCH"),
        MERGE_SHA,
        "testenv builds the merge"
    );
    assert_eq!(
        f.env_of(0, "SPIRA_GATE_SELECT_HEAD"),
        BR,
        "selection still reads the branch's changes"
    );
    assert_eq!(f.env_of(0, "SPIRA_GATE_BASE"), BASE);
    assert!(f
        .verdict_line()
        .starts_with("gate: VERDICT=PASS reason=pass"));
    assert!(f
        .stderr()
        .contains("gate PASS covered suites: test-a.sh,test-b.sh"));
}

#[test]
fn a_current_branch_is_judged_as_itself() {
    let f = Fake::new();
    f.ancestor.set(true);
    assert_eq!(f.run(), PASS);
    assert_eq!(f.checkouts.borrow()[0], BR);
    assert_eq!(f.env_of(0, "SPIRA_GATE_BRANCH"), BR);
}

#[test]
fn layer_one_reads_the_merged_content() {
    let f = Fake::new();
    f.blobs
        .borrow_mut()
        .insert(format!("{MERGE_SHA}:spira/x.sh"), b"if then".to_vec());
    f.blobs
        .borrow_mut()
        .insert(format!("{BR}:spira/x.sh"), b"true".to_vec());
    f.bash_n_bad.borrow_mut().insert(b"if then".to_vec());
    assert_eq!(f.run(), FAIL);
    assert!(f.verdict_line().contains("reason=syntax"));
    assert!(f
        .stderr()
        .contains("gate: spira/x.sh fails bash -n\nline 1: syntax error"));
}

#[test]
fn beads_data_in_the_merged_tree_is_refused() {
    let f = Fake::new();
    *f.beads.borrow_mut() = ".beads/issues.jsonl\n".into();
    assert_eq!(f.run(), FAIL);
    assert!(f.stderr().contains("gate:   .beads/issues.jsonl"));
    assert!(f.verdict_line().contains("reason=beads-data"));
}

// ---------------------------------------------------------------------------- the trial

#[test]
fn branch_red_on_a_green_base_is_the_branchs() {
    let f = Fake::new();
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (1, "test-a.sh ok\ntest-b.sh RED".into()));
    assert_eq!(f.run(), FAIL);
    assert_eq!(
        f.verdict_line(),
        "gate: VERDICT=FAIL reason=branch-red branch=spira/sp-a repo=spira suite=test-b.sh"
    );
    assert!(f
        .stderr()
        .contains("the same command passes against local/main"));
    assert_eq!(
        f.checkouts.borrow()[..],
        [MERGE_SHA.to_string(), BASE.to_string()]
    );
    assert_eq!(
        f.env_of(1, "SPIRA_VERDICT_REPEAT_CONSIDERED"),
        "base trial — confirming whether base is independently red"
    );
    assert_eq!(f.removed_trees.get(), 1, "a red tree is removed");
    assert_eq!(f.lc.borrow().len(), 0, "no bead, no certification event");
}

#[test]
fn a_red_the_base_shares_is_the_bases() {
    let f = Fake::new();
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (1, "test-b.sh RED".into()));
    f.runs
        .borrow_mut()
        .insert(BASE.into(), (1, "test-b.sh RED\ntest-c.sh RED".into()));
    assert_eq!(f.run(), BASEFAIL);
    assert_eq!(
        f.verdict_line(),
        "gate: VERDICT=BASE_FAIL reason=base-red branch=spira/sp-a repo=spira suite=test-b.sh"
    );
    let e = f.stderr();
    assert!(
        e.contains("gate: red on local/main: test-b.sh\ntest-c.sh")
            && e.contains("--- local/main's own output ---"),
        "{e}"
    );
}

#[test]
fn a_red_only_on_the_branch_is_the_branchs_even_on_a_red_base() {
    let f = Fake::new();
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (1, "test-b.sh RED\ntest-d.sh RED".into()));
    f.runs
        .borrow_mut()
        .insert(BASE.into(), (1, "test-b.sh RED".into()));
    assert_eq!(f.run(), FAIL);
    let e = f.stderr();
    assert!(e.contains("red on this branch and not on local/main: test-d.sh\ngate: local/main is red too, on: test-b.sh"), "{e}");
    assert!(f.verdict_line().ends_with("suite=test-d.sh"));
}

#[test]
fn a_base_that_only_timed_out_proves_nothing() {
    let f = Fake::new();
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (1, "test-b.sh TIMEOUT".into()));
    f.runs
        .borrow_mut()
        .insert(BASE.into(), (1, "test-b.sh TIMEOUT".into()));
    assert_eq!(f.run(), NOVERDICT);
    assert!(f
        .stderr()
        .contains("base trial timed out on test-b.sh — no verdict for spira/sp-a."));
}

#[test]
fn a_base_trial_that_did_not_run_leaves_it_untestable() {
    let f = Fake::new();
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (1, "test-b.sh RED".into()));
    f.runs
        .borrow_mut()
        .insert(BASE.into(), (124, String::new()));
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=base-untestable"));
}

#[test]
fn a_deadline_or_harness_fault_is_not_a_red() {
    for (rc, out, reason) in [
        (124, "test-a.sh ok", "timeout"),
        (75, "", "harness-fault"),
        (
            1,
            "batch: harness fault — container died mid-batch (podman gone)",
            "harness-fault",
        ),
    ] {
        let f = Fake::new();
        f.runs
            .borrow_mut()
            .insert(MERGE_SHA.into(), (rc, out.into()));
        assert_eq!(f.run(), NOVERDICT, "{reason}");
        assert!(
            f.verdict_line().contains(&format!("reason={reason}")),
            "{}",
            f.verdict_line()
        );
        assert_eq!(f.ran.borrow().len(), 1, "no base trial for {reason}");
    }
}

#[test]
fn a_silent_red_still_shows_its_status() {
    let f = Fake::new();
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (1, "  ".into()));
    assert_eq!(f.run(), FAIL);
    assert!(f
        .stderr()
        .contains("(the command printed nothing; it exited 1)"));
}

#[test]
fn an_empty_gate_string_passes_on_syntax() {
    let f = Fake::new();
    f.ctx.borrow_mut().as_mut().unwrap().gate_cmd = String::new();
    assert_eq!(f.run(), PASS);
    assert!(f.verdict_line().contains("reason=syntax-only"));
    assert!(f.ran.borrow().is_empty());
}

#[test]
fn a_gate_string_naming_a_file_the_base_lacks_is_configuration() {
    let f = Fake::new();
    f.ctx.borrow_mut().as_mut().unwrap().gate_cmd = "bash spira/missing.sh".into();
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=cmd-missing-file"));
}

#[test]
fn the_early_refusals() {
    let f = Fake::new();
    f.readable.borrow_mut().remove(Path::new("/cfg/repo-map"));
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=no-repo-map-file"));
    assert!(
        f.appended.borrow().is_empty(),
        "nothing to meter before the repository resolves"
    );

    let f = Fake::new();
    f.ctx.borrow_mut().as_mut().unwrap().repo_root = None;
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=no-repo-map"));

    let f = Fake::new();
    f.ctx.borrow_mut().as_mut().unwrap().landref = None;
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=no-base"));

    let f = Fake::new();
    *f.diff.borrow_mut() = Err("fatal: bad revision".into());
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=no-diff"));

    let f = Fake::new();
    *f.ctx.borrow_mut() = Err("exit 96".into());
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=lib-unavailable"));
}

#[test]
fn skew_init_fault_and_foreign_harness() {
    let f = Fake::new();
    f.skew.set(3);
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=skew-init-fault"));
    let f = Fake::new();
    f.skew.set(1);
    assert_eq!(f.run(), FAIL);
    assert!(f.verdict_line().contains("reason=foreign-harness"));
}

// ---------------------------------------------------------------------------- cache & key

fn key_for(f: &Fake, ejected: &str, bead: &str) -> String {
    key::gate_key(&key::KeyInputs {
        repo: "spira",
        tree: "mergedtree",
        files: &f.diff.borrow().clone().unwrap(),
        cmd: "bash spira/fence.sh && run-suites",
        harness_h: "harness",
        suites: "on",
        bead,
        ejected,
    })
}

#[test]
fn a_pass_is_cached_under_the_merged_tree() {
    let f = Fake::new();
    assert_eq!(f.run(), PASS);
    let (p, entry) = f.written.borrow()[0].clone();
    assert_eq!(
        p,
        PathBuf::from(format!("{RUN}/verdicts/{}", key_for(&f, "", "none")))
    );
    assert!(
        entry.contains("by=spira/sp-a\n")
            && entry.contains("suites=test-a.sh,test-b.sh\n")
            && entry.contains("at=1000007\n"),
        "{entry}"
    );
    assert_eq!(f.removed_trees.get(), 0, "a passing tree is kept for cargo");
}

#[test]
fn a_fresh_cached_pass_skips_admission_and_the_trial() {
    let f = Fake::new();
    let k = key_for(&f, "", "none");
    f.files.borrow_mut().insert(
        PathBuf::from(format!("{RUN}/verdicts/{k}")),
        key::render_entry(
            "2026-09-28T00:00:00Z",
            999_000,
            "aeon",
            "spira",
            BR,
            "test-a.sh",
        ),
    );
    f.admission_free.set(false);
    assert_eq!(f.run(), PASS);
    assert!(f.verdict_line().contains("reason=cached"));
    assert!(
        f.stderr()
            .contains("already passed spira's gate at 2026-09-28T00:00:00Z (aeon)")
            && f.stderr().contains("covered suites: test-a.sh")
    );
    assert!(f.ran.borrow().is_empty() && f.checkouts.borrow().is_empty());
    assert!(f.appended.borrow()[0].ends_with("rc=0 cached\n"));
}

#[test]
fn a_stale_cached_pass_runs_again() {
    let f = Fake::new();
    f.set_var("SPIRA_VERDICT_TTL", "10");
    let k = key_for(&f, "", "none");
    f.files.borrow_mut().insert(
        PathBuf::from(format!("{RUN}/verdicts/{k}")),
        key::render_entry("w", 999_000, "b", "spira", BR, "-"),
    );
    assert_eq!(f.run(), PASS);
    assert!(f.verdict_line().contains("reason=pass"));
}

#[test]
fn only_a_pass_is_cached() {
    let f = Fake::new();
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (1, "test-b.sh RED".into()));
    f.run();
    assert!(f.written.borrow().is_empty());
}

// ---------------------------------------------------------------------------- ejected suites

#[test]
fn ejected_suites_come_from_the_ejected_file_first() {
    let f = Fake::new();
    f.set_var("SPIRA_GATE_BEAD", "sp-a");
    f.files.borrow_mut().insert(
        PathBuf::from("/run/landstate/sp-a.ejected"),
        "test-x.sh,test-y.sh\nignored\n".into(),
    );
    f.files.borrow_mut().insert(
        PathBuf::from("/run/landstate/sp-a"),
        "EJECTED tip 1 test-z.sh\n".into(),
    );
    assert_eq!(f.run(), PASS);
    assert_eq!(
        f.env_of(0, "SPIRA_GATE_EJECTED_SUITES"),
        "test-x.sh,test-y.sh"
    );
    assert_eq!(
        f.written.borrow()[0].0,
        PathBuf::from(format!(
            "{RUN}/verdicts/{}",
            key_for(&f, "test-x.sh,test-y.sh", "sp-a")
        ))
    );
}

#[test]
fn ejected_suites_fall_back_to_an_ejected_landstate_row() {
    let f = Fake::new();
    f.set_var("SPIRA_GATE_BEAD", "sp-a");
    f.files.borrow_mut().insert(
        PathBuf::from("/run/landstate/sp-a"),
        "EJECTED tip 1 test-z.sh unattr\n".into(),
    );
    f.run();
    assert_eq!(f.env_of(0, "SPIRA_GATE_EJECTED_SUITES"), "test-z.sh");

    let f = Fake::new();
    f.set_var("SPIRA_GATE_BEAD", "sp-a");
    f.files.borrow_mut().insert(
        PathBuf::from("/run/landstate/sp-a"),
        "RED tip 1 test-z.sh\n".into(),
    );
    f.run();
    assert_eq!(
        f.env_of(0, "SPIRA_GATE_EJECTED_SUITES"),
        "",
        "only an EJECTED row names suites"
    );
}

// ---------------------------------------------------------------------------- admission & lock

#[test]
fn admission_times_out_as_no_verdict() {
    let f = Fake::new();
    f.admission_free.set(false);
    f.set_var("SPIRA_GATE_LOCK_WAIT", "5");
    assert_eq!(f.run(), NOVERDICT);
    assert!(f
        .stderr()
        .contains("all 2 host-wide gate admission slots busy for 5s"));
    assert!(f.ran.borrow().is_empty());
}

#[test]
fn fences_only_certification_takes_no_admission_slot() {
    let f = Fake::new();
    f.admission_free.set(false);
    f.set_var("SPIRA_GATE_SUITES", "off");
    assert_eq!(f.run(), PASS);
    assert_eq!(f.env_of(0, "SPIRA_GATE_SUITES"), "off");
}

#[test]
fn a_held_tree_times_out_and_meters_the_wait() {
    let f = Fake::new();
    f.lock_free.set(false);
    f.set_var("SPIRA_GATE_LOCK_WAIT", "3");
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=lock-timeout"));
    let row = f.appended.borrow()[0].clone();
    assert!(row.contains("waited=3s ran=0s rc=75 lock-timeout"), "{row}");
    assert_eq!(
        f.removed_trees.get(),
        0,
        "a tree it never held is not removed"
    );
}

#[test]
fn a_signal_is_no_verdict() {
    let f = Fake::new();
    f.admission_free.set(false);
    f.signal.set(true);
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=died"));
}

// ---------------------------------------------------------------------------- the finish

#[test]
fn the_meter_row_and_yield_on_a_pass() {
    let f = Fake::new();
    f.run();
    assert_eq!(
        f.appended.borrow()[0],
        "2026-09-29T00:00:00Z spira spira/sp-a waited=0s ran=7s rc=0 pass compose=suites(mode) phases=gate:7\n",
        "the composition and its phase walls trail the reason (sp-2ghui)"
    );
    assert_eq!(
        f.yields.borrow()[0],
        [RUN, "pass", "spira", BR, "tree-of-spira/sp-a"]
    );
}

#[test]
fn certification_events_follow_the_outcome() {
    for (rc, out, base, want) in [
        (0, "test-a.sh ok", 0, "pass"),
        (1, "test-b.sh RED", 0, "red"),
        (1, "test-b.sh RED", 1, "infra"),
    ] {
        let f = Fake::new();
        f.set_var("SPIRA_GATE_BEAD", "sp-a");
        f.runs
            .borrow_mut()
            .insert(MERGE_SHA.into(), (rc, out.into()));
        f.runs.borrow_mut().insert(
            BASE.into(),
            (
                base,
                if base == 0 {
                    String::new()
                } else {
                    out.to_string()
                },
            ),
        );
        f.run();
        let lc = f.lc.borrow()[0].clone();
        assert_eq!(
            lc[..3],
            ["sp-a".to_string(), BR.to_string(), want.to_string()],
            "{lc:?}"
        );
        if want == "pass" {
            assert_eq!(lc[3], key_for(&f, "", "sp-a"));
        }
    }
}

#[test]
fn a_fail_with_no_message_is_downgraded() {
    use crate::engine::{outcome, settle, Verdict};
    let v = |st: i32, r: &str, m: &str| Verdict {
        status: st,
        reason: r.into(),
        msg: m.into(),
    };
    let d = settle(v(FAIL, "syntax", " \n"));
    assert_eq!(
        (d.status, d.reason.as_str()),
        (NOVERDICT, "no-evidence:syntax")
    );
    assert!(!d.msg.is_empty());
    assert_eq!(
        settle(v(FAIL, "branch-red", "x")),
        v(FAIL, "branch-red", "x")
    );
    assert_eq!(
        settle(v(NOVERDICT, "tree-unidentified", "")),
        v(NOVERDICT, "tree-unidentified", "")
    );
    assert_eq!(
        settle(v(PASS, "syntax-only", "")),
        v(PASS, "syntax-only", "")
    );
    assert_eq!(settle(v(BASEFAIL, "base-red", "")).status, BASEFAIL);
    assert_eq!(
        [outcome(0), outcome(1), outcome(2), outcome(75), outcome(76)],
        ["PASS", "FAIL", "FAIL", "NO_VERDICT", "BASE_FAIL"]
    );
}

// ---------------------------------------------------------------------------- composition (sp-2ghui)

/// `cargo metadata --no-deps` JSON for members at `/t/<name>` with the named path deps.
fn metadata_json(members: &[(&str, &[&str])]) -> String {
    let pkgs: Vec<String> = members
        .iter()
        .map(|(n, deps)| {
            let d: Vec<String> = deps
                .iter()
                .map(|x| format!(r#"{{"name":"{x}","path":"/t/{x}"}}"#))
                .collect();
            format!(
                r#"{{"id":"{n} (path+file:///t/{n})","name":"{n}","manifest_path":"/t/{n}/Cargo.toml","dependencies":[{}]}}"#,
                d.join(",")
            )
        })
        .collect();
    let ids: Vec<String> = members
        .iter()
        .map(|(n, _)| format!(r#""{n} (path+file:///t/{n})""#))
        .collect();
    format!(
        r#"{{"workspace_root":"/t","workspace_members":[{}],"packages":[{}]}}"#,
        ids.join(","),
        pkgs.join(",")
    )
}

fn unit_fake(paths: &[&str]) -> Fake {
    let f = Fake::new();
    *f.mode.borrow_mut() = Ok(GateMode::Unit);
    *f.touched.borrow_mut() = Ok(paths
        .iter()
        .map(|p| Changed {
            path: p.to_string(),
            exec: false,
        })
        .collect());
    f
}

fn meter(f: &Fake) -> String {
    f.appended.borrow().last().cloned().unwrap_or_default()
}

#[test]
fn unit_mode_rust_only_runs_fences_then_the_touched_crates_tests_and_no_suite() {
    let f = unit_fake(&["spira-config/src/lib.rs", "docs/x.md"]);
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (0, "build-fence: make build ok".into()));
    assert_eq!(f.run(), PASS);
    let cmds = f.cmds.borrow().clone();
    assert_eq!(cmds.len(), 3, "{cmds:?}");
    assert_eq!(
        cmds[0], "bash spira/fence.sh && run-suites",
        "the gate string runs, whole"
    );
    assert_eq!(f.env_of(0, "SPIRA_GATE_SUITES"), "off", "…with suites off");
    assert_eq!(f.env_of(0, "SPIRA_CERTIFY_ALWAYS_COVERS"), "");
    // host 8 cores / SPIRA_CERTIFY_PAR 2 = 4 jobs; spira-config brings its dependent queue
    assert_eq!(
        cmds[1],
        "cargo test --profile aeon -j 4 --no-run -p queue -p spira-config"
    );
    assert_eq!(
        cmds[2],
        "cargo test --profile aeon -j 4 -p queue -p spira-config -- --test-threads=4"
    );
    assert!(f.stderr().contains(
        "gate: composition=unit — fences (suites off), then cargo test on the host for: queue spira-config (touched: spira-config)"
    ));
    assert!(
        meter(&f).ends_with(" rc=0 pass compose=unit phases=fences:7,build:7,test:7\n"),
        "{}",
        meter(&f)
    );
    let written = f.written.borrow();
    assert!(
        written[0].1.ends_with("suites=-\ncompose=unit\n"),
        "{}",
        written[0].1
    );
}

#[test]
fn unit_mode_a_bash_touching_branch_runs_todays_gate_unchanged() {
    let f = unit_fake(&["gate/src/engine.rs", "spira/lib.sh"]);
    assert_eq!(f.run(), PASS);
    assert_eq!(
        f.cmds.borrow()[..],
        ["bash spira/fence.sh && run-suites".to_string()]
    );
    assert_eq!(f.env_of(0, "SPIRA_GATE_SUITES"), "on", "suites stay on");
    assert!(
        meter(&f).contains(" compose=suites(script) phases=gate:7"),
        "{}",
        meter(&f)
    );
}

#[test]
fn unit_mode_nothing_buildable_runs_the_fences_only() {
    let f = unit_fake(&["docs/a.md", "spira/test-x.sh"]);
    assert_eq!(f.run(), PASS);
    assert_eq!(f.cmds.borrow().len(), 1);
    assert_eq!(f.env_of(0, "SPIRA_GATE_SUITES"), "off");
    assert!(
        meter(&f).contains(" compose=fences phases=fences:7"),
        "{}",
        meter(&f)
    );
}

#[test]
fn suites_mode_is_todays_gate_exactly() {
    // The default mode: the same one command, the same env, the same key as before.
    let f = Fake::new();
    *f.touched.borrow_mut() = Err("suites mode must not read the touched set".into());
    assert_eq!(f.run(), PASS);
    assert_eq!(
        f.cmds.borrow()[..],
        ["bash spira/fence.sh && run-suites".to_string()]
    );
    assert_eq!(f.env_of(0, "SPIRA_GATE_SUITES"), "on");
    assert!(meter(&f).contains(" compose=suites(mode) phases=gate:7"));
    let unit = unit_fake(&["gate/src/x.rs"]);
    assert_eq!(unit.run(), PASS);
    assert_ne!(
        f.written.borrow()[0].0,
        unit.written.borrow()[0].0,
        "a unit-mode PASS is cached under its own key"
    );
}

#[test]
fn an_unreadable_config_composes_as_suites() {
    let f = unit_fake(&["gate/src/x.rs"]);
    *f.mode.borrow_mut() = Err("repo.spira.gate_mode: unknown variant `fast`".into());
    assert_eq!(f.run(), PASS);
    assert_eq!(f.cmds.borrow().len(), 1);
    assert!(f.stderr().contains("spira.toml does not validate"));
    assert!(meter(&f).contains("compose=suites(mode)"));
}

#[test]
fn unit_mode_ejected_suites_keep_the_whole_sequence() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.set_var("SPIRA_GATE_BEAD", "sp-a");
    f.files.borrow_mut().insert(
        PathBuf::from("/run/landstate/sp-a.ejected"),
        "test-b.sh\n".into(),
    );
    assert_eq!(f.run(), PASS);
    assert_eq!(f.cmds.borrow().len(), 1);
    assert_eq!(f.env_of(0, "SPIRA_GATE_SUITES"), "on");
    assert!(meter(&f).contains("compose=suites(ejected)"));
}

#[test]
fn unit_mode_no_metadata_falls_back_to_suites() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.metadata.borrow_mut().insert(
        MERGE_SHA.into(),
        Err("error: could not find Cargo.toml".into()),
    );
    assert_eq!(f.run(), PASS);
    assert_eq!(f.cmds.borrow().len(), 1);
    assert!(meter(&f).contains("compose=suites(no-metadata)"));
}

#[test]
fn a_red_unit_test_on_a_green_base_is_the_branchs() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.unit_runs.borrow_mut().insert(
        (MERGE_SHA.into(), "test"),
        (101, "test tests::x ... FAILED".into()),
    );
    assert_eq!(f.run(), FAIL);
    assert!(f.verdict_line().contains("reason=branch-red"));
    let cmds = f.cmds.borrow().clone();
    assert_eq!(
        cmds.len(),
        6,
        "branch fences/build/test, base fences/build/test: {cmds:?}"
    );
    assert_eq!(f.checkouts.borrow()[1], BASE);
    assert!(meter(&f).contains(
        "compose=unit phases=fences:7,build:7,test:7,base-fences:7,base-build:7,base-test:7"
    ));
    assert!(f.stderr().contains("unit phase 'test' failed (exit 101)"));
}

#[test]
fn a_red_the_base_shares_in_unit_tests_is_the_bases() {
    let f = unit_fake(&["gate/src/x.rs"]);
    for at in [MERGE_SHA, BASE] {
        f.unit_runs
            .borrow_mut()
            .insert((at.into(), "build"), (101, "error[E0425]".into()));
    }
    assert_eq!(f.run(), BASEFAIL);
    assert_eq!(
        f.cmds.borrow().len(),
        4,
        "a failed build stops before the tests"
    );
}

#[test]
fn a_crate_the_base_lacks_is_not_tested_on_the_base() {
    let f = unit_fake(&["newcrate/src/lib.rs"]);
    f.metadata.borrow_mut().insert(
        MERGE_SHA.into(),
        Ok(metadata_json(&[("gate", &[]), ("newcrate", &[])])),
    );
    f.metadata
        .borrow_mut()
        .insert(BASE.into(), Ok(metadata_json(&[("gate", &[])])));
    f.unit_runs
        .borrow_mut()
        .insert((MERGE_SHA.into(), "test"), (101, "FAILED".into()));
    assert_eq!(
        f.run(),
        FAIL,
        "the base has nothing to test, so the red is the branch's"
    );
    let cmds = f.cmds.borrow().clone();
    assert_eq!(cmds.len(), 4, "base runs its fences only: {cmds:?}");
    assert!(meter(&f).contains("base-fences:7\n") || meter(&f).ends_with("base-fences:7\n"));
}

#[test]
fn a_red_fence_in_unit_mode_runs_no_unit_phase() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (1, "literal-lint: RED".into()));
    assert_eq!(f.run(), FAIL);
    // branch fences only, then base fences + build + test (base passes its fences)
    let cmds = f.cmds.borrow().clone();
    assert_eq!(cmds[0], "bash spira/fence.sh && run-suites");
    assert!(cmds[1].starts_with("bash spira/fence.sh"), "{cmds:?}");
}

#[test]
fn the_unit_phases_share_the_gate_timeout() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.set_var("SPIRA_GATE_TIMEOUT", "10");
    f.phase_secs.set(6);
    assert_eq!(f.run(), NOVERDICT);
    assert!(
        f.verdict_line().contains("reason=timeout"),
        "{}",
        f.verdict_line()
    );
    assert!(f.stderr().contains("spent before the test phase"));
}

// ---------------------------------------------------------------------- the tree certificate

const HEX_TREE: &str = "0123456789abcdef0123456789abcdef01234567";

fn hex_fake() -> Fake {
    let f = Fake::new();
    *f.merge.borrow_mut() = Merge::Clean(HEX_TREE.into());
    f
}

fn cert_written(f: &Fake) -> Option<String> {
    let want = PathBuf::from(format!("{RUN}/verdicts/trees/spira/{HEX_TREE}"));
    f.written
        .borrow()
        .iter()
        .find(|(p, _)| *p == want)
        .map(|(_, c)| c.clone())
}

#[test]
fn a_pass_certifies_the_merged_tree_it_judged() {
    let f = hex_fake();
    assert_eq!(f.run(), PASS);
    let c = cert_written(&f).expect("a PASS writes the tree certificate");
    let parsed = crate::cert::certifies(&c, "spira", HEX_TREE).expect("it certifies (spira, T)");
    assert_eq!(parsed.source, crate::cert::Source::Gate);
    assert_eq!(
        parsed.rev, MERGE_SHA,
        "the revision that carries the merged tree"
    );
    assert_eq!(parsed.branch, BR);
    assert_eq!(
        parsed.harness, "harness",
        "recorded for the reader, never matched"
    );
    assert_eq!(parsed.suites, "test-a.sh,test-b.sh");
}

#[test]
fn a_cached_pass_and_a_syntax_only_pass_certify_too() {
    let f = hex_fake();
    let k = key::gate_key(&key::KeyInputs {
        repo: "spira",
        tree: HEX_TREE,
        files: &f.diff.borrow().clone().unwrap(),
        cmd: "bash spira/fence.sh && run-suites",
        harness_h: "harness",
        suites: "on",
        bead: "none",
        ejected: "",
    });
    f.files.borrow_mut().insert(
        PathBuf::from(format!("{RUN}/verdicts/{k}")),
        key::render_entry("w", 999_000, "aeon", "spira", BR, "test-a.sh"),
    );
    assert_eq!(f.run(), PASS);
    assert!(f.verdict_line().contains("reason=cached"));
    let c = cert_written(&f).expect("a cached PASS re-certifies");
    assert!(c.contains("suites=test-a.sh\n"), "{c}");

    let f = hex_fake();
    f.ctx.borrow_mut().as_mut().unwrap().gate_cmd = String::new();
    assert_eq!(f.run(), PASS);
    assert!(
        cert_written(&f).is_some(),
        "syntax-only is the repository's whole gate"
    );
}

#[test]
fn no_verdict_other_than_pass_certifies() {
    let f = hex_fake();
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (1, "test-b.sh RED".into()));
    assert_ne!(f.run(), PASS);
    assert!(cert_written(&f).is_none());
    let f = hex_fake();
    f.admission_free.set(false);
    f.set_var("SPIRA_GATE_LOCK_WAIT", "1");
    assert_eq!(f.run(), NOVERDICT);
    assert!(cert_written(&f).is_none());
}
