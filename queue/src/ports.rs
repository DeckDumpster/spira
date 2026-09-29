//! Every effect queue has on the world, as a trait (DESIGN.md §6): git, bd reads, the
//! lib.sh seam, the harness scripts, the forge, spira-lc, the config, the clock, the
//! environment and the two output streams. `real.rs` implements them against the host;
//! the unit tests implement them as fakes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::model::{BeadRow, LandMode, LcBeadRow, RangeCommit};

/// Settings as conf.sh resolves them (seam R1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    /// The harness script directory (`$SPIRA_HOME`, where lib.sh lives).
    pub home: PathBuf,
    pub run: PathBuf,
    pub queue_dir: PathBuf,
    pub landstate: PathBuf,
    pub releases: Option<PathBuf>,
    pub forge: PathBuf,
    pub repo_map: Option<PathBuf>,
    pub batcher_bin: Option<PathBuf>,
    pub lc_bin: Option<PathBuf>,
    pub submitted_label: String,
    pub home_repo: String,
    /// `$SPIRA_DB` and `${SPIRA_BD:-bd}` for bd reads.
    pub db: String,
    pub bd: String,
    pub transition_pollsec: u64,
    pub transition_maxsec: u64,
    pub preflight_wall_secs: u64,
}

/// One repository as lib.sh resolves it (seam R1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoCtx {
    pub name: String,
    /// `repo_root`; None when the map does not carry the name.
    pub path: Option<PathBuf>,
    pub mode: LandMode,
    /// `spira_landref <path>`; None when it cannot be resolved.
    pub landref: Option<String>,
    /// `repo_field <name> land` / `base` — the legacy map's raw columns (agreement check).
    pub map_land: String,
    pub map_base: String,
    /// `spira_publish_forge <name>`: (remote, branch).
    pub publish: Option<(String, String)>,
    /// `git remote` of the checkout.
    pub remotes: Vec<String>,
}

impl RepoCtx {
    /// `ref_remote <ref> <repo>`: the prefix before the first `/` when it is a remote here.
    pub fn ref_remote(&self, r: &str) -> Option<String> {
        let (prefix, _) = r.split_once('/')?;
        self.remotes.iter().any(|x| x == prefix).then(|| prefix.to_string())
    }
}

/// `ref_branch`: everything after the first `/`.
pub fn ref_branch(r: &str) -> &str {
    r.split_once('/').map_or(r, |(_, b)| b)
}

pub trait Git {
    /// `rev-parse --verify -q <rev>`.
    fn rev_parse(&self, repo: &Path, rev: &str) -> Option<String>;
    fn is_ancestor(&self, repo: &Path, a: &str, b: &str) -> bool;
    /// `update-ref <ref> <new> [<old>]` — a CAS when `old` is given.
    fn update_ref(&self, repo: &Path, refname: &str, new: &str, old: Option<&str>) -> bool;
    /// `show-ref --verify --quiet <ref>`.
    fn ref_exists(&self, repo: &Path, refname: &str) -> bool;
    /// `symbolic-ref -q --short HEAD`.
    fn current_branch(&self, repo: &Path) -> Option<String>;
    fn fetch(&self, repo: &Path, remote: &str, branch: &str) -> bool;
    /// `log -z --format=%H%x1f%P%x1f%s <range>`, newest first.
    fn log_range(&self, repo: &Path, range: &str) -> Result<Vec<RangeCommit>, String>;
    /// `cat-file -e <sha>^{commit}`.
    fn commit_exists(&self, repo: &Path, sha: &str) -> bool;
    /// `branch [-f] <name> <sha>`.
    fn branch_set(&self, repo: &Path, name: &str, sha: &str, force: bool) -> bool;
    /// `branch -D <name>`.
    fn branch_delete(&self, repo: &Path, name: &str) -> bool;
    /// `for-each-ref --format='%(refname:short) %(objectname)' <prefix>*`.
    fn branches(&self, repo: &Path, prefix: &str) -> Vec<(String, String)>;
    fn worktree_prune(&self, repo: &Path);
    fn worktree_add_detached(&self, repo: &Path, path: &Path, sha: &str) -> bool;
    fn worktree_remove(&self, repo: &Path, path: &Path);
    /// `merge --no-edit --no-ff -F <msgfile> <tip>` as spira; false on conflict.
    fn merge_no_ff(&self, wt: &Path, message: &str, tip: &str) -> bool;
    fn merge_abort(&self, wt: &Path);
    /// `status --porcelain` is empty.
    fn is_clean(&self, wt: &Path) -> bool;
    /// `rev-parse --show-toplevel` run in `dir`; None outside a work tree.
    fn toplevel(&self, dir: &Path) -> Option<PathBuf>;
    /// Paths with a tracked modification (staged or not): `status --porcelain -z
    /// --untracked-files=no`, both sides of a rename. Err when git fails.
    fn tracked_changes(&self, repo: &Path) -> Result<Vec<String>, String>;
    /// `ls-tree -r -z --full-tree <rev>`: every path with its mode and object.
    fn tree(&self, repo: &Path, rev: &str) -> Result<BTreeMap<String, TreeEntry>, String>;
    /// `cat-file blob <sha>`.
    fn blob(&self, repo: &Path, sha: &str) -> Result<Vec<u8>, String>;
    /// `reset -q --mixed <sha>`.
    fn reset_mixed(&self, repo: &Path, sha: &str) -> bool;
}

