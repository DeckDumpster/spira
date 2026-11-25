//! The gate's boundaries. `real` implements them against git, the helper scripts, lib.sh and
//! the filesystem; the unit tests drive the engine through a recording fake.

use crate::compose::Changed;
use spira_config::GateMode;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// What one `. lib.sh` told us (DESIGN.md "Environment it reads").
#[derive(Clone, Debug, Default)]
pub struct Ctx {
    pub repo_name: String,
    pub vars: HashMap<String, String>,
    /// `repo_root <name>` — None when it refused.
    pub repo_root: Option<String>,
    /// `spira_landref <repo-path>` — None when it refused.
    pub landref: Option<String>,
    /// The base the registry declares for the repository; empty when none.
    pub declared_base: String,
    /// `repo_gate <name>`.
    pub gate_cmd: String,
    pub host_cores: String,
    /// `repo_land <name>`: how the repository's work lands (`queue.local`, `queue`, `push`, …).
    pub land: String,
}

impl Ctx {
    /// A variable as `${X:-}` reads it.
    pub fn var(&self, k: &str) -> &str {
        self.vars.get(k).map(String::as_str).unwrap_or("")
    }
    /// `${X:-default}`.
    pub fn var_or<'a>(&'a self, k: &str, d: &'a str) -> &'a str {
        let v = self.var(k);
        if v.is_empty() {
            d
        } else {
            v
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Merge {
    /// The merged tree's id.
    Clean(String),
    /// The conflicted paths.
    Conflict(Vec<String>),
    Failed(String),
}

/// What `target_on_tmpfs` did: the lines for the trial's stderr, and how the directory was filled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetOutcome {
    pub line: String,
    pub build_dir: crate::telemetry::BuildDir,
}

/// The build directory a bead's builder submitted from, which a unit gate builds in when it can lease it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Builder {
    pub bead: String,
    pub tip: String,
    pub dir: PathBuf,
    pub wait: std::time::Duration,
    /// The PATH cargo is found on, for the selective clean.
    pub path: String,
}

/// `test:<id>` for a test whose name ends in a test id, read from the last `::` segment of its last word.
pub fn flip_test_id(test: &str) -> Option<String> {
    let id = test.split_whitespace().next_back()?.rsplit("::").next()?;
    let shaped = id.len() == 7 && id.starts_with('t') && id[1..].bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase());
    shaped.then(|| format!("test:{id}"))
}

/// The incident reference of a flipping test: its id when it carries one, so a rename files no second incident.
pub fn flip_reference(repo: &str, test: &str) -> String {
    format!("test-flip:{repo}:{}", flip_test_id(test).as_deref().unwrap_or(test))
}

pub trait World {
    /// `. lib.sh` once, then the values the trial needs. Err when lib.sh would not source.
    fn context(&self, repo_name: Option<&str>) -> Result<Ctx, String>;

    /// `test-registry deny-list --at <at>`: the tests the trial leaves out, as the registry
    /// prints them. Err is the registry's message; a trial never runs with a list it could
    /// not resolve.
    fn deny_list(&self, at: u64) -> Result<String, String>;

    // ---- files
    fn readable(&self, p: &Path) -> bool;
    /// `command -v <name>`: a harness script found on the gate's PATH (the release's
    /// `spira/`, sp-gypjk) — never joined under SPIRA_HOME. None = not on PATH.
    fn which(&self, name: &str) -> Option<std::path::PathBuf>;
    fn exists(&self, p: &Path) -> bool;
    fn read(&self, p: &Path) -> Option<String>;
    fn mkdir_p(&self, p: &Path);
    /// Empty `p` and leave it an existing, empty directory.
    fn reset_dir(&self, p: &Path) -> Result<(), String>;
    /// Remove `p` and everything under it; absent is fine.
    fn remove_dir_all(&self, p: &Path);
    /// Write via `.<name>.<pid>` and rename.
    fn write_atomic(&self, dir: &Path, name: &str, content: &str);
    fn append(&self, p: &Path, line: &str);
    fn remove(&self, p: &Path);
    /// The file names directly under `dir`; empty when it cannot be read.
    fn list_dir(&self, dir: &Path) -> Vec<String>;
    /// A fresh temporary file holding `content`.
    fn temp_file(&self, content: &str) -> Option<PathBuf>;

