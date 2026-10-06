//! The decision logic driven through a recording fake of every boundary (DESIGN.md "Boundaries").

use crate::compose::Changed;
use crate::engine::{Args, Trial, BASEFAIL, BASE_RERUN_MARK, FAIL, NOVERDICT, PASS};
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
    cite: Cell<i32>,
    admission_free: Cell<bool>,
    admission_who: RefCell<Vec<String>>,
    lock_free: Cell<bool>,
    worktree: Cell<bool>,
    /// (status, output) by the SPIRA_GATE_BRANCH the trial ran with.
    runs: RefCell<HashMap<String, (i32, String)>>,
    ran: RefCell<Vec<Vec<(String, String)>>>,
    checkouts: RefCell<Vec<String>>,
    written: RefCell<Vec<(PathBuf, String)>>,
    appended: RefCell<Vec<String>>,
    /// run/tsd rows (telemetry.rs), kept apart from the gate.log meter.
    tsd: RefCell<Vec<String>>,
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
    /// `cargo build …` is the build, `cargo test …` the tests.
    unit_runs: RefCell<UnitRuns>,
    /// Every command run_gate was handed, in order.
    cmds: RefCell<Vec<String>>,
    /// Seconds each run_gate advances the clock by, per command kind.
    phase_secs: Cell<u64>,
    // ---- per-suite base judgement (sp-hh5h0)
    /// What `<landing ref>^{commit}` resolves to on each successive read; the last repeats.
    /// Empty: the ref is its own commit (the name).
    landref_tips: RefCell<Vec<String>>,
    /// (status, output) of the base re-run by (the revision, the `--suites` list); absent,
    /// every named suite passes.
    reruns: RefCell<HashMap<(String, String), (i32, String)>>,
    /// Suites the base does not have (`ls_tree_has spira/<suite>` is false).
    base_lacks: RefCell<HashSet<String>>,
    /// (status, output) of the re-entry phase by the tree revision (sp-p3srm); absent, every
    /// suite it names reports `ok`.
    reentry_runs: RefCell<HashMap<String, (i32, String)>>,
    // ---- positive controls (sp-ufbkh)
    /// Fences that exit 0 without their `fence: <name> checked` line. Every other fence the
    /// gate string names prints `checked 1` when the string exits 0, as a real one does.
    silent_fences: RefCell<HashSet<String>>,
    /// The tree each gate-string run happened in.
    trees: RefCell<Vec<PathBuf>>,
    // ---- the tree owns its gate (sp-quu2w)
    /// `<rev>:<path>` a revision does not carry (`ls_tree_has` is false for it).
    gone: RefCell<HashSet<String>>,
    // ---- tools keyed by the tree (sp-g9f3t)
    /// (status, output) a gate-string run returns when its SPIRA_LINT_BIN is this path —
    /// a binary that would answer differently from the one built from the tree under test.
    lint_by_bin: RefCell<HashMap<String, (i32, String)>>,
    /// What the gate tree's `HEAD^{tree}` reads instead of the last checkout's tree.
    tree_drift: RefCell<Option<String>>,
    /// Apply `tree_drift` only once this many checkouts have happened (0 = always).
    drift_after_checkouts: Cell<usize>,
    /// Every install_tools call: (dir, tree id). An Err to return instead, when set.
    installs: RefCell<Vec<(PathBuf, String)>>,
    install_err: RefCell<Option<String>>,
    // ---- tools keyed by their source closure (toolkey.rs)
    /// `<rev>:<path>` object ids rev_parse answers (absent: the default answer).
    objects: RefCell<HashMap<String, String>>,
    /// What tool_inputs answers (absent: Err, so nothing is published).
    tool_inputs: RefCell<Option<Vec<String>>>,
    /// Every install_tools_from call's source; an Err to return instead, when set.
    installs_from: RefCell<Vec<PathBuf>>,
    install_from_err: RefCell<Option<String>>,
    /// Every publish_tools call: (store, name, inputs).
    published: RefCell<Vec<(PathBuf, String, String)>>,
    // ---- build IO (sp-z61hj)
    /// What build_wrapper answers; the (path, setting) it was asked with.
    wrapper: RefCell<Result<spira_config::build::Wrapper, String>>,
    wrapper_asked: RefCell<Vec<(String, String)>>,
    /// What target_on_tmpfs answers; the trees it was asked for.
    target_err: RefCell<Option<String>>,
    /// The real move deletes a pre-tmpfs `target/gate-tools` from the disk; set to model it.
    target_clears_tools: Cell<bool>,
    targets: RefCell<Vec<PathBuf>>,
    released: RefCell<Vec<(PathBuf, bool)>>,
    reserve_err: RefCell<Option<String>>,
    // ---- the base-suite cache (sp-kqger)
    /// What `testenv container tag` answers; a real image tag by default so every test not
    /// about this cache specifically exercises it exactly as it would for real.
    image_tag: RefCell<(i32, String)>,
    /// `<BASE>^{tree}` answers this instead of the default `tree-of-<BASE>` (not a real git
    /// object id, so `basecache::path` refuses it) when a test needs a real-looking tree id
    /// to exercise the cache's own file path.
    base_tree_override: RefCell<Option<String>>,
    // ---- admission (sp-q20wb)
    /// What `certify_par_live` answers once `certify_par_live_after` calls have passed;
    /// None (the default, immediately) falls back to `Ctx`'s frozen `SPIRA_CERTIFY_PAR`,
    /// exactly as every test not about the live re-read expects.
    certify_par_live: Cell<Option<u64>>,
    /// Calls to `certify_par_live` before it stops answering None and starts answering
    /// `certify_par_live` — the fixture for "the file is edited while a gate already waits."
    certify_par_live_after: Cell<u32>,
    /// `admission_try` fails this many times (across every slot tried) before it answers
    /// `admission_free` — a countdown, so a test can make the wait last some real ticks
    /// before the slot is granted, rather than either instantly free or never free.
    admission_free_after: Cell<u32>,
    /// When set, only this exact slot number is ever free — every other slot always
    /// reports busy, regardless of `admission_free`/`admission_free_after`.
    admission_only_slot_free: Cell<Option<u64>>,
}

