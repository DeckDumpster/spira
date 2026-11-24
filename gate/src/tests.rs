//! The decision logic driven through a recording fake of every boundary (DESIGN.md "Boundaries").

use crate::compose::Changed;
use crate::engine::{hands_on_build, Args, Trial, Verdict, BASEFAIL, BASE_RERUN_MARK, FAIL, NOVERDICT, PASS};
use crate::fencecache;
use crate::key;
use crate::ports::{Ctx, Merge, World};
use spira_config::GateMode;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const REPO: &str = "r/spira";
const RUN: &str = "run";
const BASE: &str = "local/main";
const BR: &str = "spira/sp-a";
const MERGE_SHA: &str = "m3rg3";

type UnitRuns = HashMap<(String, &'static str), (i32, String)>;
/// (bead, tip, changed files, output) of each `lc_failure`.
type KeptLogs = (Vec<String>, Vec<(String, Vec<u8>)>);
type Recorded = (String, String, Vec<String>, String);

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
    reset_fails: Cell<bool>,
    reset_cleared: Cell<usize>,
    live_dirs: RefCell<HashSet<PathBuf>>,
    reset_log: RefCell<Vec<PathBuf>>,
    removed_dirs: RefCell<Vec<PathBuf>>,
    /// The lock is held for this many `tree_lock_try` calls, then free.
    lock_busy_tries: Cell<u32>,
    lock_open: Cell<bool>,
    locks_opened: RefCell<Vec<PathBuf>>,
    checkout_err: RefCell<Option<String>>,
    worktree: Cell<bool>,
    /// (status, output) by the SPIRA_GATE_BRANCH the trial ran with.
    runs: RefCell<HashMap<String, (i32, String)>>,
    ran: RefCell<Vec<Vec<(String, String)>>>,
    checkouts: RefCell<Vec<String>>,
    written: RefCell<Vec<(PathBuf, String)>>,
    deny_written: RefCell<Vec<(PathBuf, String)>>,
    unit_config: RefCell<Vec<(PathBuf, String)>>,
    deny: RefCell<Result<String, String>>,
    deny_calls: Cell<u32>,
    appended: RefCell<Vec<String>>,
    harness: Cell<&'static str>,
    /// run/tsd rows (telemetry.rs), kept apart from the gate.log meter.
    tsd: RefCell<Vec<String>>,
    yields: RefCell<Vec<Vec<String>>>,
    lc: RefCell<Vec<Vec<String>>>,
    failures: RefCell<Vec<Recorded>>,
    kept_logs: RefCell<Vec<KeptLogs>>,
    prior_failures: RefCell<Result<Vec<(String, String)>, String>>,
    flips: RefCell<Vec<(String, String)>>,
    flips_body: RefCell<Vec<String>>,
    flip_answer: RefCell<Result<(), String>>,
    prior_since: RefCell<Vec<i64>>,
    gate_starts: RefCell<Vec<String>>,
    /// The `build-dir` fact the bead's builder left, and why leasing it fails (when it does).
    build_dir_fact: RefCell<Option<(String, PathBuf)>>,
    builder_unusable: RefCell<Option<String>>,
    /// The line a gate's own warm directory adds, when one was adopted.
    warm_line: RefCell<Option<String>>,
    builders: RefCell<Vec<Option<crate::ports::Builder>>>,
    lc_refusal: RefCell<String>,
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
    /// What a tree's `target/cargo-timings/cargo-timing.html` reads as.
    timing_report: RefCell<Option<String>>,
    /// Every command run_gate was handed, in order.
    cmds: RefCell<Vec<String>>,
    /// Seconds each run_gate advances the clock by, per command kind.
    phase_secs: Cell<u64>,
    repo_build_secs: Cell<Option<u64>>,
    publish_run: Cell<(i32, u64)>,
    repo_build_result: RefCell<Option<(i32, String)>>,
    /// What the gate command writes to `SPIRA_GATE_STEPS`, as (offset from the phase's start, name) lines.
    steps_written: RefCell<Vec<(u64, String)>>,
    /// What testenv records to `SPIRA_TESTENV_TIER_EVENTS` for the trial of a branch key, as run.events text.
    tier_events: RefCell<HashMap<String, String>>,
    empty_temp_files: Cell<usize>,
    /// Every file `remove` was handed.
    removed: RefCell<Vec<PathBuf>>,
    /// The gate outputs kept beside the run record.
    kept_outs: RefCell<Vec<(PathBuf, String)>>,
    /// (seconds, a compiler line every n seconds, until this second; 0 = silent) a build phase takes.
    build_script: Cell<Option<(u64, u64, u64)>>,
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
    covered_runs: RefCell<HashMap<String, (i32, String)>>,
    // ---- positive controls (sp-ufbkh)
    /// Fences that exit 0 without their `fence: <name> checked` line. Every other fence the
    /// gate string names prints `checked 1` when the string exits 0, as a real one does.
    silent_fences: RefCell<HashSet<String>>,
    /// Step word (script path or tool word) -> the fence names that step prints.
    fence_prints: RefCell<Vec<(String, Vec<String>)>>,
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
    /// `<rev>:<path>` object ids rev_parse answers (absent: the default answer).
    objects: RefCell<HashMap<String, String>>,
    /// Every run_gate call: (command, the timeout it was given).
    timeouts: RefCell<Vec<(String, String)>>,
    /// The `want` every checkout was asked to prove.
    checkout_wants: RefCell<Vec<String>>,
    // ---- build IO (sp-z61hj)
    /// What build_wrapper answers; the (path, setting) it was asked with.
    wrapper: RefCell<Result<spira_config::build::Wrapper, String>>,
    wrapper_asked: RefCell<Vec<(String, String)>>,
    /// What target_on_tmpfs answers; the trees it was asked for.
    target_err: RefCell<Option<String>>,
    /// The real move deletes a pre-tmpfs `target/tree-tools` from the disk; set to model it.
    target_clears_tools: Cell<bool>,
    /// Another process removed the tree's build output after it was built.
    target_swept: Cell<bool>,
    targets: RefCell<Vec<PathBuf>>,
    target_repos: RefCell<Vec<String>>,
    target_roots: RefCell<Vec<String>>,
    released: RefCell<Vec<(PathBuf, bool)>>,
    hand_on: RefCell<Vec<bool>>,
    release_caps: RefCell<Vec<u64>>,
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
    // ---- the gate machine
    /// Every save of the run's record, in order.
    machine: RefCell<Vec<crate::machine::Run>>,
    /// The refusal `machine_save` answers.
    machine_err: RefCell<Option<String>>,
    /// A cancel request appears once the record holds this many events.
    cancel_at: Cell<Option<usize>>,
}

fn ctx() -> Ctx {
    let mut vars = HashMap::new();
    for (k, v) in [
        ("SPIRA_REPO_MAP", "cfg/repo-map"),
        ("SPIRA_RUN", RUN),
        ("SPIRA_VERDICT_TTL", "86400"),
        ("SPIRA_CERTIFY_PAR", "2"),
        ("HOME", "fixture-home"),
        ("SPIRA_RELEASE", "rel"),
        // The box's own tool tail (sp-c7b85) — cargo, for the tree builds a gate step runs.
        ("SPIRA_PATH", "box/.cargo/bin"),
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
        ("SPIRA_GATE_BUILD_STALL", "300"),
        ("SPIRA_GATE_UNIT_MAX_CRATES", "7"),
        ("SPIRA_GATE_BUILDER_DIR_WAIT_SECS", "120"),
        ("SPIRA_UNIT_TEST_DEADLINE_SECS", "60"),
        ("SPIRA_UNIT_TEST_DEADLINE_PERIODS", "2"),
    ] {
        vars.insert(k.to_string(), v.to_string());
    }
    Ctx {
        repo_name: "spira".into(),
        vars,
        repo_root: Some(REPO.into()),
        landref: Some(BASE.into()),
        declared_base: String::new(),
        gate_cmd: "bash spira/fence.sh && run-suites".into(),
        host_cores: "8".into(),
        land: "push".into(),
    }
}

const JOBS: u64 = 8;

impl Fake {
    fn new() -> Fake {
        let readable: HashSet<PathBuf> = [
            "cfg/repo-map",
            "h/exclude.sh",
            "h/skew",
            "h/spira-lc",
            "h/yield.sh",
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
            reset_fails: Cell::new(false),
            reset_cleared: Cell::new(0),
            reset_log: RefCell::new(Vec::new()),
            live_dirs: RefCell::new(HashSet::new()),
            removed_dirs: RefCell::new(Vec::new()),
            lock_busy_tries: Cell::new(0),
            lock_open: Cell::new(true),
            locks_opened: RefCell::new(Vec::new()),
            checkout_err: RefCell::new(None),
            worktree: Cell::new(true),
            runs: RefCell::new(runs),
            ran: RefCell::new(Vec::new()),
            checkouts: RefCell::new(Vec::new()),
            written: RefCell::new(Vec::new()),
            deny_written: RefCell::new(Vec::new()),
            unit_config: RefCell::new(Vec::new()),
            deny: RefCell::new(Ok(String::new())),
            deny_calls: Cell::new(0),
            appended: RefCell::new(Vec::new()),
            harness: Cell::new("harness"),
            tsd: RefCell::new(Vec::new()),
            yields: RefCell::new(Vec::new()),
            lc: RefCell::new(Vec::new()),
            failures: RefCell::new(Vec::new()),
            kept_logs: RefCell::new(Vec::new()),
            prior_failures: RefCell::new(Ok(Vec::new())),
            flips: RefCell::new(Vec::new()),
            flips_body: RefCell::new(Vec::new()),
            flip_answer: RefCell::new(Ok(())),
            prior_since: RefCell::new(Vec::new()),
            gate_starts: RefCell::new(Vec::new()),
            build_dir_fact: RefCell::new(None),
            builder_unusable: RefCell::new(None),
            warm_line: RefCell::new(None),
            builders: RefCell::new(Vec::new()),
            lc_refusal: RefCell::new(String::new()),
            removed_trees: Cell::new(0),
            err: RefCell::new(Vec::new()),
            clock: Cell::new(1_000_000),
            signal: Cell::new(false),
            mode: RefCell::new(Ok(GateMode::Suites)),
            touched: RefCell::new(Ok(Vec::new())),
            metadata: RefCell::new(HashMap::new()),
            unit_runs: RefCell::new(HashMap::new()),
            timing_report: RefCell::new(None),
            cmds: RefCell::new(Vec::new()),
            phase_secs: Cell::new(7),
            repo_build_secs: Cell::new(None),
            publish_run: Cell::new((0, 0)),
            repo_build_result: RefCell::new(None),
            steps_written: RefCell::new(Vec::new()),
            tier_events: RefCell::default(),
            empty_temp_files: Cell::new(0),
            removed: RefCell::new(Vec::new()),
            kept_outs: RefCell::new(Vec::new()),
            build_script: Cell::new(None),
            landref_tips: RefCell::new(Vec::new()),
            reruns: RefCell::new(HashMap::new()),
            base_lacks: RefCell::new(HashSet::new()),
            reentry_runs: RefCell::new(HashMap::new()),
            covered_runs: RefCell::new(HashMap::new()),
            silent_fences: RefCell::new(HashSet::new()),
            fence_prints: RefCell::new(vec![("spira/fence.sh".into(), vec!["fence".into()])]),
            trees: RefCell::new(Vec::new()),
            gone: RefCell::new(HashSet::new()),
            lint_by_bin: RefCell::new(HashMap::new()),
            tree_drift: RefCell::new(None),
            drift_after_checkouts: Cell::new(0),
            installs: RefCell::new(Vec::new()),
            install_err: RefCell::new(None),
            objects: RefCell::default(),
            timeouts: RefCell::default(),
            checkout_wants: RefCell::default(),
            wrapper: RefCell::new(Ok(spira_config::build::Wrapper::Sccache(PathBuf::from("box/.cargo/bin/sccache")))),
            wrapper_asked: RefCell::new(Vec::new()),
            target_err: RefCell::new(None),
            target_swept: Cell::new(false),
            target_clears_tools: Cell::new(false),
            targets: RefCell::new(Vec::new()),
            target_repos: RefCell::new(Vec::new()),
            target_roots: RefCell::new(Vec::new()),
            released: RefCell::new(Vec::new()),
            hand_on: RefCell::new(Vec::new()),
            release_caps: RefCell::new(Vec::new()),
            image_tag: RefCell::new((0, "tag1".into())),
            base_tree_override: RefCell::new(None),
            certify_par_live: Cell::new(None),
            certify_par_live_after: Cell::new(0),
            admission_free_after: Cell::new(0),
            admission_only_slot_free: Cell::new(None),
            machine: RefCell::default(),
            machine_err: RefCell::default(),
            cancel_at: Cell::new(None),
        }
    }
    fn plain_run(&self, tree: &Path, env: &[(String, String)], timeout: &str, cmd: &str) -> (i32, String) {
        self.timeouts.borrow_mut().push((cmd.to_string(), timeout.to_string()));
        self.ran.borrow_mut().push(env.to_vec());
        self.cmds.borrow_mut().push(cmd.to_string());
        let t0 = self.clock.get();
        if cmd.contains("--share-publish") {
            let (rc, secs) = self.publish_run.get();
            self.clock.set(t0 + secs);
            return (rc, String::new());
        }
        let secs = match self.repo_build_secs.get() {
            Some(b) if cmd.contains("--suites build") => b,
            _ => self.phase_secs.get(),
        };
        self.clock.set(t0 + secs);
        if let Some((_, path)) = env.iter().find(|(k, v)| k == "SPIRA_GATE_STEPS" && !v.is_empty()) {
            let text: String = self.steps_written.borrow().iter().map(|(at, n)| format!("{n}\t{}\n", t0 + at)).collect();
            if !text.is_empty() {
                self.files.borrow_mut().insert(PathBuf::from(path), text);
            }
        }
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
        if cmd.starts_with("{ ") && cmd.contains("SPIRA_SELECT_BIN") && !cmd.contains("SPIRA_LINT_BIN") {
            let at = self.checkouts.borrow().last().cloned().unwrap_or_default();
            return self.covered_runs.borrow().get(&at).cloned().unwrap_or((0, String::new()));
        }
        if let Some(r) = self.repo_build_result.borrow().clone().filter(|_| cmd.contains("--suites build")) {
            return r;
        }
        if let Some(list) = cmd
            .split_once("--suites ")
            .and_then(|(_, r)| r.split_whitespace().next())
        {
            // The base re-run (sp-hh5h0) names itself; anything else is the re-entry phase.
            if cmd.starts_with(BASE_RERUN_MARK) {
                let uncached = env.iter().any(|(k, v)| k == "SPIRA_BUILD_CACHE" && v == "off");
                if uncached {
                    if let Some(r) = self.reruns.borrow().get(&(format!("{br}:uncached"), list.to_string())) {
                        return r.clone();
                    }
                }
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
        if cmd.starts_with("cargo ") || cmd.starts_with("CARGO_BUILD_JOBS=") || cmd.starts_with("tools/test-sandbox/runner --around cargo ") {
            let kind = if cmd.starts_with("cargo build") || cmd.contains("cargo test --no-run") {
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
        let uncached = env.iter().any(|(k, v)| k == "SPIRA_BUILD_CACHE" && v == "off");
        let key = if uncached && self.runs.borrow().contains_key(&format!("{br}:uncached")) {
            format!("{br}:uncached")
        } else {
            br.clone()
        };
        if let Some((_, path)) = env.iter().find(|(k, v)| k == crate::tier::EVENTS_ENV && !v.is_empty()) {
            if let Some(text) = self.tier_events.borrow().get(&key) {
                self.files.borrow_mut().insert(PathBuf::from(path), text.clone());
            }
        }
        let (rc, out) = self
            .runs
            .borrow()
            .get(&key)
            .cloned()
            .unwrap_or((0, String::new()));
        if rc != 0 {
            return (rc, out);
        }
        // The fences ran and passed: each prints its positive control, unless told not to.
        let silent = self.silent_fences.borrow();
        let mut lines: Vec<String> = self
            .fence_prints
            .borrow()
            .iter()
            .filter(|(word, _)| cmd.contains(word.as_str()))
            .flat_map(|(_, names)| names.clone())
            .filter(|f| !silent.contains(f))
            .map(|f| format!("fence: {f} checked 1 files"))
            .collect();
        if !out.is_empty() {
            lines.push(out);
        }
        (0, lines.join("\n"))
    }
    /// A build phase the script drives; `stall` None is a phase nothing watches.
    fn scripted_build(&self, tree: &Path, env: &[(String, String)], timeout: &str, stall: Option<u64>, cmd: &str) -> (i32, String) {
        let scripted = self.build_script.get().filter(|_| cmd.starts_with("cargo build") || cmd.contains("cargo test --no-run"));
        let Some((secs, every, until)) = scripted else { return self.plain_run(tree, env, timeout, cmd) };
        self.timeouts.borrow_mut().push((cmd.to_string(), timeout.to_string()));
        self.cmds.borrow_mut().push(cmd.to_string());
        let t0 = self.clock.get();
        let mut watch = crate::stall::Watch::new(stall.unwrap_or(u64::MAX), t0);
        for t in 1..=secs {
            let now = t0 + t;
            if every > 0 && t % every == 0 && t <= until {
                watch.line(now, &format!("   Compiling crate-{t}"));
            }
            if watch.stalled(now) {
                self.clock.set(now);
                return (crate::stall::RC, watch.message(stall.unwrap_or(0)));
            }
        }
        self.clock.set(t0 + secs);
        (0, "build ok".into())
    }
    fn prints(&self, word: &str, names: &[&str]) {
        self.fence_prints.borrow_mut().push((word.into(), names.iter().map(|n| n.to_string()).collect()));
    }
    fn put_var(&self, k: &str, v: &str) {
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
        self.run_on(BR, release_bins)
    }
    fn run_on(&self, branch: &str, release_bins: bool) -> i32 {
        Trial::new(
            self,
            Args {
                home: PathBuf::from("h"),
                branch: branch.into(),
                repo: Some("spira".into()),
                release_bins,
            },
        )
        .run()
    }
    fn stderr(&self) -> String {
        self.err.borrow().join("\n")
    }
    /// The VERDICT line as it read before `ran=` ended it.
    fn verdict_line(&self) -> String {
        let l = self.full_verdict_line();
        l.rsplit_once(" ran=").map_or(l.clone(), |(head, _)| head.to_string())
    }
    fn full_verdict_line(&self) -> String {
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
    fn deny_list(&self, _at: u64) -> Result<String, String> {
        self.deny_calls.set(self.deny_calls.get() + 1);
        self.deny.borrow().clone()
    }
    fn context(&self, _: Option<&str>) -> Result<Ctx, String> {
        self.ctx.borrow().clone()
    }
    fn readable(&self, p: &Path) -> bool {
        self.readable.borrow().contains(p) || self.files.borrow().contains_key(p)
    }
    /// The fake PATH is one directory, /h: a script "is on PATH" when /h/<name> is readable.
    fn which(&self, name: &str) -> Option<PathBuf> {
        let p = Path::new("h").join(name);
        self.readable(&p).then_some(p)
    }
    fn exists(&self, p: &Path) -> bool {
        if p.ends_with(".git") {
            return self.worktree.get();
        }
        self.files.borrow().contains_key(p)
    }
    fn read(&self, p: &Path) -> Option<String> {
        if p.ends_with(crate::buildtiming::REPORT) {
            return self.timing_report.borrow().clone();
        }
        self.files.borrow().get(p).cloned()
    }
    fn mkdir_p(&self, _: &Path) {}
    fn reset_dir(&self, p: &Path) -> Result<(), String> {
        if self.reset_fails.get() {
            return Err("read-only file system".into());
        }
        self.reset_log.borrow_mut().push(p.to_path_buf());
        let before = self.files.borrow().len();
        self.files.borrow_mut().retain(|k, _| !k.starts_with(p));
        self.reset_cleared.set(before - self.files.borrow().len());
        self.live_dirs.borrow_mut().insert(p.to_path_buf());
        Ok(())
    }
    fn remove_dir_all(&self, p: &Path) {
        self.files.borrow_mut().retain(|k, _| !k.starts_with(p));
        self.live_dirs.borrow_mut().remove(p);
        self.removed_dirs.borrow_mut().push(p.to_path_buf());
    }
    fn write_atomic(&self, dir: &Path, name: &str, content: &str) {
        if name == "gate-unit-nextest.toml" {
            self.unit_config.borrow_mut().push((dir.join(name), content.to_string()));
            return;
        }
        if name == "gate-deny-list" {
            self.deny_written.borrow_mut().push((dir.join(name), content.to_string()));
            return;
        }
        if name.starts_with("gate-out-") {
            self.kept_outs.borrow_mut().push((dir.join(name), content.to_string()));
            return;
        }
        if let Some(held) = self.files.borrow_mut().get_mut(&dir.join(name)) {
            *held = content.to_string();
        }
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
    fn remove(&self, p: &Path) {
        self.removed.borrow_mut().push(p.to_path_buf());
    }
    fn list_dir(&self, dir: &Path) -> Vec<String> {
        self.files
            .borrow()
            .keys()
            .filter(|p| p.parent() == Some(dir))
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect()
    }
    fn temp_file(&self, content: &str) -> Option<PathBuf> {
        let n = self.empty_temp_files.get();
        if content.is_empty() {
            self.empty_temp_files.set(n + 1);
        }
        Some(PathBuf::from(match (content.is_empty(), n) {
            (true, 0) => "tmp/steps".to_string(),
            (true, n) => format!("tmp/empty-{n}"),
            (false, _) => "tmp/files".to_string(),
        }))
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
                .is_some_and(|s| rev == BASE && self.base_lacks.borrow().contains(s))
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
    fn lc_prior_failures(&self, since: i64) -> Result<Vec<(String, String)>, String> {
        self.prior_since.borrow_mut().push(since);
        self.prior_failures.borrow().clone()
    }
    fn file_flip(&self, repo: &str, test: &str, title: &str, body: &str) -> Result<(), String> {
        self.flips.borrow_mut().push((repo.into(), test.into()));
        self.flips_body.borrow_mut().push(title.into());
        self.flips_body.borrow_mut().push(body.into());
        self.flip_answer.borrow().clone()
    }
    fn lc_failure(&self, bead: &str, tip: &str, files: &[String], output: &str) {
        self.failures.borrow_mut().push((bead.into(), tip.into(), files.to_vec(), output.into()));
    }
    fn keep_logs(&self, beads: &[&str], files: &[(String, Vec<u8>)]) {
        self.kept_logs.borrow_mut().push((beads.iter().map(|b| b.to_string()).collect(), files.to_vec()));
    }
    fn lc_certify(&self, bead: &str, tip: &str, outcome: &str, detail: &str, step: &str, message: &str) -> String {
        self.lc.borrow_mut().push(
            [bead, tip, outcome, detail, step, message]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        );
        self.lc_refusal.borrow().clone()
    }
    fn lc_gate_start(&self, bead: &str) {
        self.gate_starts.borrow_mut().push(bead.to_string());
    }
    fn harness_hash(&self) -> Option<String> {
        Some(self.harness.get().into())
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
    fn tree_lock_open(&self, lock: &Path) -> bool {
        self.locks_opened.borrow_mut().push(lock.to_path_buf());
        self.lock_open.get()
    }
    fn tree_lock_try(&self) -> bool {
        let busy = self.lock_busy_tries.get();
        if busy > 0 {
            self.lock_busy_tries.set(busy - 1);
            return false;
        }
        self.lock_free.get()
    }
    fn write_holder(&self, _: &Path) {}
    fn checkout(&self, _: &Path, _: &Path, rev: &str, want: &str) -> Result<(), String> {
        if let Some(e) = self.checkout_err.borrow().clone() {
            return Err(e);
        }
        self.checkouts.borrow_mut().push(rev.to_string());
        self.checkout_wants.borrow_mut().push(want.to_string());
        Ok(())
    }
    fn build_wrapper(&self, path: &str, setting: &str) -> Result<spira_config::build::Wrapper, String> {
        self.wrapper_asked.borrow_mut().push((path.to_string(), setting.to_string()));
        self.wrapper.borrow().clone()
    }
    fn lc_build_dir(&self, _: &str) -> Option<(String, PathBuf)> {
        self.build_dir_fact.borrow().clone()
    }
    fn target_on_tmpfs(&self, tree: &Path, root: &str, _: &str, _: &crate::target::Limits, builder: Option<&crate::ports::Builder>, repo: &str) -> Result<String, String> {
        self.target_repos.borrow_mut().push(repo.to_string());
        self.builders.borrow_mut().push(builder.cloned());
        self.targets.borrow_mut().push(tree.to_path_buf());
        self.target_roots.borrow_mut().push(root.to_string());
        if self.target_clears_tools.get() {
            let tools = tree.join("target/tree-tools");
            self.files.borrow_mut().retain(|p, _| !p.starts_with(&tools));
        }
        let mut line = match self.target_err.borrow().clone() {
            Some(e) => return Err(e),
            None if !root.is_empty() => format!("gate: build on {root}/{}", tree.file_name().unwrap().to_string_lossy()),
            None => format!("gate: build on /tmp/t/{}", tree.file_name().unwrap().to_string_lossy()),
        };
        let used = match (builder, self.builder_unusable.borrow().as_ref()) {
            (Some(b), None) => {
                line.push_str(&format!("\n{}{} (leased from {}'s builder at {})", crate::warm::BUILDER_IN, b.dir.display(), b.bead, b.tip));
                true
            }
            (Some(b), Some(why)) => {
                line.push_str(&format!("\ngate: builder directory {} not used ({why})", b.dir.display()));
                false
            }
            (None, _) => false,
        };
        if let (false, Some(w)) = (used, self.warm_line.borrow().as_ref()) {
            line.push_str(&format!("\n{}{w}", crate::warm::WARM_FROM));
        }
        Ok(line)
    }
    fn target_room(&self, root: &str, _: &str) -> String {
        format!("gate: target root {root}: 1 MiB writable, gate targets hold 7 MiB there")
    }
    fn target_intact(&self, _: &Path) -> bool {
        !self.target_swept.get()
    }
    fn release_target(&self, tree: &Path, keep_release: bool, hand_on: bool, cap_mib: u64, _: &str) {
        self.hand_on.borrow_mut().push(hand_on);
        self.release_caps.borrow_mut().push(cap_mib);
        self.released.borrow_mut().push((tree.to_path_buf(), keep_release));
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
    fn remove_worktree(&self, _: &Path, _: &Path) {
        self.removed_trees.set(self.removed_trees.get() + 1);
    }
    fn declares_build_tier(&self, _: &Path) -> bool {
        self.repo_build_secs.get().is_some()
    }
    fn run_build(&self, tree: &Path, env: &[(String, String)], timeout: &str, stall: u64, cmd: &str) -> (i32, String) {
        self.scripted_build(tree, env, timeout, Some(stall), cmd)
    }
    fn run_gate(&self, tree: &Path, env: &[(String, String)], timeout: &str, cmd: &str) -> (i32, String) {
        self.scripted_build(tree, env, timeout, None, cmd)
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
    fn machine_save(&self, _: &Path, r: &crate::machine::Run) -> Result<(), String> {
        if let Some(e) = self.machine_err.borrow().clone() {
            return Err(e);
        }
        self.machine.borrow_mut().push(r.clone());
        Ok(())
    }
    fn machine_cancelled(&self, _: &Path, _: &str) -> bool {
        self.cancel_at.get().is_some_and(|n| self.machine.borrow().last().is_some_and(|r| r.events.len() >= n))
    }
    fn machine_clear_cancel(&self, _: &Path, _: &str) {}
    fn eprint(&self, s: &str) {
        self.err.borrow_mut().push(s.to_string());
    }
}

// ---------------------------------------------------------------------------- the merge

#[test_id(tm376ja)]
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

#[test_id(twoc9zp)]
#[test]
fn a_merge_that_cannot_be_computed_is_no_verdict() {
    let f = Fake::new();
    *f.merge.borrow_mut() = Merge::Failed("boom".into());
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=merge-failed"));
}

#[test_id(tm4h437)]
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

#[test_id(tzn4oh2)]
#[test]
fn a_current_branch_is_judged_as_itself() {
    let f = Fake::new();
    f.ancestor.set(true);
    assert_eq!(f.run(), PASS);
    assert_eq!(f.checkouts.borrow()[0], BR);
    assert_eq!(f.env_of(0, "SPIRA_GATE_BRANCH"), BR);
}

#[test_id(tpo7xbh)]
#[test]
fn the_gate_command_gets_the_launcher_path_set_outright_from_spira_release() {
    let f = Fake::new();
    f.ancestor.set(true);
    f.put_var("PATH", "inherited/.cargo/bin:checkout/target/release:usr/bin");
    assert_eq!(f.run(), PASS);
    assert_eq!(
        f.env_of(0, "PATH"),
        format!("rel/bin:rel/spira:{}:{}", system_paths::SYSTEM_PATH, ctx().var("SPIRA_PATH")),
        "the release's bin/ and spira/, the system dirs, then the box's own tool tail — nothing inherited"
    );
    assert_eq!(f.env_of(0, "SPIRA_RELEASE"), "rel");
}

#[test_id(tdfo29g)]
#[test]
fn the_trial_env_carries_spira_toml_so_the_box_tools_a_suites_step_runs_can_resolve_config() {
    let f = Fake::new();
    f.ancestor.set(true);
    f.put_var("SPIRA_TOML", "box/spira.toml");
    assert_eq!(f.run(), PASS);
    assert_eq!(f.env_of(0, "SPIRA_TOML"), "box/spira.toml");
}

#[test_id(t6392xf)]
#[test]
fn a_path_tail_entry_inside_a_release_is_no_verdict_naming_it_and_nothing_runs() {
    let f = Fake::new();
    f.ancestor.set(true);
    f.put_var("SPIRA_PATH", "x/spira-releases/def/bin");
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=release-unset"), "{}", f.verdict_line());
    assert!(f.stderr().contains("spira-releases"), "{}", f.stderr());
    assert!(f.ran.borrow().is_empty(), "no gate command ran");
}

#[test_id(tmxj65u)]
#[test]
fn an_unset_spira_release_is_no_verdict_naming_it_and_nothing_runs() {
    let f = Fake::new();
    f.ancestor.set(true);
    f.put_var("SPIRA_RELEASE", "");
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=release-unset"), "{}", f.verdict_line());
    assert!(f.stderr().contains("SPIRA_RELEASE is not set"));
    assert!(f.ran.borrow().is_empty(), "no gate command ran");
}

// covers: UC-gate-verdict-05
#[test_id(tg5sj8j)]
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

// covers: UC-gate-verdict-05
#[test_id(t5wg6oq)]
#[test]
fn beads_data_in_the_merged_tree_is_refused() {
    let f = Fake::new();
    *f.beads.borrow_mut() = ".beads/issues.jsonl\n".into();
    assert_eq!(f.run(), FAIL);
    assert!(f.stderr().contains("gate:   .beads/issues.jsonl"));
    assert!(f.verdict_line().contains("reason=beads-data"));
}

// ---------------------------------------------------------------------------- the trial

#[test_id(tpecqak)]
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

#[test_id(tctp413)]
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
    assert_eq!(f.run(), PASS);
    assert_eq!(
        f.verdict_line(),
        "gate: VERDICT=PASS reason=pass branch=spira/sp-a repo=spira suite=test-b.sh"
    );
    let e = f.stderr();
    assert!(
        e.contains("gate: inherited, not judged (red on local/main too, so not this branch's): test-b.sh")
            && e.contains("gate: local/main is red on: test-b.sh test-c.sh"),
        "{e}"
    );
}

#[test_id(tecc05p)]
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
    assert!(e.contains("red on this branch and not on local/main: test-d.sh\ngate: inherited, not judged (red on local/main too): test-b.sh"), "{e}");
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
#[test_id(tvi51cj)]
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
    assert_eq!(f.run(), PASS);
    assert_eq!(
        f.verdict_line(),
        format!(
            "gate: VERDICT=PASS reason=pass branch=spira/sp-a repo=spira suite={SUITE}"
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
#[test_id(t9gqlpt)]
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
    assert_eq!(f.run(), PASS);
    assert!(f.stderr().contains("gate: inherited, not judged"), "{}", f.stderr());
    assert!(f.verdict_line().ends_with(&format!("suite={SUITE}")));
    assert_eq!(f.ran.borrow().len(), 3, "the base ran it: only the confirming retry follows");
}

/// Acceptance 2: the base trial did not select the branch's red suite. It is run on the
/// base by name before judging; red there too → base-red, not branch-red.
#[test_id(ta05hd0)]
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
    assert_eq!(f.run(), PASS);
    assert!(f.stderr().contains("inherited, not judged"), "{}", f.stderr());
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

/// The same red, reproduced by the retry, is BASE_FAIL.
#[test_id(to1z6wz)]
#[test]
fn a_base_red_that_reproduces_is_inherited_not_judged() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, red(SUITE)));
    f.runs.borrow_mut().insert(BASE.into(), (1, red(SUITE)));
    f.reruns.borrow_mut().insert((BASE.into(), SUITE.into()), (1, red(SUITE)));
    assert_eq!(f.run(), PASS);
    assert!(f.stderr().contains("inherited, not judged"), "{}", f.stderr());
}

/// The re-run finds the suite green on the base: now, and only now, it is the branch's.
#[test_id(tdrjiij)]
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
    PathBuf::from(format!("run/verdicts/base-suites/spira/{BASE_TREE_HEX}/{suite}"))
}

/// A full cache hit answers every red suite without running anything extra on the base — the
/// acceptance this bead exists for: "a flip-suspect (base green in cache, branch red) re-runs
/// nothing extra."
#[test_id(td3wlkl)]
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
#[test_id(toiuxyy)]
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
    assert_eq!(f.run(), PASS);
    assert!(
        f.stderr().contains(&format!("inherited, not judged (red on local/main too, so not this branch's): {SUITE}")),
        "a cached red still names the suite: {}",
        f.stderr()
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
#[test_id(tp6zudz)]
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
#[test_id(tdfsgzt)]
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
#[test_id(tdysoxq)]
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
    assert_eq!(f.run(), PASS);
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
#[test_id(tenhmql)]
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
#[test_id(t2ovhj4)]
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
        format!("{GATE_TREE}/target/tree-tools/tree-of-{BASE}/testenv"),
        "the rerun used the base's own tree-built testenv"
    );
}

/// The same regression, but the rerun never gets the chance to run at all — if it did (the
/// defect, before the fix), this is exactly the silent BaseUntestable production saw: no
/// `SPIRA_TESTENV_BIN` reaching a tree that declared one would make a real `testenv`
/// resolve to nothing on the release's PATH. Asserting the env reaches the image-tag query
/// too (the cache's own first call) covers sp-kqger's other new call site.
#[test_id(tnw03l2)]
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
        format!("{GATE_TREE}/target/tree-tools/tree-of-{BASE}/testenv")
    );
}

/// testenv's --deadline deferred the suite on the base: not run, so re-run.
#[test_id(twzm9ra)]
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
    assert_eq!(f.run(), PASS);
}

/// A red suite the base does not have is the branch's own; nothing to run on the base.
#[test_id(tuqrrsi)]
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

#[test_id(ta31f8y)]
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
#[test_id(tznqhkv)]
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
#[test_id(t0amiz0)]
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
#[test_id(tvrwj2q)]
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

#[test_id(tewap5e)]
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

/// Fences are not charged to the deadline, so only the gate timeout can end the base trial:
/// its rc 124 leaves the base untestable, it is no deadline.
#[test_id(tle0qeh)]
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
    assert!(f.verdict_line().contains("reason=base-untestable"), "{}", f.verdict_line());
}

#[test_id(tbvpm8s)]
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

#[test_id(t49ag95)]
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

// covers: UC-gate-verdict-06
#[test_id(t9oon4u)]
#[test]
fn an_empty_gate_string_passes_on_syntax() {
    let f = Fake::new();
    f.ctx.borrow_mut().as_mut().unwrap().gate_cmd = String::new();
    assert_eq!(f.run(), PASS);
    assert!(f.verdict_line().contains("reason=syntax-only"));
    assert!(f.ran.borrow().is_empty());
}

// covers: UC-gate-verdict-07
#[test_id(t9bxk79)]
#[test]
fn a_gate_string_naming_a_file_the_base_lacks_is_configuration() {
    let f = Fake::new();
    f.ctx.borrow_mut().as_mut().unwrap().gate_cmd = "bash spira/missing.sh".into();
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=cmd-missing-file"));
}

// covers: UC-gate-verdict-25
#[test_id(tvu5zb7)]
#[test]
fn the_early_refusals() {
    let f = Fake::new();
    f.readable.borrow_mut().remove(Path::new("cfg/repo-map"));
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
    {
        let mut c = f.ctx.borrow_mut();
        let c = c.as_mut().unwrap();
        c.landref = None;
        c.declared_base = "origin/trunk".into();
    }
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.stderr().contains("declares base 'origin/trunk'"), "{}", f.stderr());
    assert!(!f.stderr().contains("give it a `base` column"), "{}", f.stderr());