/// One `ls-tree` entry: the octal mode as git prints it (`100644`, `100755`, `120000`,
/// `160000`) and the object it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub mode: String,
    pub sha: String,
}

pub trait Bd {
    /// `bd show <ids…> --json`, chunked. Err when bd fails or answers non-JSON.
    fn show(&self, ids: &[String]) -> Result<Vec<BeadRow>, String>;
}

/// The lib.sh seam (DESIGN.md §6). Every value travels on the child's stdin.
pub trait Lib {
    /// R1: settings and, when `repo` is Some (or the home repo), that repository.
    fn context(&self, repo: Option<&str>) -> Result<(Settings, RepoCtx), String>;
    /// R22: `spira_repos` — the home repository first, then every other registered name.
    /// Err when the seam fails or answers nothing.
    fn repos(&self) -> Result<Vec<String>, String>;
    /// `spira_toml_resolve` (only the transitions ask).
    fn toml_path(&self) -> Option<PathBuf>;
    /// R20: `repo_land` and `spira_landref` read back after a transition's write.
    fn readback(&self, name: &str) -> (String, String);
    fn land_mark(&self, id: &str, state: &str, tip: &str, reason: &str);
    fn bead_reopen(&self, id: &str, cause: &str, suites: &str) -> bool;
    fn cause_event(&self, id: &str, cause: &str);
    fn release_claim(&self, id: &str);
    fn bead_close_on_land(&self, id: &str, sha: &str);
    fn gh_issue_closeout(&self, id: &str, sha: &str, repo: &Path);
    fn comment(&self, id: &str, text: &str);
    fn notify(&self, repo: &str, subject: &str, body: &str);
    fn event(&self, kind: &str, title: &str, detail: &str);
    /// R11: true when forge is an ancestor of local (alarms once per foreign tip if not).
    fn divergence(&self, repo: &str, path: &Path, forge: &str, local: &str) -> bool;
    /// R12: `spira_git_push <path> -q <remote> <refspec>`.
    fn push(&self, path: &Path, remote: &str, refspec: &str) -> bool;
    /// R13: Ok, or Err(REBASE_FAILURE).
    fn rebase(&self, branch: &str, onto: &str, path: &Path, name: &str) -> Result<(), String>;
    fn land_subject(&self, id: &str) -> String;
    /// R15: `queue_sort_rows` over `<id> <tip> <epoch>` rows; returns `<id> <tip>` rows.
    fn sort_rows(&self, path: &Path, base: &str, prio_json: &str, rows: &str) -> Vec<(String, String)>;
    fn cancel_runs(&self, forge: &Path, path: &Path, branch: &str);
    fn lc_returned(&self, id: &str, reason: &str);
    fn format_batch(&self, wt: &Path, base: &str, name: &str);
    /// `_base_conflict`: true when the tip conflicts with the base.
    fn base_conflict(&self, path: &Path, base: &str, tip: &str) -> bool;
    /// `_pf_gate` under a wall of `wall_secs`: (rc, output); 124 = wall hit.
    fn pf_gate(&self, branch: &str, name: &str, stamp: &str, wall_secs: u64) -> (i32, String);
    /// R19: `_verdict_settle_publish`: 0 settled-or-waiting, 1 error, 3 red.
    fn settle_publish(&self, name: &str, path: &Path) -> i32;
    /// R23: source `<home>/conf.sh` as a freshly started unit would (every `SPIRA_*` but
    /// `SPIRA_CONF`/`SPIRA_TOML` removed) and answer the `SPIRA_DB` it resolved. Err when the
    /// seam itself could not run.
    fn conf_smoke(&self, home: &Path) -> Result<String, String>;
}