fn ctx() -> Ctx {
    let mut vars = HashMap::new();
    for (k, v) in [
        ("SPIRA_REPO_MAP", "/cfg/repo-map"),
        ("SPIRA_RUN", RUN),
        ("SPIRA_VERDICT_TTL", "86400"),
        ("SPIRA_CERTIFY_PAR", "2"),
        ("HOME", "/home/u"),
        ("SPIRA_RELEASE", "/rel"),
        // The box's own tool tail (sp-c7b85) — cargo, for the tree builds a gate step runs.
        ("SPIRA_PATH", "/box/.cargo/bin"),
        // ONE SOURCE (per Ryan 2026-10-05): engine.rs no longer falls back to a literal
        // default when one of these is absent from `Ctx.vars` — that is now `cfg`'s job,
        // once, at the real `Real::context()`. The fixture stands in for that resolved
        // value here, at `spira/conf.d`'s own default (`SPIRA_GATE_SUITES`'s registered
        // default is a file path; "on" is this suite's own convention for "not off", and
        // the engine only ever tests this value against the literal "off").
        ("SPIRA_GATE_TIMEOUT", "2700"),
        ("SPIRA_GATE_SUITES", "on"),
        ("SPIRA_GATE_BUDGET", "300"),
        ("SPIRA_GATE_DEADLINE", "300"),
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
            "/h/skew",
            "/h/commit-cite.sh",
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
            cite: Cell::new(0),
            admission_free: Cell::new(true),
            admission_who: RefCell::new(Vec::new()),
            lock_free: Cell::new(true),
            worktree: Cell::new(true),
            runs: RefCell::new(runs),
            ran: RefCell::new(Vec::new()),
            checkouts: RefCell::new(Vec::new()),
            written: RefCell::new(Vec::new()),
            appended: RefCell::new(Vec::new()),
            tsd: RefCell::new(Vec::new()),
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
            landref_tips: RefCell::new(Vec::new()),
            reruns: RefCell::new(HashMap::new()),
            base_lacks: RefCell::new(HashSet::new()),
            reentry_runs: RefCell::new(HashMap::new()),
            silent_fences: RefCell::new(HashSet::new()),
            trees: RefCell::new(Vec::new()),
            gone: RefCell::new(HashSet::new()),
            lint_by_bin: RefCell::new(HashMap::new()),
            tree_drift: RefCell::new(None),
            drift_after_checkouts: Cell::new(0),
            installs: RefCell::new(Vec::new()),
            install_err: RefCell::new(None),
            objects: RefCell::default(),
            tool_inputs: RefCell::default(),
            installs_from: RefCell::default(),
            install_from_err: RefCell::default(),
            published: RefCell::default(),
            wrapper: RefCell::new(Ok(spira_config::build::Wrapper::Sccache(PathBuf::from("/box/.cargo/bin/sccache")))),
            wrapper_asked: RefCell::new(Vec::new()),
            target_err: RefCell::new(None),
            target_clears_tools: Cell::new(false),
            reserve_err: RefCell::new(None),
            targets: RefCell::new(Vec::new()),
            released: RefCell::new(Vec::new()),
            image_tag: RefCell::new((0, "tag1".into())),
            base_tree_override: RefCell::new(None),
            certify_par_live: Cell::new(None),
            certify_par_live_after: Cell::new(0),
            admission_free_after: Cell::new(0),
            admission_only_slot_free: Cell::new(None),
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
        self.run_with(false)
    }
    fn run_with(&self, release_bins: bool) -> i32 {
        Trial::new(
            self,
            Args {
                home: PathBuf::from("/h"),
                branch: BR.into(),
                repo: Some("spira".into()),
                release_bins,
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
    /// The fake PATH is one directory, /h: a script "is on PATH" when /h/<name> is readable.
    fn which(&self, name: &str) -> Option<PathBuf> {
        let p = Path::new("/h").join(name);
        self.readable(&p).then_some(p)
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
    fn append(&self, p: &Path, line: &str) {
        if p.extension().is_some_and(|e| e == "jsonl") {
            self.tsd.borrow_mut().push(line.to_string());
            return;
        }
        self.appended.borrow_mut().push(line.to_string());
    }
    fn remove(&self, _: &Path) {}
    fn temp_file(&self, _: &str) -> Option<PathBuf> {
        Some(PathBuf::from("/tmp/files"))
    }
    fn rev_parse(&self, _: &Path, rev: &str) -> Option<String> {
        if let Some(id) = self.objects.borrow().get(rev) {
            return Some(id.clone()).filter(|i| !i.is_empty());
        }
        if rev == "HEAD^{tree}" {
            if let Some(d) = self.tree_drift.borrow().clone() {
                if self.checkouts.borrow().len() > self.drift_after_checkouts.get() {
                    return Some(d);
                }
            }
            let at = self.checkouts.borrow().last().cloned().unwrap_or_default();
            return Some(format!("tree-of-{at}"));
        }
        if rev == format!("{BASE}^{{tree}}") {
            if let Some(t) = self.base_tree_override.borrow().clone() {
                return Some(t);
            }
        }
        if rev.ends_with("^{tree}") {
            return Some(format!("tree-of-{}", rev.trim_end_matches("^{tree}")));
        }
        if rev == format!("{BASE}^{{commit}}") {
            let mut tips = self.landref_tips.borrow_mut();
            if let Some(t) = tips.first().cloned() {
                if tips.len() > 1 {
                    tips.remove(0);
                }
                return Some(t);
            }
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
    fn ls_tree_has(&self, _: &Path, rev: &str, path: &str) -> bool {
        !self.gone.borrow().contains(&format!("{rev}:{path}"))
            && path != "spira/missing.sh"
            && !path
                .strip_prefix("spira/")
                .is_some_and(|s| self.base_lacks.borrow().contains(s))
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
    fn commit_cite(&self, _: &Path, _: &Path, _: &str, _: &str) -> (i32, String) {
        (self.cite.get(), "abc cites sp-nope".into())
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
    fn certify_par_live(&self) -> Option<u64> {
        let left = self.certify_par_live_after.get();
        if left > 0 {
            self.certify_par_live_after.set(left - 1);
            return None;
        }
        self.certify_par_live.get()
    }
    fn admission_try(&self, _: &Path, slot: u64, who: &str) -> bool {
        self.admission_who.borrow_mut().push(who.to_string());
        if let Some(only) = self.admission_only_slot_free.get() {
            return slot == only;
        }
        let left = self.admission_free_after.get();
        if left > 0 {
            self.admission_free_after.set(left - 1);
            return false;
        }
        self.admission_free.get()
    }
    fn admission_wait_line(&self, _: &str, par: u64) -> String {
        format!("waiting for a gate slot: {par} of {par} held by fake")
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
    fn build_wrapper(&self, path: &str, setting: &str) -> Result<spira_config::build::Wrapper, String> {
        self.wrapper_asked.borrow_mut().push((path.to_string(), setting.to_string()));
        self.wrapper.borrow().clone()
    }
    fn target_on_tmpfs(&self, tree: &Path, _: &str, _: &str, _: &crate::target::Limits) -> Result<String, String> {
        self.targets.borrow_mut().push(tree.to_path_buf());
        if self.target_clears_tools.get() {
            let tools = tree.join("target/gate-tools");
            self.files.borrow_mut().retain(|p, _| !p.starts_with(&tools));
        }
        match self.target_err.borrow().clone() {
            Some(e) => Err(e),
            None => Ok(format!("gate: build on tmpfs at /tmp/t/{}", tree.file_name().unwrap().to_string_lossy())),
        }
    }
    fn release_target(&self, tree: &Path, keep_release: bool) {
        self.released.borrow_mut().push((tree.to_path_buf(), keep_release));
    }
    fn reserve_scratch(&self, _: &Path, _: &str, _: &str, _: &crate::target::Limits, _: &str, _: u64) -> Result<Box<dyn std::any::Any>, String> {
        match self.reserve_err.borrow().clone() {
            Some(e) => Err(e),
            None => Ok(Box::new(())),
        }
    }
    fn install_tools(&self, _: &Path, pkgs: &[String], dir: &Path, id: &str) -> Result<(), String> {
        if let Some(e) = self.install_err.borrow().clone() {
            return Err(e);
        }
        self.installs.borrow_mut().push((dir.to_path_buf(), id.to_string()));
        let mut files = self.files.borrow_mut();
        files.retain(|p, _| !p.starts_with(dir.parent().unwrap()) || p.starts_with(dir));
        files.insert(dir.join("TREE"), format!("{id}\n"));
        for p in pkgs {
            files.insert(dir.join(p), "#!built".into());
        }
        Ok(())
    }
    fn install_tools_from(&self, src: &Path, pkgs: &[String], dir: &Path, id: &str) -> Result<(), String> {
        if let Some(e) = self.install_from_err.borrow().clone() {
            return Err(e);
        }
        self.installs_from.borrow_mut().push(src.to_path_buf());
        self.installs.borrow_mut().push((dir.to_path_buf(), id.to_string()));
        let mut files = self.files.borrow_mut();
        files.insert(dir.join("TREE"), format!("{id}\n"));
        for p in pkgs {
            let from = files.get(&src.join(p)).cloned().unwrap_or_default();
            files.insert(dir.join(p), from);
        }
        Ok(())
    }
    fn tool_inputs(&self, _: &Path, _: &[String]) -> Result<Vec<String>, String> {
        self.tool_inputs.borrow().clone().ok_or_else(|| "no dep-info".to_string())
    }
    fn tool_entries(&self, store: &Path, base: &str) -> Vec<PathBuf> {
        let prefix = format!("{base}-");
        let mut v: Vec<PathBuf> = self
            .files
            .borrow()
            .keys()
            .filter(|p| p.parent() == Some(store) && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&prefix)))
            .cloned()
            .collect();
        v.sort();
        v
    }
    fn publish_tools(&self, from: &Path, pkgs: &[String], store: &Path, name: &str, inputs: &str, _: usize) -> Result<(), String> {
        self.published.borrow_mut().push((store.to_path_buf(), name.to_string(), inputs.to_string()));
        let mut files = self.files.borrow_mut();
        let e = store.join(name);
        files.insert(e.clone(), String::new());
        files.insert(e.join("KEY"), format!("{name}\n"));
        files.insert(e.join("INPUTS"), inputs.to_string());
        for p in pkgs {
            let b = files.get(&from.join(p)).cloned().unwrap_or_default();
            files.insert(e.join(p), b);
        }
        Ok(())
    }
    fn remove_worktree(&self, _: &Path, _: &Path) {
        self.removed_trees.set(self.removed_trees.get() + 1);
    }
    fn run_gate(&self, tree: &Path, env: &[(String, String)], _: &str, cmd: &str) -> (i32, String) {
        self.ran.borrow_mut().push(env.to_vec());
        self.cmds.borrow_mut().push(cmd.to_string());
        self.clock.set(self.clock.get() + self.phase_secs.get());
        // sp-kqger: the base-suite cache's fourth key component, queried once per base trial
        // that has an uncached red suite to ask about. Kept apart from the `--suites`/`cargo`
        // branches below so it is never mistaken for a rerun or a unit phase.
        if cmd == "\"${SPIRA_TESTENV_BIN:-testenv}\" container tag" {
            return self.image_tag.borrow().clone();
        }
        let br = env
            .iter()
            .find(|(k, _)| k == "SPIRA_GATE_BRANCH")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        if let Some(list) = cmd
            .split_once("--suites ")
            .and_then(|(_, r)| r.split_whitespace().next())
        {
            // The base re-run (sp-hh5h0) names itself; anything else is the re-entry phase.
            if cmd.starts_with(BASE_RERUN_MARK) {
                if let Some(r) = self.reruns.borrow().get(&(br.clone(), list.to_string())) {
                    return r.clone();
                }
                let lines: Vec<String> = list
                    .split(',')
                    .map(|s| format!("  {s:<32} ok      1s"))
                    .collect();
                return (0, lines.join("\n"));
            }
            let at = self.checkouts.borrow().last().cloned().unwrap_or_default();
            if let Some(r) = self.reentry_runs.borrow().get(&at) {
                return r.clone();
            }
            let out: Vec<String> = list
                .split(',')
                .map(|s| format!("  {s:<32} ok      3s"))
                .collect();
            return (0, out.join("\n") + "\nVERDICT GREEN");
        }
        if cmd.starts_with("cargo ") {
            let kind = if cmd.starts_with("cargo build") {
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
        self.trees.borrow_mut().push(tree.to_path_buf());
        let lint = env.iter().find(|(k, _)| k == "SPIRA_LINT_BIN").map(|(_, v)| v.clone());
        if let Some(r) = lint.and_then(|l| self.lint_by_bin.borrow().get(&l).cloned()) {
            return r;
        }
        let (rc, out) = self
            .runs
            .borrow()
            .get(&br)
            .cloned()
            .unwrap_or((0, String::new()));
        if rc != 0 {
            return (rc, out);
        }
        // The fences ran and passed: each prints its positive control, unless told not to.
        let silent = self.silent_fences.borrow();
        let mut lines: Vec<String> = crate::fence::expected(cmd)
            .into_iter()
            .filter(|f| !silent.contains(f))
            .map(|f| format!("fence: {f} checked 1 files"))
            .collect();
        if !out.is_empty() {
            lines.push(out);
        }
        (0, lines.join("\n"))
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
fn a_branch_that_does_not_merge_is_red_no_rebase_never_no_verdict() {
    let f = Fake::new();
    *f.merge.borrow_mut() = Merge::Conflict(vec!["spira/lib.sh".into(), "Cargo.lock".into()]);
    assert_eq!(f.run(), FAIL);
    assert_eq!(
        f.verdict_line(),
        "gate: VERDICT=FAIL reason=no-rebase branch=spira/sp-a repo=spira suite=-"
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
        f.appended.borrow()[0].contains("rc=1 no-rebase"),
        "the conflict is metered"
    );
    assert_eq!(f.yields.borrow()[0][1..5], ["record", "spira", BR, "1"]);
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
fn the_gate_command_gets_the_launcher_path_set_outright_from_spira_release() {
    let f = Fake::new();
    f.ancestor.set(true);
    f.set_var("PATH", "/inherited/.cargo/bin:/checkout/target/release:/usr/bin");
    assert_eq!(f.run(), PASS);
    assert_eq!(
        f.env_of(0, "PATH"),
        format!("/rel/bin:/rel/spira:/usr/local/bin:/usr/bin:/bin:{}", ctx().var("SPIRA_PATH")),
        "the release's bin/ and spira/, the system dirs, then the box's own tool tail — nothing inherited"
    );
    assert_eq!(f.env_of(0, "SPIRA_RELEASE"), "/rel");
}

#[test]
fn a_path_tail_entry_inside_a_release_is_no_verdict_naming_it_and_nothing_runs() {
    let f = Fake::new();
    f.ancestor.set(true);
    f.set_var("SPIRA_PATH", "/x/spira-releases/def/bin");
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=release-unset"), "{}", f.verdict_line());
    assert!(f.stderr().contains("spira-releases"), "{}", f.stderr());
    assert!(f.ran.borrow().is_empty(), "no gate command ran");
}

#[test]
fn an_unset_spira_release_is_no_verdict_naming_it_and_nothing_runs() {
    let f = Fake::new();
    f.ancestor.set(true);
    f.set_var("SPIRA_RELEASE", "");
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=release-unset"), "{}", f.verdict_line());
    assert!(f.stderr().contains("SPIRA_RELEASE is not set"));
    assert!(f.ran.borrow().is_empty(), "no gate command ran");
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
    f.reruns.borrow_mut().insert((BASE.into(), "test-b.sh,test-c.sh".into()), (1, "test-b.sh RED\ntest-c.sh RED".into()));
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

// ------------------------------------------- each red judged on the base on that suite (sp-hh5h0)

const SUITE: &str = "test-gate-verdict.sh";

fn red(suite: &str) -> String {
    format!("  {suite:<32} RED     rc=1 after 31s")
}

fn meter_row(f: &Fake) -> String {
    f.appended.borrow().last().cloned().unwrap_or_default()
}

/// The incident (gate.log 2026-09-29T19:45:58Z, concierge/sp-b4oct): the landing ref moved
/// between the branch trial and the base trial. The base trial must judge the commit the
/// merge was cut from, where the suite is red too — not the ref's new tip, which fixed it.
#[test]
fn the_base_trial_judges_the_base_the_merge_was_cut_from() {
    let f = Fake::new();
    *f.landref_tips.borrow_mut() = vec!["old-base".into(), "new-base".into()];
    f.runs.borrow_mut().insert(
        MERGE_SHA.into(),
        (1, format!("  test-a.sh ok\n{}", red(SUITE))),
    );
    f.runs.borrow_mut().insert(
        "old-base".into(),
        (1, format!("  test-a.sh ok\n{}", red(SUITE))),
    );
    f.runs.borrow_mut().insert(
        "new-base".into(),
        (0, format!("  test-a.sh ok\n  {SUITE} ok")),
    );
    f.reruns.borrow_mut().insert(("old-base".into(), SUITE.into()), (1, red(SUITE)));
    assert_eq!(f.run(), BASEFAIL);
    assert_eq!(
        f.verdict_line(),
        format!(
            "gate: VERDICT=BASE_FAIL reason=base-red branch=spira/sp-a repo=spira suite={SUITE}"
        )
    );
    assert_eq!(
        f.checkouts.borrow()[..],
        [MERGE_SHA.to_string(), "old-base".to_string()],
        "the base trial checks out the pinned commit"
    );
    assert_eq!(f.env_of(0, "SPIRA_GATE_BASE"), "old-base");
    assert_eq!(f.env_of(1, "SPIRA_GATE_BASE"), "old-base");
    assert_eq!(f.env_of(1, "SPIRA_GATE_BRANCH"), "old-base");
}

/// Acceptance 1: the branch trial and the base trial are both red on the same suite.
#[test]
fn the_same_suite_red_on_the_branch_and_the_base_is_base_red() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(
        MERGE_SHA.into(),
        (1, format!("  test-a.sh ok\n{}", red(SUITE))),
    );
    f.runs
        .borrow_mut()
        .insert(BASE.into(), (1, format!("  test-a.sh ok\n{}", red(SUITE))));
    f.reruns.borrow_mut().insert((BASE.into(), SUITE.into()), (1, red(SUITE)));
    assert_eq!(f.run(), BASEFAIL);
    assert!(
        f.verdict_line().contains("reason=base-red"),
        "{}",
        f.verdict_line()
    );
    assert!(f.verdict_line().ends_with(&format!("suite={SUITE}")));
    assert_eq!(f.ran.borrow().len(), 3, "the base ran it: only the confirming retry follows");
}

/// Acceptance 2: the base trial did not select the branch's red suite. It is run on the
/// base by name before judging; red there too → base-red, not branch-red.
#[test]
fn a_red_the_base_trial_did_not_run_is_run_on_the_base_first() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(
        MERGE_SHA.into(),
        (1, format!("  test-a.sh ok\n{}", red(SUITE))),
    );
    f.runs
        .borrow_mut()
        .insert(BASE.into(), (0, "  test-a.sh ok".into()));
    f.reruns
        .borrow_mut()
        .insert((BASE.into(), SUITE.into()), (1, red(SUITE)));
    assert_eq!(f.run(), BASEFAIL);
    assert!(
        f.verdict_line().contains("reason=base-red"),
        "{}",
        f.verdict_line()
    );
    let cmds = f.cmds.borrow();
    assert_eq!(
        cmds.len(),
        5,
        "branch trial, base trial, the cache's image tag, base re-run, confirming retry: {cmds:?}"
    );
    assert_eq!(cmds[2], "\"${SPIRA_TESTENV_BIN:-testenv}\" container tag", "sp-kqger: the cache is consulted first");
    assert!(
        cmds[3].contains(&format!(
            "\"${{SPIRA_TESTENV_BIN:-testenv}}\" --suites {SUITE} \"$SPIRA_GATE_BRANCH\""
        )),
        "{}",
        cmds[3]
    );
    assert!(
        !cmds[3].contains("--deadline"),
        "the re-run is not cut by a deadline"
    );
    assert_eq!(f.env_of(3, "SPIRA_GATE_BRANCH"), BASE, "re-run on the base");
    assert!(f
        .stderr()
        .contains(&format!("gate: re-ran on local/main (local/main): {SUITE}")));
    assert!(meter_row(&f).contains(",base-rerun:7"), "{}", meter_row(&f));
    assert!(meter_row(&f).contains("base-image-tag:7"), "{}", meter_row(&f));
}

/// A base red that does not reproduce on a second run is a flake: re-gated, never held.
#[test]
fn a_base_red_that_does_not_reproduce_is_a_flake_not_a_hold() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, red(SUITE)));
    f.runs.borrow_mut().insert(BASE.into(), (1, red(SUITE)));
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=base-flake"), "{}", f.verdict_line());
    assert!(f.verdict_line().ends_with(&format!("suite={SUITE}")));
    assert!(meter_row(&f).contains("base-retry:7"), "{}", meter_row(&f));
}

/// The same red, reproduced by the retry, is BASE_FAIL.
#[test]
fn a_base_red_that_reproduces_is_base_fail() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, red(SUITE)));
    f.runs.borrow_mut().insert(BASE.into(), (1, red(SUITE)));
    f.reruns.borrow_mut().insert((BASE.into(), SUITE.into()), (1, red(SUITE)));
    assert_eq!(f.run(), BASEFAIL);
    assert!(f.verdict_line().contains("reason=base-red"), "{}", f.verdict_line());
}

/// The re-run finds the suite green on the base: now, and only now, it is the branch's.
#[test]
fn a_red_the_base_re_run_passes_is_the_branchs() {
    let f = Fake::new();
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (1, red(SUITE)));
    f.runs
        .borrow_mut()
        .insert(BASE.into(), (0, "  test-a.sh ok".into()));
    assert_eq!(f.run(), FAIL);
    assert!(f.verdict_line().ends_with(&format!("suite={SUITE}")));
    assert_eq!(
        f.cmds.borrow().len(),
        4,
        "branch trial, base trial, the cache's image tag, base re-run: {:?}",
        f.cmds.borrow()
    );
}

// ------------------------------------------------- the base-suite cache (sp-kqger)

/// A real-looking git tree id (40 hex chars) — the cache's own directory component,
/// `basecache::path` refuses anything else. The default fixture's `tree-of-<rev>` stands in
/// for the base tree everywhere else; these tests need it real enough to build a path.
const BASE_TREE_HEX: &str = "0123456789abcdef0123456789abcdef01234567";

fn base_cache_path(suite: &str) -> PathBuf {
    PathBuf::from(format!("/run/verdicts/base-suites/spira/{BASE_TREE_HEX}/{suite}"))
}

/// A full cache hit answers every red suite without running anything extra on the base — the
/// acceptance this bead exists for: "a flip-suspect (base green in cache, branch red) re-runs
/// nothing extra."
#[test]
fn a_fresh_base_suite_cache_hit_skips_the_rerun_entirely() {
    let f = Fake::new();
    *f.base_tree_override.borrow_mut() = Some(BASE_TREE_HEX.into());
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, red(SUITE)));
    f.runs
        .borrow_mut()
        .insert(BASE.into(), (0, "  test-a.sh ok".into()));
    f.files.borrow_mut().insert(
        base_cache_path(SUITE),
        crate::basecache::render(true, "harness", "tag1", "2026-09-30T00:00:00Z", 100),
    );
    assert_eq!(f.run(), FAIL, "{}", f.stderr());
    assert!(f.verdict_line().ends_with(&format!("suite={SUITE}")));
    assert!(
        f.cmds.borrow().iter().any(|c| c == "\"${SPIRA_TESTENV_BIN:-testenv}\" container tag"),
        "the cache is still consulted: {:?}",
        f.cmds.borrow()
    );
    assert!(
        !f.cmds.borrow().iter().any(|c| c.contains("--suites")),
        "a full cache hit re-runs nothing extra: {:?}",
        f.cmds.borrow()
    );
}

/// A cached base-red is exactly as informative as a freshly-run one: it still names the
/// suite, and BASEFAIL still follows.
#[test]
fn a_cached_base_red_still_names_the_suite() {
    let f = Fake::new();
    *f.base_tree_override.borrow_mut() = Some(BASE_TREE_HEX.into());
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, red(SUITE)));
    f.runs
        .borrow_mut()
        .insert(BASE.into(), (0, "  test-a.sh ok".into()));
    f.files.borrow_mut().insert(
        base_cache_path(SUITE),
        crate::basecache::render(false, "harness", "tag1", "2026-09-30T00:00:00Z", 100),
    );
    f.reruns.borrow_mut().insert((BASE.into(), SUITE.into()), (1, red(SUITE)));
    assert_eq!(f.run(), BASEFAIL);
    assert!(f.verdict_line().contains("reason=base-red"), "{}", f.verdict_line());
    assert!(
        f.verdict_line().ends_with(&format!("suite={SUITE}")),
        "a cached red still names the suite: {}",
        f.verdict_line()
    );
    assert_eq!(
        f.cmds.borrow().iter().filter(|c| c.contains("--suites")).count(),
        1,
        "only the confirming retry runs: {:?}",
        f.cmds.borrow()
    );
}