    let f = Fake::new();
    *f.diff.borrow_mut() = Err("fatal: bad revision".into());
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=no-diff"));

    let f = Fake::new();
    *f.ctx.borrow_mut() = Err("exit 96".into());
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=lib-unavailable"));
}

// covers: UC-gate-verdict-05
#[test_id(t83ad5i)]
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

// covers: UC-gate-verdict-03
#[test_id(th3mi7i)]
#[test]
fn an_unreadable_repo_map_is_no_verdict_and_a_repo_absent_from_it_is_another() {
    let f = Fake::new();
    f.readable.borrow_mut().remove(Path::new("cfg/repo-map"));
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=no-repo-map-file"), "{}", f.verdict_line());
    assert!(f.ran.borrow().is_empty(), "no trial runs against a map that cannot be read");

    let f = Fake::new();
    f.ctx.borrow_mut().as_mut().unwrap().repo_root = None;
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=no-repo-map"), "{}", f.verdict_line());
    assert!(!f.verdict_line().contains("no-repo-map-file"), "{}", f.verdict_line());
}

// covers: UC-gate-verdict-04
#[test_id(tsav7l4)]
#[test]
fn an_unresolvable_branch_or_base_is_no_verdict_75_naming_the_range_the_repo_and_gits_line() {
    let f = Fake::new();
    *f.diff.borrow_mut() = Err("fatal: bad revision 'spira/sp-a'".into());
    assert_eq!(f.run(), 75);
    assert!(f.verdict_line().contains("reason=no-diff"), "{}", f.verdict_line());
    let err = f.stderr();
    assert!(err.contains(&format!("{BASE}...{BR}")), "{err}");
    assert!(err.contains(" in spira "), "{err}");
    assert!(err.contains("fatal: bad revision 'spira/sp-a'"), "{err}");

    let f = Fake::new();
    f.ctx.borrow_mut().as_mut().unwrap().landref = None;
    assert_eq!(f.run(), 75);
    assert!(f.verdict_line().contains("reason=no-base"), "{}", f.verdict_line());
}

// covers: UC-gate-verdict-05
#[test_id(tj2nio1)]
#[test]
fn a_missing_exclude_or_skew_tool_is_no_verdict_not_a_pass() {
    let f = Fake::new();
    f.readable.borrow_mut().remove(Path::new("h/exclude.sh"));
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=missing-exclude"), "{}", f.verdict_line());
    assert!(f.ran.borrow().is_empty());

    let f = Fake::new();
    f.readable.borrow_mut().remove(Path::new("h/skew"));
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=missing-skew"), "{}", f.verdict_line());
    assert!(f.ran.borrow().is_empty());
}

// covers: UC-gate-verdict-05
#[test_id(tj23wqi)]
#[test]
fn beads_data_anywhere_in_the_branch_tree_fails_though_the_diff_does_not_name_it() {
    let f = Fake::new();
    *f.diff.borrow_mut() = Ok("src/a.rs\n".into());
    *f.beads.borrow_mut() = "deep/nested/.beads/issues.jsonl\n".into();
    assert_eq!(f.run(), FAIL);
    assert!(f.verdict_line().contains("reason=beads-data"), "{}", f.verdict_line());
    assert!(f.stderr().contains("deep/nested/.beads/issues.jsonl"), "{}", f.stderr());
}

// covers: UC-gate-verdict-07
#[test_id(tsrktr9)]
#[test]
fn a_gate_command_runs_once_the_file_it_names_is_on_the_base() {
    let f = Fake::new();
    f.ctx.borrow_mut().as_mut().unwrap().gate_cmd = "bash spira/present.sh".into();
    f.prints("spira/present.sh", &["present"]);
    assert_eq!(f.run(), PASS);
    assert_eq!(f.ran.borrow().len(), 1, "the command ran");
}

// covers: UC-gate-verdict-25
#[test_id(tyzwtje)]
#[test]
fn a_verdict_after_the_lock_is_metered_once_and_a_death_by_signal_too() {
    let f = Fake::new();
    assert_eq!(f.run(), PASS);
    assert_eq!(f.appended.borrow().len(), 1, "{:?}", f.appended.borrow());

    let f = Fake::new();
    f.admission_free.set(false);
    f.signal.set(true);
    assert_eq!(f.run(), NOVERDICT);
    assert_eq!(f.appended.borrow().len(), 1, "{:?}", f.appended.borrow());
}

#[test_id(te1bwfk)]
#[test]
fn phantom_bead_id_fails_and_store_fault_is_no_verdict() {
    let f = Fake::new();
    f.cite.set(3);
    assert_eq!(f.run(), FAIL);
    assert!(f.verdict_line().contains("reason=phantom-bead-id"));
    let f = Fake::new();
    f.cite.set(2);
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=commit-cite-fault"));
}

#[test_id(tb1daun)]
#[test]
fn the_gate_and_submit_judge_the_same_messages_alike() {
    // spira-lc's `cite-check` and `work submit` both run `spira_config::cite::phantoms` and map
    // a phantom to 3, a store that cannot answer to 2; the gate is fed those codes here.
    let held = ["sp-real1".to_string()];
    for (message, refused) in [
        ("sp-real1: x\n\nanswers sp-a and sp-b", false),
        ("sp-real1: x\n\nsee sp-zzzzzz", true),
        ("sp-real1: x\n\nsee sp-real1", false),
    ] {
        let commits = vec![("0123456789abcdef".to_string(), message.to_string())];
        let found = spira_config::cite::phantoms(&commits, "sp", &mut |ids| Ok(ids.iter().filter(|i| held.contains(i)).cloned().collect())).unwrap();
        let f = Fake::new();
        f.cite.set(if found.is_empty() { 0 } else { 3 });
        assert_eq!(f.run() == FAIL, refused, "{message}");
        assert_eq!(!found.is_empty(), refused, "{message}");
    }
    let f = Fake::new();
    f.cite.set(2);
    assert_eq!(f.run(), NOVERDICT, "a store that cannot answer is no verdict, as it is CANNOT_TELL at submit");
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
        deny: "",
    })
}

#[test_id(t3cqqe5)]
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

#[test_id(tuy110r)]
#[test]
fn a_fresh_cached_pass_skips_admission_and_the_trial() {
    let f = Fake::new();
    let k = key_for(&f, "", "none");
    f.files.borrow_mut().insert(
        PathBuf::from(format!("{RUN}/verdicts/{k}")),
        key::render_entry(
            "2026-09-28T00:00:00Z",
            999_000,
            "dwarf",
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
            .contains("already passed spira's gate at 2026-09-28T00:00:00Z (dwarf)")
            && f.stderr().contains("covered suites: test-a.sh")
    );
    assert!(f.ran.borrow().is_empty() && f.checkouts.borrow().is_empty());
    assert!(f.appended.borrow()[0].contains("rc=0 cached test_clock=0s\n"));
}

#[test_id(tjt2eoz)]
#[test]
fn a_stale_cached_pass_runs_again() {
    let f = Fake::new();
    f.put_var("SPIRA_VERDICT_TTL", "10");
    let k = key_for(&f, "", "none");
    f.files.borrow_mut().insert(
        PathBuf::from(format!("{RUN}/verdicts/{k}")),
        key::render_entry("w", 999_000, "b", "spira", BR, "-"),
    );
    assert_eq!(f.run(), PASS);
    assert!(f.verdict_line().contains("reason=pass"));
}

#[test_id(tvwlvj2)]
#[test]
fn only_a_pass_is_cached() {
    let f = Fake::new();
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (1, "test-b.sh RED".into()));
    f.run();
    assert!(f.written.borrow().iter().all(|(p, _)| p.to_string_lossy().contains("verdicts/red/")), "{:?}", f.written.borrow());
}

#[test_id(tkf4kqk)]
#[test]
fn a_red_is_answered_from_its_record_and_does_not_run_again() {
    let f = Fake::new();
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (1, "test-b.sh RED".into()));
    let first = f.run();
    let (p, entry) = f.written.borrow().iter().find(|(p, _)| p.to_string_lossy().contains("verdicts/red/")).cloned().expect("a red record");
    let line = f.verdict_line();
    let g = Fake::new();
    g.put_var("SPIRA_VERDICT_TTL", "86400");
    let entry = entry.lines().map(|l| if l.starts_with("at=") { "at=999999".to_string() } else { l.to_string() }).collect::<Vec<_>>().join("\n") + "\n";
    g.files.borrow_mut().insert(p, entry);
    assert_eq!(g.run(), first);
    assert!(g.ran.borrow().is_empty(), "{:?}", g.ran.borrow());
    assert!(g.verdict_line().contains("reason=branch-red") || line.contains("reason=base-red"), "{}", g.verdict_line());
    assert!(g.written.borrow().iter().all(|(p, _)| !p.to_string_lossy().contains("verdicts/red/")));
    let log = g.appended.borrow()[0].clone();
    assert!(log.contains(" replayed=") && log.contains("replayed=key:"), "{log}");
    let err = g.stderr();
    assert!(err.contains("queue.sh absolve"), "{err}");
    assert!(err.contains(&format!("when {BASE} moves")), "{err}");
}

#[test_id(t5tqinx)]
#[test]
fn a_red_recorded_before_the_base_moved_is_not_replayed_after() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "test-b.sh RED".into()));
    f.run();
    let (p, entry) = f.written.borrow().iter().find(|(p, _)| p.to_string_lossy().contains("verdicts/red/")).cloned().expect("a red record");
    let g = Fake::new();
    g.put_var("SPIRA_VERDICT_TTL", "86400");
    *g.merge.borrow_mut() = Merge::Clean("a-merged-tree-after-the-base-moved".into());
    let entry = entry.lines().map(|l| if l.starts_with("at=") { "at=999999".to_string() } else { l.to_string() }).collect::<Vec<_>>().join("\n") + "\n";
    g.files.borrow_mut().insert(p, entry);
    g.run();
    assert!(!g.ran.borrow().is_empty(), "the moved base is judged again");
    assert!(!g.appended.borrow()[0].contains("replayed"), "{}", g.appended.borrow()[0]);
}

#[test_id(t84w535)]
#[test]
fn a_red_recorded_before_the_deny_list_changed_is_not_replayed_after() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "test-b.sh RED".into()));
    f.run();
    let (p, entry) = f.written.borrow().iter().find(|(p, _)| p.to_string_lossy().contains("verdicts/red/")).cloned().expect("a red record");
    let g = Fake::new();
    g.put_var("SPIRA_VERDICT_TTL", "86400");
    *g.deny.borrow_mut() = Ok("disabled\ttest:t0pab00\tsp-x\t9999999999\n".into());
    let entry = entry.lines().map(|l| if l.starts_with("at=") { "at=999999".to_string() } else { l.to_string() }).collect::<Vec<_>>().join("\n") + "\n";
    g.files.borrow_mut().insert(p, entry);
    g.run();
    assert!(!g.ran.borrow().is_empty(), "a changed deny list is judged again");
    assert!(!g.appended.borrow()[0].contains("replayed"), "{}", g.appended.borrow()[0]);
}

#[test_id(t5a1ykw)]
#[test]
fn a_red_recorded_under_the_same_denied_tests_is_replayed_whatever_the_header_bead_or_expiry() {
    let f = Fake::new();
    *f.deny.borrow_mut() = Ok("# test-registry deny-list at=1 rows=1\ndisabled\ttest:t0pab00\tsp-x\t9999999999\n".into());
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "test-b.sh RED".into()));
    f.run();
    let (p, entry) = f.written.borrow().iter().find(|(p, _)| p.to_string_lossy().contains("verdicts/red/")).cloned().expect("a red record");
    let g = Fake::new();
    g.put_var("SPIRA_VERDICT_TTL", "86400");
    *g.deny.borrow_mut() = Ok("# test-registry deny-list at=2 rows=1\ndisabled\ttest:t0pab00\tsp-y\t8888888888\n".into());
    let entry = entry.lines().map(|l| if l.starts_with("at=") { "at=999999".to_string() } else { l.to_string() }).collect::<Vec<_>>().join("\n") + "\n";
    g.files.borrow_mut().insert(p, entry);
    g.run();
    assert!(g.appended.borrow()[0].contains("replayed=key:"), "{}", g.appended.borrow()[0]);
}

#[test_id(terk6ed)]
#[test]
fn a_red_recorded_under_another_gate_is_not_replayed_and_the_tip_is_judged_again() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "test-b.sh RED".into()));
    f.run();
    let (p, entry) = f.written.borrow().iter().find(|(p, _)| p.to_string_lossy().contains("verdicts/red/")).cloned().expect("a red record");
    let g = Fake::new();
    g.harness.set("a-gate-that-changed");
    g.put_var("SPIRA_VERDICT_TTL", "86400");
    let entry = entry.lines().map(|l| if l.starts_with("at=") { "at=999999".to_string() } else { l.to_string() }).collect::<Vec<_>>().join("\n") + "\n";
    g.files.borrow_mut().insert(p, entry);
    assert_eq!(g.run(), PASS);
    assert!(!g.ran.borrow().is_empty(), "the changed gate ran the tip");
    assert!(!g.appended.borrow()[0].contains("replayed"), "{}", g.appended.borrow()[0]);
}

#[test_id(t0nt5zr)]
#[test]
fn forgetting_a_tip_removes_the_red_recorded_for_it_and_no_other() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "test-b.sh RED".into()));
    f.run();
    let entry = f.written.borrow().iter().find(|(p, _)| p.to_string_lossy().contains("verdicts/red/")).map(|(_, e)| e.clone()).expect("a red record");
    let tip = entry.lines().find_map(|l| l.strip_prefix("tip=")).expect("the entry names its tip").to_string();
    assert!(!tip.is_empty() && tip != "-");
    let tmp = testkit::TempDir::new("redcache-forget");
    let dir = tmp.to_path_buf();
    let red = dir.join("red");
    std::fs::create_dir_all(&red).unwrap();
    std::fs::write(red.join("aaa"), &entry).unwrap();
    std::fs::write(red.join("bbb"), entry.replace(&format!("tip={tip}"), "tip=other")).unwrap();
    assert_eq!(crate::redcache::forget_tip(&dir, &tip), 1);
    assert!(!red.join("aaa").exists() && red.join("bbb").exists());
}

// ---------------------------------------------------------------------------- ejected suites

#[test_id(tgb4qb1)]
#[test]
fn ejected_suites_come_from_the_ejected_file() {
    let f = Fake::new();
    f.put_var("SPIRA_GATE_BEAD", "sp-a");
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

#[test_id(to66wr1)]
#[test]
fn a_sidecar_under_the_retired_landstate_ledger_is_not_read() {
    // sp-2c1n0: the ledger is deleted; a stale `<id>.ejected` left under it forces nothing.
    let f = Fake::new();
    f.put_var("SPIRA_GATE_BEAD", "sp-a");
    f.files.borrow_mut().insert(PathBuf::from(format!("{RUN}/landstate/sp-a.ejected")), "test-z.sh\n".into());
    f.run();
    assert_eq!(f.env_of(0, "SPIRA_GATE_EJECTED_SUITES"), "");
}

// ---------------------------------------------------------------------------- admission & lock

#[test_id(tajpxl5)]
#[test]
fn admission_times_out_as_no_verdict() {
    let f = Fake::new();
    f.admission_free.set(false);
    f.put_var("SPIRA_GATE_LOCK_WAIT", "5");
    assert_eq!(f.run(), NOVERDICT);
    assert!(f
        .stderr()
        .contains("all 2 host-wide gate admission slots busy for 5s"));
    assert!(f.ran.borrow().is_empty());
}

#[test_id(t62rsdx)]
#[test]
fn a_full_gate_pool_is_said_naming_its_holders_and_a_taken_slot_names_the_branch() {
    let f = Fake::new();
    f.admission_free.set(false);
    f.put_var("SPIRA_GATE_LOCK_WAIT", "5");
    assert_eq!(f.run(), NOVERDICT);
    assert_eq!(f.stderr().matches("gate: waiting for a gate slot: 2 of 2 held by fake").count(), 1, "{}", f.stderr());
    // POSITIVE CONTROL: a free pool says nothing about waiting, records who holds the slot, and
    // every command the trial runs inherits the gate's admission.
    let g = Fake::new();
    g.put_var("SPIRA_VERDICT_TTL", "86400");
    assert_eq!(g.run(), PASS);
    assert!(!g.stderr().contains("waiting for a gate slot"));
    assert_eq!(g.admission_who.borrow().first().map(String::as_str), Some(BR));
    assert_eq!(g.env_of(0, "SPIRA_ADMISSION"), "gate");
}

#[test_id(ty3nezi)]
#[test]
fn fences_only_certification_takes_no_admission_slot() {
    let f = Fake::new();
    f.admission_free.set(false);
    f.put_var("SPIRA_GATE_SUITES", "off");
    assert_eq!(f.run(), PASS);
    assert_eq!(f.env_of(0, "SPIRA_GATE_SUITES"), "off");
}

#[test_id(tetj8f8)]
#[test]
fn a_held_tree_times_out_and_meters_the_wait() {
    let f = Fake::new();
    f.lock_free.set(false);
    f.put_var("SPIRA_GATE_LOCK_WAIT", "3");
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

#[test_id(tpljjxu)]
#[test]
fn a_timed_out_gate_says_it_is_a_queue_not_a_fault_and_leaves_the_holders_tree() {
    let f = Fake::new();
    f.lock_free.set(false);
    f.put_var("SPIRA_GATE_LOCK_WAIT", "2");
    assert!(f.exists(&Path::new(GATE_TREE).join(".git")), "the holder's tree is registered");
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.stderr().contains("not a fault in the branch"), "{}", f.stderr());
    assert_eq!(f.removed_trees.get(), 0);
    assert!(f.ran.borrow().is_empty(), "a gate that never got the tree judged nothing");
}

#[test_id(tevuqvc)]
#[test]
fn a_lockfile_that_cannot_be_opened_is_no_verdict_no_lockfile() {
    let f = Fake::new();
    f.lock_open.set(false);
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=no-lockfile"), "{}", f.verdict_line());
    assert!(f.stderr().contains("cannot open the gate tree's lock"), "{}", f.stderr());
    assert_eq!(f.removed_trees.get(), 0);
    assert!(f.ran.borrow().is_empty());
}

#[test_id(tlkwhwz)]
#[test]
fn gates_on_different_branches_lock_different_trees_and_one_branch_locks_one() {
    let lock_of = |branch: &str| {
        let f = Fake::new();
        f.run_on(branch, false);
        let lock = f.locks_opened.borrow().first().cloned().expect("a lock was opened");
        lock
    };
    let a = lock_of("spira/sp-a");
    let b = lock_of("spira/sp-b");
    assert_eq!(a, PathBuf::from("run/worktree/.gate.spira.spira-sp-a.lock"));
    assert_eq!(b, PathBuf::from("run/worktree/.gate.spira.spira-sp-b.lock"));
    assert_eq!(a, lock_of("spira/sp-a"), "the same branch always contends for the same lock");
}

#[test_id(tas4dem)]
#[test]
fn a_gate_that_waits_for_its_branchs_tree_meters_the_wait_and_then_judges_it() {
    let f = Fake::new();
    f.lock_busy_tries.set(3);
    f.put_var("SPIRA_GATE_LOCK_WAIT", "60");
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(f.stderr().contains(&format!("gate: waited 3s for {GATE_TREE}")), "{}", f.stderr());
    let row = f.appended.borrow()[0].clone();
    assert!(row.contains("waited=3s") && row.contains("waited_ms=3000"), "{row}");
}

#[test_id(tn6y733)]
#[test]
fn a_gate_that_never_waited_for_its_tree_meters_no_wait() {
    let f = Fake::new();
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(!f.stderr().contains("gate: waited"), "{}", f.stderr());
    let row = f.appended.borrow()[0].clone();
    assert!(row.contains("waited=0s") && !row.contains("waited_ms="), "{row}");
}

#[test_id(tgykx3n)]
#[test]
fn a_passing_gate_keeps_its_tree_and_a_red_one_removes_it() {
    let pass = Fake::new();
    assert_eq!(pass.run(), PASS, "{}", pass.stderr());
    assert_eq!(pass.removed_trees.get(), 0, "a pass keeps the tree for the next gate's checkout");
    let red = Fake::new();
    red.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "  test-a.sh RED rc=1".into()));
    red.runs.borrow_mut().insert(BASE.into(), (0, "test-a.sh ok".into()));
    assert_eq!(red.run(), FAIL, "{}", red.stderr());
    assert_eq!(red.removed_trees.get(), 1, "a red gate removes the tree it held");
}

