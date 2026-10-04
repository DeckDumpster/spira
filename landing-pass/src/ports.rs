//! The pass's boundaries, as traits: the bead store (read only), git, the lib.sh seam, the
//! harness programs it runs, process liveness, and the clock. Production implementations are
//! in `real`; the unit tests use recording fakes (`tests`).

use crate::model::{BeadRow, Rebase, Recut};
use std::path::Path;

/// The bead store, read only. Writes go through [`Lib`].
pub trait Beads {
    /// `bd show <ids…> --json`, every row that came back. Err when the store could not be
    /// read at all — never an empty Ok.
    fn show(&self, ids: &[String]) -> Result<Vec<BeadRow>, String>;
    /// lib.sh `bead_land_status`: `closed` (or submitted-labelled), another status, or `-`
    /// when it cannot be read.
    fn land_status(&self, id: &str) -> String;
    /// lib.sh `ask_already_open <subject>` (sp-31hjr, family C): true when an OPEN ask
    /// already carries `subject` in its title. `label` is `SPIRA_ASK_LABEL`.
    fn ask_open(&self, label: &str, subject: &str) -> bool;
    /// lib.sh `bead_context <id>` (sp-31hjr): a human-readable block for a mail body.
    /// `now` is a unix epoch second.
    fn context(&self, id: &str, now: i64) -> String;
}

pub trait Git {
    /// `for-each-ref refs/heads/spira/*` → (short name, tip).
    fn spira_refs(&self, repo: &Path) -> Vec<(String, String)>;
    /// `show-ref --verify refs/heads/<branch>`.
    fn branch_exists(&self, repo: &Path, branch: &str) -> bool;
    fn rev_parse(&self, repo: &Path, rev: &str) -> Option<String>;
    fn is_ancestor(&self, repo: &Path, a: &str, b: &str) -> bool;
    /// lib.sh `content_landed`: the base already holds every change on the branch.
    fn content_landed(&self, repo: &Path, branch: &str, base: &str) -> bool;
    /// `rev-list --count <range>`; None when it cannot be taken.
    fn count(&self, repo: &Path, range: &str) -> Option<u64>;
    fn fetch(&self, repo: &Path, remote: &str);
    /// A private landing worktree (push mode): exists?
    fn tree_ok(&self, tree: &Path) -> bool;
    fn tree_add_detached(&self, repo: &Path, tree: &Path, at: &str);
    /// `checkout -q -B landing <base>` in the landing tree.
    fn tree_checkout_landing(&self, tree: &Path, base: &str) -> bool;
    /// `merge --no-edit -q -m <subject> <branch>`; Err(conflicted paths) after aborting.
    fn tree_merge(&self, tree: &Path, subject: &str, branch: &str, name: &str, email: &str) -> Result<(), String>;
    fn tree_head(&self, tree: &Path) -> Option<String>;
    fn tree_reset_hard(&self, tree: &Path, to: &str);
    fn tree_merge_abort(&self, tree: &Path);
    /// `git branch -D` with SPIRA_REF_SANCTIONED=1 (halt's orphaned batch branches).
    fn delete_branch(&self, repo: &Path, branch: &str) -> bool;
    /// short names of refs matching a pattern (`for-each-ref --format=%(refname:short)`).
    fn refs_matching(&self, repo: &Path, pattern: &str) -> Vec<String>;
    /// `git merge-base <a> <b>`; None when there is no common ancestor (or git fails).
    fn merge_base(&self, repo: &Path, a: &str, b: &str) -> Option<String>;
    /// `git log --format=%s <range> [-- <paths…>]`.
    fn log_subjects(&self, repo: &Path, range: &str, paths: &[&str]) -> Option<String>;
}