/// FAIL CLOSED: an entry recorded under a different harness or testenv image is a miss, not
/// a wrong answer with the right shape — the gate runs the suite rather than trust it.
#[test]
fn a_cache_entry_under_a_different_harness_is_a_miss_and_runs() {
    let f = Fake::new();
    *f.base_tree_override.borrow_mut() = Some(BASE_TREE_HEX.into());
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, red(SUITE)));
    f.runs
        .borrow_mut()
        .insert(BASE.into(), (0, "  test-a.sh ok".into()));
    f.files.borrow_mut().insert(
        base_cache_path(SUITE),
        // Recorded PASS, but under a harness this trial's is not — must not be trusted.
        crate::basecache::render(true, "an-older-harness", "tag1", "2026-09-30T00:00:00Z", 100),
    );
    f.reruns.borrow_mut().insert(
        (BASE.into(), SUITE.into()),
        (0, format!("  {SUITE:<32} ok      1s")),
    );
    assert_eq!(f.run(), FAIL, "the stale entry is a miss: the fresh rerun answers green");
    assert!(
        f.cmds.borrow().iter().any(|c| c.contains("--suites")),
        "a mismatched entry still runs: {:?}",
        f.cmds.borrow()
    );
}

/// An unreadable image tag (the query failed) never trusts any cache entry, matching or not.
#[test]
fn an_unresolved_image_tag_never_trusts_the_cache() {
    let f = Fake::new();
    *f.base_tree_override.borrow_mut() = Some(BASE_TREE_HEX.into());
    *f.image_tag.borrow_mut() = (1, String::new());
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, red(SUITE)));
    f.runs
        .borrow_mut()
        .insert(BASE.into(), (0, "  test-a.sh ok".into()));
    f.files.borrow_mut().insert(
        base_cache_path(SUITE),
        crate::basecache::render(true, "harness", "tag1", "2026-09-30T00:00:00Z", 100),
    );
    f.reruns.borrow_mut().insert(
        (BASE.into(), SUITE.into()),
        (0, format!("  {SUITE:<32} ok      1s")),
    );
    assert_eq!(f.run(), FAIL, "no image tag, so no lookup: the fresh rerun answers green");
    assert!(f.cmds.borrow().iter().any(|c| c.contains("--suites")), "fail closed: still runs");
}

/// A fresh rerun (no prior cache entry) warms the cache for the next gate on this base tree.
#[test]
fn a_fresh_rerun_warms_the_cache_for_the_next_gate() {
    let f = Fake::new();
    *f.base_tree_override.borrow_mut() = Some(BASE_TREE_HEX.into());
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, red(SUITE)));
    f.runs
        .borrow_mut()
        .insert(BASE.into(), (0, "  test-a.sh ok".into()));
    f.reruns
        .borrow_mut()
        .insert((BASE.into(), SUITE.into()), (1, red(SUITE)));
    assert_eq!(f.run(), BASEFAIL);
    let path = base_cache_path(SUITE);
    let written = f.written.borrow();
    assert!(
        written.iter().any(|(p, c)| p == &path
            && c.contains("verdict=FAIL")
            && c.contains("harness=harness")
            && c.contains("image=tag1")),
        "a fresh run warms the cache: {written:?}"
    );
}

/// A suite the base never reports on (a fault, an unexpected timeout) is left uncached — the
/// next gate asks again rather than trusting a run that did not actually answer.
#[test]
fn a_rerun_that_faulted_writes_no_cache_entry() {
    let f = Fake::new();
    *f.base_tree_override.borrow_mut() = Some(BASE_TREE_HEX.into());
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, red(SUITE)));
    f.runs
        .borrow_mut()
        .insert(BASE.into(), (0, "  test-a.sh ok".into()));
    f.reruns
        .borrow_mut()
        .insert((BASE.into(), SUITE.into()), (75, "batch: no image".into()));
    assert_eq!(f.run(), NOVERDICT);
    let path = base_cache_path(SUITE);
    assert!(
        !f.written.borrow().iter().any(|(p, _)| p == &path),
        "a run that did not answer is never cached"
    );
}

/// THE PRODUCTION REGRESSION (2026-10-01, landed 23:17Z): base-untestable jumped from 1 in
/// 52 gates to 7 in 50 once this bead made the targeted rerun the primary path for every red
/// suite, not a rare fallback. Root cause: `spira`'s own `gate.steps` declares `bin
/// SPIRA_TESTENV_BIN testenv` ("the release's testenv could not run such a branch's suites
/// at all", sp-isom7) — the targeted rerun (and the cache's image-tag query) called bare
/// `testenv` on the release's PATH regardless, which exits 127 for such a tree, mapped to
/// NO_VERDICT. `base_out` then never gained the suite's line at all, so
/// `parse::attribute` found the branch's red suite nowhere in it: not run, not red —
/// `BaseUntestable`, not a judgement. This is the fixture `test-gate-base-selection.sh`
/// cannot reach (its gate command is a bare shell snippet, never a tree-owned `bin`), so it
/// passed throughout.
///
/// A branch red on an uncached suite, on a tree that owns a `bin SPIRA_TESTENV_BIN testenv`,
/// must still rerun X on the base through that binary and attribute branch-red or base-red
/// — never base-untestable because the rerun could not even start.
#[test]
fn a_tree_owned_testenv_reruns_through_its_own_binary_never_bare_testenv() {
    const WITH_TESTENV: &str = "# the tree's gate\nbin SPIRA_TESTENV_BIN testenv\nstep bash spira/fence.sh\n";
    let f = tree_owned(Some(WITH_TESTENV), Some(WITH_TESTENV));
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, red(SUITE)));
    // The base's own fences (now Composition::Fences, sp-kqger) pass — unrelated to the
    // regression; what matters is that the rerun below is actually judged.
    f.runs
        .borrow_mut()
        .insert(BASE.into(), (0, "fence: a-fence checked 1 files".into()));
    f.reruns.borrow_mut().insert(
        (BASE.into(), SUITE.into()),
        (0, format!("  {SUITE:<32} ok      1s")),
    );
    assert_eq!(f.run(), FAIL, "{}", f.stderr());
    assert!(
        f.verdict_line().contains("reason=branch-red"),
        "a rerun that actually ran must attribute, never base-untestable: {}",
        f.verdict_line()
    );
    // Prove it ran through the tree's OWN testenv, not a bare name that this tree's release
    // copy cannot run: the rerun call's env names the tree-keyed binary.
    let cmds = f.cmds.borrow();
    let rerun_idx = cmds
        .iter()
        .position(|c| c.contains("--suites"))
        .expect("the targeted rerun ran");
    assert_eq!(
        f.env_of(rerun_idx, "SPIRA_TESTENV_BIN"),
        format!("{GATE_TREE}/target/gate-tools/tree-of-{BASE}/testenv"),
        "the rerun used the base's own tree-built testenv"
    );
}

/// The same regression, but the rerun never gets the chance to run at all — if it did (the
/// defect, before the fix), this is exactly the silent BaseUntestable production saw: no
/// `SPIRA_TESTENV_BIN` reaching a tree that declared one would make a real `testenv`
/// resolve to nothing on the release's PATH. Asserting the env reaches the image-tag query
/// too (the cache's own first call) covers sp-kqger's other new call site.
#[test]
fn the_image_tag_query_also_uses_the_tree_owned_testenv() {
    const WITH_TESTENV: &str = "# the tree's gate\nbin SPIRA_TESTENV_BIN testenv\nstep bash spira/fence.sh\n";
    let f = tree_owned(Some(WITH_TESTENV), Some(WITH_TESTENV));
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, red(SUITE)));
    f.runs
        .borrow_mut()
        .insert(BASE.into(), (0, "fence: a-fence checked 1 files".into()));
    f.run();
    let cmds = f.cmds.borrow();
    let tag_idx = cmds
        .iter()
        .position(|c| c.contains("container tag"))
        .expect("the image tag was queried");
    assert_eq!(
        f.env_of(tag_idx, "SPIRA_TESTENV_BIN"),
        format!("{GATE_TREE}/target/gate-tools/tree-of-{BASE}/testenv")
    );
}

/// testenv's --deadline deferred the suite on the base: not run, so re-run.
#[test]
fn a_red_the_base_deferred_by_deadline_is_re_run() {
    let f = Fake::new();
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (1, red(SUITE)));
    f.runs.borrow_mut().insert(
        BASE.into(),
        (
            0,
            format!(
                "  {SUITE:<32} DEFERRED deadline\nVERDICT GREEN ran=0 deferred=1 (deadline 300s)"
            ),
        ),
    );
    f.reruns
        .borrow_mut()
        .insert((BASE.into(), SUITE.into()), (1, red(SUITE)));
    assert_eq!(f.run(), BASEFAIL);
}

/// A re-run that could not judge (a testenv fault — including a testenv missing from PATH,
/// which the shell reports as rc 127 and the runner's contract maps to 75) leaves it untestable.
#[test]
fn a_base_re_run_that_cannot_judge_is_untestable() {
    let f = Fake::new();
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (1, red(SUITE)));
    f.runs
        .borrow_mut()
        .insert(BASE.into(), (0, "  test-a.sh ok".into()));
    f.reruns
        .borrow_mut()
        .insert((BASE.into(), SUITE.into()), (75, "batch: no image".into()));
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=base-untestable"));
    // sp-e5v53-2: "when it truly can't, say exactly why" — the rerun's own fault output
    // reaches the verdict message, not just the generic "cannot be established" line.
    assert!(
        f.stderr().contains("batch: no image"),
        "the rerun's own fault output is surfaced: {}",
        f.stderr()
    );
}

/// A red suite the base does not have is the branch's own; nothing to run on the base.
#[test]
fn a_red_suite_the_base_lacks_is_the_branchs_without_a_re_run() {
    let f = Fake::new();
    f.base_lacks.borrow_mut().insert("test-new.sh".into());
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (1, red("test-new.sh")));
    f.runs
        .borrow_mut()
        .insert(BASE.into(), (0, "  test-a.sh ok".into()));
    assert_eq!(f.run(), FAIL);
    assert!(f.verdict_line().ends_with("suite=test-new.sh"));
    assert_eq!(f.cmds.borrow().len(), 2);
}

#[test]
fn the_re_run_names_only_shell_safe_suites() {
    let c = crate::engine::base_rerun_cmd(&[
        "test-a.sh".into(),
        "x;rm -rf /.sh".into(),
        "test-b.sh".into(),
    ]);
    assert!(
        c.contains("--suites test-a.sh,test-b.sh \"$SPIRA_GATE_BRANCH\""),
        "{c}"
    );
}

/// sp-kqger fix: a tree that declares its own testenv (`bin SPIRA_TESTENV_BIN testenv`,
/// sp-isom7 — "the release's testenv could not run such a branch's suites at all") needs
/// that binary for the rerun too. The command names it with a default, so a repository with
/// no such binding keeps today's bare `testenv`.
#[test]
fn the_re_run_prefers_a_tree_owned_testenv_with_a_bare_fallback() {
    let c = crate::engine::base_rerun_cmd(&["test-a.sh".into()]);
    assert!(
        c.contains("\"${SPIRA_TESTENV_BIN:-testenv}\" --suites test-a.sh"),
        "{c}"
    );
}

/// THE PRODUCTION REGRESSION (2026-10-01, after sp-e5v53 landed): base-untestable via a
/// different path than the bare-testenv defect sp-e5v53 fixed — concierge/sp-g3uwp (red on
/// test-persona-model.sh) and sp-ooh1k (red on test-install-dolt-*) both showed the rerun
/// genuinely ran (`gate.log` phases named a real `base-rerun` wall time, 13-26s, not the
/// instant 0-1s a 127 exit gives), yet still ended `base-untestable`.
///
/// Root cause: the repository's own gate string always hands testenv a *second* positional
/// argument, the repo (`"$SPIRA_TESTENV_BIN" --deadline … --suites … "$SPIRA_GATE_BRANCH"
/// "$SPIRA_GATE_REPO"`, gate.steps) — `base_rerun_cmd` never has, since sp-hh5h0. Reproduced
/// directly: `testenv --suites <suite> <rev>` with no repo argument and no `$SPIRA_REPO` in
/// the environment (never set in the gate command's `env -i`, DESIGN.md "The gate command's
/// environment") faults `VERDICT FAULT rc=2 reason=base-ref` — `resolve_repo` falls back to
/// the harness root, whose basename matches no configured repo, so `landref` finds no base
/// column for it. Adding the repo as a second argument (confirmed by hand against this very
/// repository) resolves it and the suite actually runs.
///
/// This is this bead's own positive control for the fix: the generated command must carry
/// the repo, in the same position the real gate string puts it, so a real `testenv` can
/// resolve its base ref instead of faulting before a single suite runs.
#[test]
fn the_re_run_also_names_the_repo_so_testenv_can_resolve_its_base_ref() {
    let c = crate::engine::base_rerun_cmd(&["test-persona-model.sh".into()]);
    assert!(
        c.contains("--suites test-persona-model.sh \"$SPIRA_GATE_BRANCH\" \"$SPIRA_GATE_REPO\""),
        "the repo must follow the branch, exactly as the repository's own gate string orders \
         them: {c}"
    );
}

/// The end-to-end shape of the regression: a rerun that genuinely cannot judge (here,
/// standing in for testenv's own `VERDICT FAULT rc=2 reason=base-ref`) still ends
/// `base-untestable` — a fault that reaches the rerun is not a verdict either way — but the
/// verdict message must say exactly why, not just that it "cannot be established": a reader
/// should never have to go read `gate.log`'s phases to learn the rerun was even tried.
#[test]
fn a_rerun_that_faults_on_its_own_base_ref_is_untestable_and_says_why() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, red(SUITE)));
    f.runs
        .borrow_mut()
        .insert(BASE.into(), (0, "  test-a.sh ok".into()));
    f.reruns.borrow_mut().insert(
        (BASE.into(), SUITE.into()),
        (
            75,
            format!(
                "batch: cannot resolve the base ref for spira\nbatch: add a base column to the repo-map, or run: git remote set-head origin -a\nVERDICT FAULT rc=2 ran=0 reason={}",
                "base-ref"
            ),
        ),
    );
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=base-untestable"));
    assert!(
        f.stderr().contains("cannot resolve the base ref"),
        "the fault that actually happened is named, not just \"cannot be established\": {}",
        f.stderr()
    );
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
    assert!(f.verdict_line().contains("reason=deadline"), "a base trial killed at the whole-gate deadline judged nothing");
}