#[test_id(t0o8zgr)]
#[test]
fn a_tree_that_cannot_be_checked_out_is_no_verdict_tree_unidentified_and_is_removed() {
    let f = Fake::new();
    *f.checkout_err.borrow_mut() = Some("gate: worktree add failed".into());
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=tree-unidentified"), "{}", f.verdict_line());
    assert!(f.ran.borrow().is_empty());
    assert_eq!(f.removed_trees.get(), 1, "the half-made tree is this process's to remove");
}

#[test_id(twjqjoz)]
#[test]
fn a_tree_that_was_never_created_is_not_removed() {
    let f = Fake::new();
    f.worktree.set(false);
    *f.checkout_err.borrow_mut() = Some("gate: worktree add failed".into());
    assert_eq!(f.run(), NOVERDICT);
    assert_eq!(f.removed_trees.get(), 0, "nothing to deregister");
}

#[test_id(t0ql1rj)]
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
#[test_id(tnnmo2u)]
#[test]
fn the_waiting_message_prints_exactly_once() {
    let f = Fake::new();
    f.admission_free.set(false);
    f.put_var("SPIRA_GATE_LOCK_WAIT", "3");
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
#[test_id(tag1s3e)]
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
#[test_id(tswyxmz)]
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
#[test_id(tssws7s)]
#[test]
fn a_limit_raised_in_the_live_config_admits_an_already_waiting_gate() {
    let f = Fake::new();
    f.admission_only_slot_free.set(Some(3));
    f.certify_par_live_after.set(1); // first poll still sees the frozen par (2)
    f.certify_par_live.set(Some(3)); // every poll after that sees the raised one
    f.put_var("SPIRA_GATE_LOCK_WAIT", "5");
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

#[test_id(t1ngk8r)]
#[test]
fn the_meter_row_and_yield_on_a_pass() {
    let f = Fake::new();
    f.run();
    assert_eq!(
        f.appended.borrow()[0],
        "2026-09-29T00:00:00Z spira spira/sp-a waited=0s ran=7s rc=0 pass compose=suites(mode) phases=gate:7 deny_list=1000000/e3b0c44298fc test_clock=7s\n",
        "the composition and its phase walls trail the reason (sp-2ghui)"
    );
    assert_eq!(
        f.yields.borrow()[0],
        [RUN, "pass", "spira", BR, "tree-of-spira/sp-a"]
    );
}

#[test_id(tkk6b7c)]
#[test]
fn a_red_certification_carries_the_failed_step_and_its_last_message_line() {
    let f = Fake::new();
    f.put_var("SPIRA_GATE_BEAD", "sp-a");
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "gate-step RED rc=1 :: fence-f\n  test-s1.sh RED rc=1 after 3s".into()));
    f.runs.borrow_mut().insert(BASE.into(), (0, String::new()));
    f.run();
    let lc = f.lc.borrow()[0].clone();
    assert_eq!(lc[2], "red", "{lc:?}");
    assert!(!lc[4].is_empty() && lc[5].contains("test-s1.sh RED"), "{lc:?}");
}

#[test_id(tf201gh)]
#[test]
fn a_red_fence_certifies_with_its_message_when_the_message_precedes_the_gate_step_line() {
    let f = Fake::new();
    f.put_var("SPIRA_GATE_BEAD", "sp-a");
    let out = "gate fixture-config: complete.toml does not resolve\ngate-step RED rc=1 :: fixture-config";
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, out.into()));
    f.runs.borrow_mut().insert(BASE.into(), (0, String::new()));
    f.run();
    let lc = f.lc.borrow()[0].clone();
    assert_eq!((lc[4].as_str(), lc[5].as_str()), ("fixture-config", "gate fixture-config: complete.toml does not resolve"), "{lc:?}");
}

#[test_id(t0g3wmg)]
#[test]
fn a_branch_red_row_carries_its_red_unit_and_line_and_a_pass_row_carries_neither() {
    let f = Fake::new();
    f.put_var("SPIRA_GATE_BEAD", "sp-a");
    let out = "bwrap: execvp cargo: No such file or directory\ngate-step RED rc=1 :: unit-tests";
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, out.into()));
    f.runs.borrow_mut().insert(BASE.into(), (0, String::new()));
    f.run();
    let v = tsd_row(&f);
    assert_eq!((v["status"].as_str(), v["reason"].as_str()), (Some("FAIL"), Some("branch-red")), "{v}");
    assert_eq!((v["red_unit"].as_str(), v["red_line"].as_str()), (Some("unit-tests"), Some("bwrap: execvp cargo: No such file or directory")));
    let f = unit_fake(&["spira-config/src/lib.rs"]);
    assert_eq!(f.run(), PASS);
    let v = tsd_row(&f);
    assert_eq!((v["red_unit"].as_str(), v["red_line"].as_str()), (Some(""), Some("")));
}

#[test_id(tv4eoc4)]
#[test]
fn certification_events_follow_the_outcome() {
    for (rc, out, base, want) in [
        (0, "test-a.sh ok", 0, "pass"),
        (1, "test-b.sh RED", 0, "red"),
        (1, "test-b.sh RED", 1, "infra"),
    ] {
        let f = Fake::new();
        f.put_var("SPIRA_GATE_BEAD", "sp-a");
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

#[test_id(to8i1t7)]
#[test]
fn a_red_gate_hands_its_failing_output_to_the_bead_before_certifying() {
    let f = Fake::new();
    f.put_var("SPIRA_GATE_BEAD", "sp-a");
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "test-a.sh ok\ntest-b.sh RED".into()));
    f.run();
    let got = f.failures.borrow();
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!((got[0].0.as_str(), got[0].1.as_str()), ("sp-a", BR));
    assert!(got[0].3.contains("test-b.sh"), "{:?}", got[0].3);
}

#[test_id(t80tai6)]
#[test]
fn a_red_and_a_passing_verdict_on_a_bead_tip_each_keep_one_send_back_entry_holding_the_message() {
    for (rc, out) in [(1, "test-a.sh ok\ntest-b.sh RED"), (0, "test-a.sh ok")] {
        let f = Fake::new();
        f.put_var("SPIRA_GATE_BEAD", "sp-a");
        f.runs.borrow_mut().insert(MERGE_SHA.into(), (rc, out.into()));
        f.run();
        let kept = f.kept_logs.borrow();
        assert_eq!(kept.len(), 1, "rc {rc}: {kept:?}");
        assert_eq!(kept[0].0, vec!["sp-a".to_string()]);
        assert_eq!(kept[0].1.len(), 1);
        assert_eq!(kept[0].1[0].0, "trial.out");
        let text = String::from_utf8(kept[0].1[0].1.clone()).unwrap();
        assert!(text.contains("test-a.sh"), "{text}");
        if rc == 1 {
            assert_eq!(text, f.failures.borrow()[0].3, "the verdict's message");
        }
    }
}

#[test_id(tm17dao)]
#[test]
fn a_green_gate_records_no_failure() {
    let f = Fake::new();
    f.put_var("SPIRA_GATE_BEAD", "sp-a");
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (0, "test-a.sh ok".into()));
    f.run();
    assert!(f.failures.borrow().is_empty());
}

#[test_id(tqn3b5q)]
#[test]
fn a_pass_the_row_refuses_for_its_tip_says_so_in_gate_log() {
    let f = Fake::new();
    f.put_var("SPIRA_GATE_BEAD", "sp-a");
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (0, "test-a.sh ok".into()));
    f.runs.borrow_mut().insert(BASE.into(), (0, String::new()));
    *f.lc_refusal.borrow_mut() = "spira-lc certify: sp-a refused: tip mismatch: held 9b8e, gated 30f5".into();
    f.run();
    let rows = f.appended.borrow().clone();
    assert!(rows.iter().any(|r| r.contains("tip-mismatch bead=sp-a") && r.contains("held 9b8e")), "{rows:?}");

    let g = Fake::new();
    g.put_var("SPIRA_GATE_BEAD", "sp-a");
    g.runs.borrow_mut().insert(MERGE_SHA.into(), (0, "test-a.sh ok".into()));
    g.runs.borrow_mut().insert(BASE.into(), (0, String::new()));
    g.run();
    assert!(!g.appended.borrow().iter().any(|r| r.contains("tip-mismatch")), "no refusal, no note");
}

#[test_id(tk9hkxg)]
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
                r#"{{"id":"{n} (path+file:///t/{n})","name":"{n}","manifest_path":"/t/{n}/Cargo.toml","dependencies":[{}],"targets":[{{"kind":["lib"]}},{{"kind":["bin"]}},{{"kind":["test"]}}]}}"#,
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

#[test_id(tbwpis1)]
#[test]
fn a_unit_builds_with_timings_and_the_gate_log_and_run_row_say_where_the_build_went() {
    let f = unit_fake(&["spira-config/src/lib.rs"]);
    *f.timing_report.borrow_mut() = Some(
        r#"<td>Fresh units:</td><td>3</td><td>Dirty units:</td><td>7</td>
const UNIT_DATA = [{"name":"runtime","target":"","start":4.2,"duration":88.4},{"name":"gate","target":" gate \"bin\" (test)","start":90.0,"duration":31.0}];"#
            .into(),
    );
    assert_eq!(f.run(), PASS);
    assert!(f.cmds.borrow().iter().any(|c| c.contains("cargo test --no-run") && c.contains(" --timings")), "{:?}", f.cmds.borrow());
    let m = meter(&f);
    assert!(
        m.contains(" compiled=7 fresh=3 slowest=runtime/lib:88.4@4.2,gate/bin-test:31.0@90.0"),
        "{m}"
    );
    assert!(f.tsd.borrow().iter().any(|r| r.contains("\"compiled\":7")), "{:?}", f.tsd.borrow());
    assert!(f.written.borrow().iter().any(|(p, c)| p.to_string_lossy().contains("cargo-timing") && c.contains("UNIT_DATA")));
}

#[test_id(tnpiai0)]
#[test]
fn a_unit_gate_records_the_test_phases_count_seconds_and_slowest_tests_beside_the_build() {
    let f = unit_fake(&["spira-config/src/lib.rs"]);
    f.phase_secs.set(7);
    f.unit_runs.borrow_mut().insert(
        (MERGE_SHA.into(), "test"),
        (
            0,
            "        PASS [   0.250s] ( 1/3) gate a::quick\n        PASS [   4.500s] ( 2/3) gate a::slow\n        PASS [   1.000s] ( 3/3) queue b::mid\n------------\n     Summary [   5.750s] 3 tests run: 3 passed, 0 skipped".into(),
        ),
    );
    assert_eq!(f.run(), PASS);
    let m = meter(&f);
    assert!(m.contains(" unit_build_secs=7 unit_run_secs=7 unit_tests=3 unit_slowest=gate/a::slow:4.5,queue/b::mid:1,gate/a::quick:0.25"), "{m}");
    let rows = f.tsd.borrow();
    let row = rows.iter().find(|r| r.contains("\"unit_tests\"")).unwrap_or_else(|| panic!("{rows:?}"));
    for want in ["\"unit_tests\":3", "\"unit_run_secs\":7", "\"unit_build_secs\":7", "gate/a::slow:4.5,queue/b::mid:1,gate/a::quick:0.25"] {
        assert!(row.contains(want), "{want} missing from {row}");
    }
}

const SPLIT_FIN: &str = "    Finished `test` profile [unoptimized + debuginfo] target(s) in";

#[test_id(t8zd4t2)]
#[test]
fn a_unit_gate_splits_its_test_phase_into_compile_list_run_integration_and_other_seconds() {
    let f = unit_fake(&["spira-config/src/lib.rs"]);
    f.phase_secs.set(7);
    let out = format!(
        "{SPLIT_FIN} 3.00s\n     Summary [   1.000s] 2 tests run: 2 passed, 0 skipped\ngate-substep unit secs=5\n{SPLIT_FIN} 3.00s\ngate-substep integration secs=1\n"
    );
    f.unit_runs.borrow_mut().insert((MERGE_SHA.into(), "test"), (0, out));
    assert_eq!(f.run(), PASS);
    let m = meter(&f);
    assert!(m.contains(" phases=fences:7,build:7,test-build:4,test-list:1,test:1,test-integration:0,test-other:1 "), "{m}");
    assert!(m.contains(" unit_run_secs=7 "), "{m}");
    assert!(!m.contains("test_recompiled"), "{m}");
}

#[test_id(twpxkuf)]
#[test]
fn a_test_phase_that_compiles_for_over_thirty_seconds_after_its_build_phase_is_marked_recompiled() {
    for (fin, want) in [("3m 27s", true), ("0.50s", false)] {
        let f = unit_fake(&["spira-config/src/lib.rs"]);
        f.phase_secs.set(300);
        let out = format!("{SPLIT_FIN} {fin}\n     Summary [   1.000s] 2 tests run: 2 passed, 0 skipped\ngate-substep unit secs=250\n");
        f.unit_runs.borrow_mut().insert((MERGE_SHA.into(), "test"), (0, out));
        assert_eq!(f.run(), PASS);
        assert_eq!(meter(&f).contains(" test_recompiled=1"), want, "{fin}: {}", meter(&f));
    }
}

#[test_id(tlj8iz9)]
#[test]
fn a_gate_whose_test_phase_did_not_run_records_no_unit_timings() {
    let f = unit_fake(&["docs/x.md"]);
    assert_eq!(f.run(), PASS);
    assert!(!meter(&f).contains("unit_"), "{}", meter(&f));
    let rows = f.tsd.borrow();
    assert!(rows.iter().any(|r| r.contains("\"unit_tests\":0") && r.contains("\"unit_slowest\":\"\"")), "{rows:?}");
}

#[test_id(tr50aw7)]
#[test]
fn the_unit_cap_the_config_names_narrows_the_crates_the_gate_builds() {
    let f = unit_fake(&["spira-config/src/lib.rs"]);
    f.put_var("SPIRA_GATE_UNIT_MAX_CRATES", "1");
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(f.cmds.borrow()[1].ends_with("--features integration --config profile.dev.incremental=false -p spira-config"), "{:?}", f.cmds.borrow());
}

#[test_id(t97gilr)]
#[test]
fn the_shipped_unit_cap_covers_every_workspace_member() {
    let root = testkit::manifest_dir().parent().unwrap().to_path_buf();
    let registry = spira_config::registry::Registry::from_dirs(&root.join("spira")).unwrap();
    let default = registry.get("SPIRA_GATE_UNIT_MAX_CRATES").and_then(|k| k.default_expr()).unwrap().trim_matches('"').parse::<usize>().unwrap();
    let manifest: toml::Value = std::fs::read_to_string(root.join("Cargo.toml")).unwrap().parse().unwrap();
    let members = manifest["workspace"]["members"].as_array().unwrap().len();
    assert!(default >= members, "the cap {default} is below the {members} workspace members");
}

#[test_id(ts5h8xe)]
#[test]
fn a_narrowed_unit_gate_names_the_dependents_it_left_to_the_round() {
    let f = unit_fake(&["spira-config/src/lib.rs"]);
    f.put_var("SPIRA_GATE_UNIT_MAX_CRATES", "1");
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(
        f.stderr().contains("narrowed past SPIRA_GATE_UNIT_MAX_CRATES=1: not tested here (the round tests them): queue"),
        "{}",
        f.stderr()
    );
}

#[test_id(tj9up2l)]
#[test]
fn a_unit_cap_that_is_not_a_positive_count_runs_the_gate_string_whole() {
    for bad in ["0", "", "many"] {
        let f = unit_fake(&["spira-config/src/lib.rs"]);
        f.put_var("SPIRA_GATE_UNIT_MAX_CRATES", bad);
        assert_eq!(f.run(), PASS, "{bad:?}: {}", f.stderr());
        assert!(meter(&f).contains("compose=suites(unit-cap-config)"), "{bad:?}: {}", meter(&f));
    }
}

#[test_id(td9i1yu)]
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
        "CARGO_BUILD_JOBS=8 cargo test --no-run --timings --features integration --config profile.dev.incremental=false -p queue -p spira-config"
    );
    assert_eq!(cmds[2], crate::compose::unit_commands(&["queue".into(), "spira-config".into()], crate::compose::Targets { lib: true, bin: true, test: true }, 8, &Default::default())[1].1);
    assert!(!cmds[2].contains("--test-threads"), "{}", cmds[2]);
    let cfg = f.unit_config.borrow().clone();
    assert_eq!(cfg.len(), 1, "{cfg:?}");
    assert!(cfg[0].0.ends_with("target/gate-unit-nextest.toml"), "the nextest config lives under the trial tree's target directory: {cfg:?}");
    assert_eq!(cfg[0].1, "[profile.default]\nslow-timeout = { period = \"60s\", terminate-after = 2 }\nstatus-level = \"skip\"\n");
    assert!(f.stderr().contains(
        "gate: composition=unit — fences (suites off, no build fence: the build phase is the compile check), then cargo build and test on the host for 2 crates: queue spira-config (touched: spira-config)"
    ));
    assert!(
        meter(&f).ends_with(" rc=0 pass compose=unit phases=fences:7,build:7,test:7 unit_build_secs=7 unit_run_secs=7 unit_tests=0 unit_slowest=- deny_list=1000000/e3b0c44298fc build_dir=cold test_clock=7s\n"),
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

#[test_id(trpvrqf)]
#[test]
fn a_deny_list_reaches_the_nextest_config_the_integration_command_and_the_suites_env() {
    let list = "disabled\ttest:t0aaaaa\tsp-1\t-\nquarantined\ttest:t0bbbbb\tsp-2\t-\n";
    let f = unit_fake(&["spira-config/src/lib.rs"]);
    *f.deny.borrow_mut() = Ok(list.to_string());
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (0, "build-fence: make build ok".into()));
    assert_eq!(f.run(), PASS);
    let cfg = f.unit_config.borrow().clone();
    assert!(cfg[0].1.contains("default-filter = \"not (test(/::t0aaaaa$/) | test(/::t0bbbbb$/))\"\n"), "{cfg:?}");
    let cmds = f.cmds.borrow().clone();
    assert!(cmds[2].contains("--test '*' -- --skip ::t0aaaaa --skip ::t0bbbbb"), "{}", cmds[2]);
    assert!(f.env_of(0, "SPIRA_TEST_DENY_LIST").ends_with("target/gate-deny-list"), "the gate-string step names the list file");
    let written = f.deny_written.borrow();
    assert!(written.iter().any(|(p, c)| p.ends_with("target/gate-deny-list") && c.contains("test:t0aaaaa")), "{written:?}");

    let f = returned(unit_fake(&["gate/src/x.rs"]), "test-b.sh");
    *f.deny.borrow_mut() = Ok(list.to_string());
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (0, "build-fence: make build ok".into()));
    assert_eq!(f.run(), PASS);
    let n = f.cmds.borrow().len();
    for i in [0, n - 1] {
        assert!(f.env_of(i, "SPIRA_TEST_DENY_LIST").ends_with("target/gate-deny-list"), "step {i}: the gate string and the re-entry both name the list");
    }

    let header = "# test-registry deny-list at=1 rows=0\n";
    let f = unit_fake(&["spira-config/src/lib.rs"]);
    *f.deny.borrow_mut() = Ok(header.to_string());
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (0, "build-fence: make build ok".into()));
    assert_eq!(f.run(), PASS);
    assert!(f.env_of(0, "SPIRA_TEST_DENY_LIST").ends_with("target/gate-deny-list"), "an empty list still names its file");
    let written = f.deny_written.borrow();
    assert!(written.iter().any(|(p, c)| p.ends_with("target/gate-deny-list") && c == header), "{written:?}");
    drop(written);
    assert!(!f.cmds.borrow()[2].contains("--skip"));
    assert!(!f.unit_config.borrow()[0].1.contains("default-filter"));
}

#[test_id(ts1j3kl)]
#[test]
fn a_gate_names_the_deny_list_it_ran_with_on_stderr_in_the_record_and_in_the_tsd_row() {
    let list = "disabled\ttest:t0aaaaa\tsp-1\t-\n";
    let f = unit_fake(&["spira-config/src/lib.rs"]);
    *f.deny.borrow_mut() = Ok(list.to_string());
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (0, "build-fence: make build ok".into()));
    assert_eq!(f.run(), PASS);
    assert_eq!(&crate::key::sha256_hex(b"abc")[..12], "ba7816bf8f01", "the digest test-registry prints for the same text");
    let digest = &crate::key::sha256_hex(list.as_bytes())[..12];
    let line = f.stderr().lines().find(|l| l.starts_with("gate: deny list ")).unwrap_or_default().to_string();
    let label = line.trim_start_matches("gate: deny list ").split(' ').next().unwrap().to_string();
    assert!(label.ends_with(&format!("/{digest}")) && line.ends_with("(1 rows)"), "{line}");
    assert!(f.unit_config.borrow()[0].1.contains("default-filter"), "the filter reaches the nextest config");
    assert!(meter(&f).contains(&format!(" deny_list={label} ")), "{}", meter(&f));
    assert!(f.tsd.borrow().iter().any(|r| r.contains(&format!("\"deny_list\":\"{label}\""))), "{:?}", f.tsd.borrow());
}

#[test_id(to49n2l)]
#[test]
fn a_deny_list_that_cannot_be_resolved_is_no_verdict_and_no_phase_runs() {
    for bad in [Err("registry locked".to_string()), Ok("garbage\n".to_string())] {
        let f = unit_fake(&["spira-config/src/lib.rs"]);
        *f.deny.borrow_mut() = bad;
        assert_eq!(f.run(), NOVERDICT);
        assert!(f.cmds.borrow().is_empty(), "no phase ran");
        assert!(f.stderr().contains("deny list"), "{}", f.stderr());
    }
}

