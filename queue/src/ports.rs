//! Every effect queue has on the world, as a trait (DESIGN.md §6): git, bd reads, the
//! lib.sh seam, the harness scripts, the forge, spira-lc, the config, the clock, the
//! environment and the two output streams. `real.rs` implements them against the host;
//! the unit tests implement them as fakes.

use std::path::{Path, PathBuf};

use crate::model::{BeadRow, LandMode, LcBeadRow, RangeCommit};

/// Settings as conf.sh resolves them (seam R1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    /// The harness script directory (`$SPIRA_HOME`, where lib.sh lives).
    pub home: PathBuf,
    pub run: PathBuf,
    pub queue_dir: PathBuf,
    pub releases: Option<PathBuf>,
    pub forge: PathBuf,
    pub repo_map: Option<PathBuf>,
    /// The batcher program: `batcher` by name on the launcher's PATH (sp-gypjk); a field only
    /// so a unit test can hand in a stub (None: no batcher program at all).
    pub batcher_bin: Option<PathBuf>,
    /// `SPIRA_BATCHER_ENABLE=0`: the operator cuts rounds by hand, so queue cuts none (the
    /// switch that replaced `batcher_bin = "/bin/true"`).
    pub batcher_off: bool,
    /// The spira-lc program: `spira-lc` by name on the launcher's PATH.
    pub lc_bin: Option<PathBuf>,
    pub submitted_label: String,
    pub home_repo: String,
    /// `$SPIRA_DB` and `${SPIRA_BD:-bd}` for bd reads.
    pub db: String,
    pub bd: String,
    pub transition_pollsec: u64,
    pub transition_maxsec: u64,
    pub preflight_wall_secs: u64,
    /// The verdict pass's thresholds, as conf.sh resolved them (DESIGN-verdict.md §3).
    pub verdict: VerdictSettings,
    /// `SPIRA_CERTIFY_SUITES` (declared value; law-one-source-of-config): the suites
    /// `submit` gates on.
    pub certify_suites: String,
    /// `SPIRA_GIT_NAME` / `SPIRA_GIT_EMAIL` (declared values): the identity a batch merge
    /// commits as.
    pub git_name: String,
    pub git_email: String,
    /// `SPIRA_MAIL_SESSION_MAILBOX` (declared value): the mailbox `notify`/`divergence`
    /// alarm into.
    pub mailbox: String,
    /// `SPIRA_EXPRESS_LABEL` (declared value): the bd label `sort_rows` ranks first.
    pub express_label: String,
    /// `SPIRA_ROUND_CERTIFY_WALL_SECS` (declared value): the wall `round certify` allows the
    /// round VM's corpus.
    pub round_wall_secs: u64,
}

/// `SPIRA_QUEUE_CI_MAXSEC[_<NAME>]`, `SPIRA_QUEUE_CI_IDLE_SEC[_<NAME>]` (already resolved for
/// the named repository), `SPIRA_QUEUE_INFRA_RETRIES`, `SPIRA_QUEUE_LOCK_WAIT`,
/// `SPIRA_QUEUE_LOCK_STARVE_MAX`, `SPIRA_INCIDENT_PRIORITY`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerdictSettings {
    pub ci_maxsec: u64,
    pub ci_idle_sec: u64,
    pub infra_retries: u64,
    pub lock_wait: u64,
    pub lock_starve_max: u64,
    pub incident_priority: String,
}