#[test]
fn a_deadline_or_harness_fault_is_not_a_red() {
    for (rc, out, reason) in [
        (124, "test-a.sh ok", "deadline"),
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

#[test]
fn phantom_bead_id_fails_and_store_fault_is_no_verdict() {
    let f = Fake::new();
    f.cite.set(1);
    assert_eq!(f.run(), FAIL);
    assert!(f.verdict_line().contains("reason=phantom-bead-id"));
    let f = Fake::new();
    f.cite.set(3);
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=commit-cite-fault"));
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
fn ejected_suites_come_from_the_ejected_file() {
    let f = Fake::new();
    f.set_var("SPIRA_GATE_BEAD", "sp-a");
    f.files.borrow_mut().insert(
        PathBuf::from(format!("{RUN}/ejected/sp-a")),
        "test-x.sh,test-y.sh\nignored\n".into(),
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
fn a_sidecar_under_the_retired_landstate_ledger_is_not_read() {
    // sp-2c1n0: the ledger is deleted; a stale `<id>.ejected` left under it forces nothing.
    let f = Fake::new();
    f.set_var("SPIRA_GATE_BEAD", "sp-a");
    f.files.borrow_mut().insert(PathBuf::from(format!("{RUN}/landstate/sp-a.ejected")), "test-z.sh\n".into());
    f.run();
    assert_eq!(f.env_of(0, "SPIRA_GATE_EJECTED_SUITES"), "");
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
fn a_full_gate_pool_is_said_naming_its_holders_and_a_taken_slot_names_the_branch() {
    let f = Fake::new();
    f.admission_free.set(false);
    f.set_var("SPIRA_GATE_LOCK_WAIT", "5");
    assert_eq!(f.run(), NOVERDICT);
    assert_eq!(f.stderr().matches("gate: waiting for a gate slot: 2 of 2 held by fake").count(), 1, "{}", f.stderr());
    // POSITIVE CONTROL: a free pool says nothing about waiting, records who holds the slot, and
    // every command the trial runs inherits the gate's admission.
    let g = Fake::new();
    assert_eq!(g.run(), PASS);
    assert!(!g.stderr().contains("waiting for a gate slot"));
    assert_eq!(g.admission_who.borrow().first().map(String::as_str), Some(BR));
    assert_eq!(g.env_of(0, "SPIRA_ADMISSION"), "gate");
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

// --------------------------------------------- admission is visible, logged, re-read (sp-q20wb)
//
// sp-f4ig1 (landed after this bead, concurrently) owns the wait/holder-naming messages now —
// `admission_wait_line`, backed by `spira_config::admission` — so the fake here is a stub
// (`format!("waiting for a gate slot: {par} of {par} held by fake")`) and holder-naming
// itself is tested in that crate, not here. What stays this bead's own to prove: the wait is
// announced at all (not silent), gate.log's `waited=` covers admission, and the live
// re-read of the pool's size (`certify_par_live`) reaches an already-waiting gate.

/// The wait is announced once, not once per second: a gate held for a while does not spam.
#[test]
fn the_waiting_message_prints_exactly_once() {
    let f = Fake::new();
    f.admission_free.set(false);
    f.set_var("SPIRA_GATE_LOCK_WAIT", "3");
    f.run();
    let waits = f
        .stderr()
        .lines()
        .filter(|l| l.starts_with("gate: waiting for a gate slot"))
        .count();
    assert_eq!(waits, 1, "{}", f.stderr());
}

/// Being admitted after a real wait is also announced — and the wait reaches gate.log's
/// `waited=`, which used to measure only the tree-lock wait and read 0 for a gate that had
/// in fact queued for admission.
#[test]
fn an_admission_wait_is_announced_when_admitted_and_logged_as_waited() {
    let f = Fake::new();
    f.admission_free_after.set(2); // both slots busy for exactly one full pass, then free
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(
        f.stderr().contains("gate: admitted to a gate slot after 1s"),
        "{}",
        f.stderr()
    );
    let row = f.appended.borrow()[0].clone();
    assert!(row.contains("waited=1s"), "{row}");
}

/// A gate that never has to wait for admission announces neither message — the fast path
/// prints exactly what it always did.
#[test]
fn an_instant_admission_announces_nothing() {
    let f = Fake::new();
    assert_eq!(f.run(), PASS);
    assert!(!f.stderr().contains("gate: waiting for a gate slot"));
    assert!(!f.stderr().contains("gate: admitted to a gate slot"));
}

/// sp-q20wb: raising the limit in the config document while a gate already waits must reach
/// it — the frozen `Ctx.SPIRA_CERTIFY_PAR` (conf.sh's export at this process's own start)
/// cannot. Fixture: slots 1-2 (the frozen par) are permanently busy; slot 3 is the only free
/// one, and only appears once the live config is read as 3 — one poll after the first,
/// standing in for "the operator edits the config while this gate is already waiting."
#[test]
fn a_limit_raised_in_the_live_config_admits_an_already_waiting_gate() {
    let f = Fake::new();
    f.admission_only_slot_free.set(Some(3));
    f.certify_par_live_after.set(1); // first poll still sees the frozen par (2)
    f.certify_par_live.set(Some(3)); // every poll after that sees the raised one
    f.set_var("SPIRA_GATE_LOCK_WAIT", "5");
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(
        f.stderr().contains("gate: waiting for a gate slot: 2 of 2 held by fake"),
        "the first poll still used the frozen par of 2: {}",
        f.stderr()
    );
    assert!(
        f.stderr().contains("gate: admitted to a gate slot after 1s"),
        "the second poll saw the raised par (3) and slot 3 was free: {}",
        f.stderr()
    );
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
    // the host's 8 cores, NOT divided by SPIRA_CERTIFY_PAR 2 (sp-f4ig1 D3: no per-job limit);
    // spira-config brings its dependent queue
    assert_eq!(
        cmds[1],
        "cargo build --profile aeon --config profile.aeon.incremental=false -j 8 --all-targets -p queue -p spira-config"
    );
    assert_eq!(
        cmds[2],
        "cargo test --profile aeon --config profile.aeon.incremental=false -j 8 -p queue -p spira-config -- --test-threads=8"
    );
    assert!(f.stderr().contains(
        "gate: composition=unit — fences (suites off, no build fence: the build phase is the compile check), then cargo build and test on the host for: queue spira-config (touched: spira-config)"
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

/// sp-aprxm: a unit-mode gate builds once. The build fence's cold release `make build` is
/// dropped from the gate string for a unit composition (branch and base trial alike); a
/// composition with no build phase of its own keeps it.
#[test]
fn a_unit_gate_builds_once_and_every_other_composition_keeps_the_build_fence() {
    const G: &str = "bash spira/lint.sh && bash spira/build-fence.sh && run-suites";
    let set = |f: &Fake| f.ctx.borrow_mut().as_mut().unwrap().gate_cmd = G.into();

    let f = unit_fake(&["gate/src/x.rs"]);
    set(&f);
    assert_eq!(f.run(), PASS);
    let cmds = f.cmds.borrow().clone();
    assert_eq!(cmds[0], "bash spira/lint.sh && run-suites", "{cmds:?}");
    assert!(
        cmds[1].starts_with("cargo build --profile aeon"),
        "{cmds:?}"
    );
    assert!(cmds.iter().all(|c| !c.contains("build-fence")), "{cmds:?}");

    // The base trial of a red unit branch runs the same, fence-less string.
    let f = unit_fake(&["gate/src/x.rs"]);
    set(&f);
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (1, "tsd: test x ... FAILED".into()));
    let _ = f.run();
    let cmds = f.cmds.borrow().clone();
    assert!(cmds.len() > 3, "a base trial ran: {cmds:?}");
    assert!(cmds.iter().all(|c| !c.contains("build-fence")), "{cmds:?}");

    for paths in [&["gate/src/x.rs", "spira/lib.sh"][..], &["docs/a.md"]] {
        let f = unit_fake(paths);
        set(&f);
        assert_eq!(f.run(), PASS);
        assert_eq!(
            f.cmds.borrow()[0],
            G,
            "{paths:?}: no build phase, so the fence stays"
        );
    }
    let f = Fake::new();
    set(&f);
    assert_eq!(f.run(), PASS);
    assert_eq!(
        f.cmds.borrow()[..],
        [G.to_string()],
        "suites mode: byte for byte"
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
    assert!(f.stderr().contains("the configuration does not validate"));
    assert!(meter(&f).contains("compose=suites(mode)"));
}

// ---------------------------------------------------------------------------- re-entry (sp-p3srm)

const GATE_TREE: &str = "/run/worktree/.gate.spira.spira-sp-a";

/// A fake whose bead the round returned with `suites` (the `.ejected` sidecar), each of which
/// exists on the gate tree. Its gate string runs fences only (prints no suite line) on both
/// the merge and the base.
fn returned(f: Fake, suites: &str) -> Fake {
    for at in [MERGE_SHA, BASE] {
        f.runs
            .borrow_mut()
            .insert(at.into(), (0, "fences ok".into()));
    }
    f.set_var("SPIRA_GATE_BEAD", "sp-a");
    f.files.borrow_mut().insert(
        PathBuf::from(format!("{RUN}/ejected/sp-a")),
        format!("{suites}\n"),
    );
    for s in suites.split(',') {
        f.files
            .borrow_mut()
            .insert(PathBuf::from(format!("{GATE_TREE}/spira/{s}")), "#!".into());
    }
    f
}

#[test]
fn unit_mode_rust_only_returned_bead_runs_its_unit_gate_then_exactly_the_named_suites() {
    let f = returned(unit_fake(&["gate/src/x.rs"]), "test-b.sh,test-c.sh");
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (0, "build-fence: make build ok".into()));
    assert_eq!(f.run(), PASS);
    let cmds = f.cmds.borrow().clone();
    assert_eq!(cmds.len(), 4, "fences, build, test, reentry: {cmds:?}");
    assert_eq!(
        f.env_of(0, "SPIRA_GATE_SUITES"),
        "off",
        "the gate string selects nothing"
    );
    assert!(cmds[1].contains("--all-targets -p gate"));
    assert!(
        cmds[3].contains("--suites test-b.sh,test-c.sh \"$SPIRA_GATE_BRANCH\""),
        "{}",
        cmds[3]
    );
    assert!(
        !cmds[3].contains("--deadline"),
        "the round named them: no budget cuts them"
    );
    assert!(meter(&f).contains("compose=unit+reentry phases=fences:7,build:7,test:7,reentry:7"));
    assert!(f.stderr().contains(
        "gate: re-entry — the round named suites against sp-a; each must pass in this gate: test-b.sh test-c.sh"
    ));
    assert!(f
        .stderr()
        .contains("gate PASS covered suites: test-b.sh,test-c.sh"));
}

#[test]
fn a_named_suite_red_again_on_a_green_base_fails_the_branch() {
    let f = returned(unit_fake(&["gate/src/x.rs"]), "test-b.sh");
    f.reentry_runs.borrow_mut().insert(
        MERGE_SHA.into(),
        (
            1,
            "  test-b.sh   RED     rc=1 after 4s\nVERDICT RED ran=1 red=1".into(),
        ),
    );
    assert_eq!(f.run(), FAIL);
    assert!(f.verdict_line().contains("reason=branch-red"));
    assert!(f.verdict_line().contains("suite=test-b.sh"));
    assert!(meter(&f).contains(
        "phases=fences:7,build:7,test:7,reentry:7,base-fences:7,base-build:7,base-test:7,base-reentry:7"
    ));
    assert!(f
        .written
        .borrow()
        .iter()
        .all(|(p, _)| !p.starts_with("/run/verdicts/trees")));
}

#[test]
fn a_named_suite_red_on_the_base_too_is_the_bases() {
    let f = returned(unit_fake(&["gate/src/x.rs"]), "test-b.sh");
    for at in [MERGE_SHA, BASE] {
        f.reentry_runs
            .borrow_mut()
            .insert(at.into(), (1, "  test-b.sh   RED     rc=1 after 4s".into()));
    }
    f.reruns.borrow_mut().insert((BASE.into(), "test-b.sh".into()), (1, "  test-b.sh   RED     rc=1 after 4s".into()));
    assert_eq!(f.run(), BASEFAIL);
    assert!(f.verdict_line().contains("reason=base-red"));
}

#[test]
fn a_named_suite_that_only_skips_proves_nothing() {
    let f = returned(unit_fake(&["gate/src/x.rs"]), "test-b.sh");
    f.reentry_runs.borrow_mut().insert(
        MERGE_SHA.into(),
        (
            0,
            "  test-b.sh   SKIP-REQ requires:docker\nVERDICT GREEN ran=1".into(),
        ),
    );
    assert_eq!(f.run(), NOVERDICT);
    assert!(
        f.verdict_line().contains("reason=reentry-unproven"),
        "{}",
        f.verdict_line()
    );
    assert!(f.verdict_line().contains("suite=test-b.sh"));
    assert!(
        f.written.borrow().is_empty(),
        "no PASS cached, no certificate"
    );
}

#[test]
fn a_fences_only_branch_still_reruns_the_named_suites() {
    let f = returned(unit_fake(&["docs/x.md"]), "test-b.sh");
    assert_eq!(f.run(), PASS);
    let cmds = f.cmds.borrow().clone();
    assert_eq!(cmds.len(), 2, "{cmds:?}");
    assert!(meter(&f).contains("compose=fences+reentry phases=fences:7,reentry:7"));
}

#[test]
fn a_beads_suites_off_certification_still_selects_its_covered_suites() {
    let f = unit_fake(&["spira-config/src/lib.rs"]);
    f.set_var("SPIRA_GATE_SUITES", "off");
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (0, String::new()));
    assert_eq!(f.run(), PASS);
    assert_eq!(f.env_of(0, "SPIRA_GATE_COVERED"), "", "no bead: fences only, as the batcher's cut");

    let f = unit_fake(&["spira-config/src/lib.rs"]);
    f.set_var("SPIRA_GATE_SUITES", "off");
    f.set_var("SPIRA_GATE_BEAD", "sp-a");
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (0, String::new()));
    assert_eq!(f.run(), PASS);
    assert_eq!(f.env_of(0, "SPIRA_GATE_SUITES"), "off");
    assert_eq!(f.env_of(0, "SPIRA_GATE_COVERED"), "1");

    let f = unit_fake(&["spira-config/src/lib.rs"]);
    f.set_var("SPIRA_GATE_SUITES", "off");
    f.set_var("SPIRA_GATE_BEAD", "sp-a");
    f.admission_free.set(false);
    f.set_var("SPIRA_GATE_LOCK_WAIT", "5");
    assert_eq!(f.run(), NOVERDICT, "covered suites run under the admission pool");
    assert!(f.verdict_line().contains("reason=admission-timeout"));
}

#[test]
fn a_script_branch_in_unit_mode_with_certify_suites_off_still_reruns_them() {
    // Production today: certify_suites = "off". The selector then exits before it ever
    // unions SPIRA_GATE_EJECTED_SUITES in, so the promise "recertification will force these
    // suites" was never kept. The re-entry phase keeps it.
    let f = returned(unit_fake(&["spira/lib.sh"]), "test-b.sh");
    f.set_var("SPIRA_GATE_SUITES", "off");
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (0, String::new()));
    assert_eq!(f.run(), PASS);
    assert_eq!(f.cmds.borrow().len(), 2);
    assert!(meter(&f).contains("compose=suites(script)+reentry phases=gate:7,reentry:7"));
}

#[test]
fn suites_off_with_named_suites_takes_an_admission_slot() {
    let f = returned(Fake::new(), "test-b.sh");
    f.set_var("SPIRA_GATE_SUITES", "off");
    f.admission_free.set(false);
    f.set_var("SPIRA_GATE_LOCK_WAIT", "5");
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=admission-timeout"));

    let f = Fake::new();
    f.set_var("SPIRA_GATE_SUITES", "off");
    f.admission_free.set(false);
    assert_eq!(
        f.run(),
        PASS,
        "fences-only certification with nothing named takes no slot"
    );
}

#[test]
fn suites_mode_that_already_ran_the_named_suites_adds_no_phase() {
    let f = returned(Fake::new(), "test-b.sh");
    f.runs.borrow_mut().insert(
        MERGE_SHA.into(),
        (
            0,
            "  test-a.sh   ok 2s\n  test-b.sh   ok 3s\nVERDICT GREEN ran=2".into(),
        ),
    );
    assert_eq!(f.run(), PASS);
    assert_eq!(f.cmds.borrow().len(), 1);
    assert_eq!(f.env_of(0, "SPIRA_GATE_EJECTED_SUITES"), "test-b.sh");
    assert!(meter(&f).contains("compose=suites(mode)+reentry phases=gate:7"));
}

#[test]
fn a_named_suite_the_gate_strings_budget_deferred_is_rerun_alone() {
    let f = returned(Fake::new(), "test-b.sh,test-c.sh");
    f.runs.borrow_mut().insert(
        MERGE_SHA.into(),
        (0, "  test-b.sh   ok 3s\n  test-c.sh   DEFERRED deadline\nVERDICT GREEN ran=1 deferred=1 (deadline 300s)".into()),
    );
    assert_eq!(f.run(), PASS);
    let cmds = f.cmds.borrow().clone();
    assert_eq!(cmds.len(), 2, "{cmds:?}");
    assert!(cmds[1].contains("--suites test-c.sh "), "{}", cmds[1]);
}

#[test]
fn a_named_suite_no_longer_on_the_tree_is_said_and_not_run() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.set_var("SPIRA_GATE_BEAD", "sp-a");
    f.files.borrow_mut().insert(
        PathBuf::from(format!("{RUN}/ejected/sp-a")),
        "test-gone.sh,../evil.sh\n".into(),
    );
    assert_eq!(f.run(), PASS);
    assert_eq!(f.cmds.borrow().len(), 3);
    assert!(meter(&f).contains("compose=unit phases="));
    assert!(f
        .stderr()
        .contains("(no longer on the tree, nothing to run: test-gone.sh)"));
    assert!(f
        .stderr()
        .contains("(not suite names, ignored: ../evil.sh)"));
}

#[test]
fn the_batchers_ejected_sidecar_drives_the_rerun() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (0, "fences ok".into()));
    f.set_var("SPIRA_GATE_BEAD", "sp-a");
    f.files.borrow_mut().insert(
        PathBuf::from(format!("{RUN}/ejected/sp-a")),
        "test-b.sh\n".into(),
    );
    f.files.borrow_mut().insert(
        PathBuf::from(format!("{GATE_TREE}/spira/test-b.sh")),
        "#!".into(),
    );
    assert_eq!(f.run(), PASS);
    assert!(f.cmds.borrow()[3].contains("--suites test-b.sh "));
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
    assert!(f.stderr().contains("phase 'test' failed (exit 101)"));
}

#[test]
fn a_red_the_base_shares_in_unit_tests_is_the_bases_and_names_the_test() {
    let f = unit_fake(&["gate/src/x.rs"]);
    for at in [MERGE_SHA, BASE] {
        f.unit_runs.borrow_mut().insert(
            (at.into(), "test"),
            (101, "test tests::x ... FAILED\ntest tests::y ... ok".into()),
        );
    }
    assert_eq!(f.run(), BASEFAIL);
    assert!(f.verdict_line().contains("reason=base-red"));
    assert!(f.verdict_line().contains("suite=tests::x"), "{}", f.verdict_line());
}

#[test]
fn an_unnamed_unit_base_red_is_a_gate_defect_not_the_bases_fault() {
    let f = unit_fake(&["gate/src/x.rs"]);
    for at in [MERGE_SHA, BASE] {
        f.unit_runs
            .borrow_mut()
            .insert((at.into(), "test"), (101, "killed by signal".into()));
    }
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=gate-defect"), "{}", f.verdict_line());
}

#[test]
fn a_base_whose_build_fails_before_any_test_is_untestable_never_red() {
    let f = unit_fake(&["gate/src/x.rs"]);
    for at in [MERGE_SHA, BASE] {
        f.unit_runs
            .borrow_mut()
            .insert((at.into(), "build"), (101, "error[E0425]".into()));
    }
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=base-untestable"), "{}", f.verdict_line());
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

// ------------------------------------------------------------- the gate-run row (sp-cln99)

fn tsd_row(f: &Fake) -> serde_json::Value {
    let rows = f.tsd.borrow();
    assert_eq!(rows.len(), 1, "one gate-run row per trial: {rows:?}");
    serde_json::from_str(rows[0].trim_end()).unwrap()
}

#[test]
fn a_rust_only_unit_pass_records_its_branch_type_mode_and_wall() {
    let f = unit_fake(&["spira-config/src/lib.rs"]);
    assert_eq!(f.run(), PASS);
    let v = tsd_row(&f);
    assert_eq!(v["family"], "gate-run");
    assert_eq!(v["status"], "PASS");
    assert_eq!(v["rc"], 0);
    assert_eq!(v["gate_mode"], "unit");
    assert_eq!(v["compose"], "unit");
    assert_eq!(v["branch_type"], "rust-only");
    assert_eq!(v["phases"], "fences:7,build:7,test:7");
    assert_eq!(v["wall_secs"], v["ran_secs"]);
    assert_eq!(v["repo"], "spira");
    assert_eq!(v["branch"], BR);
}

#[test]
fn suites_mode_still_records_what_the_branch_touches() {
    // Under gate_mode = suites every composition is suites(mode); the row classifies the
    // branch as unit mode would, so the Intent's by-type measure exists before the switch.
    for (paths, want) in [
        (&["spira/lib.sh"][..], "bash-touching"),
        (&["gate/src/engine.rs"][..], "rust-only"),
        (&["docs/x.md"][..], "nothing-buildable"),
    ] {
        let f = unit_fake(paths);
        *f.mode.borrow_mut() = Ok(GateMode::Suites);
        assert_eq!(f.run(), PASS);
        let v = tsd_row(&f);
        assert_eq!(v["compose"], "suites(mode)", "{paths:?}");
        assert_eq!(v["gate_mode"], "suites");
        assert_eq!(v["branch_type"], want, "{paths:?}");
    }
}

#[test]
fn a_no_verdict_trial_is_recorded_as_one() {
    let f = Fake::new();
    f.lock_free.set(false);
    f.set_var("SPIRA_GATE_LOCK_WAIT", "3");
    assert_eq!(f.run(), NOVERDICT);
    let v = tsd_row(&f);
    assert_eq!(v["status"], "NO_VERDICT");
    assert_eq!(v["reason"], "lock-timeout");
    assert_eq!(v["waited_secs"], 3);
    assert_eq!(v["branch_type"], "unknown", "it ended before a composition");
}

// ------------------------------------------------ positive controls (sp-ufbkh)

/// The production gate string's shape: bash fences, spira-lint, the selector.
const FENCED: &str = r#"bash spira/inventory.sh && "$SPIRA_LINT_BIN" && bash spira/build-fence.sh && { _s="$("$SPIRA_SELECT_BIN" gate "$SPIRA_GATE_BASE" x)" || exit 75; [ -n "$_s" ] || exit 0; }"#;

fn fenced() -> Fake {
    let f = Fake::new();
    f.ctx.borrow_mut().as_mut().unwrap().gate_cmd = FENCED.into();
    for p in ["spira/inventory.sh", "spira/build-fence.sh"] {
        f.blobs.borrow_mut().insert(format!("{BASE}:{p}"), b"x".to_vec());
    }
    f
}

#[test]
fn a_fence_that_exits_0_without_its_line_is_no_verdict_fence_silent() {
    let f = fenced();
    f.silent_fences.borrow_mut().insert("lockfile-lint".into());
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    let vl = f.verdict_line();
    assert!(vl.contains("reason=fence-silent"), "{vl}");
    assert!(vl.contains("suite=lockfile-lint"), "the fence is named: {vl}");
    assert!(f.stderr().contains("printed no `fence: <name> checked"), "{}", f.stderr());
    // No cache entry, no certificate: nothing was judged.
    assert!(
        f.written.borrow().iter().all(|(d, _)| !d.to_string_lossy().contains("verdicts")),
        "{:?}",
        f.written.borrow()
    );
}

fn branch_trial_prints(f: &Fake, out: &str) {
    f.runs.borrow_mut().insert(MERGE_SHA.to_string(), (0, out.to_string()));
}

#[test]
fn a_summary_naming_fewer_suites_than_the_runner_ran_is_no_verdict() {
    let f = Fake::new();
    branch_trial_prints(&f, "  test-a.sh   ok 1s\n  test-b.sh   ok 1s\nVERDICT GREEN ran=3");
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=summary-mismatch"), "{}", f.verdict_line());
    assert!(f.stderr().contains("ran=3 but only 2"), "{}", f.stderr());
}

#[test]
fn a_summary_naming_every_suite_the_runner_ran_still_passes() {
    let f = Fake::new();
    branch_trial_prints(&f, "  test-a.sh   ok 1s\n  test-b.sh   ok 1s\n  test-c.sh   SKIPPED\nVERDICT GREEN ran=2 skipped=1");
    assert_eq!(f.run(), PASS, "{}", f.stderr());
}

#[test]
fn a_plan_matrix_that_is_skipped_is_fence_silent_even_when_spira_lint_passes() {
    let f = fenced();
    f.silent_fences.borrow_mut().insert("plan-matrix".into());
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=fence-silent"));
    assert!(f.verdict_line().contains("suite=plan-matrix"));
}

#[test]
fn a_fence_that_reports_checked_0_is_silent() {
    let f = fenced();
    f.silent_fences.borrow_mut().insert("inventory".into());
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (0, "fence: inventory checked 0 files".into()));
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("suite=inventory"));
}

#[test]
fn every_fence_with_its_line_passes_and_the_counts_are_reported() {
    let f = fenced();
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    let e = f.stderr();
    for n in [
        "inventory=1 files",
        "spira-lint=1 files",
        "plan-matrix=1 files",
        "build-fence=1 files",
        "tier-budget-allowlist=1 files",
        "tier-budget-area-allowlist=1 files",
        "tier-budget-areas=1 files",
        "lockfile-lint=1 files",
    ] {
        assert!(e.contains(n), "{n} missing from: {e}");
    }
}

#[test]
fn a_failing_trial_is_judged_as_before_not_as_silent() {
    let f = fenced();
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (1, "lockfile-lint: serde bumped".into()));
    assert_eq!(f.run(), FAIL);
    assert!(f.verdict_line().contains("reason=branch-red"));
}

#[test]
fn the_fences_run_in_the_gate_tree_not_the_repository() {
    let f = fenced();
    assert_eq!(f.run(), PASS);
    let trees = f.trees.borrow();
    assert!(!trees.is_empty());
    for t in trees.iter() {
        let t = t.to_string_lossy();
        assert!(
            t.starts_with(&format!("{RUN}/worktree/.gate.")) && t != REPO,
            "a gate-string run outside the gate tree: {t}"
        );
    }
    // …while SPIRA_GATE_REPO stays the repository: the fences must not read it for their tree.
    assert_eq!(f.env_of(0, "SPIRA_GATE_REPO"), REPO);
}

#[test]
fn a_unit_composition_does_not_expect_the_build_fence_it_dropped() {
    let f = fenced();
    *f.mode.borrow_mut() = Ok(GateMode::Unit);
    *f.touched.borrow_mut() = Ok(vec![Changed {
        path: "gate/src/x.rs".into(),
        exec: false,
    }]);
    f.silent_fences.borrow_mut().insert("build-fence".into());
    assert_eq!(f.run(), PASS, "{}", f.stderr());
}

// ------------------------------------------------------------ the tree owns its gate (sp-quu2w)

/// A two-fence tree definition that builds spira-lint from the tree.
const STEPS: &str = "# the tree's gate\nbin SPIRA_LINT_BIN spira-lint\nstep bash spira/a-fence.sh\nstep \"$SPIRA_LINT_BIN\" --only payload-argv-lint\nstep bash spira/b-fence.sh\n";

fn tree_owned(base: Option<&str>, merged: Option<&str>) -> Fake {
    let f = Fake::new();
    f.ctx.borrow_mut().as_mut().unwrap().gate_cmd = "bash spira/config-only.sh".into();
    if let Some(b) = base {
        f.blobs.borrow_mut().insert(format!("{BASE}:gate.steps"), b.as_bytes().to_vec());
    }
    if let Some(m) = merged {
        f.blobs.borrow_mut().insert(format!("{MERGE_SHA}:gate.steps"), m.as_bytes().to_vec());
    }
    f
}

fn gate_runs(f: &Fake) -> Vec<String> {
    f.cmds.borrow().iter().filter(|c| !c.starts_with("cargo ")).cloned().collect()
}

#[test]
fn a_tree_definition_is_the_gate_and_the_column_is_ignored() {
    let f = tree_owned(Some(STEPS), Some(STEPS));
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    let runs = gate_runs(&f);
    assert_eq!(runs[0], r#"bash spira/a-fence.sh && "$SPIRA_LINT_BIN" --only payload-argv-lint && bash spira/b-fence.sh"#);
    assert!(!runs.iter().any(|c| c.contains("config-only")), "{runs:?}");
    assert!(f.stderr().contains("repo-map gate column is ignored"), "{}", f.stderr());
    // The tools phase ran first and the steps got the tree's spira-lint.
    let cmds = f.cmds.borrow();
    assert!(cmds[0].starts_with("cargo build --profile aeon") && cmds[0].contains("-p spira-lint"), "{cmds:?}");
    let lint = f.env_of(1, "SPIRA_LINT_BIN");
    // The binary the tools phase built, installed keyed by the merge's tree (sp-g9f3t).
    assert_eq!(lint, format!("{GATE_TREE}/target/gate-tools/tree-of-{MERGE_SHA}/spira-lint"));
    assert!(f.appended.borrow().iter().any(|l| l.contains("phases=tools:")), "{:?}", f.appended.borrow());
}

#[test]
fn a_branch_that_deletes_a_fence_and_its_step_passes() {
    // The base still has b-fence and names it; the branch deletes both.
    let without_b = STEPS.replace("step bash spira/b-fence.sh\n", "");
    let f = tree_owned(Some(STEPS), Some(&without_b));
    f.gone.borrow_mut().insert(format!("{MERGE_SHA}:spira/b-fence.sh"));
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(!gate_runs(&f)[0].contains("b-fence"));
}

#[test]
fn a_branch_that_deletes_a_fence_but_leaves_it_named_fails() {
    let f = tree_owned(Some(STEPS), Some(STEPS));
    f.gone.borrow_mut().insert(format!("{MERGE_SHA}:spira/b-fence.sh"));
    assert_eq!(f.run(), FAIL);
    assert!(f.verdict_line().contains("reason=gate-definition"), "{}", f.verdict_line());
    assert!(f.stderr().contains("names 'bash spira/b-fence.sh'"), "{}", f.stderr());
    assert!(f.ran.borrow().is_empty(), "nothing runs on a definition that cannot run");
}

#[test]
fn a_definition_the_base_already_breaks_is_the_bases() {
    let f = tree_owned(Some(STEPS), Some(STEPS));
    f.gone.borrow_mut().insert(format!("{MERGE_SHA}:spira/b-fence.sh"));
    f.gone.borrow_mut().insert(format!("{BASE}:spira/b-fence.sh"));
    assert_eq!(f.run(), BASEFAIL);
    assert!(f.verdict_line().contains("reason=base-gate-definition"));
    let f = tree_owned(Some("garbage\n"), Some("garbage\n"));
    assert_eq!(f.run(), BASEFAIL);
    assert!(f.verdict_line().contains("reason=base-gate-definition"));
}

#[test]
fn a_branch_that_deletes_or_breaks_the_definition_fails_closed() {
    let f = tree_owned(Some(STEPS), None);
    assert_eq!(f.run(), FAIL);
    assert!(f.verdict_line().contains("reason=gate-definition"));
    assert!(f.ran.borrow().is_empty());
    let f = tree_owned(Some(STEPS), Some("stpe bash spira/a-fence.sh\n"));
    assert_eq!(f.run(), FAIL);
    assert!(f.stderr().contains("unknown directive"), "{}", f.stderr());
    let f = tree_owned(Some(STEPS), Some("# nothing\n"));
    assert_eq!(f.run(), FAIL);
    assert!(f.stderr().contains("names no step"), "{}", f.stderr());
}

#[test]
fn a_repository_that_never_adopted_keeps_its_column() {
    let f = tree_owned(None, None);
    f.ctx.borrow_mut().as_mut().unwrap().gate_cmd = "bash spira/fence.sh && run-suites".into();
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert_eq!(gate_runs(&f)[0], "bash spira/fence.sh && run-suites");
    assert!(!f.cmds.borrow().iter().any(|c| c.contains("spira-lint")), "no tools phase");
    assert_eq!(f.env_of(0, "SPIRA_LINT_BIN"), "");
    // …and the branch that adopts one is gated by it on its own first trial.
    let f = tree_owned(None, Some(STEPS));
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(gate_runs(&f)[0].starts_with("bash spira/a-fence.sh"));
}

#[test]
fn the_base_trial_runs_the_bases_own_definition() {
    let branch_steps = STEPS.replace("step bash spira/b-fence.sh\n", "step bash spira/c-fence.sh\n");
    let f = tree_owned(Some(STEPS), Some(&branch_steps));
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "c-fence: violation".into()));
    assert_eq!(f.run(), FAIL);
    assert!(f.verdict_line().contains("reason=branch-red"), "{}", f.verdict_line());
    let runs = gate_runs(&f);
    assert!(runs[0].ends_with("bash spira/c-fence.sh"), "{runs:?}");
    assert!(runs[1].ends_with("bash spira/b-fence.sh"), "the base trial ran the branch's definition: {runs:?}");
    // Both trials built their tools, and the base's steps got the tree's binary too.
    let tools = f.cmds.borrow().iter().filter(|c| c.starts_with("cargo build")).count();
    assert_eq!(tools, 2);
}