/// sp-aprxm: a unit-mode gate builds once. The build fence's cold release `make build` is
/// dropped from the gate string for a unit composition (branch and base trial alike); a
/// composition with no build phase of its own keeps it.
#[test_id(t5rhpd5)]
#[test]
fn a_unit_gate_builds_once_and_every_other_composition_keeps_the_build_fence() {
    const G: &str = "bash spira/lint.sh && bash spira/build-fence.sh && run-suites";
    let set = |f: &Fake| {
        f.ctx.borrow_mut().as_mut().unwrap().gate_cmd = G.into();
        f.prints("spira/lint.sh", &["lint"]);
        f.prints("spira/build-fence.sh", &["build-fence"]);
    };

    let f = unit_fake(&["gate/src/x.rs"]);
    set(&f);
    assert_eq!(f.run(), PASS);
    let cmds = f.cmds.borrow().clone();
    assert_eq!(cmds[0], "bash spira/lint.sh && run-suites", "{cmds:?}");
    assert!(
        cmds[1].starts_with("CARGO_BUILD_JOBS=8 cargo test --no-run --timings"),
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
    assert_eq!(cmds.len(), 4, "branch fences, build and test, then the base's fences only: {cmds:?}");
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

#[test_id(t0clx3n)]
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

#[test_id(t33s9xy)]
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

#[test_id(tqmib66)]
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

#[test_id(tncd8nm)]
#[test]
fn a_repo_declaring_a_build_tier_builds_outside_the_cap_and_the_deadline() {
    let f = Fake::new();
    f.repo_build_secs.set(Some(400));
    f.phase_secs.set(200);
    assert_eq!(f.run(), PASS, "{}", f.verdict_line());
    let cmds = f.cmds.borrow().clone();
    assert!(cmds[0].contains("--suites build"), "{cmds:?}");
    let key = cmds[0].split("--share-keep ").nth(1).and_then(|r| r.split(' ').next()).expect("the build keeps a share").to_string();
    assert!(cmds[1].contains(&format!("--share-keep {key} ")), "{cmds:?}");
    assert!(!cmds[1].contains("bash gate.sh") && !cmds[1].contains("--suites build"), "{cmds:?}");
    let t = f.timeouts.borrow().clone();
    assert!(!t[0].1.is_empty() && t[0].1.parse::<u64>().unwrap() > 260, "the build is under no phase cap: {t:?}");
    assert!(meter(&f).contains(" phases=build:400,gate:200"), "{}", meter(&f));
    assert!(meter(&f).contains("test_clock=200s"), "{}", meter(&f));
}

#[test_id(tr9klv0)]
#[test]
fn a_repo_gate_phase_stops_at_its_verdict_and_the_publish_runs_after_it_uncapped_and_uncharged() {
    let f = Fake::new();
    f.repo_build_secs.set(Some(400));
    f.phase_secs.set(200);
    f.publish_run.set((0, 120));
    assert_eq!(f.run(), PASS, "{}", f.verdict_line());
    let cmds = f.cmds.borrow().clone();
    let key = cmds[0].split("--share-keep ").nth(1).and_then(|r| r.split(' ').next()).expect("the build keeps a share").to_string();
    assert!(cmds[1].contains(&format!("--share-keep {key} ")), "{cmds:?}");
    assert!(cmds[2].contains(&format!("--share-publish {key} ")), "{cmds:?}");
    assert!(meter(&f).contains("phases=build:400,gate:200,publish:120"), "{}", meter(&f));
    assert!(meter(&f).contains("test_clock=200s"), "{}", meter(&f));
    let t = f.timeouts.borrow().clone();
    assert!(t[2].1.parse::<u64>().unwrap() > 260, "the publish is under no phase cap: {t:?}");
}

#[test_id(tbfx5tw)]
#[test]
fn a_publish_that_fails_or_runs_long_never_changes_the_verdict() {
    let f = Fake::new();
    f.repo_build_secs.set(Some(30));
    f.phase_secs.set(200);
    f.publish_run.set((75, 300));
    assert_eq!(f.run(), PASS, "{}", f.verdict_line());
    assert!(!f.verdict_line().contains("phase-cap"), "{}", f.verdict_line());
    assert!(f.cmds.borrow().iter().any(|c| c.contains("--share-publish")), "{:?}", f.cmds.borrow());
    assert!(meter(&f).contains("gate:200,publish:300"), "{}", meter(&f));
    assert!(meter(&f).contains("test_clock=200s"), "{}", meter(&f));
}

#[test_id(t1p358b)]
#[test]
fn a_repo_build_that_stalls_is_a_build_stall_and_the_gate_phase_never_runs() {
    let f = Fake::new();
    f.repo_build_secs.set(Some(30));
    *f.repo_build_result.borrow_mut() = Some((crate::stall::RC, crate::stall::Watch::new(5, 0).message(5)));
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=build-stall"), "{}", f.verdict_line());
    assert!(f.cmds.borrow().iter().all(|c| c.contains("--suites build")), "{:?}", f.cmds.borrow());
}

#[test_id(tfo6850)]
#[test]
fn a_repo_build_that_fails_is_the_branchs_red_and_the_gate_phase_never_runs() {
    let f = Fake::new();
    f.repo_build_secs.set(Some(30));
    *f.repo_build_result.borrow_mut() = Some((1, "error[E0308]".into()));
    assert_eq!(f.run(), 1, "{}", f.verdict_line());
    assert!(meter(&f).contains("phases=build:30,"), "{}", meter(&f));
    assert!(!meter(&f).contains("build:30,gate"), "{}", meter(&f));
}

#[test_id(taggoyg)]
#[test]
fn a_tree_with_no_build_tier_runs_no_build_phase() {
    let f = Fake::new();
    assert_eq!(f.run(), PASS);
    assert!(meter(&f).contains(" phases=gate:7"), "{}", meter(&f));
}

/// A fake whose gate command declares three steps and is killed at the gate phase's cap.
fn capped_with_steps() -> Fake {
    let f = Fake::new();
    // Non-defaults, and the production relationship: the budget is a share of the deadline,
    // so a phase cut at the budget is not the whole gate running out of clock.
    f.put_var("SPIRA_GATE_DEADLINE", "900");
    f.put_var("SPIRA_GATE_BUDGET", "260");
    f.phase_secs.set(260);
    *f.steps_written.borrow_mut() = vec![(0, "build tree".into()), (20, "fences".into()), (30, "suites".into())];
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (124, "== suites\n".into()));
    f
}

#[test_id(tprq5zk)]
#[test]
fn a_phase_cap_row_names_the_step_that_ran_long_and_keeps_the_output() {
    let f = capped_with_steps();
    assert_eq!(f.run(), NOVERDICT);
    let row = meter(&f);
    assert!(row.contains(" phase-cap "), "{row}");
    assert!(row.contains(" slowest_step=suites:230 "), "{row}");
    assert!(row.contains(" out="), "{row}");
    let kept = f.kept_outs.borrow().clone();
    assert_eq!(kept.len(), 1);
    assert!(kept[0].1.contains("== suites"));
    assert!(row.contains(&format!(" out={}", kept[0].0.display())), "{row}");
    assert!(f.tsd.borrow().last().unwrap().contains(r#""slowest_step":"suites:230""#), "{:?}", f.tsd.borrow());
    assert!(f.removed.borrow().contains(&PathBuf::from("tmp/steps")), "the steps file is removed with the file list");
}

#[test_id(ts69poa)]
#[test]
fn a_pass_row_carries_the_slowest_step_and_keeps_no_output() {
    let f = Fake::new();
    *f.steps_written.borrow_mut() = vec![(0, "fences".into()), (2, "run suites".into())];
    assert_eq!(f.run(), PASS);
    let row = meter(&f);
    assert!(row.contains(" slowest_step=run_suites:5 "), "{row}");
    assert!(!row.contains(" out="), "{row}");
    assert!(f.kept_outs.borrow().is_empty());
    assert!(f.tsd.borrow().last().unwrap().contains(r#""slowest_step":"run_suites:5""#), "{:?}", f.tsd.borrow());
}

#[test_id(tlixvt8)]
#[test]
fn a_command_that_writes_no_steps_gives_a_row_with_neither_field() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (124, "slow".into()));
    f.phase_secs.set(260);
    assert_eq!(f.run(), NOVERDICT);
    assert!(!meter(&f).contains("slowest_step"), "{}", meter(&f));
}

#[test_id(t9m6a7r)]
#[test]
fn the_fifty_first_kept_output_removes_the_oldest() {
    let f = capped_with_steps();
    for i in 0..50 {
        f.files.borrow_mut().insert(PathBuf::from(format!("run/tsd/gate-out-2020-01-01T00:00:{i:02}Z.log")), String::new());
    }
    assert_eq!(f.run(), NOVERDICT);
    let removed: Vec<String> = f.removed.borrow().iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let old: Vec<&String> = removed.iter().filter(|p| p.contains("gate-out-")).collect();
    assert_eq!(old.len(), 1, "{removed:?}");
    assert!(old[0].ends_with("gate-out-2020-01-01T00:00:00Z.log"), "{old:?}");
}

#[test_id(t764by4)]
#[test]
fn a_suites_mode_gate_with_a_root_cargo_toml_builds_on_the_warm_target_of_its_repository() {
    let f = Fake::new();
    f.files.borrow_mut().insert(PathBuf::from(format!("{GATE_TREE}/Cargo.toml")), "[workspace]".into());
    assert_eq!(f.run(), PASS);
    assert_eq!(f.targets.borrow().len(), 1);
    assert_eq!(f.target_repos.borrow()[..], ["spira".to_string()]);
    assert!(meter(&f).contains(" build_dir="), "{}", meter(&f));
    assert!(meter(&f).contains("compose=suites(mode)"));
}

#[test_id(t0qp9kd)]
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

const GATE_TREE: &str = "run/worktree/.gate.spira.spira-sp-a";

/// A fake whose bead the round returned with `suites` (the `.ejected` sidecar), each of which
/// exists on the gate tree. Its gate string runs fences only (prints no suite line) on both
/// the merge and the base.
fn returned(f: Fake, suites: &str) -> Fake {
    for at in [MERGE_SHA, BASE] {
        f.runs
            .borrow_mut()
            .insert(at.into(), (0, "fences ok".into()));
    }
    f.put_var("SPIRA_GATE_BEAD", "sp-a");
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

#[test_id(thf254e)]
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
    assert!(cmds[1].contains("--features integration --config profile.dev.incremental=false -p gate"));
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

#[test_id(tzna3dk)]
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
        "phases=fences:7,build:7,test:7,reentry:7,base-fences:7,base-reentry:7"
    ));
    assert!(f
        .written
        .borrow()
        .iter()
        .all(|(p, _)| !p.starts_with("run/verdicts/trees")));
}

#[test_id(tbkumxa)]
#[test]
fn a_named_suite_red_on_the_base_too_is_the_bases() {
    let f = returned(unit_fake(&["gate/src/x.rs"]), "test-b.sh");
    for at in [MERGE_SHA, BASE] {
        f.reentry_runs
            .borrow_mut()
            .insert(at.into(), (1, "  test-b.sh   RED     rc=1 after 4s".into()));
    }
    f.reruns.borrow_mut().insert((BASE.into(), "test-b.sh".into()), (1, "  test-b.sh   RED     rc=1 after 4s".into()));
    assert_eq!(f.run(), PASS);
    assert!(f.stderr().contains("inherited, not judged"));
}

#[test_id(tbfph9b)]
#[test]
fn a_cached_replay_of_a_passed_suite_proves_it() {
    let f = returned(unit_fake(&["gate/src/x.rs"]), "test-b.sh");
    f.reentry_runs.borrow_mut().insert(
        MERGE_SHA.into(),
        (
            0,
            "batch: tree already passed at 2026-11-16T07:33:31Z — key batch-k\n  test-b.sh   ok      334s\nVERDICT GREEN ran=0 cached=2026-11-16T07:33:31Z".into(),
        ),
    );
    assert_eq!(f.run(), PASS);
    assert!(f.written.borrow()[0].1.contains("suites=test-b.sh"), "{:?}", f.written.borrow());
}

#[test_id(t2pgfqy)]
#[test]
fn a_cached_replay_without_a_passed_suite_line_proves_nothing() {
    for body in [
        "VERDICT GREEN ran=0 cached=2026-11-16T07:33:31Z",
        "  test-b.sh   SKIPPED\nVERDICT GREEN ran=0 cached=2026-11-16T07:33:31Z",
    ] {
        let f = returned(unit_fake(&["gate/src/x.rs"]), "test-b.sh");
        f.reentry_runs.borrow_mut().insert(MERGE_SHA.into(), (0, body.into()));
        assert_eq!(f.run(), NOVERDICT, "{body}");
        assert!(f.verdict_line().contains("reason=reentry-unproven"), "{}", f.verdict_line());
    }
}

#[test_id(tud1aj7)]
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

#[test_id(txnhlsw)]
#[test]
fn a_fences_only_branch_still_reruns_the_named_suites() {
    let f = returned(unit_fake(&["docs/x.md"]), "test-b.sh");
    assert_eq!(f.run(), PASS);
    let cmds = f.cmds.borrow().clone();
    assert_eq!(cmds.len(), 2, "{cmds:?}");
    assert!(meter(&f).contains("compose=fences+reentry phases=fences:7,reentry:7"));
}

#[test_id(tzp5wc1)]
#[test]
fn the_reentry_selector_defers_suites_against_the_configured_budget() {
    let f = returned(unit_fake(&["docs/x.md"]), "test-b.sh,test-c.sh");
    f.put_var("SPIRA_GATE_DEADLINE", "900");
    f.put_var("SPIRA_GATE_BUDGET", "420");
    let row = |s: &str, w: u32| format!("{{\"suite\":\"{s}\",\"ts\":\"2026-11-18T00:00:00Z\",\"wall_secs\":{w}}}\n");
    f.files.borrow_mut().insert(PathBuf::from(format!("{RUN}/tsd/suite-timing.jsonl")), row("test-b.sh", 400) + &row("test-c.sh", 100));
    assert_eq!(f.run(), PASS, "{}", f.verdict_line());
    let err = f.stderr();
    assert!(err.contains("test-c.sh deferred to the round") && err.contains("the gate's 420s suite budget"), "{err}");
    assert!(!err.contains("test-b.sh deferred"), "400s fits a 420s budget, not a 260s literal: {err}");
}

#[test_id(tfrj4s4)]
#[test]
fn a_beads_suites_off_certification_still_selects_its_covered_suites() {
    let f = unit_fake(&["spira-config/src/lib.rs"]);
    f.put_var("SPIRA_GATE_SUITES", "off");
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (0, String::new()));
    assert_eq!(f.run(), PASS);
    assert_eq!(f.env_of(0, "SPIRA_GATE_COVERED"), "", "no bead: fences only, as the batcher's cut");

    let f = unit_fake(&["spira-config/src/lib.rs"]);
    f.put_var("SPIRA_GATE_SUITES", "off");
    f.put_var("SPIRA_GATE_BEAD", "sp-a");
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (0, String::new()));
    assert_eq!(f.run(), PASS);
    assert_eq!(f.env_of(0, "SPIRA_GATE_SUITES"), "off");
    assert_eq!(f.env_of(0, "SPIRA_GATE_COVERED"), "1");

    let f = unit_fake(&["spira-config/src/lib.rs"]);
    f.put_var("SPIRA_GATE_SUITES", "off");
    f.put_var("SPIRA_GATE_BEAD", "sp-a");
    f.admission_free.set(false);
    f.put_var("SPIRA_GATE_LOCK_WAIT", "5");
    assert_eq!(f.run(), NOVERDICT, "covered suites run under the admission pool");
    assert!(f.verdict_line().contains("reason=admission-timeout"));
}

#[test_id(t0qhvbp)]
#[test]
fn a_script_branch_in_unit_mode_with_certify_suites_off_still_reruns_them() {
    // Production today: certify_suites = "off". The selector then exits before it ever
    // unions SPIRA_GATE_EJECTED_SUITES in, so the promise "recertification will force these
    // suites" was never kept. The re-entry phase keeps it.
    let f = returned(unit_fake(&["spira/lib.sh"]), "test-b.sh");
    f.put_var("SPIRA_GATE_SUITES", "off");
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (0, String::new()));
    assert_eq!(f.run(), PASS);
    assert_eq!(f.cmds.borrow().len(), 2);
    assert!(meter(&f).contains("compose=suites(script)+reentry phases=gate:7,reentry:7"));
}

#[test_id(tme61h6)]
#[test]
fn suites_off_with_named_suites_takes_an_admission_slot() {
    let f = returned(Fake::new(), "test-b.sh");
    f.put_var("SPIRA_GATE_SUITES", "off");
    f.admission_free.set(false);
    f.put_var("SPIRA_GATE_LOCK_WAIT", "5");
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=admission-timeout"));

    let f = Fake::new();
    f.put_var("SPIRA_GATE_SUITES", "off");
    f.admission_free.set(false);
    assert_eq!(
        f.run(),
        PASS,
        "fences-only certification with nothing named takes no slot"
    );
}

#[test_id(t9conl8)]
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

#[test_id(ttcazd6)]
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

#[test_id(t463iqj)]
#[test]
fn a_named_suite_no_longer_on_the_tree_is_said_and_not_run() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.put_var("SPIRA_GATE_BEAD", "sp-a");
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

#[test_id(t4ymonu)]
#[test]
fn a_branch_that_deletes_a_named_suite_is_answered_by_the_deletion() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.put_var("SPIRA_GATE_BEAD", "sp-a");
    f.files.borrow_mut().insert(
        PathBuf::from(format!("{RUN}/ejected/sp-a")),
        "test-del.sh\n".into(),
    );
    f.files
        .borrow_mut()
        .insert(PathBuf::from(format!("{GATE_TREE}/spira/test-del.sh")), "#!".into());
    f.gone.borrow_mut().insert(format!("{MERGE_SHA}:spira/test-del.sh"));
    assert_eq!(f.run(), PASS);
    assert_eq!(f.cmds.borrow().len(), 3, "no re-entry phase: {:?}", f.cmds.borrow());
    assert!(meter(&f).contains("compose=unit phases="));
    assert!(f
        .stderr()
        .contains("re-entry: test-del.sh deleted by this branch — the deletion answers it"));
}

#[test_id(tr9vgc9)]
#[test]
fn a_named_suite_on_neither_the_base_nor_the_branch_still_refuses() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.put_var("SPIRA_GATE_BEAD", "sp-a");
    f.files.borrow_mut().insert(
        PathBuf::from(format!("{RUN}/ejected/sp-a")),
        "test-typo.sh\n".into(),
    );
    f.gone.borrow_mut().insert(format!("{BASE}:spira/test-typo.sh"));
    f.gone.borrow_mut().insert(format!("{MERGE_SHA}:spira/test-typo.sh"));
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=reentry-missing"), "{}", f.verdict_line());
    assert!(f.stderr().contains("test-typo.sh"));
}

#[test_id(t8jkj2s)]
#[test]
fn the_batchers_ejected_sidecar_drives_the_rerun() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (0, "fences ok".into()));
    f.put_var("SPIRA_GATE_BEAD", "sp-a");
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

#[test_id(t2po8p5)]
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

#[test_id(tycexfx)]
#[test]
fn a_red_unit_test_on_a_green_base_is_the_branchs() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.unit_runs.borrow_mut().insert(
        (MERGE_SHA.into(), "test"),
        (101, "test tests::x ... FAILED\nerror: test failed, to rerun pass `-p gate --lib`\n     Summary [   0.010s] 1 test run: 0 passed, 1 failed, 0 skipped".into()),
    );
    assert_eq!(f.run(), FAIL);
    assert!(f.verdict_line().contains("reason=branch-red"));
    let err = f.stderr();
    let line = err.lines().find(|l| l.starts_with("gate: reproduce (from the workspace root): env -i ")).expect(&err);
    assert!(line.contains("PATH=") && line.contains("HOME="), "{line}");
    assert!(line.ends_with(" cargo test --no-fail-fast --features integration --config profile.dev.incremental=false -p gate --lib"), "{line}");
    assert!(!line.contains("--test-threads"), "{line}");
    assert_eq!(line.matches(" -p ").count(), 1, "{line}");
    let cmds = f.cmds.borrow().clone();
    assert_eq!(
        cmds.len(),
        4,
        "branch fences/build/test, base fences only: {cmds:?}"
    );
    assert!(!meter(&f).contains("base-build") && !meter(&f).contains("base-test"));
    assert_eq!(f.deny_calls.get(), 1, "the branch trial and the base trial share one snapshot");
    assert_eq!(f.checkouts.borrow()[1], BASE);
    assert!(meter(&f).contains(
        "compose=unit phases=fences:7,build:7,test:7,base-fences:7 unit_build_secs=7 unit_run_secs=7 unit_tests=1 unit_slowest=- deny_list=1000000/e3b0c44298fc build_dir=cold test_clock=7s"
    ));
    assert!(f.stderr().contains("phase 'test' failed (exit 101)"));
    assert!(!f.stderr().contains("the same command passes against"));
    assert!(f.stderr().contains("its tests were not run"));
    assert!(f.stderr().contains("so the red is charged to this branch: tests::x"));
}

#[test_id(t4qrifp)]
#[test]
fn a_red_units_failure_record_lists_bare_paths_not_name_status_lines() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.put_var("SPIRA_GATE_BEAD", "sp-a");
    f.unit_runs.borrow_mut().insert(
        (MERGE_SHA.into(), "test"),
        (101, "test tests::x ... FAILED\n     Summary [   0.010s] 1 test run: 0 passed, 1 failed, 0 skipped".into()),
    );
    assert_eq!(f.run(), FAIL);
    let got = f.failures.borrow();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].2, ["spira/x.sh", "src/a.rs"]);
}

#[test_id(tppeqw8)]
#[test]
fn a_red_unit_test_without_a_rerun_line_reproduces_with_the_phases_crates() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.unit_runs.borrow_mut().insert((MERGE_SHA.into(), "test"), (101, "test tests::x ... FAILED\n     Summary [   0.010s] 1 test run: 0 passed, 1 failed, 0 skipped".into()));
    assert_eq!(f.run(), FAIL);
    let err = f.stderr();
    let line = err.lines().find(|l| l.starts_with("gate: reproduce (from the workspace root): env -i ")).expect(&err);
    assert!(line.contains(" --features integration --config profile.dev.incremental=false -p gate"), "{line}");
}

const FLIP_OUT: &str = "---- t::flip stdout ----\n...\ntest t::flip ... FAILED\n     Summary [   0.010s] 1 test run: 0 passed, 1 failed, 0 skipped";

const FLIP_OUT_NEXTEST: &str = "        FAIL [   0.010s] (1/1) bin/t t::flip\n  stderr ───\n\n    thread 't::flip' panicked at x.rs:1:1:\n    no\n\n     Summary [   0.010s] 1 test run: 0 passed, 1 failed, 0 skipped\n        FAIL [   0.010s] (1/1) bin/t t::flip";

fn flip_fake(prior: Result<Vec<(String, String)>, String>) -> Fake {
    flip_fake_with(FLIP_OUT, prior)
}

fn flip_fake_with(out: &str, prior: Result<Vec<(String, String)>, String>) -> Fake {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.put_var("SPIRA_GATE_BEAD", "sp-a");
    f.unit_runs.borrow_mut().insert((MERGE_SHA.into(), "test"), (101, out.into()));
    *f.prior_failures.borrow_mut() = prior;
    f
}

fn other_red(bead: &str, tip: &str, file: &str) -> (String, String) {
    (bead.into(), spira_config::ejection::digest(tip, &[file.into()], FLIP_OUT))
}

fn other_nextest_red(bead: &str, tip: &str, file: &str) -> (String, String) {
    (bead.into(), spira_config::ejection::digest(tip, &[file.into()], FLIP_OUT_NEXTEST))
}

fn certified(f: &Fake, outcome: &str) -> usize {
    f.lc.borrow().iter().filter(|c| c[2] == outcome).count()
}

#[test_id(tbr44e6)]
#[test]
fn a_unit_red_whose_diff_shares_a_path_with_the_other_beads_is_the_branchs_whatever_the_status_letters() {
    let f = flip_fake(Ok(vec![other_red("sp-x", "t1", "src/a.rs"), other_red("sp-y", "t2", "b.rs")]));
    assert_eq!(f.run(), FAIL);
    assert_eq!(f.failures.borrow().len(), 1);
}

#[test_id(t67wd5w)]
#[test]
fn a_unit_red_that_two_other_beads_showed_on_disjoint_diffs_certifies_not_judged() {
    let f = flip_fake(Ok(vec![other_red("sp-x", "t1", "a.rs"), other_red("sp-y", "t2", "b.rs")]));
    assert_eq!(f.run(), PASS);
    assert_eq!((certified(&f, "pass"), certified(&f, "red")), (1, 0));
    assert!(f.failures.borrow().is_empty());
    assert!(f.stderr().contains("not judged — t::flip"), "{}", f.stderr());
}

#[test_id(tza9mex)]
#[test]
fn a_nextest_unit_red_that_two_other_beads_showed_on_disjoint_diffs_certifies_not_judged() {
    let f = flip_fake_with(FLIP_OUT_NEXTEST, Ok(vec![other_nextest_red("sp-x", "t1", "a.rs"), other_nextest_red("sp-y", "t2", "b.rs")]));
    assert_eq!(f.run(), PASS);
    assert_eq!((certified(&f, "pass"), certified(&f, "red")), (1, 0));
    assert!(f.stderr().contains("not judged — t::flip"), "{}", f.stderr());
}

#[test_id(tlaw34x)]
#[test]
fn a_nextest_unit_red_whose_diff_shares_a_path_with_the_other_beads_is_the_branchs() {
    let f = flip_fake_with(FLIP_OUT_NEXTEST, Ok(vec![other_nextest_red("sp-x", "t1", "src/a.rs"), other_nextest_red("sp-y", "t2", "b.rs")]));
    assert_eq!(f.run(), FAIL);
}

#[test_id(tuicbyc)]
#[test]
fn one_unshared_nextest_red_beside_a_shared_libtest_red_stays_the_branchs() {
    let both = format!("{FLIP_OUT}\n        FAIL [   0.010s] (1/2) bin/t t::mine\n  stderr ───\n    boom\n");
    let f = flip_fake_with(&both, Ok(vec![other_red("sp-x", "t1", "a.rs"), other_red("sp-y", "t2", "b.rs")]));
    assert_eq!(f.run(), FAIL);
    assert_eq!((certified(&f, "pass"), certified(&f, "red")), (0, 1));
}

#[test_id(t81bhuq)]
#[test]
fn a_flip_the_gate_declines_to_judge_files_one_deletion_incident() {
    let f = flip_fake(Ok(vec![other_red("sp-x", "t1", "a.rs"), other_red("sp-y", "t2", "b.rs")]));
    assert_eq!(f.run(), PASS);
    assert_eq!(*f.flips.borrow(), vec![("spira".to_string(), "t::flip".to_string())]);
    let (title, body) = (f.flips_body.borrow()[0].clone(), f.flips_body.borrow()[1].clone());
    assert_eq!(title, "test t::flip flips: red on 3 beads with disjoint diffs");
    assert!(body.contains("law-a-test-that-flips-is-deleted") && body.contains("sp-x") && body.contains("sp-y"), "{body}");
}

#[test_id(tf1r2e3)]
#[test]
fn a_flipping_test_with_an_id_is_keyed_and_named_by_the_id() {
    assert_eq!(crate::ports::flip_reference("spira", "t::flip::t0aaaaa"), "test-flip:spira:test:t0aaaaa");
    assert_eq!(crate::ports::flip_reference("spira", "t::renamed::t0aaaaa"), "test-flip:spira:test:t0aaaaa");
    assert_eq!(crate::ports::flip_reference("spira", "t::flip"), "test-flip:spira:t::flip");
}

#[test_id(t1rpfkl)]
#[test]
fn a_branch_red_files_no_flip() {
    let f = flip_fake(Ok(vec![]));
    assert_eq!(f.run(), FAIL);
    assert!(f.flips.borrow().is_empty());
}

#[test_id(tjpjv1a)]
#[test]
fn a_flip_filing_that_fails_still_certifies() {
    let f = flip_fake(Ok(vec![other_red("sp-x", "t1", "a.rs"), other_red("sp-y", "t2", "b.rs")]));
    *f.flip_answer.borrow_mut() = Err("no db".into());
    assert_eq!(f.run(), PASS);
    assert_eq!(f.flips.borrow().len(), 1);
    assert!(f.stderr().contains("could not file the deletion"), "{}", f.stderr());
}

#[test_id(tuslcxk)]
#[test]
fn the_prior_failures_are_read_from_seven_days_back() {
    let f = flip_fake(Ok(vec![]));
    f.run();
    let since = f.prior_since.borrow().clone();
    assert_eq!(since, vec![f.now() as i64 - spira_config::ejection::LOOKBACK_SECS]);
}

#[test_id(t3iip2z)]
#[test]
fn a_unit_red_no_other_bead_showed_stays_the_branchs() {
    let f = flip_fake(Ok(vec![]));
    assert_eq!(f.run(), FAIL);
    assert!(f.verdict_line().contains("reason=branch-red"));
    assert_eq!((certified(&f, "pass"), certified(&f, "red")), (0, 1));
}

#[test_id(tmkytjv)]
#[test]
fn a_unit_red_seen_on_one_other_bead_stays_the_branchs() {
    let f = flip_fake(Ok(vec![other_red("sp-x", "t1", "a.rs")]));
    assert_eq!(f.run(), FAIL);
}

#[test_id(tnm97xf)]
#[test]
fn a_shared_unit_red_beside_a_red_fence_stays_the_branchs() {
    let f = flip_fake(Ok(vec![other_red("sp-x", "t1", "a.rs"), other_red("sp-y", "t2", "b.rs")]));
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "literal-lint: RED".into()));
    assert_eq!(f.run(), FAIL);
    assert_eq!(certified(&f, "pass"), 0);
    assert!(f.prior_since.borrow().is_empty(), "the shared-red read was reached past a red fence");
}

#[test_id(tc1xp8n)]
#[test]
fn a_shared_unit_red_beside_a_red_suite_or_a_failed_build_stays_the_branchs() {
    let prior = || Ok(vec![other_red("sp-x", "t1", "a.rs"), other_red("sp-y", "t2", "b.rs")]);
    let f = flip_fake(prior());
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "gate-step RED rc=1 :: fence-f\n  test-s1.sh RED rc=1".into()));
    assert_eq!(f.run(), FAIL);
    let f = flip_fake(prior());
    f.unit_runs.borrow_mut().insert((MERGE_SHA.into(), "build"), (101, "error[E0308]: mismatched types".into()));
    assert_eq!(f.run(), FAIL);
    assert_eq!(certified(&f, "pass"), 0);
}

#[test_id(tcoen7d)]
#[test]
fn a_shared_unit_red_whose_prior_read_failed_stays_the_branchs() {
    let f = flip_fake(Err("spira-lc: gone".into()));
    assert_eq!(f.run(), FAIL);
    assert_eq!(certified(&f, "pass"), 0);
}

#[test_id(tigttz4)]
#[test]
fn a_unit_base_trial_runs_fences_only_never_the_base_build_or_tests() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.unit_runs
        .borrow_mut()
        .insert((MERGE_SHA.into(), "test"), (101, "test tests::x ... FAILED\n     Summary [   0.010s] 1 test run: 0 passed, 1 failed, 0 skipped".into()));
    assert_eq!(f.run(), FAIL);
    let m = meter(&f);
    assert!(m.contains("base-fences:7"), "{m}");
    assert!(!m.contains("base-build") && !m.contains("base-test"), "{m}");
}

#[test_id(t0s8qf4)]
#[test]
fn a_gate_on_a_tree_with_a_proved_fences_verdict_runs_no_fences_phase() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (1, "literal-lint: RED".into()));
    *f.base_tree_override.borrow_mut() = Some(BASE_TREE_HEX.into());
    f.files.borrow_mut().insert(
        PathBuf::from(format!("run/verdicts/fences/spira/{BASE_TREE_HEX}")),
        fencecache::render("harness", "bash spira/fence.sh && run-suites", "fence: x checked 3 files"),
    );
    assert_eq!(f.run(), FAIL);
    let cmds = f.cmds.borrow().clone();
    assert_eq!(cmds[0], "bash spira/fence.sh && run-suites", "{cmds:?}");
    assert_eq!(cmds.len(), 3, "the red fence is followed by build and test, and no base trial: {cmds:?}");
    assert!(!meter(&f).contains("base-fences"), "{}", meter(&f));
}

#[test_id(t4mz4kt)]
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
        .insert((MERGE_SHA.into(), "test"), (101, "        FAIL [   0.010s] (1/1) newcrate t::x\n     Summary [   0.010s] 1 test run: 0 passed, 1 failed, 0 skipped\n".into()));
    assert_eq!(
        f.run(),
        FAIL,
        "the base has nothing to test, so the red is the branch's"
    );
    let cmds = f.cmds.borrow().clone();
    assert_eq!(cmds.len(), 4, "base runs its fences only: {cmds:?}");
    assert!(meter(&f).contains("base-fences:7 unit_build_secs="));
}

#[test_id(tdqm59l)]
#[test]
fn a_gate_with_no_unit_test_deadline_is_no_verdict_config() {
    for var in ["SPIRA_UNIT_TEST_DEADLINE_SECS", "SPIRA_UNIT_TEST_DEADLINE_PERIODS"] {
        for value in [None, Some("0")] {
            let f = unit_fake(&["gate/src/x.rs"]);
            match value {
                Some(v) => f.put_var(var, v),
                None => {
                    f.ctx.borrow_mut().as_mut().unwrap().vars.remove(var);
                }
            }
            assert_eq!(f.run(), NOVERDICT, "{var}={value:?}");
            assert!(f.verdict_line().contains("reason=config"), "{}", f.verdict_line());
            assert!(f.cmds.borrow().is_empty(), "nothing ran: {:?}", f.cmds.borrow());
        }
    }
}

