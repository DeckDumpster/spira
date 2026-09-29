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
    /// `repo_gate <name>`.
    pub gate_cmd: String,
    pub host_cores: String,
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

pub trait World {
    /// `. lib.sh` once, then the values the trial needs. Err when lib.sh would not source.
    fn context(&self, repo_name: Option<&str>) -> Result<Ctx, String>;

    // ---- files
    fn readable(&self, p: &Path) -> bool;
    fn exists(&self, p: &Path) -> bool;
    fn read(&self, p: &Path) -> Option<String>;
    fn mkdir_p(&self, p: &Path);
    /// Write via `.<name>.<pid>` and rename.
    fn write_atomic(&self, dir: &Path, name: &str, content: &str);
    fn append(&self, p: &Path, line: &str);
    fn remove(&self, p: &Path);
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
    /// spira.toml is in force, Err(why) when one is and it does not validate.
    fn gate_mode(&self, repo_name: &str) -> Result<GateMode, String>;
    /// `cargo metadata --format-version 1 --no-deps --offline` in `tree` with `path` as PATH →
    /// stdout, or Err(stderr).
    fn cargo_metadata(&self, tree: &Path, path: &str, home: &str) -> Result<String, String>;

    // ---- helpers the gate runs (DESIGN.md "Non-goals": not ported here)
    /// `bash -n` over a script's content → Err(bash's message).
    fn bash_n(&self, content: &[u8]) -> Result<(), String>;
    /// `bash exclude.sh filter` over stdin.
    fn exclude_filter(&self, exclude: &Path, names: &str) -> String;
    /// `bash skew.sh foreign <repo> <base> <ref>` → (status, combined output).
    fn skew_foreign(&self, skew: &Path, repo: &Path, base: &str, branch: &str) -> (i32, String);
    fn sweep(&self, sweep: &Path, repo: &Path);
    /// `SPIRA_RUN=<run> bash yield.sh <args…>`, output discarded.
    fn yield_sh(&self, yield_sh: &Path, run: &str, args: &[&str]);
    /// lib.sh `lc_certify <bead> <tip> <outcome> <detail>`, when lib.sh defines it.
    fn lc_certify(&self, bead: &str, tip: &str, outcome: &str, detail: &str);
    /// sha256 of gate.sh, exclude.sh, skew.sh and this binary, concatenated.
    fn harness_hash(&self) -> Option<String>;

    // ---- admission and the tree
    fn nproc_all(&self) -> u64;
    fn mem_avail_mib(&self) -> u64;
    /// Try `slot.<n>.lock` without waiting; true = held until exit.
    fn admission_try(&self, dir: &Path, slot: u64) -> bool;
    /// Open `<tree>.lock`; false when it cannot be opened.
    fn tree_lock_open(&self, lock: &Path) -> bool;
    /// Try the opened tree lock without waiting.
    fn tree_lock_try(&self) -> bool;
    fn write_holder(&self, p: &Path);
    /// `gate_at`: the tree holds `rev` (proved) → Ok; Err(what went wrong) otherwise.
    fn checkout(&self, repo: &Path, tree: &Path, rev: &str, want: &str) -> Result<(), String>;
    fn remove_worktree(&self, repo: &Path, tree: &Path);

    /// `timeout <secs> bash -c <cmd>` in `tree` under exactly `env` → (status, combined output
    /// with trailing newlines stripped).
    fn run_gate(
        &self,
        tree: &Path,
        env: &[(String, String)],
        timeout: &str,
        cmd: &str,
    ) -> (i32, String);

    // ---- time and signals
    fn now(&self) -> u64;
    fn utc(&self) -> String;
    fn sleep_ms(&self, ms: u64);
    fn pid(&self) -> u32;
    /// A TERM/INT/HUP arrived.
    fn signalled(&self) -> bool;
    fn eprint(&self, s: &str);
}