#[test]
fn the_definition_is_part_of_the_verdict_key() {
    let a = tree_owned(Some(STEPS), Some(STEPS));
    assert_eq!(a.run(), PASS);
    let b = tree_owned(Some(STEPS), Some(&STEPS.replace("bin SPIRA_LINT_BIN spira-lint\n", "")));
    assert_eq!(b.run(), PASS);
    let key = |f: &Fake| f.written.borrow().iter().find(|(p, _)| p.starts_with("/run/verdicts") && !p.to_string_lossy().contains("/trees/")).map(|(p, _)| p.clone());
    assert!(key(&a).is_some() && key(&b).is_some());
    assert_ne!(key(&a), key(&b), "a different tool set is a different trial");
}

// ------------------------------------------- the trial's budget (testenv DESIGN.md §11, sp-govet)

#[test]
fn a_trial_that_gave_up_on_a_full_queue_is_a_queue_no_verdict_with_its_wait_metered() {
    let out = "testenv: gave up waiting for a slot after 90s (still 2/2 running)\ntestenv: queue-wait=95s pool=container\nVERDICT FAULT rc=2 ran=0 reason=queue";
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (75, out.into()));
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=queue"), "{}", f.verdict_line());
    assert!(f.stderr().contains("waited 95s for a testenv slot"), "{}", f.stderr());
    assert_eq!(f.ran.borrow().len(), 1, "no base trial for a queue give-up");
    assert!(meter_row(&f).contains(" queue=95"), "{}", meter_row(&f));
    assert!(f.tsd.borrow().iter().any(|r| r.contains("\"queue_secs\":95")), "{:?}", f.tsd.borrow());
}