#[test_id(thwoya8)]
#[test]
fn a_red_fence_in_unit_mode_still_runs_the_unit_phases() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (1, "literal-lint: RED".into()));
    assert_eq!(f.run(), FAIL);
    // a red fence does not stop the unit phases: they report too, then the base's fences
    let cmds = f.cmds.borrow().clone();
    assert_eq!(cmds[0], "bash spira/fence.sh && run-suites");
    assert!(cmds[1].contains(" cargo test --no-run --timings"), "{cmds:?}");
    assert!(cmds[2].contains("; tools/test-sandbox/runner --around cargo nextest run "), "{cmds:?}");
    assert!(cmds[3].starts_with("bash spira/fence.sh"), "{cmds:?}");
}

#[test_id(tz8py1z)]
#[test]
fn the_unit_phases_share_the_gate_timeout() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.put_var("SPIRA_GATE_TIMEOUT", "10");
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

#[test_id(tpxvy7t)]
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

#[test_id(txx7e0l)]
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
        deny: "",
    });
    f.files.borrow_mut().insert(
        PathBuf::from(format!("{RUN}/verdicts/{k}")),
        key::render_entry("w", 999_000, "dwarf", "spira", BR, "test-a.sh"),
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

#[test_id(ty2br1d)]
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
    f.put_var("SPIRA_GATE_LOCK_WAIT", "1");
    assert_eq!(f.run(), NOVERDICT);
    assert!(cert_written(&f).is_none());
}

// ------------------------------------------------------------- the gate-run row (sp-cln99)

fn tsd_row(f: &Fake) -> serde_json::Value {
    let rows = f.tsd.borrow();
    assert_eq!(rows.len(), 1, "one gate-run row per trial: {rows:?}");
    serde_json::from_str(rows[0].trim_end()).unwrap()
}

#[test_id(tmip7yi)]
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
    assert_eq!(v["build_dir"], "cold: no warm directory to adopt");
    assert_eq!(v["wall_secs"], v["ran_secs"]);
    assert_eq!(v["repo"], "spira");
    assert_eq!(v["branch"], BR);
}

#[test_id(t55b3i3)]
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

#[test_id(t89pu0w)]
#[test]
fn a_no_verdict_trial_is_recorded_as_one() {
    let f = Fake::new();
    f.lock_free.set(false);
    f.put_var("SPIRA_GATE_LOCK_WAIT", "3");
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
    f.prints("spira/inventory.sh", &["inventory"]);
    f.prints("spira/build-fence.sh", &["build-fence"]);
    f.prints("$SPIRA_LINT_BIN", &["spira-lint", "plan-matrix", "lockfile-lint", "tier-budget-allowlist", "tier-budget-area-allowlist", "tier-budget-areas", "test-id-row-orphan"]);
    f
}

/// The checked-in step's shape: a wrapper script that execs spira-lint and prints no line.
const WRAPPED: &str = r#"bash spira/lint-staged.sh "$SPIRA_LINT_BIN" && bash spira/build-fence.sh && { _s="$("$SPIRA_SELECT_BIN" gate "$SPIRA_GATE_BASE" x)" || exit 75; [ -n "$_s" ] || exit 0; }"#;

fn wrapped() -> Fake {
    let f = fenced();
    f.ctx.borrow_mut().as_mut().unwrap().gate_cmd = WRAPPED.into();
    f.blobs.borrow_mut().insert(format!("{BASE}:spira/lint-staged.sh"), b"x".to_vec());
    f
}

#[test_id(tsx1z7u)]
#[test]
fn a_wrapper_that_runs_spira_lint_is_judged_on_spira_lints_line() {
    let f = wrapped();
    assert_eq!(f.run(), PASS, "{}", f.stderr());
}

#[test_id(tf7ms0j)]
#[test]
fn a_unit_composition_judges_the_lint_wrapper_on_spira_lints_line() {
    let f = wrapped();
    *f.mode.borrow_mut() = Ok(GateMode::Unit);
    *f.touched.borrow_mut() = Ok(vec![Changed { path: "gate/src/x.rs".into(), exec: false }]);
    assert_eq!(f.run(), PASS, "{}", f.stderr());
}

#[test_id(tp3p99e)]
#[test]
fn a_wrapper_script_that_prints_no_fence_line_of_its_own_still_passes() {
    let f = fenced();
    f.ctx.borrow_mut().as_mut().unwrap().gate_cmd = r#"bash spira/lint-staged.sh "$SPIRA_LINT_BIN" && bash spira/build-fence.sh"#.into();
    f.blobs.borrow_mut().insert(format!("{BASE}:spira/lint-staged.sh"), b"x".to_vec());
    assert_eq!(f.run(), PASS, "{}", f.stderr());
}

#[test_id(txrm5ln)]
#[test]
fn a_wrapped_spira_lint_that_prints_nothing_is_fence_silent_on_spira_lint() {
    let f = wrapped();
    f.silent_fences.borrow_mut().insert("spira-lint".into());
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    let vl = f.verdict_line();
    assert!(vl.contains("reason=fence-silent") && vl.contains("suite=spira-lint"), "{vl}");
}

#[test_id(tw97q35)]
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

#[test_id(tqystne)]
#[test]
fn a_summary_naming_fewer_suites_than_the_runner_ran_is_no_verdict() {
    let f = Fake::new();
    branch_trial_prints(&f, "  test-a.sh   ok 1s\n  test-b.sh   ok 1s\nVERDICT GREEN ran=3");
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=summary-mismatch"), "{}", f.verdict_line());
    assert!(f.stderr().contains("ran=3 but only 2"), "{}", f.stderr());
}

const TIER_GREEN: &str = "  gate                             ok      235s\n  log (gate): /x/batch-results/14e5433d/gate.log\nVERDICT GREEN ran=1 slot_wait=0s suite=236s";

fn branch_trial_records(f: &Fake, events: &str) {
    f.tier_events.borrow_mut().insert(MERGE_SHA.to_string(), events.to_string());
}

#[test_id(tur4ita)]
#[test]
fn a_repo_tier_run_green_is_pass_naming_its_tier() {
    let f = Fake::new();
    f.repo_build_secs.set(Some(30));
    branch_trial_prints(&f, TIER_GREEN);
    branch_trial_records(&f, "1 suite gate ok");
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(f.stderr().contains("gate PASS covered suites: gate"), "{}", f.stderr());
}

#[test_id(tjmnkc8)]
#[test]
fn a_repo_tier_run_red_names_its_tier_as_the_red_unit() {
    let f = Fake::new();
    f.repo_build_secs.set(Some(30));
    f.runs.borrow_mut().insert(
        MERGE_SHA.to_string(),
        (1, "  gate                             RED     after 12s\nVERDICT RED ran=1 red=1".to_string()),
    );
    branch_trial_records(&f, "1 suite gate RED");
    f.run();
    assert!(f.verdict_line().contains("suite=gate"), "{}", f.verdict_line());
}

#[test_id(twa22hl)]
#[test]
fn two_tier_rows_and_a_harness_row_each_count_toward_ran() {
    let f = Fake::new();
    f.repo_build_secs.set(Some(30));
    branch_trial_prints(&f, "  build                            ok      45s\n  gate                             ok      235s\n  test-a.sh   ok 1s\nVERDICT GREEN ran=3");
    branch_trial_records(&f, "1 suite build ok\n2 suite gate ok");
    assert_eq!(f.run(), PASS, "{}", f.stderr());
}

#[test_id(te78cfj)]
#[test]
fn a_printed_tier_row_without_the_typed_record_is_not_a_ran_suite() {
    let f = Fake::new();
    f.repo_build_secs.set(Some(30));
    branch_trial_prints(&f, TIER_GREEN);
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=summary-mismatch"), "{}", f.verdict_line());
}

#[test_id(t8ktdtm)]
#[test]
fn a_summary_naming_every_suite_the_runner_ran_still_passes() {
    let f = Fake::new();
    branch_trial_prints(&f, "  test-a.sh   ok 1s\n  test-b.sh   ok 1s\n  test-c.sh   SKIPPED\nVERDICT GREEN ran=2 skipped=1");
    assert_eq!(f.run(), PASS, "{}", f.stderr());
}

#[test_id(t3v8o2b)]
#[test]
fn a_plan_matrix_that_is_skipped_is_fence_silent_even_when_spira_lint_passes() {
    let f = fenced();
    f.silent_fences.borrow_mut().insert("plan-matrix".into());
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=fence-silent"));
    assert!(f.verdict_line().contains("suite=plan-matrix"));
}

#[test_id(tj3yfus)]
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

#[test_id(t24nlbb)]
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

#[test_id(to20vpj)]
#[test]
fn a_failing_trial_is_judged_as_before_not_as_silent() {
    let f = fenced();
    f.runs
        .borrow_mut()
        .insert(MERGE_SHA.into(), (1, "lockfile-lint: serde bumped".into()));
    assert_eq!(f.run(), FAIL);
    assert!(f.verdict_line().contains("reason=branch-red"));
}

#[test_id(tgebhq6)]
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

#[test_id(tsxt98y)]
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
    f.prints("spira/a-fence.sh", &["a-fence"]);
    f.prints("spira/b-fence.sh", &["b-fence"]);
    f.prints("spira/c-fence.sh", &["c-fence"]);
    f.prints("$SPIRA_LINT_BIN", &["spira-lint", "plan-matrix", "lockfile-lint", "tier-budget-allowlist", "tier-budget-area-allowlist", "tier-budget-areas", "test-id-row-orphan"]);
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

#[test_id(tn0d59a)]
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
    assert!(cmds[0].starts_with("cargo build -j ") && cmds[0].contains("-p spira-lint"), "{cmds:?}");
    let lint = f.env_of(1, "SPIRA_LINT_BIN");
    // The binary the tools phase built, installed keyed by the merge's tree (sp-g9f3t).
    assert_eq!(lint, format!("{GATE_TREE}/target/tree-tools/tree-of-{MERGE_SHA}/spira-lint"));
    assert!(f.appended.borrow().iter().any(|l| l.contains("phases=tools:")), "{:?}", f.appended.borrow());
}

#[test_id(tfmjfge)]
#[test]
fn a_branch_that_deletes_a_fence_and_its_step_passes() {
    // The base still has b-fence and names it; the branch deletes both.
    let without_b = STEPS.replace("step bash spira/b-fence.sh\n", "");
    let f = tree_owned(Some(STEPS), Some(&without_b));
    f.gone.borrow_mut().insert(format!("{MERGE_SHA}:spira/b-fence.sh"));
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(!gate_runs(&f)[0].contains("b-fence"));
}

#[test_id(txcnz63)]
#[test]
fn a_branch_that_deletes_a_fence_but_leaves_it_named_fails() {
    let f = tree_owned(Some(STEPS), Some(STEPS));
    f.gone.borrow_mut().insert(format!("{MERGE_SHA}:spira/b-fence.sh"));
    assert_eq!(f.run(), FAIL);
    assert!(f.verdict_line().contains("reason=gate-definition"), "{}", f.verdict_line());
    assert!(f.stderr().contains("names 'bash spira/b-fence.sh'"), "{}", f.stderr());
    assert!(f.ran.borrow().is_empty(), "nothing runs on a definition that cannot run");
}

#[test_id(toiw4kj)]
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

#[test_id(t6l9lhj)]
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

#[test_id(t710ll7)]
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

#[test_id(tox60cw)]
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

#[test_id(ty9mtyg)]
#[test]
fn the_definition_is_part_of_the_verdict_key() {
    let a = tree_owned(Some(STEPS), Some(STEPS));
    assert_eq!(a.run(), PASS);
    let b = tree_owned(Some(STEPS), Some(&STEPS.replace("bin SPIRA_LINT_BIN spira-lint\n", "")));
    assert_eq!(b.run(), PASS);
    let key = |f: &Fake| f.written.borrow().iter().find(|(p, _)| p.starts_with("run/verdicts") && !p.to_string_lossy().contains("/trees/")).map(|(p, _)| p.clone());
    assert!(key(&a).is_some() && key(&b).is_some());
    assert_ne!(key(&a), key(&b), "a different tool set is a different trial");
}

// ------------------------------------------- the trial's budget (testenv DESIGN.md §11, sp-govet)

#[test_id(tbrxtyd)]
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

#[test_id(tfw6bk7)]
#[test]
fn queue_wait_words_are_summed_across_pools() {
    assert_eq!(crate::parse::queue_secs("a queue-wait=5s pool=compile\nqueue-wait=7s pool=test\nqueue-wait=x"), 12);
    assert_eq!(crate::parse::queue_secs("nothing waited"), 0);
}

#[test_id(tsq6sqc)]
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

#[test_id(t7omhqx)]
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
#[test_id(tttkcbu)]
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

#[test_id(tsmfpix)]
#[test]
fn the_runners_budget_knobs_reach_the_gate_command() {
    let f = Fake::new();
    f.put_var("SPIRA_TESTENV_SETUP_SHARE", "70");
    f.put_var("SPIRA_TESTENV_WARM_SLOTS", "0");
    f.run();
    assert_eq!(f.env_of(0, "SPIRA_TESTENV_SETUP_SHARE"), "70");
    assert_eq!(f.env_of(0, "SPIRA_TESTENV_WARM_SLOTS"), "0");
}

// ------------------------------------------------- tools keyed by the tree (sp-g9f3t)

/// The 2026-09-30 base-red, as a fixture: the branch trial is red on its own, so the base
/// trial runs in the same gate tree. A spira-lint from another tree — the unkeyed
/// `target/debug/spira-lint` the branch trial left, or a keyed directory stamped for some other
/// tree — flags literals the base's own spira-lint allows. Reading either would charge the
/// branch's red to local/main (BASE_FAIL); the base's own tools, keyed by its tree, pass.
#[test_id(t963hxx)]
#[test]
fn the_base_trial_runs_tools_keyed_by_the_base_tree_never_a_stale_one() {
    let f = tree_owned(Some(STEPS), Some(STEPS));
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "a-fence: red on the branch".into()));
    // literal-ok: the 2026-09-30 finding, quoted as the stale tool's answer.
    let stale = (1, "literal-lint: dwarf/src/tests.rs:249: \"needs-ryan\"".to_string());
    // path-ok: the unkeyed build output the branch trial left in the gate tree.
    let unkeyed = format!("{GATE_TREE}/target/debug/spira-lint");
    let other = format!("{GATE_TREE}/target/tree-tools/tree-of-OTHER/spira-lint");
    f.lint_by_bin.borrow_mut().insert(unkeyed.clone(), stale.clone());
    f.lint_by_bin.borrow_mut().insert(other.clone(), stale);
    f.files.borrow_mut().insert(PathBuf::from(&other), "#!stale".into());
    f.files.borrow_mut().insert(PathBuf::from(format!("{GATE_TREE}/target/tree-tools/tree-of-OTHER/TREE")), "tree-of-OTHER\n".into());
    assert_eq!(f.run(), FAIL, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=branch-red"), "{}", f.verdict_line());
    // Every gate-string run named a binary keyed by the tree that run judged.
    let ran = f.ran.borrow();
    let lints: Vec<String> = ran
        .iter()
        .filter_map(|e| e.iter().find(|(k, _)| k == "SPIRA_LINT_BIN").map(|(_, v)| v.clone()))
        .collect();
    assert!(!lints.is_empty());
    assert!(lints.contains(&format!("{GATE_TREE}/target/tree-tools/tree-of-{BASE}/spira-lint")), "{lints:?}");
    assert!(!lints.iter().any(|l| l == &unkeyed || l == &other), "{lints:?}");
}

/// The gate tree does not hold the tree the trial judges: no tool is built or read, and the
/// branch trial is refused, never judged.
#[test_id(tepksr8)]
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
#[test_id(tyatw13)]
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
#[test_id(tfpd1ea)]
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
#[test_id(th0xcb1)]
#[test]
fn tools_stamped_for_the_same_tree_are_reused() {
    let f = tree_owned(Some(STEPS), Some(STEPS));
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "a-fence: red on the branch".into()));
    for id in [MERGE_SHA, BASE] {
        let dir = format!("{GATE_TREE}/target/tree-tools/tree-of-{id}");
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
    let dir = format!("{GATE_TREE}/target/tree-tools/tree-of-{MERGE_SHA}");
    f.files.borrow_mut().insert(PathBuf::from(format!("{dir}/TREE")), format!("tree-of-{MERGE_SHA}\n"));
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(f.cmds.borrow()[0].starts_with("cargo build"), "{:?}", f.cmds.borrow());
}

/// A gate tree from before the tmpfs build holds stamped tools on the disk; moving the build
/// to tmpfs empties them, and the first gate must rebuild and judge, not NO_VERDICT.
#[test_id(tfw7539)]
#[test]
fn a_pre_tmpfs_tree_with_stamped_tools_on_disk_gets_a_verdict_on_its_first_gate() {
    let f = tree_owned(Some(STEPS), Some(STEPS));
    f.target_clears_tools.set(true);
    for id in [MERGE_SHA, BASE] {
        let dir = format!("{GATE_TREE}/target/tree-tools/tree-of-{id}");
        f.files.borrow_mut().insert(PathBuf::from(format!("{dir}/TREE")), format!("tree-of-{id}\n"));
        f.files.borrow_mut().insert(PathBuf::from(format!("{dir}/spira-lint")), "#!built".into());
    }
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(!f.verdict_line().contains("tools-unattributed"), "{}", f.verdict_line());
}

// ----------------------------------------------------------------- build IO (sp-z61hj)

/// Every gate-string run gets the build cache's wrapper, resolved on the PATH the command
/// gets (the release's plus the box tail), and the switch reaches testenv.
#[test_id(t8qx9gv)]
#[test]
fn every_run_compiles_through_the_wrapper_resolved_on_the_commands_path() {
    let f = Fake::new();
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    let asked = f.wrapper_asked.borrow().clone();
    assert_eq!(asked.len(), 1);
    assert!(asked[0].0.ends_with(":box/.cargo/bin"), "{asked:?}");
    assert_eq!(f.env_of(0, "RUSTC_WRAPPER"), "box/.cargo/bin/sccache");
    assert_eq!(f.env_of(0, "SCCACHE_IGNORE_SERVER_IO_ERROR"), "1");
    // No CARGO_* variable: sccache hashes those into every key (DESIGN-build-cache.md §2.2).
    for e in f.ran.borrow().iter() {
        assert!(e.iter().all(|(k, _)| !k.starts_with("CARGO_")), "{e:?}");
    }
}

/// The configured cargo home reaches the gate command and the release-bins build alike; an
/// empty key sets neither.
#[test_id(tfg1l2e)]
#[test]
fn the_configured_cargo_home_reaches_the_gate_command_and_the_release_bins_build() {
    let f = Fake::new();
    f.put_var("SPIRA_CARGO_HOME", "vol/cargo");
    assert_eq!(f.run_with(true), PASS, "{}", f.stderr());
    let ran = f.ran.borrow().clone();
    assert_eq!(f.env_of(0, "CARGO_HOME"), "vol/cargo");
    assert!(ran.last().unwrap().contains(&("CARGO_HOME".to_string(), "vol/cargo".to_string())), "release-bins: {:?}", ran.last());
    let g = Fake::new();
    g.put_var("SPIRA_CARGO_HOME", "");
    assert_eq!(g.run_with(true), PASS, "{}", g.stderr());
    assert!(g.ran.borrow().iter().all(|e| e.iter().all(|(k, _)| k != "CARGO_HOME")));
    let h = Fake::new();
    h.put_var("SPIRA_CARGO_HOME", "vol/cargo");
    *h.wrapper.borrow_mut() = Err("sccache is not on the build's PATH (/x)".into());
    assert_eq!(h.run(), PASS, "{}", h.stderr());
    assert_eq!(h.env_of(0, "CARGO_HOME"), "vol/cargo", "a refused wrapper still gets the cargo home");
}

/// Without `SPIRA_SCCACHE_DAV_ADDR` set, no webdav var reaches the build at all — the
/// no-store case stays exactly as it was before sp-xtdqi.
#[test_id(t9rlil9)]
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
#[test_id(t6or818)]
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
#[test_id(tuca7cr)]
#[test]
fn the_opt_out_is_loud_and_reaches_the_command() {
    let f = Fake::new();
    f.put_var("SPIRA_BUILD_CACHE", "off");
    *f.wrapper.borrow_mut() = Ok(spira_config::build::Wrapper::Off);
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert_eq!(f.wrapper_asked.borrow()[0].1, "off");
    assert!(f.stderr().contains("build cache: OFF"), "{}", f.stderr());
    assert_eq!(f.env_of(0, "RUSTC_WRAPPER"), "");
    assert_eq!(f.env_of(0, "SPIRA_BUILD_CACHE"), "off");
}

/// The gate tree's build goes to tmpfs before anything builds; a target that cannot be prepared is no verdict.
#[test_id(tmtf9zn)]
#[test]
fn the_gate_tree_builds_on_tmpfs_and_an_unpreparable_target_is_no_verdict() {
    let f = tree_owned(Some(STEPS), Some(STEPS));
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert_eq!(f.targets.borrow().as_slice(), &[PathBuf::from(GATE_TREE)]);
    assert!(f.stderr().contains("gate: build on /tmp/t/"), "{}", f.stderr());

    assert_eq!(f.released.borrow().as_slice(), &[(PathBuf::from(GATE_TREE), false)], "a finished gate drops its target");

    let f = tree_owned(Some(STEPS), Some(STEPS));
    *f.target_err.borrow_mut() = Some("gate: cannot create /tmp/t/target: Permission denied".into());
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=scratch-short"), "{}", f.verdict_line());
    assert!(f.cmds.borrow().is_empty(), "nothing built: {:?}", f.cmds.borrow());
}

/// The configured target root reaches the build, and the gate says where it builds.
#[test_id(t8wi44l)]
#[test]
fn the_gate_builds_on_the_configured_target_root() {
    let f = tree_owned(Some(STEPS), Some(STEPS));
    f.put_var("SPIRA_GATE_TARGET_ROOT", "cfg/root");
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert_eq!(f.target_roots.borrow().as_slice(), &["cfg/root".to_string()]);
    assert!(f.stderr().contains("gate: build on cfg/root/"), "{}", f.stderr());
}

/// The gate's target root is config; a scratch-short verdict names the room the root had.
#[test_id(t492kus)]
#[test]
fn a_scratch_short_verdict_names_the_configured_root_and_what_the_targets_held() {
    let f = Fake::new();
    f.put_var("SPIRA_GATE_TARGET_ROOT", "cfg/root");
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "test-a.sh RED\nDisk quota exceeded (os error 122)".into()));
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    let e = f.stderr();
    assert!(e.contains("gate: target root cfg/root: 1 MiB writable, gate targets hold 7 MiB there"), "{e}");
    assert!(e.contains("the user's quota"), "{e}");
}

/// A judged tree and one cut at a cap hand their build directory on; a refused one does not.
#[test_id(tv5vqzu)]
#[test]
fn a_build_directory_is_handed_on_when_judged_or_cut_and_not_when_refused() {
    let f = tree_owned(Some(STEPS), Some(STEPS));
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert_eq!(f.hand_on.borrow().as_slice(), &[true]);

    let f = capped_with_steps();
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=phase-cap"), "{}", f.verdict_line());
    assert!(f.hand_on.borrow().contains(&true), "{:?}", f.hand_on.borrow());

    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "test-a.sh RED\nNo space left on device".into()));
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(!f.hand_on.borrow().contains(&true), "{:?}", f.hand_on.borrow());
}

#[test_id(te4guhs)]
#[test]
fn only_the_cap_reasons_and_judged_statuses_hand_a_build_directory_on() {
    let vd = |status, reason: &str| Verdict { status, reason: reason.into(), msg: String::new() };
    for r in ["phase-cap", "budget", "deadline", "timeout"] {
        assert!(hands_on_build(&vd(NOVERDICT, r)), "{r}");
    }
    let src: String = include_str!("engine.rs").chars().filter(|c| !c.is_whitespace()).collect();
    let mut seen = 0;
    for part in src.split("v(NOVERDICT,\"").skip(1) {
        let r = &part[..part.find('"').unwrap()];
        seen += 1;
        let cap = matches!(r, "phase-cap" | "budget" | "deadline" | "timeout");
        assert_eq!(hands_on_build(&vd(NOVERDICT, r)), cap, "{r}");
    }
    assert!(seen >= 45, "the scan must find the reasons: {seen}");
    for s in [PASS, 1, 76] {
        assert!(hands_on_build(&vd(s, "branch-red")), "{s}");
    }
}

/// A build that dies on the exhausted scratch filesystem is infrastructure — never the
/// branch's red, and never the base's.
#[test_id(tqedjoj)]
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

/// A build that loses its sccache server is infrastructure, whatever the base re-run says;
/// a real compile error without transport errors stays the branch's red.
#[test_id(tnu9o4w)]
#[test]
fn a_build_that_loses_sccache_is_no_verdict_but_a_compile_error_is_red() {
    let wedged = "test-a.sh RED\nsccache: error: failed to execute compile\ncaused by: Connection reset by peer (os error 104)";
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, wedged.into()));
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=build-cache-fault"), "{}", f.verdict_line());
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "test-a.sh RED\nerror[E0308]: mismatched types".into()));
    assert_eq!(f.run(), FAIL, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=branch-red"), "{}", f.verdict_line());
}

/// A trial that loses its sccache server is run once more with the cache off, and the gate
/// says so; if the uncached run is clean the gate passes.
#[test_id(trhlbk8)]
#[test]
fn a_trial_that_loses_sccache_retries_once_without_the_cache() {
    let wedged = "test-a.sh RED\nsccache: error: failed to execute compile\ncaused by: Connection reset by peer (os error 104)";
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, wedged.into()));
    f.runs.borrow_mut().insert(format!("{MERGE_SHA}:uncached"), (0, String::new()));
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(f.stderr().contains("retrying the trial once without the compiler cache"), "{}", f.stderr());
    let n = f.ran.borrow().len();
    assert_eq!(f.env_of(n - 1, "SPIRA_BUILD_CACHE"), "off");
    assert_eq!(f.env_of(n - 1, "RUSTC_WRAPPER"), "");
    assert!(meter(&f).contains(" phases=gate:7,gate:7 deny_list=1000000/e3b0c44298fc test_clock=14s"), "{}", meter(&f));
}

const TEMP_DIR_FAULT: &str = "sccache: error: Failed to create temp dir\nsccache: caused by: Failed to create temp dir\nsccache: caused by: No such file or directory (os error 2) at path \"/w/sp-x/.runtime/session-tmp-1/sccacheAbC\"";

/// An sccache server that cannot create its temporary folder is a lost cache: the trial is
/// retried once without it.
#[test_id(t3eyqzo)]
#[test]
fn a_trial_whose_cache_server_cannot_create_its_temp_dir_retries_without_the_cache() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, format!("test-a.sh RED\n{TEMP_DIR_FAULT}")));
    f.runs.borrow_mut().insert(format!("{MERGE_SHA}:uncached"), (0, String::new()));
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(f.stderr().contains("retrying the trial once without the compiler cache"), "{}", f.stderr());
    let n = f.ran.borrow().len();
    assert_eq!(f.env_of(n - 1, "RUSTC_WRAPPER"), "");
}

/// A trial that loses the cache on both runs is build-cache-fault quoting the server's line.
#[test_id(t5lkzq1)]
#[test]
fn a_trial_that_cannot_create_the_cache_temp_dir_twice_is_build_cache_fault() {
    let f = Fake::new();
    let out = format!("test-a.sh RED\n{TEMP_DIR_FAULT}");
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, out.clone()));
    f.runs.borrow_mut().insert(format!("{MERGE_SHA}:uncached"), (1, out));
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=build-cache-fault"), "{}", f.verdict_line());
    assert!(f.stderr().contains("os error 2) at path"), "{}", f.stderr());
}

/// The same fault on the base re-run, twice, is build-cache-fault quoting the server's line.
#[test_id(tv3glth)]
#[test]
fn a_base_rerun_that_cannot_create_the_cache_temp_dir_twice_is_build_cache_fault() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, red(SUITE)));
    f.reruns.borrow_mut().insert((BASE.into(), SUITE.into()), (1, TEMP_DIR_FAULT.into()));
    f.reruns.borrow_mut().insert((format!("{BASE}:uncached"), SUITE.into()), (1, TEMP_DIR_FAULT.into()));
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=build-cache-fault"), "{}", f.verdict_line());
    assert!(f.stderr().contains("os error 2) at path"), "{}", f.stderr());
}

