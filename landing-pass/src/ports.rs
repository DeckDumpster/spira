//! The pass's boundaries, as traits: the bead store (read only), git, the lib.sh seam, the
//! harness programs it runs, process liveness, and the clock. Production implementations are
//! in `real`; the unit tests use recording fakes (`tests`).

use crate::model::{BeadRow, Rebase, Recut};
use std::path::Path;

/// The bead store, read only. Writes go through [`Lib`].
pub trait Beads {
    /// `bd show <ids…> --json`, every row that came back, each joined to its lifecycle
    /// row's state (the read-only view design §3.4 names). Err when the store or the
    /// lifecycle machine could not be read at all — never an empty Ok.
    fn show(&self, ids: &[String]) -> Result<Vec<BeadRow>, String>;
    /// The bead's lifecycle state (spira-lc), re-read live: `-` when the machine holds no
    /// row or cannot be read. [`crate::model::handed_on`] says whether it may land.
    fn bead_lc_state(&self, id: &str) -> String;
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
    /// lib.sh `content_on_base`: the base already holds every change on the branch.
    fn content_on_base(&self, repo: &Path, branch: &str, base: &str) -> bool;
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
    /// `git log --format=%H%x09%s --grep=<grep> -F <refs…>` (family R, sp-81t4d — `landed`/
    /// `landed_sha`'s one search): "sha\tsubject" per candidate commit, newest first. `refs`
    /// empty or unresolvable is the caller's own "cannot tell" (law-closed-is-not-landed);
    /// this never guesses a ref on its own.
    fn log_grep(&self, repo: &Path, grep: &str, refs: &[String]) -> Option<String>;
    /// `git merge-base <a> <b>`; None when there is no common ancestor (or git fails).
    fn merge_base(&self, repo: &Path, a: &str, b: &str) -> Option<String>;
    /// `git log --format=%s <range> [-- <paths…>]`.
    fn log_subjects(&self, repo: &Path, range: &str, paths: &[&str]) -> Option<String>;
    /// `git log -1 --format=%B <sha>^{commit}` — the full commit message body.
    fn commit_body(&self, repo: &Path, sha: &str) -> Option<String>;
}

/// The lib.sh seam (DESIGN.md §6). Every call is one fixed script; every value on stdin.
pub trait Lib {
    fn reopen(&self, id: &str, cause: &str, note: &str);
    fn event(&self, kind: &str, id: &str, title: &str, detail: &str);
    fn noverdict(&self, id: &str, branch: &str, repo: &str, reason: &str, outcome: &str, out: &str);
    /// incident.sh file; Ok(the printed output, whose last line is the id).
    fn incident(&self, labels: &str, repo: &str, ext_ref: &str, title: &str, payload: &str) -> Result<String, i32>;
    fn ask_rebase_loop(&self, args: &[&str]);
    fn ask_red_recurring(&self, id: &str, branch: &str, repo: &str, class: &str, first_at: &str);
    fn ask_rebase_refused(&self, id: &str, branch: &str, repo: &str, reason: &str);
    fn ask_budget_deferred(&self, branch: &str, repo: &str, n: u32);
    fn ask_repo_unreadable(&self, repo: &str, path: &Path);
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

/// A pull request's red required check: the head it was red at, the failing job and the
/// failing lines of its log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrRed {
    pub head: String,
    pub jobs: Vec<String>,
    pub fail_lines: Vec<String>,
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
    /// `forge pr-mergeability <repo> <selector>` → `DIRTY`/`CLEAN`/`UNKNOWN`; None only when
    /// the program could not be run.
    fn forge_pr_mergeability(&self, repo: &Path, selector: &str) -> Option<String>;
    /// `forge pr-red <repo> <selector>` → the PR's red required check, or None when it is
    /// not red or cannot be read.
    fn forge_pr_red(&self, repo: &Path, selector: &str) -> Option<PrRed>;
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

impl PrRed {
    /// Parse `forge pr-red` output; empty output is not red.
    pub fn parse(out: &str) -> Option<PrRed> {
        let mut red = PrRed { head: String::new(), jobs: Vec::new(), fail_lines: Vec::new() };
        for l in out.lines() {
            if let Some(h) = l.strip_prefix("head ") {
                red.head = h.trim().to_string();
            } else if let Some(j) = l.strip_prefix("job ") {
                red.jobs.push(j.trim().to_string());
            } else if let Some(f) = l.strip_prefix("fail-line: ") {
                red.fail_lines.push(f.to_string());
            }
        }
        (!red.head.is_empty() && !red.jobs.is_empty()).then_some(red)
    }
}