#[test]
fn queue_wait_words_are_summed_across_pools() {
    assert_eq!(crate::parse::queue_secs("a queue-wait=5s pool=compile\nqueue-wait=7s pool=test\nqueue-wait=x"), 12);
    assert_eq!(crate::parse::queue_secs("nothing waited"), 0);
}

#[test]
fn a_suites_trial_cut_at_its_setup_share_is_a_budget_no_verdict_naming_the_phase() {
    for (out, phase) in [
        ("x\nVERDICT FAULT rc=2 ran=0 reason=deadline-up", "up"),
        ("VERDICT FAULT rc=2 ran=0 reason=deadline-build", "build"),
        ("  test-b.sh DEFERRED deadline\nVERDICT FAULT rc=2 ran=0 reason=deadline-suites", "suites"),
    ] {
        let f = Fake::new();
        f.runs.borrow_mut().insert(MERGE_SHA.into(), (75, out.into()));
        assert_eq!(f.run(), NOVERDICT);
        assert!(f.verdict_line().contains("reason=budget"), "{}", f.verdict_line());
        assert!(
            f.stderr().contains(&format!("phase `{phase}` was cut at its share of SPIRA_GATE_BUDGET=300s")),
            "{}",
            f.stderr()
        );
        assert_eq!(f.ran.borrow().len(), 1, "no base trial for a budget cut");
    }
    // any other runner fault stays a harness fault
    let f = Fake::new();
    f.runs.borrow_mut().insert(
        MERGE_SHA.into(),
        (75, "VERDICT FAULT rc=2 ran=0 reason=container-up".into()),
    );
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=harness-fault"));
}

#[test]
fn a_red_before_the_suites_step_is_judged_on_the_bases_fences_only() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(
        MERGE_SHA.into(),
        (1, "spira-lint: 1 violation(s) of rule literal-lint".into()),
    );
    assert_eq!(f.run(), FAIL);
    assert!(f.verdict_line().contains("reason=branch-red"), "{}", f.verdict_line());
    assert_eq!(f.env_of(0, "SPIRA_GATE_SUITES"), "on", "the branch ran its suites string");
    assert_eq!(f.env_of(1, "SPIRA_GATE_SUITES"), "off", "the base answers only the fence's question");
    assert!(meter(&f).contains("compose=suites("), "{}", meter(&f));
}

/// sp-kqger: a red inside the suites step used to make the base trial re-run the branch's
/// whole suite selection (SPIRA_GATE_SUITES=on for the base too) — the 275-337 s mirror this
/// bead removes. The base's main command is fences-only now, whatever the branch's red
/// looked like; the red suite itself is still judged, by the cache or a targeted rerun.
#[test]
fn a_red_inside_the_suites_step_is_still_judged_on_the_base_per_suite() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(
        MERGE_SHA.into(),
        (1, format!("{}\nVERDICT RED ran=3 red=1", red("test-b.sh"))),
    );
    f.reruns.borrow_mut().insert(
        (BASE.into(), "test-b.sh".into()),
        (0, "  test-b.sh                          ok      1s".into()),
    );
    assert_eq!(f.run(), FAIL);
    assert_eq!(f.env_of(1, "SPIRA_GATE_SUITES"), "off");
    assert!(f.cmds.borrow().iter().any(|c| c == "\"${SPIRA_TESTENV_BIN:-testenv}\" container tag"), "{:?}", f.cmds.borrow());
    assert!(
        f.cmds.borrow().iter().any(|c| c.contains("--suites test-b.sh")),
        "the red suite is still judged on the base, just not by a mirrored selection: {:?}",
        f.cmds.borrow()
    );

    // a runner that faulted with rc 4 (the candidate did not build) also reached the step,
    // but names no suite — nothing for the cache or a rerun to ask about.
    let f = Fake::new();
    f.runs.borrow_mut().insert(
        MERGE_SHA.into(),
        (4, "VERDICT FAULT rc=4 ran=0 reason=build".into()),
    );
    f.run();
    assert_eq!(f.env_of(1, "SPIRA_GATE_SUITES"), "off");
    assert!(
        !f.cmds.borrow().iter().any(|c| c == "\"${SPIRA_TESTENV_BIN:-testenv}\" container tag"),
        "no suite named: nothing to ask the cache about"
    );
}

#[test]
fn the_runners_budget_knobs_reach_the_gate_command() {
    let f = Fake::new();
    f.set_var("SPIRA_TESTENV_SETUP_SHARE", "70");
    f.set_var("SPIRA_TESTENV_WARM_SLOTS", "0");
    f.run();
    assert_eq!(f.env_of(0, "SPIRA_TESTENV_SETUP_SHARE"), "70");
    assert_eq!(f.env_of(0, "SPIRA_TESTENV_WARM_SLOTS"), "0");
}

// ------------------------------------------------- tools keyed by the tree (sp-g9f3t)

/// The 2026-09-30 base-red, as a fixture: the branch trial is red on its own, so the base
/// trial runs in the same gate tree. A spira-lint from another tree — the unkeyed
/// `target/aeon/spira-lint` the branch trial left, or a keyed directory stamped for some other
/// tree — flags literals the base's own spira-lint allows. Reading either would charge the
/// branch's red to local/main (BASE_FAIL); the base's own tools, keyed by its tree, pass.
#[test]
fn the_base_trial_runs_tools_keyed_by_the_base_tree_never_a_stale_one() {
    let f = tree_owned(Some(STEPS), Some(STEPS));
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "a-fence: red on the branch".into()));
    // literal-ok: the 2026-09-30 finding, quoted as the stale tool's answer.
    let stale = (1, "literal-lint: aeon/src/tests.rs:249: \"needs-ryan\"".to_string());
    // path-ok: the unkeyed build output the branch trial left in the gate tree.
    let unkeyed = format!("{GATE_TREE}/target/aeon/spira-lint");
    let other = format!("{GATE_TREE}/target/gate-tools/tree-of-OTHER/spira-lint");
    f.lint_by_bin.borrow_mut().insert(unkeyed.clone(), stale.clone());
    f.lint_by_bin.borrow_mut().insert(other.clone(), stale);
    f.files.borrow_mut().insert(PathBuf::from(&other), "#!stale".into());
    f.files.borrow_mut().insert(PathBuf::from(format!("{GATE_TREE}/target/gate-tools/tree-of-OTHER/TREE")), "tree-of-OTHER\n".into());
    assert_eq!(f.run(), FAIL, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=branch-red"), "{}", f.verdict_line());
    // Every gate-string run named a binary keyed by the tree that run judged.
    let ran = f.ran.borrow();
    let lints: Vec<String> = ran
        .iter()
        .filter_map(|e| e.iter().find(|(k, _)| k == "SPIRA_LINT_BIN").map(|(_, v)| v.clone()))
        .collect();
    assert!(!lints.is_empty());
    assert!(lints.contains(&format!("{GATE_TREE}/target/gate-tools/tree-of-{BASE}/spira-lint")), "{lints:?}");
    assert!(!lints.iter().any(|l| l == &unkeyed || l == &other), "{lints:?}");
}

/// The gate tree does not hold the tree the trial judges: no tool is built or read, and the
/// branch trial is refused, never judged.
#[test]
fn a_gate_tree_holding_another_tree_is_refused_before_its_tools_are_built() {
    let f = tree_owned(Some(STEPS), Some(STEPS));
    *f.tree_drift.borrow_mut() = Some("tree-of-SOMETHING-ELSE".into());
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=tools-unattributed"), "{}", f.verdict_line());
    assert!(f.stderr().contains("holds tree tree-of-SOMETHING-ELSE, not tree-of-m3rg3"), "{}", f.stderr());
    assert!(f.cmds.borrow().is_empty(), "{:?}", f.cmds.borrow());
}

/// An install that fails leaves nothing keyed: the trial is refused, never judged with the
/// unkeyed build.
#[test]
fn a_failed_install_is_refused_not_judged() {
    let f = tree_owned(Some(STEPS), Some(STEPS));
    *f.install_err.borrow_mut() = Some("disk full".into());
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=tools-unattributed"), "{}", f.verdict_line());
    assert!(f.stderr().contains("disk full"), "{}", f.stderr());
    // The build ran; no step did.
    assert_eq!(gate_runs(&f).len(), 0, "{:?}", f.cmds.borrow());
}

/// The base trial's tree cannot be attributed: no base trial, so whose red it is stays
/// unestablished — NO_VERDICT, never BASE_FAIL on a guess.
#[test]
fn a_base_trial_whose_tree_cannot_be_attributed_does_not_run() {
    let f = tree_owned(Some(STEPS), Some(STEPS));
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "a-fence: red".into()));
    f.runs.borrow_mut().insert(BASE.into(), (1, "a-fence: red".into()));
    assert_eq!(f.run(), BASEFAIL, "control: the base trial runs and is red: {}", f.stderr());

    let f = tree_owned(Some(STEPS), Some(STEPS));
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "a-fence: red".into()));
    f.runs.borrow_mut().insert(BASE.into(), (1, "a-fence: red".into()));
    // The base checkout does not leave the gate tree holding the base's tree.
    f.drift_after_checkouts.set(1);
    *f.tree_drift.borrow_mut() = Some("tree-of-STALE".into());
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=base-untestable"), "{}", f.verdict_line());
    assert!(f.stderr().contains(&format!("holds tree tree-of-STALE, not tree-of-{BASE}")), "{}", f.stderr());
}

/// Tools stamped for the very tree a trial judges are reused, not rebuilt; the stamp and
/// every package prove them.
#[test]
fn tools_stamped_for_the_same_tree_are_reused() {
    let f = tree_owned(Some(STEPS), Some(STEPS));
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "a-fence: red on the branch".into()));
    for id in [MERGE_SHA, BASE] {
        let dir = format!("{GATE_TREE}/target/gate-tools/tree-of-{id}");
        f.files.borrow_mut().insert(PathBuf::from(format!("{dir}/TREE")), format!("tree-of-{id}\n"));
        f.files.borrow_mut().insert(PathBuf::from(format!("{dir}/spira-lint")), "#!built".into());
    }
    assert_eq!(f.run(), FAIL, "{}", f.stderr());
    assert!(!f.cmds.borrow().iter().any(|c| c.starts_with("cargo build")), "{:?}", f.cmds.borrow());
    assert!(f.stderr().contains(&format!("tools for tree tree-of-{BASE} reused")), "{}", f.stderr());
    let meter = f.appended.borrow().join("\n");
    assert!(!meter.contains("tools:"), "{meter}");
    // A stamp for the tree with a package missing is not reuse: it is rebuilt.
    let f = tree_owned(Some(STEPS), Some(STEPS));
    let dir = format!("{GATE_TREE}/target/gate-tools/tree-of-{MERGE_SHA}");
    f.files.borrow_mut().insert(PathBuf::from(format!("{dir}/TREE")), format!("tree-of-{MERGE_SHA}\n"));
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(f.cmds.borrow()[0].starts_with("cargo build"), "{:?}", f.cmds.borrow());
}