/// A base re-run that loses its sccache server is run once more with the cache off; a
/// re-run that then passes lets the branch's red stand as the branch's own.
#[test_id(t0mddil)]
#[test]
fn a_base_rerun_that_loses_sccache_retries_once_without_the_cache_then_passes() {
    let wedged = "sccache: error: failed to execute compile\ncaused by: Connection reset by peer (os error 104)";
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, red(SUITE)));
    f.reruns.borrow_mut().insert((BASE.into(), SUITE.into()), (1, wedged.into()));
    f.reruns.borrow_mut().insert((format!("{BASE}:uncached"), SUITE.into()), (0, format!("  {SUITE:<32} ok      1s")));
    assert_eq!(f.run(), FAIL, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=branch-red"), "{}", f.verdict_line());
    assert!(f.stderr().contains("retrying it once without the compiler cache"), "{}", f.stderr());
    let cmds = f.cmds.borrow();
    let reruns: Vec<usize> = (0..cmds.len()).filter(|&i| cmds[i].starts_with(BASE_RERUN_MARK)).collect();
    assert_eq!(reruns.len(), 2, "{cmds:?}");
    assert_eq!(f.env_of(reruns[0], "SPIRA_BUILD_CACHE"), "");
    assert_eq!(f.env_of(reruns[1], "SPIRA_BUILD_CACHE"), "off");
    assert!(meter_row(&f).contains(",base-rerun:14"), "{}", meter_row(&f));
}

/// A base re-run that fails the same way after the retry is the gate's NO_VERDICT, retried once.
#[test_id(t1dlsyz)]
#[test]
fn a_base_rerun_that_loses_sccache_twice_is_build_cache_fault() {
    let wedged = "sccache: error: failed to execute compile\ncaused by: Connection reset by peer (os error 104)";
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, red(SUITE)));
    f.reruns.borrow_mut().insert((BASE.into(), SUITE.into()), (1, wedged.into()));
    f.reruns.borrow_mut().insert((format!("{BASE}:uncached"), SUITE.into()), (1, wedged.into()));
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=build-cache-fault"), "{}", f.verdict_line());
    assert_eq!(f.stderr().matches("retrying it once").count(), 1, "{}", f.stderr());
}

/// A trial whose cache is already off, or whose red is not a cache fault, is never retried; a
/// retry that fails the same way is the verdict.
#[test_id(trds6hr)]
#[test]
fn only_a_cache_fault_on_a_cached_trial_is_retried_and_a_failed_retry_is_no_verdict() {
    let wedged = "test-a.sh RED\nsccache: error: failed to execute compile\ncaused by: Connection reset by peer (os error 104)";
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, wedged.into()));
    f.runs.borrow_mut().insert(format!("{MERGE_SHA}:uncached"), (1, wedged.into()));
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=build-cache-fault"), "{}", f.verdict_line());
    let retries = f.stderr().matches("retrying the trial once").count();
    assert_eq!(retries, 1, "a retry that fails the same way is not retried again: {}", f.stderr());

    let f = Fake::new();
    f.put_var("SPIRA_BUILD_CACHE", "off");
    *f.wrapper.borrow_mut() = Ok(spira_config::build::Wrapper::Off);
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, wedged.into()));
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(!f.stderr().contains("retrying the trial"), "{}", f.stderr());

    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "test-a.sh RED\nerror[E0308]: mismatched types".into()));
    assert_eq!(f.run(), FAIL, "{}", f.stderr());
    assert!(!f.stderr().contains("retrying the trial"), "{}", f.stderr());
}

#[test_id(t93iu5r)]
#[test]
fn the_uncached_env_empties_the_wrapper_and_says_the_cache_is_off() {
    let env = vec![("RUSTC_WRAPPER".to_string(), "box/.cargo/bin/sccache".to_string())];
    assert!(crate::engine::cache_in_force(&env));
    let off = crate::engine::without_cache(env);
    assert!(off.contains(&("RUSTC_WRAPPER".to_string(), String::new())), "{off:?}");
    assert!(off.contains(&("SPIRA_BUILD_CACHE".to_string(), "off".to_string())), "{off:?}");
    assert!(!crate::engine::cache_in_force(&off));
}

/// A test binary the gate built and something else removed is the host's fault: the red text
/// may name `CARGO_BIN_EXE_*` or only say NotFound, and the branch is not blamed for either.
#[test_id(tu21dsd)]
#[test]
fn a_gate_whose_built_binaries_vanished_is_no_verdict_never_branch_red() {
    let missing = "test-a.sh RED\nthread 'main' panicked at spira-claim/tests/bin_is_built.rs:7:5:\nassertion failed: Path::new(env!(\"CARGO_BIN_EXE_spira-claim\")).is_file()";
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, missing.into()));
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=target-vanished"), "{}", f.verdict_line());

    let f = Fake::new();
    f.target_swept.set(true);
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "test-a.sh RED\nspira-runtime runs: Os { code: 2, kind: NotFound }".into()));
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=target-vanished"), "{}", f.verdict_line());

    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "test-a.sh RED\nspira-runtime runs: Os { code: 2, kind: NotFound }".into()));
    assert_eq!(f.run(), FAIL, "a NotFound with the target intact is the branch's own: {}", f.stderr());
}

#[test_id(tpafwxk)]
#[test]
fn the_tools_build_opts_out_of_incremental_like_the_unit_phases() {
    let d = crate::def::parse("bin SPIRA_LINT_BIN spira-lint\nstep \"$SPIRA_LINT_BIN\"\n").unwrap();
    let c = d.tools_command(4).unwrap();
    assert!(c.contains(&format!(" {} ", spira_config::build::one_shot_words("dev"))) && !c.contains("--profile") && !c.contains("CARGO_INCREMENTAL"), "{c}");
}

/// A tree-built testenv is told its harness is the gate tree: its own executable now
/// resolves into the tmpfs build root, above which no harness marker lies (sp-z61hj). A
/// definition that builds no testenv sets nothing (the release's testenv finds its own).
#[test_id(tz5kmsz)]
#[test]
fn a_tree_built_testenv_is_given_the_gate_tree_as_its_harness() {
    let steps = "bin SPIRA_LINT_BIN spira-lint\nbin SPIRA_TESTENV_BIN testenv\nstep bash spira/a-fence.sh\n";
    let f = tree_owned(Some(steps), Some(steps));
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    let ran = f.ran.borrow();
    let e = ran.last().unwrap();
    let get = |k: &str| e.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone());
    assert_eq!(get("SPIRA_TESTENV_HARNESS").as_deref(), Some(GATE_TREE));
    assert!(get("SPIRA_TESTENV_BIN").unwrap().starts_with(&format!("{GATE_TREE}/target/tree-tools/")));
    drop(ran);
    let f = tree_owned(Some(STEPS), Some(STEPS));
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(f.ran.borrow().iter().all(|e| e.iter().all(|(k, _)| k != "SPIRA_TESTENV_HARNESS")));
}

/// `--release-bins` on a PASS: the judged tree's release profile is built in the gate tree
/// (tmpfs) through the build cache, one-shot, and the landing is told where it is.
#[test_id(tats1v1)]
#[test]
fn release_bins_builds_the_judged_tree_in_the_gate_tree_after_a_pass() {
    let f = Fake::new();
    assert_eq!(f.run_with(true), PASS, "{}", f.stderr());
    let cmds = f.cmds.borrow().clone();
    let last = cmds.last().unwrap();
    assert_eq!(last, &crate::engine::release_bins_command(JOBS));
    assert!(last.starts_with("cargo build -j 8 --release --workspace --locked --config profile.release.incremental=false"), "{last}");
    assert!(last.contains("find target/release -mindepth 1 -delete"), "a failed build leaves nothing to land: {last}");
    let env = f.ran.borrow().last().unwrap().clone();
    assert!(env.contains(&("RUSTC_WRAPPER".to_string(), "box/.cargo/bin/sccache".to_string())), "{env:?}");
    assert!(env.iter().any(|(k, v)| k == "PATH" && v.ends_with(":box/.cargo/bin")), "{env:?}");
    assert_eq!(f.trees.borrow().last().map(PathBuf::as_path), Some(Path::new(GATE_TREE)));
    assert!(f.stderr().contains(&format!("--worktree {GATE_TREE}")), "{}", f.stderr());
    // Without the flag nothing extra is built.
    let g = Fake::new();
    g.put_var("SPIRA_VERDICT_TTL", "86400");
    assert_eq!(g.run(), PASS);
    assert!(!g.cmds.borrow().iter().any(|c| c.contains("--release")), "{:?}", g.cmds.borrow());
}

/// A cached PASS with --release-bins keeps its cached verdict and still builds the judged
/// tree's release binaries in the gate tree.
#[test_id(tplau07)]
#[test]
fn a_cached_pass_with_release_bins_still_builds_the_judged_tree() {
    let f = Fake::new();
    let k = key_for(&f, "", "none");
    f.files.borrow_mut().insert(
        PathBuf::from(format!("{RUN}/verdicts/{k}")),
        key::render_entry("2026-09-28T00:00:00Z", 999_000, "dwarf", "spira", BR, "test-a.sh"),
    );
    assert_eq!(f.run_with(true), PASS, "{}", f.stderr());
    assert!(f.stderr().contains("VERDICT=PASS reason=cached"), "{}", f.stderr());
    let cmds = f.cmds.borrow().clone();
    assert_eq!(cmds.last(), Some(&crate::engine::release_bins_command(JOBS)), "{cmds:?}");
    assert!(cmds.iter().all(|c| c.contains("--release")), "a cached verdict runs no trial: {cmds:?}");
    assert!(f.checkouts.borrow().iter().any(|_| true), "the judged tree is checked out");
    assert!(f.stderr().contains(&format!("--worktree {GATE_TREE}")), "{}", f.stderr());
}

/// No PASS, no release build; a failed release build is said out loud and the verdict stands.
#[test_id(ta08srl)]
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
#[test_id(tx5xy5v)]
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

/// sp-f4ig1-fix: a gate never loses its verdict to an admission wait. Every command that can
/// compile — the trial's own and the after-PASS release-bins, which runs outside the trial's
/// environment — carries the gate's admission token, so testenv runs it on the gate's slot without queueing behind agent builds.
#[test_id(tu8tf25)]
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
    assert_eq!(f.cmds.borrow().last().unwrap(), &crate::engine::release_bins_command(JOBS));
}

/// A `spira-admit` on the PATH is not a compiler wrapper: every cargo the gate runs compiles
/// through sccache, with the gate's token for the pools that still read it.
#[test_id(t9l20op)]
#[test]
fn the_gates_cargo_compiles_through_sccache_even_with_spira_admit_on_the_path() {
    let f = Fake::new();
    f.files.borrow_mut().insert(PathBuf::from("h/spira-admit"), String::new());
    assert_eq!(f.run_with(true), PASS, "{}", f.stderr());
    for (i, env) in f.ran.borrow().iter().enumerate() {
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("RUSTC_WRAPPER"), Some("box/.cargo/bin/sccache"), "command {i}: {env:?}");
        assert_eq!(get("SPIRA_ADMIT_INNER"), None, "command {i}");
        assert_eq!(get("SPIRA_ADMISSION"), Some("gate"), "command {i}");
    }
}

#[test_id(tlh14t4)]
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

#[test_id(t06hk90)]
#[test]
fn a_phase_that_outlasts_the_deadline_is_no_verdict_naming_the_phase() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (124, String::new()));
    assert_eq!(f.run(), NOVERDICT);
    let line = f.verdict_line();
    assert!(line.contains("reason=deadline"), "{line}");
    assert!(f.stderr().contains("phase `gate` was running"), "{}", f.stderr());
}

#[test_id(tlinvyu)]
#[test]
fn a_build_that_outlasts_the_deadline_still_reaches_a_verdict() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.phase_secs.set(20);
    f.build_script.set(Some((900, 60, 900)));
    assert_eq!(f.run(), PASS, "{}", f.verdict_line());
}

#[test_id(tdjsdvb)]
#[test]
fn tests_that_outlast_the_deadline_are_a_deadline_naming_the_test_phase() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.phase_secs.set(20);
    f.build_script.set(Some((900, 60, 900)));
    f.unit_runs.borrow_mut().insert((MERGE_SHA.into(), "test"), (124, String::new()));
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=deadline"), "{}", f.verdict_line());
    assert!(f.stderr().contains("phase `test` was running"), "{}", f.stderr());
}

#[test_id(to1kmlx)]
#[test]
fn a_build_with_no_progress_for_the_stall_bound_is_a_build_stall() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.build_script.set(Some((900, 0, 0)));
    assert_eq!(f.run(), NOVERDICT);
    let line = f.verdict_line();
    assert!(line.contains("reason=build-stall"), "{line}");
    assert!(f.stderr().contains("phase `build`"), "{}", f.stderr());
}

#[test_id(tohquq7)]
#[test]
fn a_build_stall_names_the_last_compiler_line() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.put_var("SPIRA_GATE_BUILD_STALL", "100");
    f.build_script.set(Some((900, 60, 120)));
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.stderr().contains("last compiler output: Compiling crate-120"), "{}", f.stderr());
}

#[test_id(tl5dyeq)]
#[test]
fn an_operator_timeout_shorter_than_the_deadline_stays_a_timeout() {
    let f = Fake::new();
    f.put_var("SPIRA_GATE_TIMEOUT", "10");
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (124, String::new()));
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=timeout"), "{}", f.verdict_line());
}

#[test_id(tnjm2zs)]
#[test]
fn the_gate_phase_is_cut_at_the_configured_budget_never_a_literal() {
    // Both keys pinned to non-defaults: asserting against the shipped default passes just as
    // well when the code carries the number, which is how a 260s literal outlived a 300s
    // budget and a 900s deadline in production.
    let f = Fake::new();
    f.put_var("SPIRA_GATE_DEADLINE", "900");
    f.put_var("SPIRA_GATE_BUDGET", "420");
    f.repo_build_secs.set(Some(30));
    f.phase_secs.set(200);
    assert_eq!(f.run(), PASS, "{}", f.verdict_line());
    let t = f.timeouts.borrow().clone();
    let (_, to) = t
        .iter()
        .find(|(c, _)| c.contains("testenv") && !c.contains("--suites build") && !c.contains("--share-publish"))
        .expect("a gate phase ran");
    assert_eq!(to, "420", "the gate phase's cutoff is the configured budget: {t:?}");
    assert_eq!(crate::engine::phase_cap("base-gate", 420), Some(420));
    assert_eq!(crate::engine::phase_cap("reentry", 420), None);
    assert_eq!(crate::engine::phase_cap("publish", 420), None);
    assert_eq!(crate::engine::phase_cap("tools", 420), None);
}

#[test_id(t40odj7)]
#[test]
fn the_declared_build_tier_gate_phase_alone_hands_testenv_the_budget() {
    let f = Fake::new();
    f.put_var("SPIRA_GATE_DEADLINE", "900");
    f.put_var("SPIRA_GATE_BUDGET", "420");
    f.repo_build_secs.set(Some(30));
    assert_eq!(f.run(), PASS, "{}", f.verdict_line());
    let t = f.timeouts.borrow().clone();
    let has = |pick: &dyn Fn(&str) -> bool| t.iter().find(|(c, _)| c.contains("testenv") && pick(c)).map(|(c, _)| c.clone()).expect("phase ran");
    let gate = has(&|c| !c.contains("--suites build") && !c.contains("--share-publish"));
    assert!(gate.contains("--deadline 420 "), "{gate}");
    assert!(!has(&|c| c.contains("--suites build")).contains("--deadline"));
    assert!(!has(&|c| c.contains("--share-publish")).contains("--deadline"));
}

#[test_id(t27uwa5)]
#[test]
fn an_unparsable_gate_budget_refuses_naming_the_key() {
    let f = Fake::new();
    f.put_var("SPIRA_GATE_BUDGET", "abc");
    assert_eq!(f.run(), NOVERDICT);
    let l = f.verdict_line();
    assert!(l.contains("reason=config"), "{l}");
    assert!(f.stderr().contains("SPIRA_GATE_BUDGET"), "{}", f.stderr());
}

#[test_id(tsgziv0)]
#[test]
fn a_phase_killed_at_its_cap_is_not_a_deadline() {
    use crate::engine::killed_verdict;
    let ph = |n: &str, s: u64| vec![("tools".to_string(), 8), (n.to_string(), s)];
    let at_cap = killed_verdict(&ph("gate", 260), 260, 300, 270, "c", "o");
    assert_eq!(at_cap.reason, "phase-cap");
    assert!(at_cap.msg.contains("`gate`") && at_cap.msg.contains("260s cap"), "{}", at_cap.msg);
    assert_eq!(killed_verdict(&ph("gate", 40), 260, 300, 300, "c", "o").reason, "deadline");
    assert_eq!(killed_verdict(&ph("gate", 260), 260, 270, 270, "c", "o").reason, "deadline");
    assert_eq!(killed_verdict(&ph("reentry", 500), 260, 300, 100, "c", "o").reason, "deadline");
}

#[test_id(t5519ry)]
#[test]
fn a_gate_with_no_declared_deadline_refuses() {
    let f = Fake::new();
    f.put_var("SPIRA_GATE_DEADLINE", "");
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=config"), "{}", f.verdict_line());
}

// ------------------------------------------------------- tools from the active release

/// The merge's tree id, as the fake resolves `<MERGE_SHA>^{tree}`.
fn want() -> String {
    format!("tree-of-{MERGE_SHA}")
}

/// A tree that builds spira-lint, whose closure is spira-lint + spira-config (its normal
/// dependency) — not gate, a member outside it — against a release that has spira-lint.
fn release_fake() -> Fake {
    let f = tree_owned(Some(STEPS), Some(STEPS));
    f.metadata.borrow_mut().insert(
        MERGE_SHA.into(),
        Ok(metadata_json(&[("spira-lint", &["spira-config"]), ("spira-config", &[]), ("gate", &["spira-config"])])),
    );
    f.files.borrow_mut().insert(PathBuf::from("rel/bin/spira-lint"), "#!release".into());
    f
}

fn touch(f: &Fake, paths: &[&str]) {
    *f.touched.borrow_mut() = Ok(paths.iter().map(|p| Changed { path: p.to_string(), exec: false }).collect());
}

fn built(f: &Fake) -> bool {
    f.cmds.borrow().iter().any(|c| c.starts_with("cargo build"))
}

fn gate_log(f: &Fake) -> String {
    f.appended.borrow().join("")
}

/// A bead whose diff touches nothing the tools are built from runs its steps with the active
/// release's binaries, builds nothing, and says so in gate.log.
#[test_id(t334dbp)]
#[test]
fn a_diff_outside_the_tools_closure_runs_the_steps_with_the_releases_binaries() {
    let f = release_fake();
    touch(&f, &["gate/src/engine.rs", "spira/a-fence.sh"]);
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(!built(&f), "{:?}", f.cmds.borrow());
    assert_eq!(f.env_of(0, "SPIRA_LINT_BIN"), "rel/bin/spira-lint");
    let log = gate_log(&f);
    assert!(log.contains(" tools=release"), "{log}");
    assert!(!log.contains("tools:"), "no tools phase: {log}");
}

/// A bead that changes a tool's source, or what every tool is built from, builds the tools from
/// its tree for this run and its steps use those.
#[test_id(tma6clu)]
#[test]
fn a_diff_inside_the_tools_closure_builds_the_tools_from_the_tree() {
    for changed in ["spira-config/src/lib.rs", "spira-lint/src/main.rs", "Cargo.lock", "rust-toolchain.toml", ".cargo/config.toml"] {
        let f = release_fake();
        touch(&f, &[changed]);
        assert_eq!(f.run(), PASS, "{changed}: {}", f.stderr());
        assert!(built(&f), "{changed}: {:?}", f.cmds.borrow());
        assert_eq!(f.env_of(1, "SPIRA_LINT_BIN"), format!("{GATE_TREE}/target/tree-tools/{}/spira-lint", want()), "{changed}");
        let log = gate_log(&f);
        assert!(log.contains(" tools=tree:") && log.contains("phases=tools:"), "{changed}: {log}");
    }
}

/// The base trial takes the release's tools even when the branch trial built from its tree:
/// the base touches nothing, and only the branch's build is paid for.
#[test_id(tsycovv)]
#[test]
fn the_base_trial_uses_the_releases_tools_when_the_branch_built_its_own() {
    let f = release_fake();
    touch(&f, &["spira-lint/src/main.rs"]);
    let meta = f.metadata.borrow()[MERGE_SHA].clone();
    f.metadata.borrow_mut().insert(BASE.into(), meta);
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "spira-lint: payload-argv-lint: violation at x:1".into()));
    assert_eq!(f.run(), FAIL, "{}", f.stderr());
    let builds = f.cmds.borrow().iter().filter(|c| c.starts_with("cargo build")).count();
    assert_eq!(builds, 1, "{:?}", f.cmds.borrow());
    let n = f.ran.borrow().len();
    assert!(n >= 2, "branch and base steps both ran");
    assert_eq!(f.env_of(0, "SPIRA_LINT_BIN"), format!("{GATE_TREE}/target/tree-tools/{}/spira-lint", want()));
    assert_eq!(f.env_of(n - 1, "SPIRA_LINT_BIN"), "rel/bin/spira-lint");
    assert!(gate_log(&f).contains(" tools=tree:"), "{}", gate_log(&f));
}

/// A release without a tool the steps name is a harness fault naming the tool, never a build.
#[test_id(t17kavs)]
#[test]
fn a_release_lacking_a_named_tool_is_a_fault_naming_it_not_a_silent_build() {
    let f = release_fake();
    f.files.borrow_mut().remove(&PathBuf::from("rel/bin/spira-lint"));
    touch(&f, &["gate/src/engine.rs"]);
    assert_ne!(f.run(), PASS);
    assert!(!built(&f), "{:?}", f.cmds.borrow());
    let out = format!("{}{}", f.stderr(), f.verdict_line());
    assert!(out.contains("lacks spira-lint") && out.contains("tools-unattributed"), "{out}");
}

/// Tools whose closure cannot be read are built from the tree: the choice fails closed.
#[test_id(t0qj4qw)]
#[test]
fn a_closure_that_cannot_be_read_builds_from_the_tree() {
    let f = release_fake();
    f.metadata.borrow_mut().insert(MERGE_SHA.into(), Err("no cargo".into()));
    touch(&f, &["gate/src/engine.rs"]);
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(built(&f));
}

/// The closure a tool is built from follows normal and build dependencies transitively and
/// leaves dev dependencies and unrelated members out; a path is touched when it is inside a
/// closure directory or is a workspace-wide build input.
#[test_id(ty2a5r0)]
#[test]
fn the_tools_closure_follows_build_dependencies_and_not_dev_ones() {
    use crate::toolkey::{closure, first_touched};
    let m = |n: &str, build: &[&str], dev: &[&str]| crate::compose::Member {
        name: n.into(),
        dir: n.into(),
        deps: build.iter().chain(dev).map(|d| d.to_string()).collect(),
        build_deps: build.iter().map(|d| d.to_string()).collect(),
        targets: Default::default(),
    };
    let ms = [m("lint", &["config"], &["testkit"]), m("config", &["leaf"], &[]), m("leaf", &[], &[]), m("testkit", &[], &[]), m("gate", &["config"], &[])];
    let c = closure(&ms, &["lint".into()]).unwrap();
    let dirs: Vec<String> = c.iter().map(|m| m.dir.clone()).collect();
    assert_eq!(dirs, ["lint", "config", "leaf"]);
    let touched = |p: &str| first_touched(&[p.to_string()], &dirs).is_some();
    assert!(touched("leaf/src/lib.rs") && touched("Cargo.lock") && touched(".cargo/config.toml") && touched("rust-toolchain"));
    assert!(!touched("testkit/src/lib.rs") && !touched("gate/src/main.rs") && !touched("leafy/src/lib.rs") && !touched("spira/a.sh"));
    assert!(closure(&ms, &["nope".into()]).unwrap_err().contains("nope is not a workspace member"));
}

const BASE_F_S1: &str = "gate-step RED rc=1 :: fence-f\n  test-s1.sh RED rc=1 after 3s";

#[test_id(t850zrg)]
#[test]
fn a_branch_green_but_for_the_bases_reds_passes_with_them_inherited() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "gate-step RED rc=1 :: fence-f\n  test-s1.sh RED rc=1 after 3s\n  test-s9.sh ok".into()));
    f.runs.borrow_mut().insert(BASE.into(), (1, BASE_F_S1.into()));
    f.reruns.borrow_mut().insert((BASE.into(), "test-s1.sh".into()), (1, "  test-s1.sh RED rc=1".into()));
    assert_eq!(f.run(), PASS);
    let e = f.stderr();
    assert!(e.contains("inherited, not judged (red on local/main too, so not this branch's): test-s1.sh fence-f"), "{e}");
}

#[test_id(tjbo95h)]
#[test]
fn a_branch_that_adds_a_suite_red_and_a_fence_red_is_held_for_those_only() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(
        MERGE_SHA.into(),
        (1, "gate-step RED rc=1 :: fence-f\ngate-step RED rc=1 :: fence-g\n  test-s1.sh RED rc=1\n  test-s2.sh RED rc=1".into()),
    );
    f.runs.borrow_mut().insert(BASE.into(), (1, BASE_F_S1.into()));
    f.reruns.borrow_mut().insert((BASE.into(), "test-s1.sh,test-s2.sh".into()), (1, "  test-s1.sh RED rc=1\n  test-s2.sh ok".into()));
    f.reruns.borrow_mut().insert((BASE.into(), "test-s2.sh".into()), (0, "  test-s2.sh ok".into()));
    assert_eq!(f.run(), FAIL);
    let e = f.stderr();
    assert!(e.contains("red on this branch and not on local/main: test-s2.sh fence-g"), "{e}");
    assert!(e.contains("inherited, not judged (red on local/main too): test-s1.sh fence-f"), "{e}");
}

mod machine_events {
    use super::*;
    use crate::machine::GateState::{self, *};

    fn states(f: &Fake) -> Vec<GateState> {
        f.machine.borrow().last().map(|r| r.events.iter().map(|e| e.to).collect()).unwrap_or_default()
    }

    fn last_reason(f: &Fake) -> String {
        f.machine.borrow().last().and_then(|r| r.events.last().map(|e| e.reason.clone())).unwrap_or_default()
    }

    #[test_id(tnhsz6d)]
    #[test]
    fn a_trial_is_recorded_from_queued_to_its_verdict() {
        let f = Fake::new();
        assert_eq!(f.run(), 0, "{}", f.stderr());
        assert_eq!(states(&f), vec![Queued, Admitted, Rebased, Trial, Verdict]);
        assert!(last_reason(&f).starts_with("PASS pass"), "{}", last_reason(&f));
    }

    #[test_id(tkdq0ii)]
    #[test]
    fn a_branch_that_does_not_merge_ends_at_its_verdict_from_queued() {
        let f = Fake::new();
        *f.merge.borrow_mut() = Merge::Conflict(vec!["a".into()]);
        assert_eq!(f.run(), 1);
        assert_eq!(states(&f), vec![Queued, Verdict]);
        assert!(last_reason(&f).starts_with("FAIL no-rebase"), "{}", last_reason(&f));
    }

    #[test_id(tp8e3lk)]
    #[test]
    fn a_fences_only_run_passes_through_fences_and_takes_no_slot() {
        let f = Fake::new();
        f.put_var("SPIRA_GATE_SUITES", "off");
        assert_eq!(f.run(), 0, "{}", f.stderr());
        let got = states(&f);
        assert_eq!(&got[..3], &[Queued, Admitted, Rebased], "{got:?}");
        assert!(got.contains(&Trial) || got.contains(&Fences), "{got:?}");
        assert_eq!(*got.last().unwrap(), Verdict);
        let slot = f.machine.borrow().last().unwrap().events[1].reason.clone();
        assert!(slot.contains("no slot"), "{slot}");
    }

    #[test_id(tccr54o)]
    #[test]
    fn a_record_that_cannot_be_written_refuses_the_run() {
        let f = Fake::new();
        *f.machine_err.borrow_mut() = Some("disk full".into());
        assert_eq!(f.run(), 75);
        assert!(f.verdict_line().contains("reason=machine-fault"), "{}", f.verdict_line());
        assert!(f.ran.borrow().is_empty(), "nothing ran");
    }