impl Default for VerdictSettings {
    fn default() -> Self {
        VerdictSettings { ci_maxsec: 3600, ci_idle_sec: 600, infra_retries: 2, lock_wait: 90, lock_starve_max: 5, incident_priority: "1".into() }
    }
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
    /// `merge-base <a> <b>`; None when they share no history.
    fn merge_base(&self, repo: &Path, a: &str, b: &str) -> Option<String>;
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
    /// `branch -D <name>` with `SPIRA_REF_SANCTIONED=1` — the ref guard's consent for a
    /// `spira/*` deletion (the verdict's own batch branch and stale queue refs).
    fn branch_delete_sanctioned(&self, repo: &Path, name: &str) -> bool;
    /// `for-each-ref --format='%(refname:short) %(objectname)' <prefix>*`.
    fn branches(&self, repo: &Path, prefix: &str) -> Vec<(String, String)>;
    fn worktree_prune(&self, repo: &Path);
    fn worktree_add_detached(&self, repo: &Path, path: &Path, sha: &str) -> bool;
    fn worktree_remove(&self, repo: &Path, path: &Path);
    /// `merge --no-edit --no-ff -F <msgfile> <tip>` as `git_name <git_email>`; false on
    /// conflict.
    fn merge_no_ff(&self, wt: &Path, message: &str, tip: &str, git_name: &str, git_email: &str) -> bool;
    fn merge_abort(&self, wt: &Path);
    /// `status --porcelain` is empty.
    fn is_clean(&self, wt: &Path) -> bool;
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
    fn bead_reopen(&self, id: &str, cause: &str, suites: &str) -> bool;
    fn cause_event(&self, id: &str, cause: &str);
    fn release_claim(&self, id: &str);
    /// `bdq close <id> --reason-file -`, the reason on stdin. `true` only on a clean exit.
    fn bead_close(&self, id: &str, reason: &str) -> bool;
    /// `sending reap-landed-branch <id> <branch> <root> <why>` for the repository the
    /// `repo:` label names. Ok(false): that repository or branch is not here, nothing to
    /// reap; Err: the Sending refused or failed (it remains the backstop).
    fn reap_landed_branch(&self, id: &str, repo: &str, branch: &str, why: &str) -> Result<bool, String>;
    fn gh_issue_closeout(&self, id: &str, sha: &str, repo: &Path);
    fn comment(&self, id: &str, text: &str);
    /// `mailbox`: `Settings::mailbox` (`SPIRA_MAIL_SESSION_MAILBOX`'s declared value).
    fn notify(&self, mailbox: &str, repo: &str, subject: &str, body: &str);
    fn event(&self, kind: &str, title: &str, detail: &str);
    /// R11: is forge an ancestor of local (alarms once per foreign tip if not). `mailbox`:
    /// `Settings::mailbox`.
    fn divergence(&self, mailbox: &str, queue_dir: &Path, repo: &str, path: &Path, forge: &str, local: &str) -> Divergence;
    /// R12: `spira_git_push <path> -q <remote> <refspec>`.
    fn push(&self, path: &Path, remote: &str, refspec: &str) -> bool;
    /// R13: Ok, or Err(REBASE_FAILURE).
    fn rebase(&self, branch: &str, onto: &str, path: &Path, name: &str) -> Result<(), String>;
    fn land_subject(&self, id: &str) -> String;
    /// R15: `queue_sort_rows` over `<id> <tip> <epoch>` rows; returns `<id> <tip>` rows.
    /// `express_label`: `Settings::express_label` (`SPIRA_EXPRESS_LABEL`'s declared value).
    fn sort_rows(&self, express_label: &str, path: &Path, base: &str, prio_json: &str, rows: &str) -> Vec<(String, String)>;
    fn cancel_runs(&self, forge: &Path, path: &Path, branch: &str);
    fn format_batch(&self, wt: &Path, base: &str, name: &str);
    /// `_base_conflict`: true when the tip conflicts with the base.
    fn base_conflict(&self, path: &Path, base: &str, tip: &str) -> bool;
    /// `_pf_gate` under a wall of `wall_secs`: (rc, output); 124 = wall hit.
    fn pf_gate(&self, branch: &str, name: &str, stamp: &str, wall_secs: u64) -> (i32, String);
    /// R23: `BEADS_ACTOR=<actor> bdq create <title> --type bug --priority P --labels L
    /// --body-file <body> --silent` → the new id (None when bd created nothing).
    fn create_bug(&self, actor: &str, title: &str, priority: &str, labels: &str, body: &str) -> Option<String>;
    /// Append `note` to bead `id` when it is still open; false when it is closed, unknown
    /// or the note did not land — the caller then files a new bead.
    fn amend_bug(&self, actor: &str, id: &str, note: &str) -> bool;
}

/// The harness scripts and binaries queue runs as whole programs.
pub trait Scripts {
    /// `gate.sh <branch> <repo>` with SPIRA_GATE_BEAD / SPIRA_GATE_SUITES: (rc, output).
    fn gate(&self, branch: &str, repo: &str, bead: &str, suites: &str) -> (i32, String);
    /// `batcher judgement-ci <repo> --suites CSV --members CSV --evidence T --home --run --db`,
    /// stdout and stderr combined (the verdict reads `id=` off it).
    fn judgement_ci(&self, bin: &Path, s: &Settings, repo: &str, suites: &str, members: &str, evidence: &str) -> RunOut;
    /// `testenv suites observe-flake <suite> <sha>`, best-effort.
    fn observe_flake(&self, suite: &str, sha: &str);
    /// `mail send operator --from "Spira Queue <queue@spira>" --subject S`, body on stdin.
    fn mail_operator(&self, subject: &str, body: &str);
    fn batcher_cut(&self, bin: &Path, repo: &str, wait_zero: bool) -> i32;
    fn czar_fence(&self, class: &str) -> bool;
    /// `<bin> <args…>` — the release producer. `bin` is resolved by the caller
    /// (`deploy::release_bin`, §8 D14): the round's own `<bins>/release` when it exists,
    /// never bare `release` from the launcher's PATH while a bin-dir is in play — the
    /// builder and the build must be the same commit. `SPIRA_DB=<db>` is in its environment
    /// (`release verify`'s pre-activate store check reads it). Stdout and stderr are kept
    /// apart: `release build` answers the sha on stdout.
    fn release(&self, bin: &Path, args: &[String], db: &str) -> RunOut;
    /// `round-vm run <tree> --results-dir <results> --base <base>` under `timeout <wall_secs>`: the full
    /// corpus of `tree` on the round VM. Exit 0/1 ran (the results say which suites are red);
    /// 124/137 hit the wall; anything else is the harness's fault.
    fn round_vm(&self, tree: &Path, results: &Path, base: &str, wall_secs: u64) -> RunOut;
}