/// A gate tree from before the tmpfs build holds stamped tools on the disk; moving the build
/// to tmpfs empties them, and the first gate must rebuild and judge, not NO_VERDICT.
#[test]
fn a_pre_tmpfs_tree_with_stamped_tools_on_disk_gets_a_verdict_on_its_first_gate() {
    let f = tree_owned(Some(STEPS), Some(STEPS));
    f.target_clears_tools.set(true);
    for id in [MERGE_SHA, BASE] {
        let dir = format!("{GATE_TREE}/target/gate-tools/tree-of-{id}");
        f.files.borrow_mut().insert(PathBuf::from(format!("{dir}/TREE")), format!("tree-of-{id}\n"));
        f.files.borrow_mut().insert(PathBuf::from(format!("{dir}/spira-lint")), "#!built".into());
    }
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(!f.verdict_line().contains("tools-unattributed"), "{}", f.verdict_line());
}

// ----------------------------------------------------------------- build IO (sp-z61hj)

/// Every gate-string run gets the build cache's wrapper, resolved on the PATH the command
/// gets (the release's plus the box tail), and the switch reaches testenv.
#[test]
fn every_run_compiles_through_the_wrapper_resolved_on_the_commands_path() {
    let f = Fake::new();
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    let asked = f.wrapper_asked.borrow().clone();
    assert_eq!(asked.len(), 1);
    assert!(asked[0].0.ends_with(":/box/.cargo/bin"), "{asked:?}");
    assert_eq!(f.env_of(0, "RUSTC_WRAPPER"), "/box/.cargo/bin/sccache");
    assert_eq!(f.env_of(0, "SCCACHE_IGNORE_SERVER_IO_ERROR"), "1");
    // No CARGO_* variable: sccache hashes those into every key (DESIGN-build-cache.md §2.2).
    for e in f.ran.borrow().iter() {
        assert!(e.iter().all(|(k, _)| !k.starts_with("CARGO_")), "{e:?}");
    }
}

/// THE POSITIVE CONTROL (sp-xtdqi): a configured shared store reaches the build's own
/// environment — `SPIRA_SCCACHE_DAV_ADDR` on `ctx.vars` (resolved in-process by
/// `merge_resolved_config`/`RETIRED_VARS` in real life; injected directly here, same as
/// every other `ctx.var` this suite drives the engine through) must produce
/// `SCCACHE_WEBDAV_ENDPOINT` on the command the trial actually runs.
#[test]
fn a_configured_shared_store_reaches_the_build_as_webdav_vars() {
    let f = Fake::new();
    f.set_var("SPIRA_SCCACHE_DAV_ADDR", "192.168.1.56:9431");
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert_eq!(f.env_of(0, "SCCACHE_WEBDAV_ENDPOINT"), "http://192.168.1.56:9431");
    assert_eq!(f.env_of(0, "SCCACHE_WEBDAV_KEY_PREFIX"), "/");
}

/// Without `SPIRA_SCCACHE_DAV_ADDR` set, no webdav var reaches the build at all — the
/// no-store case stays exactly as it was before sp-xtdqi.
#[test]
fn no_configured_store_means_no_webdav_vars_on_the_build() {
    let f = Fake::new();
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert_eq!(f.env_of(0, "SCCACHE_WEBDAV_ENDPOINT"), "");
    assert_eq!(f.env_of(0, "SCCACHE_WEBDAV_KEY_PREFIX"), "");
}

/// No sccache: a trial that builds in the tree (here, the definition's tools) refuses before
/// any build — never a cold build of every dependency. A trial that builds nothing (a
/// column-gated repository) does not need the cache and is judged.
#[test]
fn an_absent_build_cache_refuses_a_building_trial_before_anything_builds() {
    let f = tree_owned(Some(STEPS), Some(STEPS));
    *f.wrapper.borrow_mut() = Err("sccache is not on the build's PATH (/x)".into());
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=no-build-cache"), "{}", f.verdict_line());
    assert!(f.cmds.borrow().is_empty(), "nothing built: {:?}", f.cmds.borrow());

    let f = Fake::new();
    *f.wrapper.borrow_mut() = Err("sccache is not on the build's PATH (/x)".into());
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(f.targets.borrow().is_empty(), "no build, no tmpfs target");
    assert!(f.ran.borrow().iter().all(|e| e.iter().all(|(k, _)| k != "RUSTC_WRAPPER")));
}

/// SPIRA_BUILD_CACHE=off is honoured and said out loud; the switch is passed on.
#[test]
fn the_opt_out_is_loud_and_reaches_the_command() {
    let f = Fake::new();
    f.set_var("SPIRA_BUILD_CACHE", "off");
    *f.wrapper.borrow_mut() = Ok(spira_config::build::Wrapper::Off);
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert_eq!(f.wrapper_asked.borrow()[0].1, "off");
    assert!(f.stderr().contains("build cache: OFF"), "{}", f.stderr());
    assert_eq!(f.env_of(0, "RUSTC_WRAPPER"), "");
    assert_eq!(f.env_of(0, "SPIRA_BUILD_CACHE"), "off");
}

/// The gate tree's build goes to tmpfs before anything builds; short of room is a refusal.
#[test]
fn the_gate_tree_builds_on_tmpfs_and_short_room_is_no_verdict() {
    let f = tree_owned(Some(STEPS), Some(STEPS));
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert_eq!(f.targets.borrow().as_slice(), &[PathBuf::from(GATE_TREE)]);
    assert!(f.stderr().contains("gate: build on tmpfs at"), "{}", f.stderr());

    assert_eq!(f.released.borrow().as_slice(), &[(PathBuf::from(GATE_TREE), false)], "a finished gate drops its target");

    let f = tree_owned(Some(STEPS), Some(STEPS));
    *f.target_err.borrow_mut() = Some("gate: MemAvailable is 12 MiB, below the 4096 MiB".into());
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=scratch-short"), "{}", f.verdict_line());
    assert!(f.cmds.borrow().is_empty(), "nothing built: {:?}", f.cmds.borrow());
}

/// A reservation the shared ledger refuses is the same refusal as short room.
#[test]
fn a_refused_scratch_reservation_is_no_verdict_before_anything_builds() {
    let f = tree_owned(Some(STEPS), Some(STEPS));
    *f.reserve_err.borrow_mut() = Some("scratch: gate-x needs 4096 MiB but only 100 MiB is free".into());
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=scratch-short"), "{}", f.verdict_line());
    assert!(f.cmds.borrow().is_empty(), "nothing built: {:?}", f.cmds.borrow());
}

/// A build that dies on the exhausted scratch filesystem is infrastructure — never the
/// branch's red, and never the base's.
#[test]
fn a_build_that_hits_enospc_is_no_verdict_never_red() {
    for text in ["rustc-LLVM ERROR: Disk quota exceeded", "ld: error writing x: No space left on device"] {
        let f = Fake::new();
        f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, format!("test-a.sh RED\n{text}")));
        assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
        assert!(f.verdict_line().contains("reason=scratch-short"), "{}", f.verdict_line());
    }
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "test-a.sh RED".into()));
    f.runs.borrow_mut().insert(BASE.into(), (1, "test-a.sh RED\nDisk quota exceeded".into()));
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=scratch-short"), "{}", f.verdict_line());
}

/// The tools phase and the unit phases are one-shot builds: no incremental cache, by a
/// command-line switch (a CARGO_INCREMENTAL variable would split the cache).
#[test]
fn tree_builds_are_one_shot() {
    let d = crate::def::parse("bin SPIRA_LINT_BIN spira-lint\nstep \"$SPIRA_LINT_BIN\"\n").unwrap();
    assert!(d.tools_command(4).unwrap().contains("--config profile.aeon.incremental=false"));
    for (_, c) in crate::compose::unit_commands(&["tsd".to_string()], 4) {
        assert!(c.contains("--config profile.aeon.incremental=false"), "{c}");
    }
}

/// A tree-built testenv is told its harness is the gate tree: its own executable now
/// resolves into the tmpfs build root, above which no harness marker lies (sp-z61hj). A
/// definition that builds no testenv sets nothing (the release's testenv finds its own).
#[test]
fn a_tree_built_testenv_is_given_the_gate_tree_as_its_harness() {
    let steps = "bin SPIRA_LINT_BIN spira-lint\nbin SPIRA_TESTENV_BIN testenv\nstep bash spira/a-fence.sh\n";
    let f = tree_owned(Some(steps), Some(steps));
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    let ran = f.ran.borrow();
    let e = ran.last().unwrap();
    let get = |k: &str| e.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone());
    assert_eq!(get("SPIRA_TESTENV_HARNESS").as_deref(), Some(GATE_TREE));
    assert!(get("SPIRA_TESTENV_BIN").unwrap().starts_with(&format!("{GATE_TREE}/target/gate-tools/")));
    drop(ran);
    let f = tree_owned(Some(STEPS), Some(STEPS));
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(f.ran.borrow().iter().all(|e| e.iter().all(|(k, _)| k != "SPIRA_TESTENV_HARNESS")));
}

/// `--release-bins` on a PASS: the judged tree's release profile is built in the gate tree
/// (tmpfs) through the build cache, one-shot, and the landing is told where it is.
#[test]
fn release_bins_builds_the_judged_tree_in_the_gate_tree_after_a_pass() {
    let f = Fake::new();
    assert_eq!(f.run_with(true), PASS, "{}", f.stderr());
    let cmds = f.cmds.borrow().clone();
    let last = cmds.last().unwrap();
    assert_eq!(last, &crate::engine::release_bins_command());
    assert!(last.starts_with("cargo build --release --workspace --locked --config profile.release.incremental=false"), "{last}");
    assert!(last.contains("find target/release -mindepth 1 -delete"), "a failed build leaves nothing to land: {last}");
    let env = f.ran.borrow().last().unwrap().clone();
    assert!(env.contains(&("RUSTC_WRAPPER".to_string(), "/box/.cargo/bin/sccache".to_string())), "{env:?}");
    assert!(env.iter().any(|(k, v)| k == "PATH" && v.ends_with(":/box/.cargo/bin")), "{env:?}");
    assert_eq!(f.trees.borrow().last().map(PathBuf::as_path), Some(Path::new(GATE_TREE)));
    assert!(f.stderr().contains(&format!("--worktree {GATE_TREE}")), "{}", f.stderr());
    // Without the flag nothing extra is built.
    let g = Fake::new();
    assert_eq!(g.run(), PASS);
    assert!(!g.cmds.borrow().iter().any(|c| c.contains("--release")), "{:?}", g.cmds.borrow());
}

/// A cached PASS with --release-bins keeps its cached verdict and still builds the judged
/// tree's release binaries in the gate tree.
#[test]
fn a_cached_pass_with_release_bins_still_builds_the_judged_tree() {
    let f = Fake::new();
    let k = key_for(&f, "", "none");
    f.files.borrow_mut().insert(
        PathBuf::from(format!("{RUN}/verdicts/{k}")),
        key::render_entry("2026-09-28T00:00:00Z", 999_000, "aeon", "spira", BR, "test-a.sh"),
    );
    assert_eq!(f.run_with(true), PASS, "{}", f.stderr());
    assert!(f.stderr().contains("VERDICT=PASS reason=cached"), "{}", f.stderr());
    let cmds = f.cmds.borrow().clone();
    assert_eq!(cmds.last(), Some(&crate::engine::release_bins_command()), "{cmds:?}");
    assert!(cmds.iter().all(|c| c.contains("--release")), "a cached verdict runs no trial: {cmds:?}");
    assert!(f.checkouts.borrow().iter().any(|_| true), "the judged tree is checked out");
    assert!(f.stderr().contains(&format!("--worktree {GATE_TREE}")), "{}", f.stderr());
}

/// No PASS, no release build; a failed release build is said out loud and the verdict stands.
#[test]
fn release_bins_never_builds_for_a_red_tree_and_a_failed_build_is_loud() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "test-b.sh FAIL".into()));
    f.run_with(true);
    assert!(!f.cmds.borrow().iter().any(|c| c.contains("--release")), "{:?}", f.cmds.borrow());

    let f = Fake::new();
    for at in [MERGE_SHA, BR, BASE] {
        f.unit_runs.borrow_mut().insert((at.to_string(), "build"), (101, "error: could not compile `x`".into()));
    }
    assert_eq!(f.run_with(true), PASS, "{}", f.stderr());
    assert!(f.stderr().contains("the release build FAILED (exit 101)"), "{}", f.stderr());
}

/// A release build that wrote into a full disk or an exhausted quota is a failure even when
/// cargo exited 0: the target is emptied and the message names EDQUOT/ENOSPC.
#[test]
fn release_bins_write_failure_is_loud_and_empties_the_target() {
    let f = Fake::new();
    for at in [MERGE_SHA, BR, BASE] {
        f.unit_runs.borrow_mut().insert((at.to_string(), "build"), (0, "error: failed to write: Disk quota exceeded".into()));
    }
    assert_eq!(f.run_with(true), PASS, "{}", f.stderr());
    assert!(f.stderr().contains("EDQUOT/ENOSPC"), "{}", f.stderr());
    assert!(!f.stderr().contains("are in "), "{}", f.stderr());
    assert!(f.cmds.borrow().last().unwrap().contains("find target/release -mindepth 1 -delete"), "{:?}", f.cmds.borrow());
}

/// sp-f4ig1-fix: a gate never loses its verdict to a compile-slot wait. Every command that can
/// compile — the trial's own and the after-PASS release-bins, which runs outside the trial's
/// environment — carries the gate's admission token, so spira-admit (env or cargo config) and
/// testenv run it on the gate's slot without queueing behind agent builds.
#[test]
fn every_build_the_gate_runs_rides_on_the_gate_slot_including_release_bins() {
    let f = Fake::new();
    assert_eq!(f.run_with(true), PASS, "{}", f.stderr());
    let ran = f.ran.borrow().clone();
    assert!(ran.len() >= 2, "{ran:?}");
    for (i, env) in ran.iter().enumerate() {
        assert!(
            env.iter().any(|(k, v)| k == "SPIRA_ADMISSION" && v == "gate"),
            "command {i} ({}) runs outside the gate's slot: {env:?}",
            f.cmds.borrow()[i]
        );
    }
    // POSITIVE CONTROL: the release-bins command is the one that used to lack it.
    assert_eq!(f.cmds.borrow().last().unwrap(), &crate::engine::release_bins_command());
}

/// D11 (option 2): with spira-admit on the PATH, every cargo the gate runs compiles through it
/// with the gate's token, so each takes a compile lease for its cargo WITHOUT WAITING and agent
/// builds queue behind it; sccache stays the inner compiler, and the lease names the branch.
#[test]
fn the_gates_cargo_takes_compile_leases_through_spira_admit_with_the_gate_token() {
    let f = Fake::new();
    f.files.borrow_mut().insert(PathBuf::from("/h/spira-admit"), String::new());
    assert_eq!(f.run_with(true), PASS, "{}", f.stderr());
    for (i, env) in f.ran.borrow().iter().enumerate() {
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("RUSTC_WRAPPER"), Some("/h/spira-admit"), "command {i}: {env:?}");
        assert_eq!(get("SPIRA_ADMIT_INNER"), Some("/box/.cargo/bin/sccache"), "command {i}");
        assert_eq!(get("SPIRA_ADMIT_WHO"), Some(format!("gate:{BR}").as_str()), "command {i}");
        assert_eq!(get("SPIRA_ADMISSION"), Some("gate"), "command {i}");
    }
}