    // ---- git
    fn rev_parse(&self, repo: &Path, rev: &str) -> Option<String>;
    /// `diff --name-only <range>` → Ok(stdout, trailing newlines stripped) / Err(combined output).
    fn diff_names(&self, repo: &Path, range: &str) -> Result<String, String>;
    fn diff_name_status(&self, repo: &Path, range: &str) -> String;
    fn merge_tree(&self, repo: &Path, base: &str, branch: &str) -> Merge;
    fn is_ancestor(&self, repo: &Path, a: &str, b: &str) -> bool;
    /// A merge commit of `tree` with parents `base`, `branch`, fixed identity and date.
    fn commit_merge(&self, repo: &Path, tree: &str, base: &str, branch: &str) -> Option<String>;
    fn show_blob(&self, repo: &Path, rev: &str, path: &str) -> Option<Vec<u8>>;
    /// `ls-tree -r --name-only <rev>`.
    fn ls_tree_all(&self, repo: &Path, rev: &str) -> String;
    /// `ls-tree --name-only <rev> <path>` names `path`.
    fn ls_tree_has(&self, repo: &Path, rev: &str, path: &str) -> bool;

    /// `git diff --raw -z --no-renames <base> <rev>`: the touched set (DESIGN.md
    /// "Composition"). Err(output) when git refused.
    fn diff_raw(&self, repo: &Path, base: &str, rev: &str) -> Result<Vec<Changed>, String>;

    // ---- composition (DESIGN.md "Composition")
    /// `[repo.<name>] gate_mode`, read through the spira-config library; Ok(Suites) when no
    /// configuration document is in force, Err(why) when one is and it does not validate.
    fn gate_mode(&self, repo_name: &str) -> Result<GateMode, String>;
    /// `cargo metadata --format-version 1 --no-deps --offline` in `tree` with `path` as PATH →
    /// stdout, or Err(stderr).
    fn cargo_metadata(&self, tree: &Path, path: &str, home: &str) -> Result<String, String>;

    // ---- helpers the gate runs (DESIGN.md "Non-goals": not ported here)
    /// `bash -n` over a script's content → Err(bash's message).
    fn bash_n(&self, content: &[u8]) -> Result<(), String>;
    /// `bash exclude.sh filter` over stdin.
    fn exclude_filter(&self, exclude: &Path, names: &str) -> String;
    /// `skew foreign <repo> <base> <ref>` → (status, combined output).
    fn skew_foreign(&self, skew: &Path, repo: &Path, base: &str, branch: &str) -> (i32, String);
    /// `spira-lc cite-check --repo <repo> --base <base> --branch <ref>` → (status, combined output):
    /// 0 clean, 3 a phantom citation, anything else the check did not run.
    fn commit_cite(&self, spira_lc: &Path, repo: &Path, base: &str, branch: &str) -> (i32, String);
    fn sweep(&self, sweep: &Path, repo: &Path);
    /// `SPIRA_RUN=<run> bash yield.sh <args…>`, output discarded.
    fn yield_sh(&self, yield_sh: &Path, run: &str, args: &[&str]);
    /// Pushes the finished trial's metrics; a world with no metrics store pushes nothing.
    fn push_metrics(&self, _run: &crate::telemetry::GateRun) {}
    /// This process's cgroup counters now; `None` where they cannot be read.
    fn cgroup_mark(&self) -> Option<spira_otel::cgroup::Mark> {
        None
    }
    /// Sends one finished operation to the log store; a world with none sends nothing.
    fn emit_op(&self, _op: &spira_otel::ops::Op) {}
    /// `spira-lc certify <bead> <tip> <outcome> <detail> gate` (the machine reads its switch).
    /// Returns spira-lc's stderr when it refused (a tip the row does not hold), else empty.
    fn lc_certify(&self, bead: &str, tip: &str, outcome: &str, detail: &str, step: &str, message: &str) -> String;
    /// Records the red's failing tests and output tail as the bead's `eject-failure` fact, before its certify.
    fn lc_failure(&self, _bead: &str, _tip: &str, _files: &[String], _output: &str) {}
    /// Keeps `files` as one send-back log entry and references it from each bead's facts.
    fn keep_logs(&self, _beads: &[&str], _files: &[(String, Vec<u8>)]) {}
    /// The `eject-failure` digests recorded since `since` (unix seconds), as (bead, digest).
    fn lc_prior_failures(&self, _since: i64) -> Result<Vec<(String, String)>, String> {
        Err("no lifecycle store".into())
    }
    /// `spira-lc fact add <bead> --kind gate-start`: the gate holds its tree and begins running.
    fn lc_gate_start(&self, bead: &str);
    /// Files the Ops incident asking for the flipping `test` of `repo` to be deleted. Dedups on the test.
    fn file_flip(&self, _repo: &str, _test: &str, _title: &str, _body: &str) -> Result<(), String> {
        Ok(())
    }
    /// Files the fix bead for a test that flipped in this gate and returns its id.
    fn file_flip_fix(&self, _repo: &str, _test: &str, _title: &str, _body: &str) -> Result<String, String> {
        Err("no bead filing in this world".into())
    }
    /// Writes the registry row that disables `test` for the registry's hold, naming `bead`.
    fn registry_disable(&self, _test: &str, _bead: &str, _reason: &str) -> Result<crate::flip::DisabledRow, String> {
        Err("no test registry in this world".into())
    }
    /// sha256 of gate.sh, exclude.sh, the resolved `skew` binary and this binary, concatenated.
    fn harness_hash(&self) -> Option<String>;