/// A finished child: its exit status (127 when it could not run), stdout and stderr.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunOut {
    pub rc: i32,
    pub out: String,
    pub err: String,
}

pub trait Forge {
    /// `pr-create <repo> <head> <base> <title>`, body on stdin → the PR number.
    fn pr_create(&self, forge: &Path, repo: &Path, head: &str, base: &str, title: &str, body: &str) -> Option<String>;
    fn pr_close(&self, forge: &Path, repo: &Path, pr: &str);
    /// One bounded line (DESIGN.md §5).
    fn pr_comment(&self, forge: &Path, repo: &Path, pr: &str, line: &str);
    fn branch_protect(&self, forge: &Path, repo: &Path, branch: &str) -> bool;
    /// `pr-state <repo> <pr>` → `open`, `closed`, `merged` or `unknown`; None when the call failed.
    fn pr_state(&self, forge: &Path, repo: &Path, pr: &str) -> Option<String>;
    /// `check-status <repo> <pr> <branch>` → stdout, or None when the call failed.
    fn check_status(&self, forge: &Path, repo: &Path, pr: &str, branch: &str) -> Option<String>;
    /// `run-id <repo> <branch>` → the latest run's id (None when empty or failed).
    fn run_id(&self, forge: &Path, repo: &Path, branch: &str) -> Option<String>;
    /// `run-metadata <repo> <run>` → its stdout (empty when it failed).
    fn run_metadata(&self, forge: &Path, repo: &Path, run: &str) -> String;
    fn run_cancel(&self, forge: &Path, repo: &Path, run: &str);
    fn workflow_rerun(&self, forge: &Path, repo: &Path, run: &str);
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
    /// `event batch <id> --expect S --version V --actor A --kind K` (one CAS'd event).
    fn batch_event(&self, batch_id: &str, state: &str, version: &str, actor: &str, kind: &str) -> Result<(), (i32, String)>;
    /// `show <bead>` → the bead row's (state, version); None when it has no row.
    fn bead_state(&self, bead: &str) -> Option<(String, String)>;
    /// `event bead <id> --expect S --version V --actor A --kind K` (one CAS'd event).
    fn bead_event(&self, bead: &str, state: &str, version: &str, actor: &str, kind: &str) -> Result<(), (i32, String)>;
    /// `land <id> --expect GREEN --version V --actor A --sha S`.
    fn land_batch(&self, batch_id: &str, version: &str, actor: &str, sha: &str) -> Result<(), (i32, String)>;
    /// A reachability probe (one read against the lifecycle database). Err names why.
    fn probe(&self) -> Result<(), String>;
    /// `list [--state S]`: every bead row (with `since`). Err = cannot tell.
    fn bead_rows(&self, state: Option<&str>) -> Result<Vec<LcBeadRow>, String>;
    /// `show <bead>` → the bead row; None when it has no row or the machine cannot say.
    fn bead_row(&self, bead: &str) -> Option<LcBeadRow>;
    /// `certify <bead> <tip> pass <detail> <actor>`: record a gate pass at `tip`.
    fn certify(&self, bead: &str, tip: &str, detail: &str, actor: &str) -> Result<(), (i32, String)>;
}

/// The config documents, through the spira-config library only
/// (law-config-through-the-cli-only): queue never opens, parses or writes either file.
pub trait ConfigStore {
    /// `[repo.<name>]` (mode, base) from `toml`; empty strings when absent.
    fn repo_row(&self, toml: &Path, name: &str) -> Result<(String, String), String>;
    /// Write `[repo.<name>] mode` and `base` (validated, atomic).
    fn set_repo_row(&self, toml: &Path, name: &str, mode: &str, base: &str) -> Result<(), String>;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Divergence {
    Ancestor,
    Diverged(String),
    CannotCheck(String),
}