#[test]
fn the_base_trial_never_names_a_suite_the_base_does_not_have() {
    let f = returned(unit_fake(&["gate/src/x.rs"]), "test-b.sh,test-new.sh");
    f.base_lacks.borrow_mut().insert("test-new.sh".into());
    f.reentry_runs.borrow_mut().insert(
        MERGE_SHA.into(),
        (1, "  test-b.sh   RED     rc=1 after 4s\nVERDICT RED ran=2 red=1".into()),
    );
    f.run();
    let cmds = f.cmds.borrow().clone();
    let base_reentry: Vec<&String> = cmds.iter().filter(|c| c.contains("--suites") && !c.contains("test-new.sh") && c.contains("test-b.sh")).collect();
    let named_new_on_base = cmds.iter().skip(4).any(|c| c.contains("--suites") && c.contains("test-new.sh"));
    assert!(!named_new_on_base, "the base trial named a suite the base lacks: {cmds:?}");
    assert!(!base_reentry.is_empty(), "the base trial still re-runs the suites the base has: {cmds:?}");
    assert!(!f.verdict_line().contains("base-untestable"), "{}", f.verdict_line());
}

#[test]
fn a_phase_that_outlasts_the_deadline_is_no_verdict_naming_the_phase() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (124, String::new()));
    assert_eq!(f.run(), NOVERDICT);
    let line = f.verdict_line();
    assert!(line.contains("reason=deadline"), "{line}");
    assert!(f.stderr().contains("phase `gate` was running"), "{}", f.stderr());
}

#[test]
fn an_operator_timeout_shorter_than_the_deadline_stays_a_timeout() {
    let f = Fake::new();
    f.set_var("SPIRA_GATE_TIMEOUT", "10");
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (124, String::new()));
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=timeout"), "{}", f.verdict_line());
}

#[test]
fn phase_caps_sum_under_the_deadline() {
    let fixed: u64 = ["tools", "gate"].iter().filter_map(|p| crate::engine::phase_cap(p)).sum();
    assert!(fixed < 300, "{fixed}");
    assert_eq!(crate::engine::phase_cap("base-tools"), crate::engine::phase_cap("tools"));
    assert_eq!(crate::engine::phase_cap("reentry"), None);
}

#[test]
fn a_gate_with_no_declared_deadline_refuses() {
    let f = Fake::new();
    f.set_var("SPIRA_GATE_DEADLINE", "");
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=config"), "{}", f.verdict_line());
}

// ------------------------------------- tools keyed by their source closure (toolkey.rs)

/// The merge's tree id, as the fake resolves `<MERGE_SHA>^{tree}`.
fn want() -> String {
    format!("tree-of-{MERGE_SHA}")
}

/// A tree that builds spira-lint, whose closure is spira-lint + spira-config (its normal
/// dependency) — not gate, a member outside it — and whose last build read, besides the
/// closure's own files, spira/conf.d (spira-config's build script), a registry crate and a
/// generated file under target/.
fn shared_fake() -> Fake {
    let f = tree_owned(Some(STEPS), Some(STEPS));
    f.metadata.borrow_mut().insert(
        MERGE_SHA.into(),
        Ok(metadata_json(&[("spira-lint", &["spira-config"]), ("spira-config", &[]), ("gate", &["spira-config"])])),
    );
    for (p, id) in [("spira-lint", "t-lint-1"), ("spira-config", "t-config-1"), ("gate", "t-gate-1"), ("Cargo.lock", "b-lock-1"), ("spira/conf.d", "t-confd-1")] {
        f.objects.borrow_mut().insert(format!("{}:{p}", want()), id.into());
    }
    *f.tool_inputs.borrow_mut() = Some(vec![
        format!("{GATE_TREE}/spira-lint/src/main.rs"),
        format!("{GATE_TREE}/spira-config/src/lib.rs"),
        format!("{GATE_TREE}/spira-config/../spira/conf.d"),
        "/home/u/.cargo/registry/src/regex-1/src/lib.rs".into(),
        format!("{GATE_TREE}/target/aeon/build/spira-config-1/out/spira_section.rs"),
    ]);
    f
}

/// A second gate after `first`: the store it published survives, the gate tree's own keyed
/// tools do not (a new branch's tree), and the trial sees `objects` changed.
fn next_gate(first: &Fake, changed: &[(&str, &str)]) -> Fake {
    let f = shared_fake();
    let store = PathBuf::from(format!("{RUN}/gate-tools/spira"));
    for (p, b) in first.files.borrow().iter() {
        if p.starts_with(&store) {
            f.files.borrow_mut().insert(p.clone(), b.clone());
        }
    }
    for (p, id) in changed {
        f.objects.borrow_mut().insert(format!("{}:{p}", want()), id.to_string());
    }
    f
}

fn built(f: &Fake) -> bool {
    f.cmds.borrow().iter().any(|c| c.starts_with("cargo build"))
}

/// The first gate builds and publishes, recording only what the base key does not cover;
/// a later tree whose closure is unchanged reuses those tools without a build, installed
/// and proved under its own tree id.
#[test]
fn tools_with_an_unchanged_source_closure_are_reused_across_trees() {
    let f = shared_fake();
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(built(&f));
    let published = f.published.borrow().clone();
    assert_eq!(published.len(), 1, "{}", f.stderr());
    let (store, name, inputs) = &published[0];
    assert_eq!(store, &PathBuf::from(format!("{RUN}/gate-tools/spira")));
    // Only the build script's out-of-closure read is recorded; the closure's own files, the
    // registry and target/ are not.
    assert_eq!(inputs, "spira/conf.d\tt-confd-1\n");
    assert!(name.ends_with(&format!("-{}", &crate::toolkey::entry_name("x", inputs)[2..])), "{name}");

    let g = next_gate(&f, &[]);
    assert_eq!(g.run(), PASS, "{}", g.stderr());
    assert!(!built(&g), "{:?}", g.cmds.borrow());
    assert_eq!(g.installs_from.borrow().as_slice(), [store.join(name)]);
    assert!(g.stderr().contains("built from the same sources"), "{}", g.stderr());
    assert_eq!(g.env_of(0, "SPIRA_LINT_BIN"), format!("{GATE_TREE}/target/gate-tools/{}/spira-lint", want()));
    assert!(g.published.borrow().is_empty());
}

/// A change to a file of a closure package (its tree id) or to a root input is a different
/// base key: the tools are rebuilt from the tree, and published under the new key.
#[test]
fn a_change_inside_the_closure_rebuilds_the_tools() {
    let f = shared_fake();
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    for change in [("spira-config", "t-config-2"), ("spira-lint", "t-lint-2"), ("Cargo.lock", "b-lock-2"), ("rust-toolchain.toml", "b-tc")] {
        let g = next_gate(&f, &[change]);
        assert_eq!(g.run(), PASS, "{}", g.stderr());
        assert!(built(&g), "{change:?}: {:?}", g.cmds.borrow());
        assert!(g.installs_from.borrow().is_empty(), "{change:?}");
        let p = g.published.borrow();
        assert_eq!(p.len(), 1, "{change:?}");
        assert_ne!(p[0].1, f.published.borrow()[0].1, "{change:?}: published under the old key");
    }
}

/// A change outside the closure — another member, a script — reuses the tools; a change to
/// an input the last build recorded outside the closure (spira-config's build script reads
/// spira/conf.d) rebuilds them.
#[test]
fn a_change_outside_the_closure_reuses_but_a_recorded_input_rebuilds() {
    let f = shared_fake();
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    let g = next_gate(&f, &[("gate", "t-gate-2"), ("spira/a-fence.sh", "b-fence-2")]);
    assert_eq!(g.run(), PASS, "{}", g.stderr());
    assert!(!built(&g), "{:?}", g.cmds.borrow());
    let g = next_gate(&f, &[("spira/conf.d", "t-confd-2")]);
    assert_eq!(g.run(), PASS, "{}", g.stderr());
    assert!(built(&g), "{:?}", g.cmds.borrow());
    // Both entries now sit under the same base key; the matching one is chosen.
    assert_eq!(g.published.borrow()[0].2, "spira/conf.d\tt-confd-2\n");
}

/// An entry that does not prove itself — wrong stamp, a missing package, a manifest that
/// does not hash to its name — is never reused; one that cannot be installed is built.
#[test]
fn a_shared_entry_that_does_not_prove_itself_is_built_instead() {
    let f = shared_fake();
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    let (store, name, _) = f.published.borrow()[0].clone();
    let e = store.join(&name);
    for spoil in ["KEY", "spira-lint", "INPUTS"] {
        let g = next_gate(&f, &[]);
        match spoil {
            "KEY" => g.files.borrow_mut().insert(e.join("KEY"), "another\n".into()),
            "INPUTS" => g.files.borrow_mut().insert(e.join("INPUTS"), "spira/conf.d\tforged\n".into()),
            p => g.files.borrow_mut().remove(&e.join(p)),
        };
        assert_eq!(g.run(), PASS, "{}", g.stderr());
        assert!(built(&g), "{spoil}: {:?}", g.cmds.borrow());
    }
    let g = next_gate(&f, &[]);
    *g.install_from_err.borrow_mut() = Some("evicted".into());
    assert_eq!(g.run(), PASS, "{}", g.stderr());
    assert!(built(&g), "{:?}", g.cmds.borrow());
    assert!(g.stderr().contains("evicted — building them"), "{}", g.stderr());
}

/// No key, no sharing — and still a verdict: a bin package that is not a workspace member,
/// or a closure package with no tree, builds for this tree alone; nothing is published.
#[test]
fn tools_that_cannot_be_keyed_are_built_for_the_tree_alone() {
    let f = shared_fake();
    f.metadata.borrow_mut().insert(MERGE_SHA.into(), Ok(metadata_json(&[("gate", &[])])));
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(built(&f) && f.published.borrow().is_empty());
    assert!(f.stderr().contains("spira-lint is not a workspace member"), "{}", f.stderr());
    let f = shared_fake();
    f.objects.borrow_mut().insert(format!("{}:spira-config", want()), String::new());
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(built(&f) && f.published.borrow().is_empty());
    // A build whose inputs cannot be read is not published either.
    let f = shared_fake();
    *f.tool_inputs.borrow_mut() = None;
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(f.published.borrow().is_empty());
}

#[test]
fn toolkey_reads_dep_info_build_outputs_and_paths() {
    use crate::toolkey::*;
    assert_eq!(
        dep_info_paths("/t/target/aeon/x: /t/a/src/main.rs /t/a\\ b/c.rs\n\n/t/a/src/main.rs:\n"),
        ["/t/a/src/main.rs", "/t/a b/c.rs"]
    );
    assert_eq!(rerun_paths("cargo:rerun-if-env-changed=CC\ncargo:rerun-if-changed=/t/x/../spira/conf.d\ncargo::rerun-if-changed=build.rs\n"), ["/t/x/../spira/conf.d", "build.rs"]);
    let t = Path::new("/t");
    assert_eq!(relative(t, "/t/x/../spira/conf.d").as_deref(), Some("spira/conf.d"));
    assert_eq!(relative(t, "/t/target/aeon/out.rs"), None);
    assert_eq!(relative(t, "/home/u/.cargo/registry/a.rs"), None);
    assert_eq!(relative(t, "/tx/a.rs"), None);
    let dirs = vec!["spira-lint".to_string()];
    assert!(covered("spira-lint/src/a.rs", &dirs) && covered("Cargo.lock", &dirs) && covered(".cargo/config.toml", &dirs));
    assert!(!covered("spira-lint2/a.rs", &dirs) && !covered("spira/conf.d", &dirs));
    assert_eq!(parse_manifest(&manifest(&[("b".into(), "2".into()), ("a".into(), "1".into())])), Some(vec![("a".into(), "1".into()), ("b".into(), "2".into())]));
    assert_eq!(parse_manifest("no-tab\n"), None);
    // The closure follows normal and build dependencies, never dev ones.
    let m = |n: &str, deps: &[&str], build: &[&str]| crate::compose::Member {
        name: n.into(),
        dir: n.into(),
        deps: deps.iter().map(|s| s.to_string()).collect(),
        build_deps: build.iter().map(|s| s.to_string()).collect(),
    };
    let ms = [m("lint", &["cfg", "testkit"], &["cfg"]), m("cfg", &["tsd"], &["tsd"]), m("tsd", &[], &[]), m("testkit", &[], &[])];
    let names: Vec<&str> = closure(&ms, &["lint".into()]).unwrap().iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["lint", "cfg", "tsd"]);
    assert!(closure(&ms, &["nope".into()]).is_err());
    // The key moves with any recipe, root or package id, and not with their order.
    let k = base_key("r", &[("Cargo.lock".into(), "1".into())], &[("a".into(), "a".into(), "1".into()), ("b".into(), "b".into(), "1".into())]);
    assert_eq!(k, base_key("r", &[("Cargo.lock".into(), "1".into())], &[("b".into(), "b".into(), "1".into()), ("a".into(), "a".into(), "1".into())]));
    assert_ne!(k, base_key("r2", &[("Cargo.lock".into(), "1".into())], &[("a".into(), "a".into(), "1".into()), ("b".into(), "b".into(), "1".into())]));
    assert_ne!(k, base_key("r", &[("Cargo.lock".into(), "2".into())], &[("a".into(), "a".into(), "1".into()), ("b".into(), "b".into(), "1".into())]));
    assert_ne!(k, base_key("r", &[("Cargo.lock".into(), "1".into())], &[("a".into(), "a".into(), "2".into()), ("b".into(), "b".into(), "1".into())]));
}

/// The real store: an atomic publish, a second publish of the same entry is no error, and
/// only the most recently used entries are kept.
#[test]
fn the_real_store_publishes_atomically_and_keeps_the_newest() {
    use std::fs;
    let t = testkit::TempDir::new("gate-toolstore");
    let from = t.path().join("from");
    fs::create_dir_all(&from).unwrap();
    fs::write(from.join("spira-lint"), "bin").unwrap();
    let store = t.path().join("store");
    let pk = ["spira-lint".to_string()];
    crate::real::publish_tools_at(&from, &pk, &store, "k-1", "a\t1\n", 2).unwrap();
    crate::real::publish_tools_at(&from, &pk, &store, "k-1", "a\t1\n", 2).unwrap();
    assert_eq!(fs::read_to_string(store.join("k-1/KEY")).unwrap(), "k-1\n");
    assert_eq!(fs::read_to_string(store.join("k-1/INPUTS")).unwrap(), "a\t1\n");
    assert_eq!(fs::read_to_string(store.join("k-1/spira-lint")).unwrap(), "bin");
    for n in ["k-2", "j-1"] {
        std::thread::sleep(std::time::Duration::from_millis(20));
        crate::real::publish_tools_at(&from, &pk, &store, n, "", 2).unwrap();
    }
    let mut left: Vec<String> = fs::read_dir(&store).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    assert_eq!(left, ["j-1", "k-2"], "the oldest entry and every temporary are gone");
    assert_eq!(crate::real::tool_entries_at(&store, "k"), [store.join("k-2")]);
    // A missing binary publishes nothing.
    assert!(crate::real::publish_tools_at(&from, &["nope".into()], &store, "k-3", "", 2).is_err());
    assert!(!store.join("k-3").exists());
}