    #[test_id(tckkqr3)]
    #[test]
    fn a_cancel_requested_while_the_run_is_live_ends_it_cancelled() {
        let f = Fake::new();
        f.cancel_at.set(Some(2));
        assert_eq!(f.run(), 75, "{}", f.stderr());
        assert!(f.verdict_line().contains("reason=cancelled"), "{}", f.verdict_line());
        assert_eq!(states(&f), vec![Queued, Admitted, Verdict]);
        assert!(last_reason(&f).starts_with("NO_VERDICT cancelled"), "{}", last_reason(&f));
    }

    #[test_id(tj3mk9n)]
    #[test]
    fn a_cancel_that_stops_a_waiting_run_reads_as_cancelled_not_died() {
        let f = Fake::new();
        f.signal.set(true);
        f.cancel_at.set(Some(4));
        assert_eq!(f.run(), 75);
        assert!(f.verdict_line().contains("reason=cancelled"), "{}", f.verdict_line());
    }
}

// ------------------------------------------------- family J: what the verdict suites held

#[test_id(t8nwoi7)]
#[test]
fn a_timeout_names_its_budget_and_command_and_is_not_cached() {
    let f = Fake::new();
    f.put_var("SPIRA_GATE_TIMEOUT", "10");
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (124, String::new()));
    assert_eq!(f.run(), NOVERDICT);
    let e = f.stderr();
    assert!(e.contains("killed at 10s") && e.contains("gate: command: bash spira/fence.sh"), "{e}");
    assert!(f.verdict_line().contains("VERDICT=NO_VERDICT reason=timeout"), "{}", f.verdict_line());
    assert!(f.written.borrow().is_empty(), "{:?}", f.written.borrow());
}

#[test_id(tckpv96)]
#[test]
fn a_harness_fault_is_never_cached_or_charged_to_the_base() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (75, String::new()));
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=harness-fault"));
    assert!(!f.stderr().contains("BASE_FAIL") && !f.verdict_line().contains("branch-red"));
    assert!(f.written.borrow().is_empty() && f.ran.borrow().len() == 1);
}

#[test_id(tuzt0qr)]
#[test]
fn a_container_death_found_only_at_the_deadline_keeps_its_real_cause() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(
        MERGE_SHA.into(),
        (124, "batch: harness fault — container died mid-batch (ExitCode=137 OOMKilled=false)".into()),
    );
    assert_eq!(f.run(), NOVERDICT);
    let line = f.verdict_line();
    assert!(line.contains("reason=harness-fault") && !line.contains("reason=timeout"), "{line}");
    assert!(f.stderr().contains("ExitCode=137 OOMKilled=false"), "{}", f.stderr());
    assert!(f.written.borrow().is_empty());
}

#[test_id(t6i8595)]
#[test]
fn both_trials_see_the_same_env_contract() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "lint said no".into()));
    f.runs.borrow_mut().insert(BASE.into(), (1, "lint said no".into()));
    f.run();
    assert_eq!(f.ran.borrow().len(), 2, "branch trial and base trial");
    for i in 0..2 {
        assert_eq!(f.env_of(i, "SPIRA_GATE_SELECT_HEAD"), BR, "trial {i}");
        assert_eq!(f.env_of(i, "SPIRA_GATE_BASE"), BASE, "trial {i}");
        assert_eq!(f.env_of(i, "SPIRA_GATE_BUDGET"), "300", "trial {i}");
        assert_eq!(f.env_of(i, "SPIRA_RUN"), RUN, "trial {i}");
        assert!(
            !f.ran.borrow()[i].iter().any(|(k, _)| k == "SPIRA_GATE_HOST_CORES"),
            "trial {i}: the retired host-cores export is back"
        );
    }
}

fn tmp_for(f: &Fake) -> String {
    let t = f.env_of(0, "TMPDIR");
    assert!(
        t.starts_with(&format!("{RUN}/worktree/.gate.")) && t.ends_with(".tmp"),
        "TMPDIR is <RUN>/worktree/.gate.<repo>.<key>.tmp, got {t:?}"
    );
    t
}

#[test_id(t5uhj97)]
#[test]
fn every_trial_gets_a_per_tree_tmpdir_on_the_run_disk() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "lint said no".into()));
    f.runs.borrow_mut().insert(BASE.into(), (1, "lint said no".into()));
    f.run();
    assert_eq!(f.ran.borrow().len(), 2);
    let t = tmp_for(&f);
    assert_eq!(f.env_of(1, "TMPDIR"), t);
}

#[test_id(tr9y3be)]
#[test]
fn a_finished_trial_leaves_no_tmpdir_pass_or_red() {
    for red in [false, true] {
        let f = Fake::new();
        if red {
            f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "lint said no".into()));
        }
        f.run();
        let t = PathBuf::from(tmp_for(&f));
        assert!(!f.live_dirs.borrow().contains(&t), "red={red}: {t:?} left behind");
        assert!(f.removed_dirs.borrow().contains(&t), "red={red}: never removed");
    }
}

#[test_id(tu3lld8)]
#[test]
fn a_stale_tmpdir_is_emptied_before_the_gate_command_runs() {
    let learn = Fake::new();
    learn.run();
    let t = PathBuf::from(tmp_for(&learn));
    let f = Fake::new();
    f.files.borrow_mut().insert(t.join("libduckdb/duckdb.h"), "stale".into());
    f.run();
    assert_eq!(f.reset_cleared.get(), 1, "the stale file was emptied at trial start");
    assert_eq!(f.reset_log.borrow().first(), Some(&t), "reset before any command ran");
}

#[test_id(tmotvvd)]
#[test]
fn a_tmpdir_that_cannot_be_reset_is_a_noverdict_naming_the_path_and_runs_nothing() {
    let f = Fake::new();
    f.reset_fails.set(true);
    f.run();
    assert!(f.ran.borrow().is_empty(), "no gate command may run");
    let err = f.err.borrow().join("\n");
    assert!(err.contains(".tmp") && err.contains("read-only"), "{err}");
    assert!(err.contains("VERDICT=NO_VERDICT reason=tmpdir-unusable"), "{err}");
    assert!(f.live_dirs.borrow().is_empty(), "nothing left behind: {err}");
}

#[test_id(ta78tpg)]
#[test]
fn a_red_keeps_its_early_diagnostic_whole_and_a_pass_prints_none_of_it() {
    let noisy = format!("DIAG: kept\n{}", (1..=25).map(|i| format!("suite-{i}.sh ok\n")).collect::<String>());
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, noisy.clone()));
    f.runs.borrow_mut().insert(BASE.into(), (0, String::new()));
    assert_eq!(f.run(), FAIL);
    assert!(f.stderr().contains("DIAG: kept"), "{}", f.stderr());
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (0, noisy));
    assert_eq!(f.run(), PASS);
    assert!(!f.stderr().contains("DIAG: kept"), "{}", f.stderr());
}

#[test_id(twyn39j)]
#[test]
fn an_inherited_red_carries_the_bases_own_output() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, red("test-shared.sh")));
    f.runs.borrow_mut().insert(BASE.into(), (1, format!("{}\nBASE-ONLY-EVIDENCE", red("test-shared.sh"))));
    f.reruns.borrow_mut().insert((BASE.into(), "test-shared.sh".into()), (1, red("test-shared.sh")));
    assert_eq!(f.run(), PASS);
    let e = f.stderr();
    assert!(e.contains("inherited, not judged") && e.contains("BASE-ONLY-EVIDENCE"), "{e}");
    assert!(!f.verdict_line().contains("base-red"));
}

#[test_id(tdm1gbi)]
#[test]
fn a_base_with_reds_and_timeouts_inherits_the_shared_red() {
    let both = format!("{}\n  test-slow.sh                     TIMEOUT after 600s", red("test-real.sh"));
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, both.clone()));
    f.runs.borrow_mut().insert(BASE.into(), (1, both));
    f.reruns.borrow_mut().insert((BASE.into(), "test-real.sh".into()), (1, red("test-real.sh")));
    assert_eq!(f.run(), PASS);
    assert!(f.stderr().contains("inherited, not judged") && f.stderr().contains("test-real.sh"));
}

#[test_id(tnl1a2n)]
#[test]
fn an_unnamed_red_on_both_trials_is_the_bases_with_no_suite() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "lint said no".into()));
    f.runs.borrow_mut().insert(BASE.into(), (1, "lint said no".into()));
    f.run();
    let line = f.verdict_line();
    assert!(line.contains("reason=base-red") && line.ends_with("suite=-"), "{line}");
}

#[test_id(tenhvdi)]
#[test]
fn a_base_trial_that_reports_a_harness_fault_is_no_verdict_not_base_fail() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "lint said no".into()));
    f.runs.borrow_mut().insert(BASE.into(), (75, String::new()));
    assert_eq!(f.run(), NOVERDICT);
    assert!(!f.verdict_line().contains("BASE_FAIL") && !f.verdict_line().contains("base-red"), "{}", f.verdict_line());
}

// ------------------------------------------------- gate_mode = fences (fences and build only)

/// The repository's own checked-in definition: what production runs.
fn checked_in_steps() -> String {
    std::fs::read_to_string(spira_config::resolve::checkout_root_for_process().unwrap().join("gate.steps")).unwrap()
}

/// A fences-mode fake on the checked-in definition, certifying bead sp-a (suites off and a
/// bead: the case where every other mode still selects the covering suites).
fn fences_fake(paths: &[&str]) -> Fake {
    let steps = checked_in_steps();
    let f = tree_owned(Some(&steps), Some(&steps));
    f.prints("$SPIRA_GUARD_BIN", &["lifecycle-guard"]);
    f.prints("spira/build-fence.sh", &["build-fence"]);
    f.prints("$SPIRA_FIXTURE_BIN", &["fixture-config"]);
    f.prints("spira/round-image-preflight.sh", &["round-image-preflight"]);
    f.prints("spira/ci-spill-guard.sh", &["ci-spill-guard"]);
    f.prints("spira/boundary.sh", &["boundary"]);
    *f.mode.borrow_mut() = Ok(GateMode::Fences);
    *f.touched.borrow_mut() = Ok(paths.iter().map(|p| Changed { path: p.to_string(), exec: false }).collect());
    f.put_var("SPIRA_GATE_BEAD", "sp-a");
    f.put_var("SPIRA_GATE_SUITES", "off");
    f
}

#[test_id(tl6wvtd)]
#[test]
fn fences_mode_runs_the_fence_tools_the_fences_and_the_build_fence_and_no_test_or_suite() {
    // A workspace input and a script, no crate: nothing here asks for a unit test.
    let f = fences_fake(&["Cargo.lock", "spira/lib.sh"]);
    f.put_var("SPIRA_GATE_BEAD", "");
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    let cmds = f.cmds.borrow().clone();
    assert_eq!(cmds.len(), 2, "the tools phase, then the fences: {cmds:?}");
    // The tools phase builds what the fences read — not the selector, not the suite runner.
    assert!(cmds[0].starts_with("cargo build -j "), "{cmds:?}");
    assert!(cmds[0].contains("-p spira-lint") && cmds[0].contains("-p lifecycle-guard"), "{}", cmds[0]);
    assert!(!cmds[0].contains("suite-select") && !cmds[0].contains("testenv"), "{}", cmds[0]);
    // The fences: the checked-in definition without its suites step, the build fence kept.
    let want = crate::def::parse(&checked_in_steps()).unwrap().fences_only(&[]).command();
    assert_eq!(cmds[1], want);
    assert!(cmds[1].contains("bash spira/build-fence.sh"), "{}", cmds[1]);
    assert!(cmds[1].contains("\"$SPIRA_LINT_BIN\"") && cmds[1].contains("\"$SPIRA_GUARD_BIN\""), "{}", cmds[1]);
    for gone in ["SPIRA_SELECT_BIN", "SPIRA_TESTENV_BIN", "--suites"] {
        assert!(!cmds[1].contains(gone), "{gone}: {}", cmds[1]);
    }
    assert!(!cmds.iter().any(|c| c.contains("cargo nextest run")), "no unit test phase: {cmds:?}");
    assert_eq!(f.env_of(1, "SPIRA_GATE_SUITES"), "off");
    assert_eq!(f.env_of(1, "SPIRA_GATE_COVERED"), "", "a gate with no bead selects no covering suites");
    assert_eq!(f.env_of(1, "SPIRA_TESTENV_BIN"), "", "the suite runner is not built or handed over");
    assert!(f.stderr().contains("gate: composition=fences"), "{}", f.stderr());
    assert!(meter(&f).contains(" compose=fences phases=tools:7,fences:7"), "{}", meter(&f));
}

#[test_id(tr3f12q)]
#[test]
fn fences_mode_a_planted_fence_violation_still_fails_the_gate_and_the_base_runs_the_same_mode() {
    let f = fences_fake(&["docs/x.md"]);
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "spira-lint: payload-argv-lint: violation at docs/x.md:1".into()));
    assert_eq!(f.run(), FAIL, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=branch-red"), "{}", f.verdict_line());
    let runs = gate_runs(&f);
    let want = crate::def::parse(&checked_in_steps()).unwrap().fences_only(&[]).command();
    assert_eq!(runs, [want.clone(), want], "the base trial composes fences mode too");
    assert!(!f.cmds.borrow().iter().any(|c| c.contains("; tools/test-sandbox/runner --around cargo nextest run ")));
}

#[test_id(thpv4e6)]
#[test]
fn fences_mode_a_unit_test_failure_in_a_touched_crate_fails_the_branch_at_the_gate() {
    // Fences mode runs the touched crates' unit tests after the fences: a broken unit test is
    // red here, not in a round 15 minutes and a VM pass later (Ryan 2026-10-29).
    let f = fences_fake(&["gate/src/x.rs"]);
    f.unit_runs.borrow_mut().insert((MERGE_SHA.into(), "test"), (101, "test x ... FAILED\n     Summary [   0.010s] 1 test run: 0 passed, 1 failed, 0 skipped".into()));
    assert_eq!(f.run(), FAIL, "{}", f.stderr());
    assert!(f.cmds.borrow().iter().any(|c| c.contains("; tools/test-sandbox/runner --around cargo nextest run ")), "the touched crate's tests ran");
    // Control: a branch touching no crate runs no unit tests and passes on the same fixture.
    let d = fences_fake(&["docs/x.md"]);
    d.unit_runs.borrow_mut().insert((MERGE_SHA.into(), "test"), (101, "test x ... FAILED\n     Summary [   0.010s] 1 test run: 0 passed, 1 failed, 0 skipped".into()));
    assert_eq!(d.run(), PASS, "{}", d.stderr());
    assert!(!d.cmds.borrow().iter().any(|c| c.contains("cargo nextest run")));
}

#[test_id(t4lfip0)]
#[test]
fn fences_mode_a_re_entering_bead_runs_exactly_its_named_suites_and_a_fresh_one_none() {
    // sp-p3srm holds in fences mode: the round's named suites, and the suites the branch
    // changed, run against the tree — through the tree's own suite runner, which the tools
    // phase then builds.
    let f = returned(fences_fake(&["docs/x.md", "spira/test-c.sh"]), "test-b.sh");
    *f.diff.borrow_mut() = Ok("docs/x.md\nspira/test-c.sh".into());
    f.files.borrow_mut().insert(PathBuf::from(format!("{GATE_TREE}/spira/test-c.sh")), "#!".into());
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    let cmds = f.cmds.borrow().clone();
    assert_eq!(cmds.len(), 4, "tools, fences, covered, re-entry: {cmds:?}");
    assert!(cmds[0].contains("-p testenv"), "{}", cmds[0]);
    assert!(!cmds[1].contains("--suites") && !cmds[1].contains("SPIRA_SELECT_BIN"), "{}", cmds[1]);
    assert!(cmds[2].contains("SPIRA_SELECT_BIN"), "{}", cmds[2]);
    assert!(cmds[3].contains("--suites test-b.sh,test-c.sh \"$SPIRA_GATE_BRANCH\""), "{}", cmds[3]);
    assert!(f.env_of(3, "SPIRA_TESTENV_BIN").ends_with("/testenv"), "the tree's own runner");
    assert!(!cmds.iter().any(|c| c.contains("cargo nextest run")), "{cmds:?}");
    assert!(meter(&f).contains("compose=fences+reentry phases=tools:7,fences:7,covered:7,reentry:7"), "{}", meter(&f));
    assert!(f.stderr().contains("gate PASS covered suites: test-b.sh,test-c.sh"), "{}", f.stderr());

    // A named suite red again fails the bead before it rides a round.
    let r = returned(fences_fake(&["docs/x.md"]), "test-b.sh");
    r.reentry_runs.borrow_mut().insert(MERGE_SHA.into(), (1, "  test-b.sh   RED     rc=1 after 4s\nVERDICT RED ran=1 red=1".into()));
    assert_eq!(r.run(), FAIL, "{}", r.stderr());
    assert!(r.verdict_line().contains("suite=test-b.sh"), "{}", r.verdict_line());

    // A fresh bead (nothing named, no suite changed): no suite of any kind, no runner built.
    let fresh = fences_fake(&["docs/x.md"]);
    *fresh.diff.borrow_mut() = Ok("docs/x.md".into());
    assert_eq!(fresh.run(), PASS, "{}", fresh.stderr());
    let cmds = fresh.cmds.borrow().clone();
    assert_eq!(cmds.len(), 3, "tools, fences, covered: {cmds:?}");
    assert!(!cmds.iter().any(|c| c.contains("release config-stage")), "no re-entry command: {cmds:?}");
    assert!(!meter(&fresh).contains("reentry"), "{}", meter(&fresh));
}

#[test_id(t4dd3cq)]
#[test]
fn fences_mode_a_branch_touching_one_suite_runs_that_suite_through_the_trees_runner() {
    let f = fences_fake(&["spira/test-c.sh"]);
    *f.diff.borrow_mut() = Ok("spira/test-c.sh".into());
    f.files.borrow_mut().insert(PathBuf::from(format!("{GATE_TREE}/spira/test-c.sh")), "#!".into());
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (0, "fences ok".into()));
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    let cmds = f.cmds.borrow().clone();
    assert_eq!(cmds.len(), 4, "tools, fences, covered, re-entry: {cmds:?}");
    assert!(cmds[0].contains("-p testenv"), "{}", cmds[0]);
    assert!(cmds[3].contains("--suites test-c.sh \"$SPIRA_GATE_BRANCH\""), "{}", cmds[3]);
    assert!(f.env_of(3, "SPIRA_TESTENV_BIN").ends_with("/testenv"), "the tree's own runner");
    assert!(!f.env_of(3, "SPIRA_GATE_REPO").is_empty());
    assert!(cmds[3].contains("release config-stage ."), "the runner reads the tree's config delta: {}", cmds[3]);
}

const COVERED_OUT: &str = "  test-a.sh   ok      3s\nbatch: phases: resolve:0,build:20,up:1,suites:3 (setup 21s, warm: cold)\nVERDICT GREEN ran=1";

#[test_id(tnc0g7w)]
#[test]
fn a_fences_mode_certification_runs_its_covered_suites_as_their_own_phase() {
    let f = fences_fake(&["docs/x.md"]);
    f.put_var("SPIRA_GATE_BUDGET", "140");
    f.covered_runs.borrow_mut().insert(MERGE_SHA.into(), (0, COVERED_OUT.into()));
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    let cmds = f.cmds.borrow().clone();
    assert_eq!(cmds.len(), 3, "tools, fences, covered: {cmds:?}");
    assert_eq!(f.env_of(2, "SPIRA_GATE_COVERED"), "1");
    assert_eq!(f.env_of(2, "SPIRA_GATE_SUITES"), "off");
    assert!(cmds[2].contains("--deadline \"${SPIRA_GATE_BUDGET:-300}\""), "{}", cmds[2]);
    assert_eq!(f.env_of(2, "SPIRA_GATE_BUDGET"), "140", "the phase is bounded by the budget it was given");
    assert!(f.env_of(2, "SPIRA_SELECT_BIN").ends_with("/suite-select") && f.env_of(2, "SPIRA_TESTENV_BIN").ends_with("/testenv"));
    assert!(!cmds[1].contains("SPIRA_SELECT_BIN"), "the fences string, and so its cache, is unchanged: {}", cmds[1]);
    let m = meter(&f);
    assert!(m.contains(" compose=fences phases=tools:7,fences:7,covered-build:4,covered:3"), "{m}");
    assert!(m.contains(" covered=7 covered_deferred=0"), "{m}");
    assert!(f.tsd.borrow().iter().any(|r| r.contains("\"covered_secs\":7") && r.contains("\"covered_deferred\":0")), "{:?}", f.tsd.borrow());
}

#[test_id(tonbu0b)]
#[test]
fn a_covered_fences_pass_is_never_reused_for_a_gate_with_no_bead() {
    let key = |f: &Fake| f.written.borrow().iter().find(|(p, _)| p.starts_with("run/verdicts") && !p.to_string_lossy().contains("/trees/")).map(|(p, _)| p.clone());
    let with = fences_fake(&["docs/x.md"]);
    assert_eq!(with.run(), PASS, "{}", with.stderr());
    let without = fences_fake(&["docs/x.md"]);
    without.put_var("SPIRA_GATE_BEAD", "");
    assert_eq!(without.run(), PASS, "{}", without.stderr());
    let (a, b) = (key(&with).expect("a PASS is cached"), key(&without).expect("a PASS is cached"));
    assert!(a != b, "{a:?}");
}

#[test_id(tyae1md)]
#[test]
fn a_fences_mode_gate_with_no_bead_runs_no_covered_phase_and_takes_no_slot() {
    let f = fences_fake(&["docs/x.md"]);
    f.put_var("SPIRA_GATE_BEAD", "");
    f.admission_free.set(false);
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert_eq!(f.cmds.borrow().len(), 2);
    assert!(!meter(&f).contains("covered"), "{}", meter(&f));
    // Control: the same gate with a bead waits for a slot.
    let g = fences_fake(&["docs/x.md"]);
    g.admission_free.set(false);
    g.put_var("SPIRA_GATE_LOCK_WAIT", "5");
    assert_eq!(g.run(), NOVERDICT);
    assert!(g.verdict_line().contains("reason=admission-timeout"), "{}", g.verdict_line());
}

#[test_id(tkgtdol)]
#[test]
fn what_the_covered_phase_deferred_is_counted_as_a_set_of_suites_and_recorded_by_name() {
    let f = fences_fake(&["docs/x.md"]);
    f.covered_runs.borrow_mut().insert(
        MERGE_SHA.into(),
        (
            0,
            "suite-select budget: dropped test-x.sh (tier=T1 predicted=92.000s from p90 — over budget)\nsuite-select budget: dropped test-x.sh (tier=T1 predicted=92.000s from p90 — over budget)\n  test-y.sh                        DEFERRED deadline\n  test-a.sh   ok      3s\nVERDICT GREEN ran=1"
                .into(),
        ),
    );
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(meter(&f).contains(" covered_deferred=2"), "{}", meter(&f));
    assert!(f.tsd.borrow().iter().any(|r| r.contains("\"covered_deferred_suites\":\"test-x.sh,test-y.sh\"")), "{:?}", f.tsd.borrow());
}

#[test_id(tn656j1)]
#[test]
fn a_covered_phase_red_is_the_branchs_only_when_the_base_is_not_red_on_it_too() {
    let red = "  test-b.sh   RED     rc=1 after 4s\nVERDICT RED ran=1 red=1";
    let f = fences_fake(&["docs/x.md"]);
    f.covered_runs.borrow_mut().insert(MERGE_SHA.into(), (1, red.into()));
    assert_eq!(f.run(), FAIL, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=branch-red") && f.verdict_line().contains("suite=test-b.sh"), "{}", f.verdict_line());

    let g = fences_fake(&["docs/x.md"]);
    g.covered_runs.borrow_mut().insert(MERGE_SHA.into(), (1, red.into()));
    g.reruns.borrow_mut().insert((BASE.into(), "test-b.sh".into()), (1, "  test-b.sh   RED     rc=1 after 4s".into()));
    assert_eq!(g.run(), PASS, "{}", g.stderr());
    assert!(g.stderr().contains("inherited, not judged"), "{}", g.stderr());
}

#[test_id(t6lc120)]
#[test]
fn a_named_suite_the_covered_phase_showed_ok_is_not_run_again_by_reentry() {
    let f = returned(fences_fake(&["docs/x.md"]), "test-b.sh");
    f.covered_runs.borrow_mut().insert(MERGE_SHA.into(), (0, "  test-b.sh   ok      3s\nVERDICT GREEN ran=1".into()));
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(!meter(&f).contains("reentry:"), "{}", meter(&f));
    assert!(!f.cmds.borrow().iter().any(|c| c.contains("release config-stage")), "{:?}", f.cmds.borrow());
    // Control: a covered phase that did not show it ok leaves it to the re-entry phase.
    let g = returned(fences_fake(&["docs/x.md"]), "test-b.sh");
    assert_eq!(g.run(), PASS, "{}", g.stderr());
    assert!(meter(&g).contains("reentry:"), "{}", meter(&g));
}

#[test_id(tmryito)]
#[test]
fn a_fences_mode_column_repository_stays_fences_only_and_says_so() {
    let f = Fake::new();
    *f.mode.borrow_mut() = Ok(GateMode::Fences);
    f.put_var("SPIRA_GATE_BEAD", "sp-a");
    f.put_var("SPIRA_GATE_SUITES", "off");
    f.admission_free.set(false);
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert_eq!(f.env_of(0, "SPIRA_GATE_COVERED"), "");
    assert!(f.stderr().contains("runs its fences only"), "{}", f.stderr());
}

// covers: UC-gate-verdict-22
#[test_id(tulg020)]
#[test]
fn every_verdict_is_recorded_with_its_branch_suite_and_tree_and_a_failing_recorder_changes_nothing() {
    let f = Fake::new();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, "test-a.sh ok\ntest-b.sh RED".into()));
    assert_eq!(f.run(), FAIL);
    assert_eq!(f.yields.borrow()[0], [RUN, "record", "spira", BR, "1", "branch-red", "tree-of-spira/sp-a", "test-b.sh", ""], "a red names branch, reason, tree and suite");

    let f = tree_owned(Some("garbage\n"), Some("garbage\n"));
    assert_eq!(f.run(), BASEFAIL);
    assert_eq!(f.yields.borrow()[0][1..6], ["record", "spira", BR, "76", "base-gate-definition"], "a base fault is recorded as 76, which yield.sh files as a gate fault");

    let f = Fake::new();
    assert_eq!(f.run(), PASS);
    assert_eq!(f.yields.borrow()[0][1..4], ["pass", "spira", BR]);

    let t = testkit::TempDir::new("gate-yield-port");
    let (recorder, seen) = (t.path().join("yield.sh"), t.path().join("seen"));
    std::fs::write(&recorder, format!("printf '%s %s' \"$SPIRA_RUN\" \"$*\" > {}\nexit 9\n", seen.display())).unwrap();
    crate::ports::World::yield_sh(&crate::real::Real::new(t.path().into()), &recorder, "run-dir", &["record", "spira", BR]);
    assert_eq!(std::fs::read_to_string(&seen).unwrap(), format!("run-dir record spira {BR}"), "the recorder ran with the gate's run directory, and its failure came back as nothing");
}

#[test_id(tjlwivt)]
#[test]
fn a_harness_fault_record_carries_the_runners_words() {
    let f = fences_fake(&["spira/test-c.sh"]);
    *f.diff.borrow_mut() = Ok("spira/test-c.sh".into());
    f.files.borrow_mut().insert(PathBuf::from(format!("{GATE_TREE}/spira/test-c.sh")), "#!".into());
    f.reentry_runs.borrow_mut().insert(MERGE_SHA.into(), (75, "batch: SPIRA_X is not declared; refusing\nVERDICT FAULT rc=2 ran=0 reason=settings-refused".into()));
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    let y = f.yields.borrow().iter().find(|y| y.get(1).map(String::as_str) == Some("record")).cloned().expect("a record");
    let detail = y.last().unwrap();
    assert!(detail.contains("SPIRA_X is not declared") && !detail.contains('\n'), "{y:?}");
}