    // ---- admission and the tree
    fn nproc_all(&self) -> u64;
    fn mem_avail_mib(&self) -> u64;
    /// `[spira] certify_par` read fresh from the config document on every call (sp-q20wb) —
    /// never the frozen `Ctx.SPIRA_CERTIFY_PAR`, which conf.sh resolved once, before this
    /// process's `exec`, and which therefore cannot see a limit raised in the document while
    /// a gate already waits on it. None when no document is in force, or it sets no
    /// `certify_par`: the caller then falls back to the environment, then the derived
    /// default, as before. The one mechanism `admission_par` re-reads from on every poll;
    /// `admission_wait_line` below takes the result as its own `par` argument rather than
    /// re-deriving it, so there is still only one source for what the pool's size is.
    fn certify_par_live(&self) -> Option<u64>;
    /// Try `slot.<n>.lock` without waiting; true = held until exit, with `slot.<n>.holder`
    /// naming `who` (sp-f4ig1; advisory, for `spira-admit status` and waiting lines).
    fn admission_try(&self, dir: &Path, slot: u64, who: &str) -> bool;
    /// The waiting line for a full gate pool of `par` under `run` (spira_config::admission).
    fn admission_wait_line(&self, run: &str, par: u64) -> String;
    /// Open `<tree>.lock`; false when it cannot be opened.
    fn tree_lock_open(&self, lock: &Path) -> bool;
    /// Try the opened tree lock without waiting. A lock file unlinked or replaced under the
    /// held descriptor is not the lock: release, reopen by path and answer false so the caller
    /// tries again, so two gates never hold one tree.
    fn tree_lock_try(&self) -> bool;
    fn write_holder(&self, p: &Path);
    /// `gate_at`: the tree holds `rev` (proved) → Ok; Err(what went wrong) otherwise.
    fn checkout(&self, repo: &Path, tree: &Path, rev: &str, want: &str) -> Result<(), String>;
    fn remove_worktree(&self, repo: &Path, tree: &Path);
    /// Install a trial's freshly built tools (sp-g9f3t): copy each `<tree>/target/debug/<pkg>`
    /// into `<dir>.tmp`, write `<dir>/TREE` = `tree_id`, rename it to `dir`, and remove every
    /// other entry of `dir`'s parent. Err(why) when any step fails; nothing half-installed
    /// is left at `dir`.
    fn install_tools(&self, tree: &Path, pkgs: &[String], dir: &Path, tree_id: &str) -> Result<(), String>;
    /// The compiler wrapper the trial's builds run through (sp-z61hj): sccache resolved on
    /// `path`, or the refusal (spira_config::build::wrapper). `setting` is SPIRA_BUILD_CACHE.
    fn build_wrapper(&self, path: &str, setting: &str) -> Result<spira_config::build::Wrapper, String>;
    /// Put the gate tree's build directories on tmpfs (sp-z61hj, target.rs): Ok(a line for the
    /// trial's stderr), Err(the refusal — short of room; never a fall back to the disk).
    /// What the target root has left for this user and what the gate's own targets hold there.
    fn target_room(&self, explicit_root: &str, run: &str) -> String;
    /// With a `builder`, the tree's `debug` links into that leased directory (the line says
    /// `warm::BUILDER_IN`); any step that fails leaves the gate's own directory, warm or cold.
    fn target_on_tmpfs(&self, tree: &Path, explicit_root: &str, run: &str, lim: &crate::target::Limits, builder: Option<&Builder>, repo: &str) -> Result<TargetOutcome, String>;
    /// The newest `build-dir` fact of `bead` (its last `work submit`): the tip and the directory.
    fn lc_build_dir(&self, _bead: &str) -> Option<(String, PathBuf)> {
        None
    }
    /// False when a build directory `target_on_tmpfs` linked no longer resolves: something
    /// else removed the output this trial built.
    fn target_intact(&self, tree: &Path) -> bool;
    /// The names in `bins` that are not files in the trial's `target/debug`.
    fn absent_bins(&self, tree: &Path, bins: &[String]) -> Vec<String>;
    /// Drop the tmpfs build output of a finished trial (target::release_tree); `keep_release`
    /// spares `release` for a caller that still has to stage it; a trial whose build directory is sound — judged, or cut at a cap — has its
    /// `debug` join the warm pool, which is held under `cap_mib`.
    fn release_target(&self, tree: &Path, keep_release: bool, hand_on: bool, cap_mib: u64, repo: &str);