/// The lib.sh seam (DESIGN.md §6). Every call is one fixed script; every value on stdin.
pub trait Lib {
    fn land_mark(&self, id: &str, state: &str, tip: &str, reason: &str);
    fn reopen(&self, id: &str, cause: &str, note: &str);
    fn event(&self, kind: &str, id: &str, title: &str, detail: &str);
    fn noverdict(&self, id: &str, branch: &str, repo: &str, reason: &str, outcome: &str, out: &str);
    /// incident.sh file; Ok(the printed output, whose last line is the id).
    fn incident(&self, labels: &str, repo: &str, ext_ref: &str, title: &str, payload: &str) -> Result<String, i32>;
    fn ask_rebase_loop(&self, args: &[&str]);
    fn ask_red_recurring(&self, id: &str, branch: &str, repo: &str, class: &str, first_at: &str);
    fn ask_rebase_refused(&self, id: &str, branch: &str, repo: &str, reason: &str);
    fn ask_budget_deferred(&self, branch: &str, repo: &str, n: u32);
    fn rebase(&self, branch: &str, onto: &str, repo: &Path, name: &str) -> Rebase;
    fn recut(&self, branch: &str, onto: &str, repo: &Path, name: &str) -> Recut;
    fn bump_requeue(&self, id: &str, reason: &str);
    fn requeues_of(&self, id: &str) -> u32;
    fn conflict_note(&self, args: &[&str]) -> String;
    fn other_beads(&self, repo: &Path, branch: &str, base: &str, files: &str) -> String;
    fn pr_merged(&self, repo: &Path, branch: &str) -> bool;
    fn note(&self, id: &str, text: &str);
    /// spira_git_push; Err(its stderr).
    fn push(&self, tree: &Path, remote: &str, refspec: &str) -> Result<(), String>;
    fn land_subject(&self, id: &str) -> String;
    fn deliver_delivered(&self, id: &str, sha: &str);
    fn deliver_requeued(&self, id: &str, tip: &str);
    fn deliver_returned(&self, id: &str, reason: &str);
    fn closeout(&self, id: &str, sha: &str, repo: &Path);
    fn close_on_land(&self, id: &str, sha: &str);
    fn prune_worktrees(&self, repo: &Path);
    fn gh_unlanded_scan(&self);
    /// `spira_ask_refresh_loop <repo> <name> <branch> <id> <base_fq> <n>` — needs_refresh's
    /// escalation when a pull request has been refreshed `SPIRA_PR_REFRESH_MAX` times.
    fn ask_refresh_loop(&self, repo: &Path, name: &str, branch: &str, id: &str, base_fq: &str, n: u32);
    /// `spira-lc deliver pr-merged <repo> <id> <br> <merge-sha>`.
    fn deliver_pr_merged(&self, repo: &Path, id: &str, branch: &str, merge_sha: &str);
    /// `spira-lc deliver pr-closed <id> <reason>`.
    fn deliver_pr_closed(&self, id: &str, reason: &str);
    /// `spira_git_push --force-with-lease -u <remote> <branch>`; Err(its stderr).
    fn force_push(&self, repo: &Path, remote: &str, branch: &str) -> Result<(), String>;
}

/// The harness programs the pass runs as subprocesses (DESIGN.md §2.5).
pub trait Tools {
    /// gate.sh <branch> <repo> with SPIRA_GATE_LOCK_WAIT / SPIRA_GATE_BEAD → (status, transcript).
    fn gate(&self, branch: &str, repo: &str, lock_wait: &str, bead: &str) -> (i32, String);
    /// Start gate.sh exactly as [`Tools::gate`] runs it, without waiting (DESIGN.md §8 D14).
    /// Returns a ticket that [`Tools::gate_wait_any`] hands back with the gate's result.
    fn gate_start(&self, branch: &str, repo: &str, lock_wait: &str, bead: &str) -> u64;
    /// Block until any started gate finishes → (ticket, status, transcript); None when none
    /// is running.
    fn gate_wait_any(&self) -> Option<(u64, i32, String)>;
    /// Free slots in gate.sh's host-wide admission pool of `par` slots, probed now; None
    /// when it cannot be told.
    fn gate_slots_free(&self, par: usize) -> Option<usize>;
    /// gate-run.sh --status <branch> <repo>: Some(output) when it exits 0.
    fn gate_status(&self, branch: &str, repo: &str) -> Option<String>;
    /// confine.sh <id> <branch> <repo-path> <base> <labels> → (status, output).
    fn confine(&self, id: &str, branch: &str, repo: &Path, base: &str, labels: &str) -> (i32, String);
    /// `queue step <repo>` → its output lines; Err when there is no queue binary to run.
    fn queue_step(&self, repo: &str) -> Result<Vec<String>, String>;
    fn skew_refresh(&self, repo: &Path) -> String;
    /// An ensure program, by bare name (`unit-ensure`, `target-reap`), or a script path
    /// (systemd/ is not on PATH), when executable → its lines.
    fn ensure(&self, script: &Path) -> Vec<String>;
    /// `rebase-stale <id> <repo>` for a branch the gate found no longer merges (gate
    /// NO_VERDICT reason=conflict, gate/DESIGN.md) → its exit: 0 rebased and certified, 1
    /// reopened on a real conflict, 2 reopened on a red gate, 3 not attempted.
    fn rebase_stale(&self, id: &str, repo: &str) -> i32;

    // ── forge (sp-t4y60): land_pr's GitHub calls, the pr pass's own seam onto the forge ──

    /// `forge pr-state <repo> <selector>` → `open`/`merged`/`closed`/`unknown`; None only
    /// when the program itself could not be run (never a guess at the word).
    fn forge_pr_state(&self, repo: &Path, selector: &str) -> Option<String>;
    /// `forge pr-create <repo> <head> <base> <title>` (body on stdin) → the new PR number,
    /// or None.
    fn forge_pr_create(&self, repo: &Path, head: &str, base: &str, title: &str, body: &str) -> Option<u64>;
    /// `forge pr-list-open <repo>` → `(number, headRefName)` per open PR.
    fn forge_pr_list_open(&self, repo: &Path) -> Vec<(u64, String)>;
    /// `forge pr-automerge <repo> <selector>` → armed?
    fn forge_pr_automerge(&self, repo: &Path, selector: &str) -> bool;
}

pub trait Procs {
    /// lib.sh `holder_alive`: a live hold pidfile, or a live aeon pidfile whose process is
    /// the aeon binary (or the retired aeon.sh).
    fn holder_alive(&self, id: &str) -> bool;
}

pub trait Clock {
    fn now(&self) -> u64;
    fn sleep(&self, secs: u64);
}