#[test_id(tuadvkc)]
#[test]
fn fences_mode_is_its_own_verdict_key_and_gate_all_runs_the_string_whole() {
    let key = |f: &Fake| f.written.borrow().iter().find(|(p, _)| p.starts_with("run/verdicts") && !p.to_string_lossy().contains("/trees/")).map(|(p, _)| p.clone());
    let mut keys = Vec::new();
    for m in [GateMode::Fences, GateMode::Unit, GateMode::Suites] {
        let f = tree_owned(Some(STEPS), Some(STEPS));
        *f.mode.borrow_mut() = Ok(m);
        *f.touched.borrow_mut() = Ok(vec![Changed { path: "gate/src/x.rs".into(), exec: false }]);
        assert_eq!(f.run(), PASS, "{m:?}: {}", f.stderr());
        keys.push(key(&f).expect("a PASS is cached"));
    }
    assert!(keys[0] != keys[1] && keys[0] != keys[2], "{keys:?}");
    // SPIRA_GATE_ALL=1 asks for the whole sequence, in any mode.
    const WITH_SUITES: &str = "bin SPIRA_LINT_BIN spira-lint\nbin SPIRA_SELECT_BIN suite-select\nstep \"$SPIRA_LINT_BIN\"\nstep { \"$SPIRA_SELECT_BIN\" gate x; }\n";
    let f = tree_owned(Some(WITH_SUITES), Some(WITH_SUITES));
    *f.mode.borrow_mut() = Ok(GateMode::Fences);
    f.put_var("SPIRA_GATE_ALL", "1");
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(gate_runs(&f)[0].contains("SPIRA_SELECT_BIN"), "{:?}", gate_runs(&f));
    assert!(meter(&f).contains("compose=suites(gate-all)"), "{}", meter(&f));
}

#[test_id(th6zjdb)]
#[test]
fn a_gate_records_its_start_for_the_bead_once_it_holds_the_tree() {
    let f = Fake::new();
    f.put_var("SPIRA_GATE_BEAD", "sp-a");
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (0, "test-a.sh ok".into()));
    f.runs.borrow_mut().insert(BASE.into(), (0, String::new()));
    f.run();
    assert_eq!(*f.gate_starts.borrow(), ["sp-a".to_string()]);

    let g = Fake::new();
    g.runs.borrow_mut().insert(MERGE_SHA.into(), (0, "test-a.sh ok".into()));
    g.runs.borrow_mut().insert(BASE.into(), (0, String::new()));
    g.run();
    assert!(g.gate_starts.borrow().is_empty(), "a gate with no bead has no fact to write");
}

fn builder_fake() -> Fake {
    let f = unit_fake(&["spira-config/src/lib.rs"]);
    f.put_var("SPIRA_GATE_BEAD", "sp-a");
    *f.build_dir_fact.borrow_mut() = Some(("abc123".into(), PathBuf::from("pool/dir.4")));
    f
}

#[test_id(tqu9q74)]
#[test]
fn a_unit_gate_builds_in_the_directory_its_beads_builder_submitted_from() {
    let f = builder_fake();
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    let asked = f.builders.borrow();
    let b = asked[0].as_ref().expect("the build tree was prepared for the builder's directory");
    assert_eq!((b.bead.as_str(), b.tip.as_str(), b.dir.as_path()), ("sp-a", "abc123", Path::new("pool/dir.4")));
    assert_eq!(b.wait, std::time::Duration::from_secs(120));
    assert!(b.path.contains("box/.cargo/bin"), "{}", b.path);
    assert!(f.stderr().contains("gate: build in pool/dir.4 (leased from sp-a's builder at abc123)"), "{}", f.stderr());
    assert!(meter(&f).contains(" build_dir=builder "), "{}", meter(&f));
    assert_eq!(tsd_row(&f)["build_dir"], "builder");
}

#[test_id(taqwu5f)]
#[test]
fn a_unit_gate_whose_bead_has_no_build_dir_fact_takes_the_warm_or_cold_path() {
    let f = builder_fake();
    *f.build_dir_fact.borrow_mut() = None;
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert_eq!(f.builders.borrow()[0], None);
    assert!(meter(&f).contains(" build_dir=cold "), "{}", meter(&f));

    let w = builder_fake();
    *w.build_dir_fact.borrow_mut() = None;
    *w.warm_line.borrow_mut() = Some("c0ffee (3 unchanged files aged)".into());
    assert_eq!(w.run(), PASS);
    assert!(meter(&w).contains(" build_dir=warm "), "{}", meter(&w));
}

#[test_id(tgaaa1a)]
#[test]
fn a_builders_directory_that_cannot_be_leased_cleaned_or_aged_leaves_the_gate_on_its_own_directory() {
    for (warm, kind) in [(None, "cold"), (Some("c0ffee (3 unchanged files aged)".to_string()), "warm")] {
        let f = builder_fake();
        *f.builder_unusable.borrow_mut() = Some("it is gone, held, or not a pool directory".into());
        *f.warm_line.borrow_mut() = warm;
        assert_eq!(f.run(), PASS, "{}", f.stderr());
        assert!(f.builders.borrow()[0].is_some(), "the gate tried the builder's directory first");
        assert!(meter(&f).contains(&format!(" build_dir={kind} ")), "{}", meter(&f));
        assert!(f.stderr().contains("builder directory pool/dir.4 not used"), "{}", f.stderr());
    }
}

#[test_id(t2aanyy)]
#[test]
fn a_gate_with_no_lease_wait_configured_does_not_look_for_the_builders_directory() {
    let f = builder_fake();
    f.put_var("SPIRA_GATE_BUILDER_DIR_WAIT_SECS", "");
    assert_eq!(f.run(), PASS);
    assert_eq!(f.builders.borrow()[0], None);
}

const REENTRY_PHASES: &str = "  test-b.sh   ok   3s\nbatch: phases: resolve:0,build:290,up:1,suites:8 (setup 291s, warm: cold)\nVERDICT GREEN";

#[test_id(t8clq9n)]
#[test]
fn a_reentry_phase_is_charged_for_its_suites_not_its_container_build() {
    let f = returned(unit_fake(&["gate/src/x.rs"]), "test-b.sh");
    f.phase_secs.set(53);
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (0, "build-fence: make build ok".into()));
    f.reentry_runs.borrow_mut().insert(MERGE_SHA.into(), (0, REENTRY_PHASES.into()));
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    let line = meter(&f);
    assert!(line.contains("test_clock=61s"), "{line}");
    assert!(line.contains("reentry-build:45,reentry:8"), "{line}");
}

#[test_id(t0am1gx)]
#[test]
fn a_reentry_whose_suites_run_past_the_deadline_is_still_a_deadline() {
    let f = returned(unit_fake(&["gate/src/x.rs"]), "test-b.sh");
    f.phase_secs.set(53);
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (0, "build-fence: make build ok".into()));
    let slow = REENTRY_PHASES.replace("suites:8", "suites:400");
    f.reentry_runs.borrow_mut().insert(MERGE_SHA.into(), (0, slow));
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=deadline"), "{}", f.verdict_line());
}

#[test_id(t7s450c)]
#[test]
fn a_deadline_record_carries_the_tail_of_the_phase_output() {
    let f = returned(unit_fake(&["gate/src/x.rs"]), "test-b.sh");
    f.phase_secs.set(53);
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (0, "build-fence: make build ok".into()));
    let slow = REENTRY_PHASES.replace("suites:8", "suites:400").replace("test-b.sh   ok", "test-b.sh   RUNNING-WHEN-CUT");
    f.reentry_runs.borrow_mut().insert(MERGE_SHA.into(), (0, slow));
    assert_eq!(f.run(), NOVERDICT);
    let y = f.yields.borrow().iter().find(|y| y.get(1).map(String::as_str) == Some("record")).cloned().expect("a record");
    let detail = y.last().unwrap();
    assert!(detail.contains("RUNNING-WHEN-CUT"), "{y:?}");
}

#[test_id(tdg0ljk)]
#[test]
fn a_reentry_with_no_phases_report_is_charged_its_whole_wall() {
    let f = returned(unit_fake(&["gate/src/x.rs"]), "test-b.sh");
    f.phase_secs.set(53);
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (0, "build-fence: make build ok".into()));
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(meter(&f).contains("test_clock=106s"), "{}", meter(&f));
}

#[test_id(t9md1co)]
#[test]
fn a_deadline_record_keeps_only_the_last_4000_bytes() {
    let f = returned(unit_fake(&["gate/src/x.rs"]), "test-b.sh");
    f.phase_secs.set(53);
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (0, "build-fence: make build ok".into()));
    let long = format!("HEAD-MARK\n{}\n{}", "x".repeat(6000), REENTRY_PHASES.replace("suites:8", "suites:400"));
    f.reentry_runs.borrow_mut().insert(MERGE_SHA.into(), (0, long));
    assert_eq!(f.run(), NOVERDICT);
    let y = f.yields.borrow().iter().find(|y| y.get(1).map(String::as_str) == Some("record")).cloned().expect("a record");
    let detail = y.last().unwrap();
    assert!(detail.contains("suites:400") && !detail.contains("HEAD-MARK") && detail.len() <= 4100, "{}", detail.len());
}

/// The configured cap on the gate's target root also bounds the warm build directory.
#[test_id(tfhb3ds)]
#[test]
fn the_configured_target_cap_reaches_the_release_of_the_gate_tree() {
    let f = tree_owned(Some(STEPS), Some(STEPS));
    f.put_var("SPIRA_GATE_TARGET_CAP_MIB", "7777");
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert_eq!(f.release_caps.borrow().as_slice(), &[7777]);
}

#[test_id(txb2k3l)]
#[test]
fn a_unit_gate_whose_nextest_run_printed_no_summary_is_a_harness_fault_not_branch_red() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.unit_runs.borrow_mut().insert((MERGE_SHA.into(), "test"), (1, "bwrap: execvp cargo: No such file or directory\n".into()));
    assert_eq!(f.run(), NOVERDICT);
    assert!(f.verdict_line().contains("reason=harness-fault"), "{}", f.verdict_line());
    assert!(f.stderr().contains("printed no `Summary [` line"), "{}", f.stderr());
    assert!(!f.stderr().contains("charged to this branch"));
    assert!(!f.stderr().contains("reason=branch-red"));

    let g = unit_fake(&["gate/src/x.rs"]);
    g.unit_runs.borrow_mut().insert(
        (MERGE_SHA.into(), "test"),
        (101, "        FAIL [   0.010s] (1/3) gate tests::x\n     Summary [   1.000s] 3 tests run: 2 passed, 1 failed, 0 skipped\n".into()),
    );
    assert_eq!(g.run(), FAIL);
    assert!(g.verdict_line().contains("reason=branch-red"), "{}", g.verdict_line());
    assert!(g.stderr().contains("tests::x"));
    assert!(!g.stderr().contains("harness-fault"), "{}", g.stderr());
}

#[test_id(tbbvdtr)]
#[test]
fn a_unit_run_cut_at_its_deadline_without_a_summary_is_not_the_missing_report_fault() {
    let f = unit_fake(&["gate/src/x.rs"]);
    f.unit_runs.borrow_mut().insert((MERGE_SHA.into(), "test"), (124, String::new()));
    f.run();
    assert!(!f.stderr().contains("printed no `Summary [` line"), "{}", f.stderr());
}

// ------------------------------------------------- the lint step reads the tree's config

/// A tree whose fixture config says `mode`, holding `files`, with the tree's own
/// `spira/lint-staged.sh`; fake `release` and `lint` stand in for the built tools.
fn lint_fixture(mode: &str, files: &[(&str, &str)]) -> (testkit::TempDir, std::path::PathBuf) {
    let t = testkit::TempDir::new("gate-lint-cfg");
    let w = |rel: &str, body: &str| {
        let p = t.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    w("spira/testlib/complete.toml", &format!("[spira]\nffx_vocabulary_mode = \"{mode}\"\n"));
    let root = spira_config::resolve::checkout_root_for_process().unwrap();
    w("spira/lint-staged.sh", &std::fs::read_to_string(root.join("spira/lint-staged.sh")).unwrap());
    for (p, b) in files {
        w(p, b);
    }
    let bin = t.path().join("fakebin");
    std::fs::create_dir_all(&bin).unwrap();
    testkit::write_exe(bin.join("release"), "#!/bin/bash\n[ \"$1\" = config-stage ] && echo \"$SPIRA_TOML\"\n");
    testkit::write_exe(
        bin.join("lint"),
        concat!(
            "#!/bin/bash\nif grep -q 'ffx_vocabulary_mode = \"enforce\"' \"$SPIRA_TOML\" && grep -rq aeon rule; then echo 'ffx-vocabulary: rule/tests/vocab.rs:16: the term \"aeon\"'; exit 1; fi\n", // ffx-ok
            "if [ \"$1\" = --deny-list ]; then\n",
            "  [ -n \"$2\" ] || { echo 'spira-lint: --deny-list names no file' >&2; exit 2; }\n",
            "  echo \"fence: test-id-row-orphan checked 1 registry ($2: 1 rows, 1 test rows)\"\n",
            "else echo 'fence: test-id-row-orphan checked 0 registry (none configured)'; fi\n",
        ),
    );
    (t, bin)
}

fn run_lint_step(t: &testkit::TempDir, bin: &std::path::Path, caller_mode: &str) -> (bool, String) {
    let deny = t.path().join("target/gate-deny-list");
    std::fs::create_dir_all(deny.parent().unwrap()).unwrap();
    std::fs::write(&deny, "disabled\ttest:t0aaaaa\tsp-1\t-\n").unwrap();
    run_lint_step_with_deny(t, bin, caller_mode, Some(deny.to_str().unwrap()))
}

fn run_lint_step_with_deny(t: &testkit::TempDir, bin: &std::path::Path, caller_mode: &str, deny: Option<&str>) -> (bool, String) {
    let steps = checked_in_steps();
    let def = crate::def::parse(&steps).unwrap();
    let step = def.steps.iter().find(|s| s.contains("SPIRA_LINT_BIN")).expect("a spira-lint step");
    let caller = t.path().join("caller.toml");
    std::fs::write(&caller, format!("[spira]\nffx_vocabulary_mode = \"{caller_mode}\"\n")).unwrap();
    let mut cmd = std::process::Command::new(system_paths::BIN_BASH);
    cmd.args(["-c", step])
        .current_dir(t.path())
        .env_clear()
        .env("PATH", format!("{}:{}", bin.display(), system_paths::SYSTEM_PATH_MIN))
        .env("SPIRA_LINT_BIN", bin.join("lint"))
        .env("SPIRA_TOML", &caller);
    if let Some(d) = deny {
        cmd.env("SPIRA_TEST_DENY_LIST", d);
    }
    let out = cmd.output().unwrap();
    (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

#[test_id(t130o27)]
#[test]
fn the_lint_step_enforces_the_trees_ffx_mode_whatever_the_callers_config_says() {
    let (t, bin) = lint_fixture("enforce", &[("rule/tests/vocab.rs", "// the aeon // ffx-ok\n")]);
    let (ok, out) = run_lint_step(&t, &bin, "report");
    assert!(!ok && out.contains("ffx-vocabulary"), "{out}");
}

#[test_id(t7xka2e)]
#[test]
fn the_lint_step_passes_a_tree_with_no_ffx_term() {
    let (t, bin) = lint_fixture("enforce", &[("rule/tests/vocab.rs", "// nothing here\n")]);
    let (ok, out) = run_lint_step(&t, &bin, "report");
    assert!(ok, "{out}");
}

#[test_id(tj7ga9w)]
#[test]
fn the_lint_step_hands_spira_lint_the_gates_deny_list_file() {
    let (t, bin) = lint_fixture("enforce", &[("rule/tests/vocab.rs", "// nothing here\n")]);
    let deny = t.path().join("target/gate-deny-list");
    let (ok, out) = run_lint_step(&t, &bin, "report");
    assert!(ok, "{out}");
    assert!(out.contains(&format!("checked 1 registry ({}", deny.display())), "{out}");
}

#[test_id(tziw3qq)]
#[test]
fn the_lint_step_refuses_when_the_gate_handed_no_deny_list() {
    let (t, bin) = lint_fixture("enforce", &[("rule/tests/vocab.rs", "// nothing here\n")]);
    for deny in [None, Some("")] {
        let (ok, out) = run_lint_step_with_deny(&t, &bin, "report", deny);
        assert!(!ok && out.contains("--deny-list"), "{deny:?}: {out}");
    }
}

// ------------------------------------------------------------ a declared CI gates a queue.local repository (sp-euuj38)

const DECLARED_CI: &str = "ci_entry = \"bash gate.sh\"\ndefault = [\"gate\"]\n[tier.gate]\ncmd = \"x\"\n";

/// A `queue.local` repository with an empty column whose landing ref and branch both declare a CI.
fn testenv_gated(decl: &str) -> Fake {
    let f = Fake::new();
    {
        let mut c = f.ctx.borrow_mut();
        let c = c.as_mut().unwrap();
        c.gate_cmd = String::new();
        c.land = "queue.local".into();
    }
    for rev in [BASE, MERGE_SHA] {
        f.blobs.borrow_mut().insert(format!("{rev}:.spira/testenv.toml"), decl.as_bytes().to_vec());
        f.gone.borrow_mut().insert(format!("{rev}:spira/testenv/Containerfile"));
    }
    f
}

fn testenv_calls(f: &Fake) -> Vec<String> {
    f.cmds.borrow().iter().filter(|c| c.contains("testenv")).cloned().collect()
}

#[test_id(tp3mdys)]
#[test]
fn an_empty_column_with_a_declared_ci_runs_its_default_tiers_not_syntax_only() {
    let f = testenv_gated(DECLARED_CI);
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    let line = f.full_verdict_line();
    assert!(line.contains("reason=pass") && line.ends_with(" ran=testenv:gate"), "{line}");
    let calls = testenv_calls(&f);
    assert_eq!(calls.len(), 2, "the gate phase, then the uncharged publish: {calls:?}");
    assert!(!calls[0].contains("--suites") && calls[0].contains("--deadline 300 "), "{}", calls[0]);
    assert!(meter(&f).contains(" phases=gate:7,publish:0"), "{}", meter(&f));
}

#[test_id(tf6qwhn)]
#[test]
fn a_declared_build_tier_runs_first_and_the_verdict_line_names_the_tiers_only() {
    let f = testenv_gated(&format!("{DECLARED_CI}[tier.build]\ncmd = \"y\"\n"));
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(testenv_calls(&f)[0].contains("--suites build"), "{:?}", testenv_calls(&f));
    assert!(f.full_verdict_line().ends_with(" ran=testenv:gate"), "{}", f.verdict_line());
}

#[test_id(tk7vcxe)]
#[test]
fn a_red_tier_green_on_the_base_is_the_branchs_and_the_base_runs_testenv() {
    let f = testenv_gated(DECLARED_CI);
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, String::new()));
    f.tier_events.borrow_mut().insert(MERGE_SHA.into(), "1 suite gate RED\n".into());
    f.tier_events.borrow_mut().insert(BASE.into(), "1 suite gate ok\n".into());
    assert_eq!(f.run(), FAIL, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=branch-red"), "{}", f.verdict_line());
    let calls = testenv_calls(&f);
    let base = calls.last().unwrap();
    assert!(base.contains("--deadline 300 ") && !base.contains("--suites") && !base.contains("--share"), "{base}");
    assert_eq!(f.checkouts.borrow().last().map(String::as_str), Some(BASE));
    assert!(meter(&f).contains(",base-gate:"), "{}", meter(&f));
}

#[test_id(tn2yzub)]
#[test]
fn a_tier_red_on_the_base_too_is_base_fail() {
    let f = testenv_gated(DECLARED_CI);
    for rev in [MERGE_SHA, BASE] {
        f.runs.borrow_mut().insert(rev.into(), (1, String::new()));
        f.tier_events.borrow_mut().insert(rev.into(), "1 suite gate RED\n".into());
    }
    assert_eq!(f.run(), BASEFAIL, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=base-red"), "{}", f.verdict_line());
    assert!(testenv_calls(&f).last().unwrap().contains("testenv"));
}

#[test_id(tz9kxra)]
#[test]
fn a_red_where_the_landing_ref_declares_no_ci_is_the_branchs_own() {
    let f = testenv_gated(DECLARED_CI);
    f.blobs.borrow_mut().remove(&format!("{BASE}:.spira/testenv.toml"));
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, String::new()));
    f.tier_events.borrow_mut().insert(MERGE_SHA.into(), "1 suite gate RED\n".into());
    assert_eq!(f.run(), FAIL, "{}", f.stderr());
    assert_eq!(testenv_calls(&f).len(), 1, "no base trial: {:?}", testenv_calls(&f));
}

#[test_id(tx5ldcv)]
#[test]
fn a_tier_killed_at_the_cap_is_no_verdict_and_certifies_nothing() {
    let f = testenv_gated(DECLARED_CI);
    f.put_var("SPIRA_GATE_DEADLINE", "900");
    f.phase_secs.set(300);
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (124, String::new()));
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=phase-cap"), "{}", f.verdict_line());
    assert!(f.written.borrow().iter().all(|(_, c)| !c.contains("compose=")), "{:?}", f.written.borrow());
    let green = testenv_gated(DECLARED_CI);
    assert_eq!(green.run(), PASS);
    assert!(green.written.borrow().iter().any(|(_, c)| c.contains("compose=")), "a PASS writes the verdict entry this test looks for");
}

#[test_id(tza89fu)]
#[test]
fn a_base_whose_declaration_does_not_read_leaves_the_red_unjudged() {
    let f = testenv_gated(DECLARED_CI);
    f.blobs.borrow_mut().insert(format!("{BASE}:.spira/testenv.toml"), b"ci_entry = [".to_vec());
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, String::new()));
    f.tier_events.borrow_mut().insert(MERGE_SHA.into(), "1 suite gate RED\n".into());
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=base-untestable"), "{}", f.verdict_line());
}

#[test_id(tqup6qx)]
#[test]
fn a_red_tier_the_landing_refs_declaration_lacks_is_the_branchs_own() {
    let f = testenv_gated(DECLARED_CI);
    let widened = "ci_entry = \"bash gate.sh\"\ndefault = [\"gate\", \"extra\"]\n[tier.gate]\ncmd = \"x\"\n[tier.extra]\ncmd = \"y\"\n";
    f.blobs.borrow_mut().insert(format!("{MERGE_SHA}:.spira/testenv.toml"), widened.as_bytes().to_vec());
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, String::new()));
    f.tier_events.borrow_mut().insert(MERGE_SHA.into(), "1 suite gate ok\n2 suite extra RED\n".into());
    f.runs.borrow_mut().insert(BASE.into(), (1, String::new()));
    f.tier_events.borrow_mut().insert(BASE.into(), "1 suite gate RED\n".into());
    assert_eq!(f.run(), FAIL, "{}", f.stderr());
    assert!(f.verdict_line().ends_with("suite=extra"), "{}", f.verdict_line());
}

#[test_id(tx4xvs0)]
#[test]
fn a_harness_containerfile_keeps_a_queue_local_repository_on_its_column() {
    let f = testenv_gated(DECLARED_CI);
    f.ctx.borrow_mut().as_mut().unwrap().gate_cmd = "bash spira/fence.sh".into();
    f.gone.borrow_mut().clear();
    assert_eq!(f.run(), PASS, "{}", f.stderr());
    assert!(f.full_verdict_line().ends_with(" ran=column"), "{}", f.full_verdict_line());
    assert!(testenv_calls(&f).is_empty(), "{:?}", f.cmds.borrow());
}

#[test_id(t9xinkj)]
#[test]
fn the_definition_of_a_declared_repository_is_the_testenv_call() {
    let f = testenv_gated(DECLARED_CI);
    let d = crate::engine::definition(&f, Some("spira")).unwrap();
    assert!(d.contains("testenv") && !d.contains("--suites") && !d.contains("bash gate.sh"), "{d}");
}

#[test_id(ts4hbwm)]
#[test]
fn a_branch_removing_the_declaration_or_its_ci_entry_fails_the_definition() {
    let f = testenv_gated(DECLARED_CI);
    f.blobs.borrow_mut().remove(&format!("{MERGE_SHA}:.spira/testenv.toml"));
    assert_eq!(f.run(), FAIL, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=gate-definition"), "{}", f.verdict_line());
    let g = testenv_gated(DECLARED_CI);
    g.blobs.borrow_mut().insert(format!("{MERGE_SHA}:.spira/testenv.toml"), b"default = [\"gate\"]\n[tier.gate]\ncmd = \"x\"\n".to_vec());
    assert_eq!(g.run(), FAIL, "{}", g.stderr());
    assert!(g.verdict_line().contains("reason=gate-definition"), "{}", g.verdict_line());
    assert!(testenv_calls(&g).is_empty());
}

#[test_id(tg1ujpe)]
#[test]
fn a_column_beside_a_declaration_is_never_run_by_the_trial_or_its_base() {
    let f = testenv_gated(DECLARED_CI);
    f.ctx.borrow_mut().as_mut().unwrap().gate_cmd = "bash gate.sh".into();
    f.runs.borrow_mut().insert(MERGE_SHA.into(), (1, String::new()));
    f.tier_events.borrow_mut().insert(MERGE_SHA.into(), "1 suite gate RED\n".into());
    assert_eq!(f.run(), FAIL, "{}", f.stderr());
    assert!(f.cmds.borrow().iter().all(|c| !c.contains("bash gate.sh")), "{:?}", f.cmds.borrow());
    assert!(f.cmds.borrow().len() >= 2, "the base ran too: {:?}", f.cmds.borrow());
}

#[test_id(tr8wnaq)]
#[test]
fn a_declaration_does_not_gate_a_forge_repository() {
    let f = testenv_gated(DECLARED_CI);
    f.ctx.borrow_mut().as_mut().unwrap().land = "queue".into();
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=certifies-nothing") && f.full_verdict_line().ends_with(" ran=syntax"), "{}", f.verdict_line());
    assert!(testenv_calls(&f).is_empty());
}

// ------------------------------------------------------------ a declared test tier is not certified on syntax alone

const DECLARED_NOT_RUN: &str = "default = [\"gate\"]\n[tier.gate]\ncmd = \"x\"\n";

/// A `queue.forge` repository with an empty column whose landing ref and branch both declare tiers.
fn forge_declaring() -> Fake {
    let f = hex_fake();
    {
        let mut c = f.ctx.borrow_mut();
        let c = c.as_mut().unwrap();
        c.gate_cmd = String::new();
        c.land = "queue.forge".into();
    }
    for rev in [BASE, MERGE_SHA] {
        f.blobs.borrow_mut().insert(format!("{rev}:.spira/testenv.toml"), DECLARED_NOT_RUN.as_bytes().to_vec());
    }
    f
}

#[test_id(tlgsslq)]
#[test]
fn a_repository_that_declares_tiers_its_gate_does_not_run_is_not_certified_on_syntax() {
    let f = forge_declaring();
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    let line = f.verdict_line();
    assert!(line.contains(&format!("reason={}", spira_config::gate_source::CERTIFIES_NOTHING)), "{line}");
    assert!(f.stderr().contains(".spira/testenv.toml") && f.stderr().contains("ci_entry") && f.stderr().contains("gate.steps"), "{}", f.stderr());
    assert!(cert_written(&f).is_none());
}

#[test_id(tmw2obm)]
#[test]
fn a_declaring_repository_whose_changed_script_fails_bash_n_still_fails_on_syntax() {
    let f = forge_declaring();
    f.blobs.borrow_mut().insert(format!("{MERGE_SHA}:spira/x.sh"), b"if then".to_vec());
    f.bash_n_bad.borrow_mut().insert(b"if then".to_vec());
    assert_eq!(f.run(), FAIL);
    assert!(f.verdict_line().contains("reason=syntax"), "{}", f.verdict_line());
}

#[test_id(t3ucmpf)]
#[test]
fn a_branch_that_deletes_the_declaration_the_landing_ref_has_is_not_certified_on_syntax() {
    let f = forge_declaring();
    f.blobs.borrow_mut().remove(&format!("{MERGE_SHA}:.spira/testenv.toml"));
    assert_eq!(f.run(), NOVERDICT, "{}", f.stderr());
    assert!(f.verdict_line().contains("reason=certifies-nothing"), "{}", f.verdict_line());
    assert!(cert_written(&f).is_none());
}