    /// `timeout <secs> bash -c <cmd>` in `tree` under exactly `env` → (status, combined output
    /// with trailing newlines stripped).
    fn run_gate(
        &self,
        tree: &Path,
        env: &[(String, String)],
        timeout: &str,
        cmd: &str,
    ) -> (i32, String);

    /// Whether `tree` declares its build apart from its tests (`[tier.build]`).
    fn declares_build_tier(&self, tree: &Path) -> bool {
        crate::own::build_tier(tree)
    }

    /// `run_gate` for a phase no clock bounds: ended with [`crate::stall::RC`] when it shows no
    /// progress for `stall` seconds. The operator's `timeout` still applies.
    fn run_build(
        &self,
        tree: &Path,
        env: &[(String, String)],
        timeout: &str,
        _stall: u64,
        cmd: &str,
    ) -> (i32, String) {
        self.run_gate(tree, env, timeout, cmd)
    }

    // ---- the gate machine (machine.rs)
    /// Persist the run's record. Err is a refusal: a gate that cannot record its moves judges nothing.
    fn machine_save(&self, run: &Path, r: &crate::machine::Run) -> Result<(), String>;
    /// `gate cancel` left a request for `id`.
    fn machine_cancelled(&self, run: &Path, id: &str) -> bool;
    fn machine_clear_cancel(&self, run: &Path, id: &str);

    // ---- time and signals
    fn now(&self) -> u64;
    fn now_ms(&self) -> u64 {
        self.now() * 1000
    }
    /// The clock a recorded `at=` is written and aged on, so a reader on the world's clock
    /// agrees with it; waits and deadlines stay on `now`.
    fn stamp(&self) -> u64 {
        self.now()
    }
    fn utc(&self) -> String;
    fn sleep_ms(&self, ms: u64);
    fn pid(&self) -> u32;
    /// A TERM/INT/HUP arrived, or `gate cancel` was requested for the run being recorded.
    fn signalled(&self) -> bool;
    fn eprint(&self, s: &str);
}