/// The harness scripts and binaries queue runs as whole programs.
pub trait Scripts {
    /// `gate.sh <branch> <repo>` with SPIRA_GATE_BEAD / SPIRA_GATE_SUITES: (rc, output).
    fn gate(&self, branch: &str, repo: &str, bead: &str, suites: &str) -> (i32, String);
    /// `lc_off`: lifecycle_enforce is OFF — the child must not reach spira-lc (real.rs pins
    /// `SPIRA_LC_BIN` to [`crate::real::LC_OFF_BIN`] and `SPIRA_LIFECYCLE_ENFORCE=0`).
    fn batch_sweep(&self, repo: &str, wait_zero: bool, lc_off: bool) -> i32;
    fn verdict(&self, repo: &str, lc_off: bool) -> i32;
    fn batcher_cut(&self, bin: &Path, repo: &str, wait_zero: bool, lc_off: bool) -> i32;
    fn czar_fence(&self, class: &str) -> bool;
    /// `build-tarball.sh build --bin-dir … <head> <repo>` → the tarball path it printed.
    fn build_tarball(&self, bins: &Path, repo_name: &str, name: &str, out: &Path, head: &str, repo: &Path) -> Option<PathBuf>;
    /// `activate.sh <tarball>` (with SPIRA_ACTIVATE_LAND_LOCAL=1 when `land_local`).
    fn activate(&self, tarball: &Path, land_local: bool) -> (i32, String);
}

pub trait Forge {
    /// `pr-create <repo> <head> <base> <title>`, body on stdin → the PR number.
    fn pr_create(&self, forge: &Path, repo: &Path, head: &str, base: &str, title: &str, body: &str) -> Option<String>;
    fn pr_close(&self, forge: &Path, repo: &Path, pr: &str);
    /// One bounded line (DESIGN.md §5).
    fn pr_comment(&self, forge: &Path, repo: &Path, pr: &str, line: &str);
    fn branch_protect(&self, forge: &Path, repo: &Path, branch: &str) -> bool;
}

/// spira-lc (the lifecycle machine's CLI).
pub trait Lc {
    fn available(&self) -> bool;
    /// `show-batch` → (state, version); None when the batch row does not exist.
    fn batch_state(&self, batch_id: &str) -> Option<(String, String)>;
    fn create_bead(&self, id: &str);
    /// `cut` → Ok(version) or Err((rc, output)).
    fn cut(&self, batch_id: &str, repo: &str, head: &str, base: &str, members: &str, actor: &str) -> Result<String, (i32, String)>;
    fn abandon_batch(&self, batch_id: &str, state: &str, version: &str, actor: &str, reason: &str) -> Result<(), (i32, String)>;
    fn eject_member(&self, batch_id: &str, bead: &str, state: &str, version: &str, actor: &str, reason: &str) -> Result<(), (i32, String)>;
    /// A reachability probe (one read against the lifecycle database). Err names why.
    fn probe(&self) -> Result<(), String>;
    /// `list --state IN_DELIVERY`. Err = cannot tell.
    fn in_delivery(&self) -> Result<Vec<LcBeadRow>, String>;
}

/// The config documents, through the spira-config library only
/// (law-config-through-the-cli-only): queue never opens, parses or writes either file.
pub trait ConfigStore {
    /// `[repo.<name>]` (mode, base) from `toml`; empty strings when absent.
    fn repo_row(&self, toml: &Path, name: &str) -> Result<(String, String), String>;
    /// Write `[repo.<name>] mode` and `base` (validated, atomic).
    fn set_repo_row(&self, toml: &Path, name: &str, mode: &str, base: &str) -> Result<(), String>;
    /// `spira.lifecycle_enforce` from the resolved document; false when absent/unreadable.
    fn lifecycle_enforce(&self, toml: Option<&Path>) -> bool;
    /// The legacy map's land/base columns for `name` (spira_config::legacy_map), atomic.
    fn set_legacy_map_row(&self, map: &Path, name: &str, land: &str, base: &str) -> Result<(), String>;
}

pub trait Clock {
    fn now(&self) -> u64;
    /// `date -u +%Y%m%dT%H%M%SZ`.
    fn stamp(&self) -> String;
    fn sleep(&self, secs: u64);
}

pub trait Env {
    fn var(&self, k: &str) -> Option<String>;
    fn pid(&self) -> u32;
    fn read_stdin(&self) -> Result<String, String>;
    fn read_file(&self, p: &Path) -> Result<String, String>;
}

pub trait Emit {
    fn out(&self, s: &str);
    fn err(&self, s: &str);
}

/// Everything an operation may touch.
pub struct World<'a> {
    pub git: &'a dyn Git,
    pub bd: &'a dyn Bd,
    pub lib: &'a dyn Lib,
    pub scripts: &'a dyn Scripts,
    pub forge: &'a dyn Forge,
    pub lc: &'a dyn Lc,
    pub config: &'a dyn ConfigStore,
    pub clock: &'a dyn Clock,
    pub env: &'a dyn Env,
    pub io: &'a dyn Emit,
}

impl World<'_> {
    pub fn out(&self, s: impl AsRef<str>) {
        let mut l = s.as_ref().to_string();
        l.push('\n');
        self.io.out(&l);
    }
    pub fn err(&self, s: impl AsRef<str>) {
        let mut l = s.as_ref().to_string();
        l.push('\n');
        self.io.err(&l);
    }
    pub fn var(&self, k: &str) -> Option<String> {
        self.env.var(k).filter(|v| !v.is_empty())
    }
}
