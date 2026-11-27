//! Contract tests (DESIGN.md §2.2, §8), one world of fakes per test: git, bd, the lib.sh
//! seam, the scripts, the forge, spira-lc, the config and the clock are traits; the queue's
//! own files live in a scratch directory.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::cli::{self, Cmd};
use crate::model::{BeadRow, LandMode, LcBeadRow, RangeCommit};
use crate::ports::*;
use crate::records::Kv;
use crate::testutil::tmpdir;

mod forge_store;
mod hand_land;
mod rebase;
mod round;
mod round_store;
mod transport;
mod verdict;

// ------------------------------------------------------------------------------- fakes

#[derive(Default)]
struct FGit {
    refs: RefCell<BTreeMap<String, String>>,
    anc: RefCell<BTreeSet<(String, String)>>,
    bases: RefCell<BTreeMap<(String, String), String>>,
    commits: RefCell<BTreeSet<String>>,
    trees: RefCell<BTreeMap<String, String>>,
    log: RefCell<Vec<RangeCommit>>,
    current: RefCell<Option<String>>,
    fetch_ok: Cell<bool>,
    /// `git branch -f` refuses a branch checked out in a worktree.
    branch_set_fails: Cell<bool>,
    merge_fail: RefCell<BTreeSet<String>>,
    /// Runs on every merge with its tip: the world moving while git works.
    on_merge: RefCell<Option<MergeHook>>,
    /// Tips whose rebase onto the round's head conflicts, with the paths it names.
    rebase_fail: RefCell<BTreeMap<String, Vec<String>>>,
    /// Tips that conflict with the base, with the paths the merge names.
    conflict_with_base: RefCell<BTreeMap<String, Vec<String>>>,
    branches: RefCell<Vec<(String, String)>>,
    /// `numstat` answers by (a, b); a pair not listed has no changes.
    #[allow(clippy::type_complexity)]
    stats: RefCell<BTreeMap<(String, String), Vec<(String, u32, u32)>>>,
    /// "rev:path" -> contents, for `path_exists` and `file_at`.
    files: RefCell<BTreeMap<String, String>>,
    /// needle -> files that hold it at any rev; "rev:needle" -> files that hold it at that rev.
    greps: RefCell<BTreeMap<String, Vec<String>>>,
    calls: RefCell<Vec<String>>,
    /// The base the last worktree was cut at, and whether a merge then names it in the head it makes.
    cut_on: RefCell<String>,
    heads_name_their_base: Cell<bool>,
    /// A worktree add makes its directory and a worktree remove deletes it, as the real git does.
    materialise: Cell<bool>,
    /// A sha `worktree_add_detached` refuses to check out.
    add_refused: RefCell<Option<String>>,
}

impl FGit {
    fn stat(&self, a: &str, b: &str, files: &[(&str, u32, u32)]) {
        self.stats.borrow_mut().insert(
            (a.into(), b.into()),
            files
                .iter()
                .map(|(p, x, y)| (p.to_string(), *x, *y))
                .collect(),
        );
    }
    fn set(&self, r: &str, sha: &str) {
        self.refs.borrow_mut().insert(r.into(), sha.into());
        self.commits.borrow_mut().insert(sha.into());
    }
    fn get(&self, r: &str) -> Option<String> {
        self.refs.borrow().get(r).cloned()
    }
    fn ancestor(&self, a: &str, b: &str) {
        self.anc.borrow_mut().insert((a.into(), b.into()));
        self.commits.borrow_mut().insert(a.into());
        self.commits.borrow_mut().insert(b.into());
    }
}

impl Git for FGit {
    fn rev_parse(&self, _repo: &Path, rev: &str) -> Option<String> {
        if let Some(t) = rev.strip_suffix("^{tree}") {
            let sha = self.rev_parse(_repo, t)?;
            return self.trees.borrow().get(&sha).cloned();
        }
        let refs = self.refs.borrow();
        refs.get(rev)
            .or_else(|| refs.get(&format!("refs/heads/{rev}")))
            .cloned()
            .or_else(|| self.commits.borrow().contains(rev).then(|| rev.to_string()))
    }
    fn is_ancestor(&self, _: &Path, a: &str, b: &str) -> bool {
        a == b || self.anc.borrow().contains(&(a.to_string(), b.to_string()))
    }
    fn base_conflict_paths(&self, _: &Path, _: &str, tip: &str) -> Option<Vec<String>> {
        self.conflict_with_base.borrow().get(tip).cloned()
    }
    fn merge_base(&self, _: &Path, a: &str, b: &str) -> Option<String> {
        let m = self.bases.borrow();
        m.get(&(a.to_string(), b.to_string()))
            .or_else(|| m.get(&(b.to_string(), a.to_string())))
            .cloned()
    }
    fn update_ref(&self, _: &Path, r: &str, new: &str, old: Option<&str>) -> bool {
        self.calls
            .borrow_mut()
            .push(format!("update-ref {r} {new} {}", old.unwrap_or("-")));
        if let Some(o) = old {
            if self.get(r).as_deref() != Some(o) {
                return false;
            }
        }
        self.set(r, new);
        true
    }
    fn ref_exists(&self, _: &Path, r: &str) -> bool {
        self.refs.borrow().contains_key(r)
    }
    fn current_branch(&self, _: &Path) -> Option<String> {
        self.current.borrow().clone()
    }
    fn fetch(&self, _: &Path, remote: &str, branch: &str) -> bool {
        self.calls
            .borrow_mut()
            .push(format!("fetch {remote} {branch}"));
        self.fetch_ok.get()
    }
    fn log_range(&self, _: &Path, range: &str) -> Result<Vec<RangeCommit>, String> {
        self.calls.borrow_mut().push(format!("log {range}"));
        Ok(self.log.borrow().clone())
    }
    fn commit_exists(&self, _: &Path, sha: &str) -> bool {
        self.commits.borrow().contains(sha)
    }
    fn numstat(&self, _: &Path, a: &str, b: &str) -> Result<Vec<(String, u32, u32)>, String> {
        Ok(self
            .stats
            .borrow()
            .get(&(a.to_string(), b.to_string()))
            .cloned()
            .unwrap_or_default())
    }
    fn path_exists(&self, _: &Path, rev: &str, path: &str) -> bool {
        self.files.borrow().contains_key(&format!("{rev}:{path}"))
    }
    fn file_at(&self, _: &Path, rev: &str, path: &str) -> Option<String> {
        self.files.borrow().get(&format!("{rev}:{path}")).cloned()
    }
    fn grep_files(&self, _: &Path, rev: &str, needle: &str, _: &str) -> Vec<String> {
        let greps = self.greps.borrow();
        greps
            .get(&format!("{rev}:{needle}"))
            .or_else(|| greps.get(needle))
            .cloned()
            .unwrap_or_default()
    }
    fn branch_set(&self, _: &Path, name: &str, sha: &str, _force: bool) -> bool {
        self.calls.borrow_mut().push(format!("branch-set {name}"));
        if self.branch_set_fails.get() {
            return false;
        }
        self.set(&format!("refs/heads/{name}"), sha);
        true
    }
    fn branch_delete(&self, _: &Path, name: &str) -> bool {
        self.calls.borrow_mut().push(format!("branch -D {name}"));
        self.refs
            .borrow_mut()
            .remove(&format!("refs/heads/{name}"))
            .is_some()
    }
    fn branch_delete_sanctioned(&self, _: &Path, name: &str) -> bool {
        self.calls
            .borrow_mut()
            .push(format!("branch -D sanctioned {name}"));
        self.refs
            .borrow_mut()
            .remove(&format!("refs/heads/{name}"))
            .is_some()
    }
    fn branches(&self, _: &Path, _prefix: &str) -> Vec<(String, String)> {
        self.branches.borrow().clone()
    }
    fn worktree_prune(&self, _: &Path) {}
    fn worktree_add_detached(&self, _: &Path, p: &Path, sha: &str) -> bool {
        if self.add_refused.borrow().as_deref() == Some(sha) {
            return false;
        }
        self.refs
            .borrow_mut()
            .insert(format!("{}:HEAD", p.display()), sha.into());
        *self.cut_on.borrow_mut() = sha.into();
        if self.materialise.get() {
            let _ = std::fs::create_dir_all(p);
        }
        true
    }
    fn worktree_reset(&self, _: &Path, p: &Path, sha: &str) -> Result<(), String> {
        if self.add_refused.borrow().as_deref() == Some(sha) {
            return Err(format!("fatal: invalid reference: {sha}"));
        }
        self.refs
            .borrow_mut()
            .insert(format!("{}:HEAD", p.display()), sha.into());
        *self.cut_on.borrow_mut() = sha.into();
        if self.materialise.get() {
            let _ = std::fs::create_dir_all(p);
        }
        Ok(())
    }
    fn worktree_remove(&self, _: &Path, p: &Path) {
        if self.materialise.get() {
            let _ = std::fs::remove_dir_all(p);
        }
        self.calls
            .borrow_mut()
            .push(format!("worktree-remove {}", p.display()));
    }
    fn merge_no_ff(
        &self,
        wt: &Path,
        msg: &str,
        tip: &str,
        _git_name: &str,
        _git_email: &str,
    ) -> bool {
        self.calls.borrow_mut().push(format!("merge {tip} {msg}"));
        if let Some(f) = self.on_merge.borrow().as_ref() {
            f(tip);
        }
        if self.merge_fail.borrow().contains(tip) {
            return false;
        }
        let base = self.cut_on.borrow().clone();
        let head = if self.heads_name_their_base.get() && base != "b0" {
            format!("merged-{tip}@{base}")
        } else {
            format!("merged-{tip}")
        };
        self.anc.borrow_mut().insert((base, head.clone()));
        self.refs.borrow_mut().insert("HEAD".into(), head);
        let _ = wt;
        true
    }
    fn merge_abort(&self, _: &Path) {}
    fn rebase_onto(
        &self,
        _: &Path,
        _: &Path,
        tip: &str,
        _: &str,
        onto: &str,
        _: &str,
        _: &str,
    ) -> Result<String, Vec<String>> {
        self.calls
            .borrow_mut()
            .push(format!("rebase-onto {tip} {onto}"));
        match self.rebase_fail.borrow().get(tip) {
            Some(paths) => Err(paths.clone()),
            None => Ok(format!("rebased-{tip}")),
        }
    }
    fn is_clean(&self, _: &Path) -> bool {
        true
    }
}

#[derive(Default)]
struct FBd {
    rows: RefCell<Vec<BeadRow>>,
    fail: Cell<bool>,
}

impl Bd for FBd {
    fn show(&self, ids: &[String]) -> Result<Vec<BeadRow>, String> {
        if self.fail.get() {
            return Err("bd down".into());
        }
        Ok(self
            .rows
            .borrow()
            .iter()
            .filter(|r| ids.contains(&r.id))
            .cloned()
            .collect())
    }
}

struct FLib {
    /// Every `notify`, as (subject, body), in order sent.
    mails: RefCell<Vec<(String, String)>>,
    s: Settings,
    r: RefCell<RepoCtx>,
    calls: RefCell<Vec<String>>,
    toml: Option<PathBuf>,
    readback: RefCell<(String, String)>,
    diverged: Cell<bool>,
    cannot_check: Cell<bool>,
    push_ok: Cell<bool>,
    /// What `create_bug` answers (None: bd created nothing).
    bug_id: RefCell<Option<String>>,
    amend_ok: Cell<bool>,
    dead_beads: RefCell<Vec<String>>,
    pf_rc: Cell<i32>,
    /// R22's answer (step --all).
    repos: RefCell<Result<Vec<String>, String>>,
    /// Per-name contexts; a name not here resolves to `r`.
    by_name: RefCell<BTreeMap<String, Result<RepoCtx, String>>>,
    /// `bead_close` fails (bd refused the close).
    close_fail: Cell<bool>,
    /// Beads whose `bead_reopen` answers false.
    reopen_fails: RefCell<BTreeSet<String>>,
}

impl FLib {
    fn log(&self, s: String) {
        self.calls.borrow_mut().push(s);
    }
    fn has(&self, prefix: &str) -> bool {
        self.calls.borrow().iter().any(|c| c.starts_with(prefix))
    }
}

impl Lib for FLib {
    fn context(&self, repo: Option<&str>) -> Result<(Settings, RepoCtx), String> {
        if let Some(r) = repo.and_then(|n| self.by_name.borrow().get(n).cloned()) {
            return r.map(|r| (self.s.clone(), r));
        }
        Ok((self.s.clone(), self.r.borrow().clone()))
    }
    fn repos(&self) -> Result<Vec<String>, String> {
        self.repos.borrow().clone()
    }
    fn toml_path(&self) -> Option<PathBuf> {
        self.toml.clone()
    }
    fn readback(&self, _: &str) -> (String, String) {
        self.readback.borrow().clone()
    }
    fn bead_reopen(&self, id: &str, cause: &str, suites: &str) -> bool {
        self.log(format!("bead_reopen {id} {cause} {suites}"));
        !self.reopen_fails.borrow().contains(id)
    }
    fn cause_event(&self, id: &str, cause: &str) {
        self.log(format!("cause_event {id} {cause}"));
    }
    fn release_claim(&self, id: &str) {
        self.log(format!("release_claim {id}"));
    }
    /// Logged as `bead_close <id> <sha>`, the sha read back out of the reason it carries.
    fn bead_close(&self, id: &str, reason: &str) -> bool {
        let sha = reason
            .split("landed at ")
            .nth(1)
            .and_then(|r| r.split_whitespace().next())
            .unwrap_or("?");
        self.log(format!("bead_close {id} {sha}"));
        !self.close_fail.get()
    }
    fn bead_retire(&self, id: &str, reason: &str) -> Result<(), String> {
        self.log(format!(
            "bead_close {id} {}",
            reason.split_whitespace().next().unwrap_or("?")
        ));
        if self.close_fail.get() {
            Err("refused".into())
        } else {
            Ok(())
        }
    }
    fn reap_landed_branch(
        &self,
        id: &str,
        repo: &str,
        branch: &str,
        why: &str,
    ) -> Result<bool, String> {
        self.log(format!("reap {id} {repo} {branch} {why}"));
        Ok(true)
    }
    fn gh_issue_closeout(&self, id: &str, sha: &str, _: &Path) {
        self.log(format!("gh_closeout {id} {sha}"));
    }
    fn comment(&self, id: &str, text: &str) {
        self.log(format!("comment {id} {text}"));
    }
    fn notify(&self, _mailbox: &str, repo: &str, subject: &str, body: &str) {
        self.log(format!("notify {repo} {subject}"));
        self.mails.borrow_mut().push((subject.to_string(), body.to_string()));
    }
    fn event(&self, kind: &str, title: &str, detail: &str) {
        self.log(format!("event {kind} {title} {detail}"));
    }
    fn divergence(
        &self,
        _mailbox: &str,
        _: &Path,
        _repo: &str,
        _path: &Path,
        forge: &str,
        local: &str,
    ) -> Divergence {
        self.log(format!("divergence {forge} {local}"));
        if self.cannot_check.get() {
            Divergence::CannotCheck("no queue dir".into())
        } else if self.diverged.get() {
            Divergence::Diverged(format!("{local}..{forge}"))
        } else {
            Divergence::Ancestor
        }
    }
    fn push(&self, _: &Path, remote: &str, refspec: &str) -> bool {
        self.log(format!("push {remote} {refspec}"));
        self.push_ok.get()
    }
    fn rebase(&self, br: &str, onto: &str, _: &Path, _: &str) -> Result<(), String> {
        self.log(format!("rebase {br} {onto}"));
        Ok(())
    }
    fn land_subject(&self, id: &str) -> String {
        format!("spira: land {id}")
    }
    fn sort_rows(
        &self,
        _express: &HashSet<String>,
        prio: &str,
        rows: &str,
    ) -> Vec<(String, String)> {
        self.log(format!("sort_rows {prio}"));
        rows.lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                Some((it.next()?.to_string(), it.next()?.to_string()))
            })
            .collect()
    }
    fn cancel_runs(&self, _: &Path, _: &Path, br: &str) {
        self.log(format!("cancel_runs {br}"));
    }
    fn format_batch(&self, _: &Path, _: &str, _: &str) {
        self.log("format_batch".into());
    }
    fn pf_gate(&self, br: &str, _: &str, _: &str, wall: u64) -> (i32, String) {
        self.log(format!("pf_gate {br} {wall}"));
        (self.pf_rc.get(), "pf output".into())
    }
    fn create_bug(
        &self,
        actor: &str,
        title: &str,
        prio: &str,
        labels: &str,
        body: &str,
    ) -> Option<String> {
        self.log(format!(
            "create_bug {actor} {prio} {labels} {title}\n{body}"
        ));
        self.bug_id.borrow().clone()
    }
    fn amend_bug(&self, actor: &str, id: &str, note: &str) -> bool {
        self.log(format!("amend_bug {actor} {id} {note}"));
        self.amend_ok.get()
    }
    fn bead_live(&self, id: &str) -> bool {
        !self.dead_beads.borrow().iter().any(|d| d == id)
    }
}

#[derive(Default)]
struct FScripts {
    gate_rc: Cell<i32>,
    /// What `testenv lint` answers by tip (absent = pass); `verify` answers only for recorded tips.
    lint_red: RefCell<BTreeMap<String, String>>,
    suite_red: RefCell<Vec<String>>,
    suite_rc: RefCell<BTreeMap<String, i32>>,
    suite_out: RefCell<BTreeMap<String, (i32, String)>>,
    /// Set once `release activate` ran; the store's fake reads it to begin its outage.
    activated: std::rc::Rc<Cell<bool>>,
    /// `release <sub>` exit status by subcommand (absent = 0), and the stderr line it prints.
    release_rc: RefCell<BTreeMap<String, (i32, String)>>,
    /// What `release build` answers on stdout (absent = the commit it was asked for).
    build_answers: RefCell<Option<String>>,
    /// When set, `release activate` records whether `spira`'s queue lock was free under it.
    lock_probe: RefCell<Option<PathBuf>>,
    lock_free_at_activate: RefCell<Vec<bool>>,
    lock_free_at_gate: RefCell<Vec<bool>>,
    during_gate: RefCell<Option<Box<dyn Fn()>>>,
    /// `release activate` answers consumed in order before `release_rc` applies.
    activate_seq: RefCell<std::collections::VecDeque<(i32, String)>>,
    /// The unit the caller runs in, and the keys `release_detached` was started under.
    caller_unit: RefCell<Option<String>>,
    /// A rolled-back activation: exits 0 and leaves `<releases>/current` where it was.
    leave_current: Cell<bool>,
    detached_keys: RefCell<Vec<String>>,
    /// `ActiveState` answers in order (the last repeats; empty reads `active`), and the units asked about.
    unit_states: RefCell<std::collections::VecDeque<String>>,
    unit_asked: RefCell<Vec<String>>,
    /// Live round legs as (repo, batch, unit); `stop_round_legs` ends the ones it is asked for.
    round_legs: RefCell<Vec<(String, String, String)>>,
    round_legs_stubborn: std::cell::Cell<bool>,
    /// `stop_round_legs` fails while `stop_pass_scopes` still works.
    step_stubborn: std::cell::Cell<bool>,
    /// Working directories of processes that outlive their legs; `stop_tree_holders` ends those under the tree.
    tree_holders: RefCell<Vec<PathBuf>>,
    /// The holders ended, each with whether the tree they held still stood when they were.
    tree_holders_stubborn: std::cell::Cell<bool>,
    stopped_holders: RefCell<Vec<(PathBuf, bool)>>,
    /// Every `release` call: the bin it was run as, its argv joined, and the SPIRA_DB it
    /// was handed.
    release_calls: RefCell<Vec<(String, String, String)>>,
    fence_ok: Cell<bool>,
    build_refused: RefCell<Option<String>>,
    calls: RefCell<Vec<String>>,
    /// What `batcher judgement-ci` answers.
    judgement: RefCell<RunOut>,
    /// What `round-vm run` answers, and the `<suite>.result` files it leaves in --results-dir.
    round_vm: RefCell<RunOut>,
    /// What `test-registry deny-list` answers.
    /// None answers an empty snapshot of that moment.
    deny_list: RefCell<Option<Result<String, String>>>,
    /// What `test-registry clear-landed` answers: the cleared test ids, or its failure.
    /// None answers that nothing was cleared.
    clear_landed: RefCell<Option<Result<Vec<String>, String>>>,
    /// Answers `round_vm` gives before it falls back to `round_vm`.
    round_vm_queue: RefCell<Vec<RunOut>>,
    /// `round-vm recipe-sha` of the release and of the round tree; None is unreadable.
    recipe_release: RefCell<Option<String>>,
    recipe_tree: RefCell<Option<String>>,
    /// `round-vm ami-for` answers in order; the last one repeats.
    ami_for: RefCell<Vec<AmiLookup>>,
    /// `image_check` answers by rev; a rev not listed is present.
    image_check: RefCell<BTreeMap<String, ImageCheck>>,
    repo_testenv: RefCell<RunOut>,
    /// The status `ci.result` carries when it is not the one the exit code implies.
    repo_ci_status: RefCell<Option<String>>,
    /// The entry never reached its verdict, so no `ci.result` is left.
    repo_no_ci_result: Cell<bool>,
    /// The `round-fault.out` left beside the results.
    repo_round_fault: RefCell<Option<String>>,
    /// Files every `repo_round_vm` pass leaves in its results.
    repo_files: RefCell<Vec<(String, String)>>,
    round_vm_results: RefCell<Vec<(String, String)>>,
    /// The `runner.meta` it leaves beside them (empty = none).
    round_vm_meta: RefCell<String>,
    round_vm_files: RefCell<Vec<(String, String)>>,
    /// Files only the next `round_vm` call writes, on top of `round_vm_files`.
    round_vm_files_once: RefCell<Vec<(String, String)>>,
    /// What a `round_vm` call that names an `only` step leaves in its results, instead of `round_vm_results` and `round_vm_files`.
    only_results: RefCell<Vec<(String, String)>>,
    only_files: RefCell<Vec<(String, String)>>,
    /// Whether the pass a round's handle names is alive.
    pass_alive: Cell<bool>,
    /// What `rebase-stale <id>` answers, by bead (absent = exit 0, nothing printed).
    rebase: RefCell<BTreeMap<String, RunOut>>,
    /// `round-vm` records the pass's verdict on the batch itself while it runs: the shared view
    /// it moves, and where to.
    round_vm_records: RefCell<Option<(SharedView, BatchView)>>,
    /// Runs while a round VM runs: what another process does in the meantime.
    during_vm: RefCell<Option<Box<dyn Fn()>>>,
}

impl Scripts for FScripts {
    fn lint(&self, tip: &str, _: &Path, verify: bool) -> (i32, String) {
        self.calls
            .borrow_mut()
            .push(format!("lint {tip} verify={verify}"));
        match self.lint_red.borrow().get(tip) {
            Some(first) => (
                1,
                format!("refused: {tip} failed its compile-and-lint check at {first}"),
            ),
            None => (0, String::new()),
        }
    }
    fn suite_run(&self, check: &str, tip: &str, repo: &str, first_check: bool, tests: &[String], deny_list: &Path) -> (i32, String) {
        let only = if tests.is_empty() { String::new() } else { format!(" tests={}", tests.join(",")) };
        self.calls.borrow_mut().push(format!(
            "suite_run {check} {tip} {repo} first={first_check}{only} deny={}",
            deny_list.display()
        ));
        if let Some(out) = self.suite_out.borrow().get(check) {
            return out.clone();
        }
        if let Some(rc) = self.suite_rc.borrow().get(check) {
            return (
                *rc,
                if *rc == 5 {
                    "VERDICT EMPTY rc=5 ran=0 skipped=1".to_string()
                } else {
                    format!("VERDICT FAULT rc={rc} ran=0 reason=preflight")
                },
            );
        }
        if self.suite_red.borrow().iter().any(|c| c == check) {
            (
                1,
                format!("{check}: 2 failed\nVERDICT RED ran=1 red={check}"),
            )
        } else {
            (0, format!("{check}: all passed"))
        }
    }
    fn gate(&self, br: &str, repo: &str, bead: &str, lock_wait: &str) -> (i32, String) {
        self.calls
            .borrow_mut()
            .push(format!("gate {br} {repo} bead={bead} lock_wait={lock_wait}"));
        if let Some(dir) = self.lock_probe.borrow().as_ref() {
            let free = matches!(
                crate::lock::try_lock(dir, "spira"),
                crate::lock::Acquire::Held(_)
            );
            self.lock_free_at_gate.borrow_mut().push(free);
        }
        if let Some(f) = self.during_gate.borrow().as_ref() {
            f();
        }
        (self.gate_rc.get(), "gate says".into())
    }
    fn judgement_ci(
        &self,
        bin: &Path,
        s: &Settings,
        repo: &str,
        suites: &str,
        members: &str,
        evidence: &str,
    ) -> RunOut {
        self.calls.borrow_mut().push(format!(
            "judgement-ci {} {repo} {suites} {members} {evidence} home={}",
            bin.display(),
            s.home.display()
        ));
        self.judgement.borrow().clone()
    }
    fn mail_operator(&self, subject: &str, body: &str) {
        self.calls
            .borrow_mut()
            .push(format!("mail-operator {subject}\n{body}"));
    }
    fn batcher_cut(&self, _: &Path, repo: &str, wz: bool) -> i32 {
        self.calls
            .borrow_mut()
            .push(format!("cut {repo} wait0={wz}"));
        0
    }
    fn path_attribution(&self, _: &Path, _: &str, _: &str, _: &Path) -> String {
        "verdict=ambiguous".into()
    }
    fn czar_fence(&self, class: &str) -> bool {
        self.calls.borrow_mut().push(format!("czar {class}"));
        self.fence_ok.get()
    }
    fn pass_alive(&self, _: u32) -> bool {
        self.pass_alive.get()
    }
    fn rebase_stale(&self, ids: &[String], repo: &str) -> Vec<(String, RunOut)> {
        ids.iter()
            .map(|id| {
                self.calls
                    .borrow_mut()
                    .push(format!("rebase-stale {id} {repo}"));
                (
                    id.clone(),
                    self.rebase.borrow().get(id).cloned().unwrap_or_default(),
                )
            })
            .collect()
    }
    fn boundary_sync(&self, tree: &Path) -> RunOut {
        self.calls
            .borrow_mut()
            .push(format!("boundary-sync {}", tree.display()));
        RunOut::default()
    }
    fn rebase_waiting_start(&self, repo: &str) -> bool {
        self.calls
            .borrow_mut()
            .push(format!("rebase-waiting-start {repo}"));
        true
    }
    fn recipe_sha(&self, tree: Option<&Path>) -> Option<String> {
        match tree {
            Some(_) => self.recipe_tree.borrow().clone(),
            None => self.recipe_release.borrow().clone(),
        }
    }
    fn image_check(&self, _: &Path, rev: &str) -> ImageCheck {
        self.calls.borrow_mut().push(format!("image-check {rev}"));
        self.image_check.borrow().get(rev).cloned().unwrap_or(ImageCheck::Present)
    }
    fn ami_for(&self, sha: &str) -> AmiLookup {
        self.calls.borrow_mut().push(format!("ami-for {sha}"));
        let mut q = self.ami_for.borrow_mut();
        match q.len() {
            0 => AmiLookup::Missing,
            1 => q[0].clone(),
            _ => q.remove(0),
        }
    }
    fn round_vm(
        &self,
        tree: &Path,
        results: &Path,
        base: &str,
        round: (&str, &str),
        wall_secs: u64,
        handle: &Path,
        ami: Option<&str>,
        candidate_recipe: Option<&str>,
        deny_list: Option<&Path>,
        host_only: bool,
        only: Option<&str>,
    ) -> RunOut {
        self.calls.borrow_mut().push(format!(
            "round-vm {} base={base} round={}/{} wall={wall_secs}{}{}{}{}{}",
            tree.display(),
            round.0,
            round.1,
            ami.map(|a| format!(" ami={a}")).unwrap_or_default(),
            candidate_recipe
                .map(|r| format!(" candidate-recipe={r}"))
                .unwrap_or_default(),
            deny_list
                .map(|d| format!(" deny-list={}", d.display()))
                .unwrap_or_default(),
            if host_only { " place=host" } else { "" },
            only.map(|o| format!(" only={o}")).unwrap_or_default()
        ));
        fs::write(handle, "777").unwrap();
        if let Some(f) = self.during_vm.borrow().as_ref() {
            f();
        }
        fs::create_dir_all(results).unwrap();
        if only.is_some() {
            for (suite, status) in self.only_results.borrow().iter() {
                fs::write(results.join(format!("{suite}.result")), format!("{status}\n")).unwrap();
            }
            for (name, body) in self.only_files.borrow().iter() {
                fs::write(results.join(name), body).unwrap();
            }
        }
        for (suite, status) in self.round_vm_results.borrow().iter().filter(|_| only.is_none()) {
            fs::write(
                results.join(format!("{suite}.result")),
                format!("{status}\n"),
            )
            .unwrap();
        }
        for (name, body) in self.round_vm_files.borrow().iter().filter(|_| only.is_none()) {
            fs::write(results.join(name), body).unwrap();
        }
        for (name, body) in self.round_vm_files_once.borrow_mut().drain(..) {
            fs::write(results.join(name), body).unwrap();
        }
        if !self.round_vm_meta.borrow().is_empty() {
            fs::write(results.join("runner.meta"), &*self.round_vm_meta.borrow()).unwrap();
        }
        if let Some((view, to)) = self.round_vm_records.borrow().as_ref() {
            *view.borrow_mut() = Some(to.clone());
        }
        let mut queued = self.round_vm_queue.borrow_mut();
        if queued.is_empty() {
            self.round_vm.borrow().clone()
        } else {
            queued.remove(0)
        }
    }
    fn clear_landed(&self, beads: &[String]) -> Result<Vec<String>, String> {
        self.calls.borrow_mut().push(format!("clear-landed {}", beads.join(",")));
        self.clear_landed.borrow().clone().unwrap_or(Ok(Vec::new()))
    }
    fn deny_list(&self, at: u64) -> Result<String, String> {
        self.calls.borrow_mut().push(format!("deny-list --at {at}"));
        self.deny_list
            .borrow()
            .clone()
            .unwrap_or_else(|| Ok(format!("# test-registry deny-list at={at} rows=0\n")))
    }
    fn repo_round_vm(
        &self,
        tree: &Path,
        results: &Path,
        entry: &gate::own::CiEntry,
        round: (&str, &str),
        wall_secs: u64,
        handle: &Path,
    ) -> RunOut {
        self.calls.borrow_mut().push(format!(
            "repo-round-vm {} entry={} image={} workdir={} round={}/{} wall={wall_secs}",
            tree.display(),
            entry.command,
            entry.image.as_deref().unwrap_or_default(),
            entry.workdir,
            round.0,
            round.1
        ));
        fs::write(handle, "777").unwrap();
        if let Some(f) = self.during_vm.borrow().as_ref() {
            f();
        }
        fs::create_dir_all(results).unwrap();
        let out = self.repo_testenv.borrow().clone();
        if let Some(fault) = self.repo_round_fault.borrow().as_ref() {
            fs::write(results.join("round-fault.out"), fault).unwrap();
        }
        for (name, body) in self.repo_files.borrow().iter() {
            fs::write(results.join(name), body).unwrap();
        }
        let status = self.repo_ci_status.borrow().clone().or_else(|| match out.rc {
            0 => Some("ok".into()),
            1 => Some("red".into()),
            _ => None,
        });
        if let Some(status) = status.filter(|_| !self.repo_no_ci_result.get()) {
            fs::write(results.join("ci.result"), format!("{status} 1 1\n")).unwrap();
        }
        out
    }
    fn stop_round_legs(&self, repo: &str, batch: &str) -> Result<Vec<String>, String> {
        if self.round_legs_stubborn.get() || self.step_stubborn.get() {
            return Err("1 still running after its stop".into());
        }
        let (mine, rest): (Vec<_>, Vec<_>) = self
            .round_legs
            .borrow()
            .iter()
            .cloned()
            .partition(|(r, b, _)| r == repo && b == batch);
        *self.round_legs.borrow_mut() = rest;
        Ok(mine.into_iter().map(|(_, _, u)| u).collect())
    }
    fn stop_tree_holders(&self, tree: &Path) -> Result<Vec<String>, String> {
        if self.tree_holders_stubborn.get() {
            return Err("1 still running after its stop".into());
        }
        let (mine, rest): (Vec<_>, Vec<_>) = self.tree_holders.borrow().iter().cloned().partition(|d| d.starts_with(tree));
        *self.tree_holders.borrow_mut() = rest;
        self.stopped_holders.borrow_mut().extend(mine.iter().map(|d| (d.clone(), tree.exists())));
        Ok(mine.iter().map(|d| d.display().to_string()).collect())
    }
    fn round_legs_live(&self, repo: &str, batch: &str) -> bool {
        self.round_legs
            .borrow()
            .iter()
            .any(|(r, b, _)| r == repo && b == batch)
    }
    fn stop_pass_scopes(&self, repo: &str, batch: &str) -> Result<Vec<String>, String> {
        if self.round_legs_stubborn.get() {
            return Err("1 still running after its stop".into());
        }
        let (mine, rest): (Vec<_>, Vec<_>) = self
            .round_legs
            .borrow()
            .iter()
            .cloned()
            .partition(|(r, b, u)| r == repo && b == batch && u.ends_with(".scope"));
        *self.round_legs.borrow_mut() = rest;
        Ok(mine.into_iter().map(|(_, _, u)| u).collect())
    }
    fn unit_active_state(&self, unit: &str) -> String {
        self.unit_asked.borrow_mut().push(unit.to_string());
        let mut q = self.unit_states.borrow_mut();
        if q.len() > 1 {
            q.pop_front().unwrap()
        } else {
            q.front().cloned().unwrap_or_else(|| "active".into())
        }
    }
    fn release_tool(&self) -> Result<PathBuf, String> {
        Ok(PathBuf::from("release"))
    }
    fn build_release(&self, wt: &Path, who: &str, _repo: &str) -> Result<Box<dyn std::any::Any>, String> {
        self.calls
            .borrow_mut()
            .push(format!("build_release {} {who}", wt.display()));
        if let Some(why) = self.build_refused.borrow().as_ref() {
            return Err(why.clone());
        }
        let dir = wt.join("target/release");
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        testkit::write_exe(dir.join("release"), "#!/bin/sh\n");
        Ok(Box::new(()))
    }
    fn own_unit_restarted_by(&self, _rel: &Path) -> Option<String> {
        self.caller_unit.borrow().clone()
    }
    fn release_detached(&self, key: &str, bin: &Path, args: &[String], db: &str, _log: &Path) -> RunOut {
        self.detached_keys.borrow_mut().push(key.to_string());
        self.release(bin, args, db)
    }
    fn release(&self, bin: &Path, args: &[String], db: &str) -> RunOut {
        self.release_calls.borrow_mut().push((
            bin.display().to_string(),
            args.join(" "),
            db.to_string(),
        ));
        self.calls.borrow_mut().push(format!("release {}", args[0]));
        if args[0] == "activate" {
            self.activated.set(true);
            if let Some(dir) = self.lock_probe.borrow().as_ref() {
                let free = matches!(
                    crate::lock::try_lock(dir, "spira"),
                    crate::lock::Acquire::Held(_)
                );
                self.lock_free_at_activate.borrow_mut().push(free);
            }
        }
        let seq = if args[0] == "activate" {
            self.activate_seq.borrow_mut().pop_front()
        } else {
            None
        };
        let (rc, err) = seq.unwrap_or_else(|| {
            self.release_rc
                .borrow()
                .get(&args[0])
                .cloned()
                .unwrap_or((0, String::new()))
        });
        let out = if args[0] == "build" && rc == 0 {
            format!(
                "{}\n",
                self.build_answers
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| args[1].clone())
            )
        } else {
            String::new()
        };
        if args[0] == "activate" && rc == 0 && !self.leave_current.get() {
            if let Some(rel) = args.iter().position(|a| a == "--releases").and_then(|i| args.get(i + 1)) {
                let cur = Path::new(rel).join("current");
                let _ = fs::remove_file(&cur);
                let _ = std::os::unix::fs::symlink(&args[1], &cur);
            }
        }
        RunOut { rc, out, err }
    }
}

#[derive(Default)]
struct FForge {
    pr: RefCell<Option<String>>,
    /// Open `spira/queue/*` PRs as `(number, branch)`.
    queue_prs: RefCell<Vec<(String, String)>>,
    /// Open `spira/publish/*` PRs as `(number, branch)`.
    publish_prs: RefCell<Vec<(String, String)>>,
    /// What `pr-checks` answers; `None` is a forge that cannot say.
    checks: RefCell<Option<Vec<(String, String)>>>,
    calls: RefCell<Vec<String>>,
    /// check-status answers in order; when none is left the answer is `pending`. A `None`
    /// entry is a failed call.
    status: RefCell<Vec<Option<String>>>,
    /// pr-state answer; `None` reads as `open`.
    pr_state: RefCell<Option<String>>,
    /// pr-merge-commit answer.
    merge_commit: RefCell<Option<String>>,
    run: RefCell<Option<String>>,
    /// `workflow-rerun` fails (the forge refused it).
    rerun_refused: RefCell<bool>,
    dispatch_refused: RefCell<bool>,
    /// `workflow-rerun` is refused as a decided run, with this reason.
    rerun_decided: RefCell<Option<String>>,
    meta: RefCell<String>,
    /// What `fail-lines` answers.
    fails: RefCell<String>,
    arm_fails: std::cell::Cell<bool>,
    /// ami-runs answers in order; when none is left no run is in progress. `None` is an unreadable forge.
    ami_runs: RefCell<Vec<Option<Vec<String>>>>,
    /// ami-result answers in order; the last one repeats. Empty is a run not started.
    ami_results: RefCell<Vec<Option<AmiBuild>>>,
    /// When set, ami-result answers a branch no ami-dispatch has named as not started.
    ami_branches_dispatched_only: Cell<bool>,
    /// pr-create makes this PR number upstream, then reports this error.
    create_then_err: RefCell<Option<(String, String)>>,
    record_at_close: RefCell<Option<(PathBuf, String)>>,
    /// pr-preflight refuses with this message.
    preflight_err: RefCell<Option<String>>,
    /// `pr-list-open` cannot be read.
    queue_prs_unreadable: Cell<bool>,
}

impl Forge for FForge {
    fn pr_preflight(&self, _: &Path, _: &Path, head: &str) -> Result<(), String> {
        self.calls.borrow_mut().push(format!("pr-preflight {head}"));
        match self.preflight_err.borrow().clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
    fn pr_create(
        &self,
        _: &Path,
        _: &Path,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<String, String> {
        self.calls
            .borrow_mut()
            .push(format!("pr-create {head} {base} {title}\n{body}"));
        if let Some((n, e)) = self.create_then_err.borrow().clone() {
            self.queue_prs.borrow_mut().push((n, head.to_string()));
            return Err(e);
        }
        self.pr
            .borrow()
            .clone()
            .ok_or_else(|| "rc=1: stub".to_string())
    }
    fn pr_automerge(&self, _: &Path, _: &Path, pr: &str) -> bool {
        self.calls.borrow_mut().push(format!("pr-automerge {pr}"));
        !self.arm_fails.get()
    }
    fn pr_close(&self, _: &Path, _: &Path, pr: &str) {
        self.calls.borrow_mut().push(format!("pr-close {pr}"));
        let mut watch = self.record_at_close.borrow_mut();
        if let Some((path, seen)) = watch.as_mut() {
            *seen = fs::read_to_string(&*path).unwrap_or_default();
        }
    }
    fn queue_prs(&self, _: &Path, _: &Path) -> Option<Vec<(String, String)>> {
        (!self.queue_prs_unreadable.get()).then(|| self.queue_prs.borrow().clone())
    }
    fn publish_prs(&self, _: &Path, _: &Path) -> Option<Vec<(String, String)>> {
        Some(self.publish_prs.borrow().clone())
    }
    fn pr_comment(&self, _: &Path, _: &Path, pr: &str, line: &str) {
        self.calls
            .borrow_mut()
            .push(format!("pr-comment {pr} {line}"));
    }
    fn branch_protect(&self, _: &Path, _: &Path, b: &str) -> bool {
        self.calls.borrow_mut().push(format!("protect {b}"));
        true
    }
    fn pr_state(&self, _: &Path, _: &Path, pr: &str) -> Option<String> {
        self.calls.borrow_mut().push(format!("pr-state {pr}"));
        Some(
            self.pr_state
                .borrow()
                .clone()
                .unwrap_or_else(|| "open".into()),
        )
    }
    fn pr_merge_commit(&self, _: &Path, _: &Path, pr: &str) -> Option<String> {
        self.calls
            .borrow_mut()
            .push(format!("pr-merge-commit {pr}"));
        self.merge_commit.borrow().clone()
    }
    fn check_status(&self, _: &Path, _: &Path, pr: &str, branch: &str) -> Option<String> {
        self.calls
            .borrow_mut()
            .push(format!("check-status {pr} {branch}"));
        let mut q = self.status.borrow_mut();
        if q.is_empty() {
            Some("pending\n".into())
        } else {
            q.remove(0)
        }
    }
    fn pr_checks(&self, _: &Path, _: &Path, pr: &str) -> Option<Vec<(String, String)>> {
        self.calls.borrow_mut().push(format!("pr-checks {pr}"));
        self.checks.borrow().clone()
    }
    fn run_id(&self, _: &Path, _: &Path, branch: &str) -> Option<String> {
        self.calls.borrow_mut().push(format!("run-id {branch}"));
        self.run.borrow().clone()
    }
    fn run_metadata(&self, _: &Path, _: &Path, run: &str) -> String {
        self.calls.borrow_mut().push(format!("run-metadata {run}"));
        self.meta.borrow().clone()
    }
    fn fail_lines(&self, _: &Path, _: &Path, run: &str, suites: &str) -> String {
        self.calls
            .borrow_mut()
            .push(format!("fail-lines {run} {suites}"));
        self.fails.borrow().clone()
    }
    fn run_cancel(&self, _: &Path, _: &Path, run: &str) {
        self.calls.borrow_mut().push(format!("run-cancel {run}"));
    }
    fn workflow_dispatch(&self, _: &Path, _: &Path, ref_: &str, _: &str) -> bool {
        self.calls.borrow_mut().push(format!("dispatch {ref_}"));
        !*self.dispatch_refused.borrow()
    }
    fn workflow_rerun(&self, _: &Path, _: &Path, run: &str) -> Rerun {
        self.calls
            .borrow_mut()
            .push(format!("workflow-rerun {run}"));
        if let Some(why) = self.rerun_decided.borrow().clone() {
            Rerun::Decided(why)
        } else if *self.rerun_refused.borrow() {
            Rerun::Failed
        } else {
            Rerun::Started
        }
    }
    fn ami_runs(&self, _: &Path, _: &Path) -> Option<Vec<String>> {
        self.calls.borrow_mut().push("ami-runs".into());
        let mut q = self.ami_runs.borrow_mut();
        if q.is_empty() {
            Some(vec![])
        } else {
            q.remove(0)
        }
    }
    fn ami_dispatch(&self, _: &Path, _: &Path, ref_: &str) -> bool {
        self.calls.borrow_mut().push(format!("ami-dispatch {ref_}"));
        true
    }
    fn ami_result(&self, _: &Path, _: &Path, branch: &str) -> Option<AmiBuild> {
        self.calls.borrow_mut().push(format!("ami-result {branch}"));
        if self.ami_branches_dispatched_only.get()
            && !self
                .calls
                .borrow()
                .contains(&format!("ami-dispatch {branch}"))
        {
            return Some(AmiBuild::NotStarted);
        }
        let mut q = self.ami_results.borrow_mut();
        match q.len() {
            0 => Some(AmiBuild::NotStarted),
            1 => q[0].clone(),
            _ => q.remove(0),
        }
    }
}

/// A batch row as `show-batch` answers it: (state, version, pass, phase).
type BatchView = (String, u64, u32, String);
/// One batch row's view, shared between the fake lifecycle and a fake that writes to it.
type SharedView = std::rc::Rc<RefCell<Option<BatchView>>>;

struct FLc {
    /// A fresh read whose argv contains `.0` is refused with `.1`.
    fresh_refused: RefCell<Option<(&'static str, String)>>,
    available: Cell<bool>,
    bead_rows: RefCell<std::collections::HashMap<String, (String, String)>>,
    rows: RefCell<Result<Vec<LcBeadRow>, String>>,
    /// A bead's row as a fresh read sees it, when it moved since the pool was listed.
    moved_rows: RefCell<std::collections::HashMap<String, LcBeadRow>>,
    /// The rows an applied `abandon-batch` leaves behind; they become `moved_rows`.
    requeue_on_abandon: RefCell<std::collections::HashMap<String, LcBeadRow>>,
    certify_refused: Cell<bool>,
    land_refused: Cell<Option<&'static str>>,
    /// Another landing records the batch first: the next record writes LANDED and is refused.
    landed_first: Cell<bool>,
    /// The next record gets no reply.
    land_no_reply_once: Cell<bool>,
    promote_refused: Cell<Option<&'static str>>,
    cut_refused: Cell<Option<&'static str>>,
    cut_refuses_member: Cell<Option<&'static str>>,
    /// Every cut applies, then answers that its reply was lost; no later read of the batch row answers.
    cut_reply_lost: Cell<bool>,
    /// The next write of this verb (cut, abandon-batch, eject-member) applies, then reports a deadline.
    deadline_after_write: Cell<Option<&'static str>>,
    /// Every cut answers "cannot tell" without writing; `apply_late_cut` is the server catching up.
    cut_applies_late: Cell<bool>,
    late_cut: RefCell<Option<LateCut>>,
    /// Per-batch answers to `show-batch`, ahead of `batch_view`.
    batch_states: RefCell<BTreeMap<String, (String, String)>>,
    /// Per-batch `progress` record of `show-batch`.
    batch_progress: RefCell<BTreeMap<String, serde_json::Value>>,
    /// Reads of `show-batch` that fail once the release is activated (the store restarting).
    /// Once the release is activated, every record step fails: the new release retired a key
    /// the running (old) release's tools still require.
    retired_keys: Cell<bool>,
    /// `show-batch` finds no row for any batch.
    no_batch_row: Cell<bool>,
    activated: std::rc::Rc<Cell<bool>>,
    eject_refused: Cell<bool>,
    eject_refused_by_a_pass_starting: Cell<bool>,
    /// Every bead event is refused with this text (a locked lifecycle store).
    event_refused: Cell<Option<&'static str>>,
    /// A batch event whose kind names this is refused.
    batch_event_refused: Cell<Option<&'static str>>,
    /// A batch event whose kind names this applies, then reports that the reply timed out.
    batch_event_times_out: Cell<Option<&'static str>>,
    /// Bead events report success and change nothing.
    event_ignored: Cell<bool>,
    /// The next bead event finds the row already moved: its version bumped, the call refused.
    event_races: RefCell<Option<(String, String)>>,
    /// The next N batch events are refused StaleVersion before they apply.
    stale_batch_events: Cell<u32>,
    /// `show <bead>` cannot answer (the bulk `list` still does).
    row_fails: Cell<bool>,
    /// When set, the batch answers (state, version, pass, phase) and follows the pass events it is sent.
    batch_view: SharedView,
    /// A SuitesStarted or Failed for a pass the batch has left (or judged) is refused as the
    /// machine refuses a late boundary.
    late_boundaries: Cell<bool>,
    /// The latest mended tip per bead.
    mend_tips: RefCell<BTreeMap<String, String>>,
    /// Set: the batch machine is on this control store, so the rounds are its `batches/` records.
    store: std::cell::OnceCell<crate::batches::mem::MemDoor>,
    /// `abandon-batch` is refused with this text.
    abandon_refused: Cell<Option<&'static str>>,
    /// The next `mending mend` is refused with this text.
    mend_refused: RefCell<Option<String>>,
    /// `lock show round/<repo>/<batch>` per `<repo>/<batch>`; absent is unleased.
    leases: RefCell<BTreeMap<String, Result<Option<RoundLease>, String>>>,
    /// Every compare-on-a-field write finds the field already moved by another writer.
    cas_lost: Cell<bool>,
    /// Integration ids whose `set-carrier --by` the fake refuses.
    carrier_refused: RefCell<Vec<String>>,
    /// Members whose record the fake already has a carrier on.
    carried: RefCell<Vec<String>>,
    calls: RefCell<Vec<String>>,
}

impl Default for FLc {
    fn default() -> Self {
        FLc {
            moved_rows: Default::default(),
            requeue_on_abandon: Default::default(),
            fresh_refused: RefCell::default(),
            available: Cell::new(false),
            bead_rows: RefCell::default(),
            rows: RefCell::new(Ok(Vec::new())),
            certify_refused: Cell::new(false),
            land_refused: Cell::new(None),
            landed_first: Cell::new(false),
            land_no_reply_once: Cell::new(false),
            promote_refused: Cell::new(None),
            cut_refused: Cell::new(None),
            cut_refuses_member: Cell::new(None),
            cut_reply_lost: Cell::new(false),
            deadline_after_write: Cell::new(None),
            cut_applies_late: Cell::new(false),
            late_cut: RefCell::default(),
            retired_keys: Cell::new(false),
            no_batch_row: Cell::new(false),
            activated: Default::default(),
            batch_states: RefCell::default(),
            eject_refused: Cell::new(false),
            eject_refused_by_a_pass_starting: Cell::new(false),
            event_refused: Cell::new(None),
            batch_event_refused: Cell::new(None),
            batch_event_times_out: Cell::new(None),
            event_ignored: Cell::new(false),
            event_races: RefCell::default(),
            stale_batch_events: Cell::new(0),
            row_fails: Cell::new(false),
            batch_view: Default::default(),
            batch_progress: Default::default(),
            late_boundaries: Cell::new(false),
            mend_tips: RefCell::default(),
            store: Default::default(),
            mend_refused: RefCell::default(),
            abandon_refused: Cell::new(None),
            leases: RefCell::default(),
            cas_lost: Cell::new(false),
            carrier_refused: RefCell::default(),
            carried: RefCell::default(),
            calls: RefCell::default(),
        }
    }
}

type LateCut = (String, String, String, Option<Kv>);

impl FLc {
    fn apply_late_cut(&self) {
        let late = self.late_cut.borrow_mut().take();
        if let (Some((repo, id, members, round)), Some(d)) = (late, self.store.get()) {
            let ms: Vec<(&str, &str)> = members.split(',').filter_map(|m| m.split_once(':')).collect();
            d.cut_round(&repo, &id, "OPEN", &ms, 0, round.as_ref());
        }
    }
    fn has(&self, prefix: &str) -> bool {
        self.calls.borrow().iter().any(|c| c.starts_with(prefix))
    }

    /// The machine moves round `id` to `state` on the store, when it holds the round.
    fn store_step(&self, id: &str, state: &str) {
        self.store_step_with(id, state, None);
    }

    /// [`FLc::store_step`], with the queue's fields of the round in the step's txn.
    fn store_step_with(&self, id: &str, state: &str, round: Option<&Kv>) {
        if let Some((repo, ..)) = self.store.get().and_then(|d| d.find(id)) {
            self.store.get().unwrap().step(&repo, id, state, round);
        }
    }
}

impl Lc for FLc {
    fn fresh_read(&self, args: &[&str]) -> Result<String, String> {
        let line = args.join(" ");
        self.calls.borrow_mut().push(format!("fresh-read {line}"));
        match &*self.fresh_refused.borrow() {
            Some((needle, why)) if line.contains(needle) => Err(why.clone()),
            _ => Ok(String::new()),
        }
    }
    fn round_lease(&self, repo: &str, batch: &str) -> Result<Option<RoundLease>, String> {
        let key = format!("{repo}/{batch}");
        self.calls
            .borrow_mut()
            .push(format!("lock-show round/{key}"));
        self.leases.borrow().get(&key).cloned().unwrap_or(Ok(None))
    }
    fn available(&self) -> bool {
        self.calls.borrow_mut().push("available".into());
        self.available.get()
    }
    fn save_batch_if(
        &self,
        repo: &str,
        kv: &Kv,
        field: &str,
        expect: &str,
    ) -> Result<bool, String> {
        if self.cas_lost.get() {
            return Ok(false);
        }
        crate::batches::save_if(
            self.rounds_door().ok_or("no store")?,
            repo,
            kv,
            field,
            expect,
        )
    }
    fn rounds_door(&self) -> Option<&dyn crate::batches::Door> {
        self.store.get().map(|d| d as &dyn crate::batches::Door)
    }
    fn requeue_orphans(
        &self,
        _: &str,
        running: &dyn Fn(&str, &str) -> bool,
    ) -> Result<String, (i32, String)> {
        self.calls.borrow_mut().push("requeue-orphans".into());
        if running("spira", "hand-1") {
            self.calls
                .borrow_mut()
                .push("requeue-orphans saw hand-1 running".into());
        }
        Ok(String::new())
    }
    fn probe(&self) -> Result<(), String> {
        self.calls.borrow_mut().push("probe".into());
        self.rows.borrow().clone().map(|_| ())
    }
    fn batch_state(&self, id: &str) -> Option<(String, String)> {
        self.calls.borrow_mut().push(format!("show-batch {id}"));
        if self.no_batch_row.get() || self.cut_reply_lost.get() || self.late_cut.borrow().is_some() {
            return None;
        }
        if let Some((_, state, version)) = self.store.get().and_then(|d| d.find(id)) {
            return Some((state, version));
        }
        if let Some(s) = self.batch_states.borrow().get(id) {
            return Some(s.clone());
        }
        match &*self.batch_view.borrow() {
            Some((s, v, _, _)) => Some((s.clone(), v.to_string())),
            None => Some(("CI_RUNNING".into(), "4".into())),
        }
    }
    fn batch_pass(&self, id: &str) -> Option<(u32, String)> {
        self.calls.borrow_mut().push(format!("show-batch {id}"));
        match &*self.batch_view.borrow() {
            Some((_, _, n, p)) => Some((*n, p.clone())),
            None => Some((1, "suites".into())),
        }
    }
    fn batch_passes(&self, id: &str) -> Vec<serde_json::Value> {
        let prefix = format!("event batch {id} ");
        let mut rows: Vec<serde_json::Value> = Vec::new();
        for call in self.calls.borrow().iter() {
            let Some(kind) = call.strip_prefix(&prefix).and_then(|c| c.splitn(3, ' ').nth(2)) else {
                continue;
            };
            let Ok(ev) = serde_json::from_str::<serde_json::Value>(kind) else { continue };
            for (name, verdict) in [("PassGreen", "green"), ("PassRed", "red")] {
                if let Some(b) = ev.get(name) {
                    rows.push(serde_json::json!({"n": b["n"], "verdict": verdict, "runner": b["runner"]}));
                }
            }
        }
        rows
    }
    fn batch_progress(&self, id: &str) -> Option<serde_json::Value> {
        self.calls.borrow_mut().push(format!("show-batch {id}"));
        self.batch_progress.borrow().get(id).cloned()
    }
    fn create_bead(&self, id: &str) {
        self.calls.borrow_mut().push(format!("create-bead {id}"));
    }
    fn create_bead_in(&self, id: &str, repo: &str) {
        self.calls
            .borrow_mut()
            .push(format!("create-bead {id} --repo {repo}"));
    }
    fn fact(&self, bead: &str, kind: &str, actor: &str, cause: &str) -> Result<(), (i32, String)> {
        self.calls.borrow_mut().push(format!("fact {bead} {kind} {actor} [{cause}]"));
        Ok(())
    }
    fn set_carrier(&self, members: &[&str], by: Option<&str>, only_if_unset: bool) -> Result<String, (i32, String)> {
        let tail = by.map_or("--clear".to_string(), |b| format!("--by {b}"));
        let only = if only_if_unset { " --only-if-unset" } else { "" };
        self.calls.borrow_mut().push(format!("set-carrier {} {tail}{only}", members.join(" ")));
        if by.is_some_and(|b| self.carrier_refused.borrow().iter().any(|r| r == b)) {
            return Err((1, format!("set-carrier: no lifecycle record for {}", members.last().unwrap_or(&""))));
        }
        let carried = self.carried.borrow();
        Ok(members.iter().filter(|m| carried.iter().any(|c| c == *m)).map(|m| format!("kept {m}\n")).collect())
    }
    fn cut(
        &self,
        id: &str,
        repo: &str,
        _: &str,
        _: &str,
        members: &str,
        _: &str,
        round: Option<&Kv>,
    ) -> Result<String, (i32, String)> {
        self.calls.borrow_mut().push(format!("cut {id} {members}"));
        if let Some(why) = self.cut_refused.get() {
            return Err((3, why.into()));
        }
        if let Some(bad) = self
            .cut_refuses_member
            .get()
            .filter(|b| members.split(',').any(|m| m.split(':').next() == Some(*b)))
        {
            return Err((2, format!("cut: Deliver refused unexpectedly for {bad}: Some(EjectedRedTip {{ tip: \"x\" }})")));
        }
        if self.cut_applies_late.get() {
            *self.late_cut.borrow_mut() =
                Some((repo.into(), id.into(), members.into(), round.cloned()));
            return Err((2, "cannot tell: deadline".into()));
        }
        self.batch_view
            .borrow_mut()
            .get_or_insert(("OPEN".into(), 1, 0, String::new()));
        if let Some(d) = self.store.get() {
            let ms: Vec<(&str, &str)> = members
                .split(',')
                .filter_map(|m| m.split_once(':'))
                .collect();
            d.cut_round(repo, id, "OPEN", &ms, 0, round);
        }
        if self.cut_reply_lost.get() {
            return Err((2, "cannot tell: the lifecycle service took the request but its reply was lost (no reply)".into()));
        }
        if self.deadline_after_write.get() == Some("cut") {
            self.deadline_after_write.set(None);
            return Err((2, "cannot tell: deadline".into()));
        }
        Ok("1".into())
    }
    fn stage(
        &self,
        id: &str,
        _: &str,
        head: &str,
        base: &str,
        members: &str,
        parent: &str,
        _: &str,
    ) -> Result<String, (i32, String)> {
        self.calls.borrow_mut().push(format!(
            "stage {id} {head} {base} {members} behind {parent}"
        ));
        Ok("0".into())
    }
    fn promote(
        &self,
        id: &str,
        head: &str,
        base: &str,
        _: &str,
        round: Option<&Kv>,
    ) -> Result<(), (i32, String)> {
        self.calls
            .borrow_mut()
            .push(format!("promote {id} {head} {base}"));
        match self.promote_refused.get() {
            Some(why) => Err((3, why.into())),
            None => {
                self.store_step_with(id, "OPEN", round);
                Ok(())
            }
        }
    }
    fn abandon_batch(
        &self,
        id: &str,
        s: &str,
        v: &str,
        _: &str,
        r: &str,
    ) -> Result<(), (i32, String)> {
        self.calls
            .borrow_mut()
            .push(format!("abandon-batch {id} {s} {v} {r}"));
        if let Some(why) = self.abandon_refused.get() {
            return Err((3, why.into()));
        }
        self.store_step(id, "ABANDONED");
        self.moved_rows
            .borrow_mut()
            .extend(self.requeue_on_abandon.borrow_mut().drain());
        if self.deadline_after_write.get() == Some("abandon-batch") {
            self.deadline_after_write.set(None);
            self.batch_states
                .borrow_mut()
                .insert(id.into(), ("ABANDONED".into(), "9".into()));
            return Err((2, "cannot tell: deadline".into()));
        }
        Ok(())
    }
    fn eject_member(
        &self,
        id: &str,
        bead: &str,
        s: &str,
        v: &str,
        _: &str,
        r: &str,
    ) -> Result<(), (i32, String)> {
        self.calls
            .borrow_mut()
            .push(format!("eject-member {id} {bead} {s} {v} {r}"));
        if self.deadline_after_write.get() == Some("eject-member") {
            self.deadline_after_write.set(None);
            return Err((2, "cannot tell: deadline".into()));
        }
        if self.eject_refused_by_a_pass_starting.get() {
            *self.batch_view.borrow_mut() = Some(("CI_RUNNING".into(), 5, 2, "suites".into()));
            return Err((3, "refused: Some(PassRunning)".into()));
        }
        if self.eject_refused.get() {
            return Err((3, "IllegalTransition".into()));
        }
        Ok(())
    }
    fn batch_event(
        &self,
        id: &str,
        s: &str,
        v: &str,
        _: &str,
        kind: &str,
        round: Option<&Kv>,
    ) -> Result<(), (i32, String)> {
        self.calls
            .borrow_mut()
            .push(format!("event batch {id} {s} {v} {kind}"));
        if self.stale_batch_events.get() > 0 {
            self.stale_batch_events
                .set(self.stale_batch_events.get() - 1);
            return Err((1, "refused: StaleVersion { given: 4, current: 5 }".into()));
        }
        if self
            .batch_event_refused
            .get()
            .is_some_and(|k| kind.contains(k))
        {
            return Err((1, "refused: IllegalTransition".into()));
        }
        if let (true, Some((state, _, pass, _))) =
            (self.late_boundaries.get(), &*self.batch_view.borrow())
        {
            let boundary = ["{\"SuitesStarted\"", "{\"Failed\""]
                .iter()
                .any(|b| kind.starts_with(b))
                || (kind.starts_with("{\"PassIncomplete\"") && state == "ATTRIBUTING");
            let n: u32 = kind
                .split("\"n\":")
                .nth(1)
                .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|d| d.parse().ok())
                .unwrap_or(0);
            if boundary && (n < *pass || (n == *pass && state != "CI_RUNNING")) {
                return Err((3, format!("refused: Some(LateBoundary {{ state: \"{state} (pass {pass})\", event: \"{kind}\", pass: {n} }})")));
            }
        }
        if let Some(view) = self.batch_view.borrow_mut().as_mut() {
            let n: u32 = kind
                .split("\"n\":")
                .nth(1)
                .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|d| d.parse().ok())
                .unwrap_or(view.2);
            let (state, pass, phase) = match kind
                .trim_start_matches("{\"")
                .split('"')
                .next()
                .unwrap_or("")
            {
                "PassStarted" => ("CI_RUNNING", n, "build"),
                "SuitesStarted" => ("CI_RUNNING", view.2, "suites"),
                "PassGreen" => ("GREEN", view.2, ""),
                "PassRed" | "PassIncomplete" => ("ATTRIBUTING", view.2, ""),
                "PassRebuilt" => ("OPEN", view.2, ""),
                "" if kind == "\"BaseMoved\"" => ("REBUILDING", view.2, ""),
                _ => (view.0.as_str(), view.2, view.3.as_str()),
            };
            *view = (state.to_string(), view.1 + 1, pass, phase.to_string());
            self.store_step_with(id, &view.0, round);
        } else if let (Some(_), Some((_, state, _))) =
            (round, self.store.get().and_then(|d| d.find(id)))
        {
            self.store_step_with(id, &state, round);
        }
        if self
            .batch_event_times_out
            .get()
            .is_some_and(|k| kind.contains(k))
        {
            return Err((2, "cannot tell: deadline".into()));
        }
        Ok(())
    }
    fn bead_state(&self, bead: &str) -> Option<(String, String)> {
        self.calls.borrow_mut().push(format!("show {bead}"));
        Some(
            self.bead_rows
                .borrow()
                .get(bead)
                .cloned()
                .unwrap_or(("CERTIFIED".into(), "3".into())),
        )
    }
    fn bead_event(
        &self,
        bead: &str,
        s: &str,
        v: &str,
        _: &str,
        kind: &str,
    ) -> Result<(), (i32, String)> {
        self.calls
            .borrow_mut()
            .push(format!("event bead {bead} {s} {v} {kind}"));
        if let Some((state, version)) = self.event_races.borrow_mut().take() {
            self.bead_rows
                .borrow_mut()
                .insert(bead.into(), (state, version));
            return Err((1, "stale version".into()));
        }
        match self.event_refused.get() {
            Some(why) => Err((1, why.into())),
            None => {
                if kind.contains("batch-ejected-red") {
                    if let Ok(rows) = self.rows.borrow_mut().as_mut() {
                        if let Some(r) = rows.iter_mut().find(|r| r.bead_id == bead) {
                            r.ejected_red_tip = r.tip.clone();
                        }
                    }
                }
                if kind.contains("\"GateRed\"") && !self.event_ignored.get() {
                    self.bead_rows
                        .borrow_mut()
                        .insert(bead.into(), ("REWORK".into(), "4".into()));
                }
                Ok(())
            }
        }
    }
    fn land_batch(&self, id: &str, v: &str, _: &str, sha: &str) -> Result<(), (i32, String)> {
        self.calls.borrow_mut().push(format!("land {id} {v} {sha}"));
        if self.activated.get() && self.retired_keys.get() {
            return Err((2, "SPIRA_RETIRED_KEY is not declared".into()));
        }
        if self.land_no_reply_once.replace(false) {
            return Err((1, "no reply".into()));
        }
        if self.landed_first.get() {
            self.store_step(id, "LANDED");
            self.batch_states
                .borrow_mut()
                .insert(id.into(), ("LANDED".into(), "5".into()));
            return Err((3, "refused: Some(ExpectMismatch { expected: \"GREEN\", actual: \"LANDED\" })".into()));
        }
        match self.land_refused.get() {
            Some(why) => Err((3, why.into())),
            None => {
                self.store_step(id, "LANDED");
                Ok(())
            }
        }
    }
    fn bead_rows(&self, state: Option<&str>) -> Result<Vec<LcBeadRow>, String> {
        self.calls
            .borrow_mut()
            .push(format!("list {}", state.unwrap_or("")));
        self.rows.borrow().clone().map(|r| {
            r.into_iter()
                .filter(|b| state.is_none_or(|s| b.state == s))
                .collect()
        })
    }
    fn bead_rows_of(&self, ids: &[String]) -> Result<Vec<LcBeadRow>, String> {
        self.calls.borrow_mut().push(format!("list --ids {}", ids.join(",")));
        self.rows.borrow().clone().map(|r| r.into_iter().filter(|b| ids.contains(&b.bead_id)).collect())
    }
    fn express_ids(&self) -> Result<Vec<String>, String> {
        self.calls.borrow_mut().push("list --express".into());
        Ok(Vec::new())
    }
    fn bead_row(&self, bead: &str) -> Option<LcBeadRow> {
        self.calls.borrow_mut().push(format!("row {bead}"));
        if self.row_fails.get() {
            return None;
        }
        if let Some(moved) = self.moved_rows.borrow().get(bead) {
            return Some(moved.clone());
        }
        self.rows
            .borrow()
            .clone()
            .ok()?
            .into_iter()
            .find(|b| b.bead_id == bead)
    }
    fn mending_close_ended(&self, _: &str) -> Result<(), (i32, String)> {
        self.calls.borrow_mut().push("mending-close-ended".into());
        if self.activated.get() && self.retired_keys.get() {
            return Err((2, "SPIRA_RETIRED_KEY is not declared".into()));
        }
        Ok(())
    }
    fn mending_event(
        &self,
        id: &str,
        pass: u32,
        suite: &str,
        kind: &str,
        _: &str,
    ) -> Result<(), (i32, String)> {
        self.calls
            .borrow_mut()
            .push(format!("mending-event {id} {pass} {suite} {kind}"));
        Ok(())
    }
    fn mend(
        &self,
        id: &str,
        pass: u32,
        suite: &str,
        members: &str,
        facts: &str,
        fences: &str,
        actor: &str,
    ) -> Result<(), (i32, String)> {
        self.calls.borrow_mut().push(format!(
            "mend {id} {pass} {suite} {members} facts={facts} fences={fences} by={actor}"
        ));
        match self.mend_refused.borrow().clone() {
            Some(why) => Err((3, why)),
            None => Ok(()),
        }
    }
    fn mend_tips(&self, _: &[String]) -> Result<BTreeMap<String, String>, String> {
        Ok(self.mend_tips.borrow().clone())
    }
    fn certify(&self, bead: &str, tip: &str, _: &str, _: &str) -> Result<(), (i32, String)> {
        self.calls
            .borrow_mut()
            .push(format!("certify {bead} {tip}"));
        if self.certify_refused.get() {
            return Err((3, "refused".into()));
        }
        Ok(())
    }
    fn retip(
        &self,
        bead: &str,
        tip: &str,
        actor: &str,
        reason: &str,
        evidence: &str,
        evidence_tip: &str,
    ) -> Result<(), (i32, String)> {
        self.calls.borrow_mut().push(format!(
            "retip {bead} {tip} {actor} [{reason}] [{evidence}] {evidence_tip}"
        ));
        let mut rows = self.rows.borrow_mut();
        let Ok(rows) = rows.as_mut() else {
            return Err((2, "cannot tell".into()));
        };
        let Some(r) = rows.iter_mut().find(|r| r.bead_id == bead) else {
            return Err((1, "no row".into()));
        };
        if r.state == "REWORK" && !r.holds.iter().any(|h| h == "manual") {
            return Err((3, "refused: NotManuallyHeld".into()));
        }
        r.state = "SUBMITTED".into();
        r.tip = Some(tip.into());
        if r.ejected_red_tip.as_deref() == Some(tip) {
            r.ejected_red_tip = None;
        }
        Ok(())
    }
    fn absolve(
        &self,
        bead: &str,
        tip: &str,
        actor: &str,
        reason: &str,
        evidence: &str,
        evidence_tip: &str,
    ) -> Result<(), (i32, String)> {
        self.calls.borrow_mut().push(format!(
            "absolve {bead} {tip} {actor} [{reason}] [{evidence}] {evidence_tip}"
        ));
        let mut rows = self.rows.borrow_mut();
        let Ok(rows) = rows.as_mut() else {
            return Err((2, "cannot tell".into()));
        };
        let Some(r) = rows.iter_mut().find(|r| r.bead_id == bead) else {
            return Err((1, "no row".into()));
        };
        if r.ejected_red_tip.as_deref() != Some(tip) {
            return Err((3, "refused: illegal".into()));
        }
        r.ejected_red_tip = None;
        if r.state == "REWORK" {
            r.state = "SUBMITTED".into();
        }
        Ok(())
    }
}

#[derive(Default)]
struct FConfig {
    rows: RefCell<BTreeMap<String, (String, String)>>,
    legacy: RefCell<BTreeMap<String, (String, String)>>,
    legacy_fail: Cell<bool>,
}

impl ConfigStore for FConfig {
    fn repo_row(&self, _: &Path, name: &str) -> Result<(String, String), String> {
        Ok(self.rows.borrow().get(name).cloned().unwrap_or_default())
    }
    fn set_repo_row(&self, _: &Path, name: &str, m: &str, b: &str) -> Result<(), String> {
        self.rows
            .borrow_mut()
            .insert(name.into(), (m.into(), b.into()));
        Ok(())
    }
    fn set_legacy_map_row(&self, _: &Path, name: &str, l: &str, b: &str) -> Result<(), String> {
        if self.legacy_fail.get() {
            return Err("legacy map write failed".into());
        }
        self.legacy
            .borrow_mut()
            .insert(name.into(), (l.into(), b.into()));
        Ok(())
    }
}

type MergeHook = Box<dyn Fn(&str)>;

struct FClock {
    now: std::rc::Rc<Cell<u64>>,
    /// Milliseconds the world has moved beyond `now`'s whole seconds.
    extra_ms: std::rc::Rc<Cell<u64>>,
    /// Runs on every sleep: the world moving while a verb waits.
    on_sleep: RefCell<Option<Box<dyn Fn()>>>,
}

impl Clock for FClock {
    fn now(&self) -> u64 {
        self.now.get()
    }
    fn now_ms(&self) -> u64 {
        self.now.get() * 1000 + self.extra_ms.get()
    }
    fn stamp(&self) -> String {
        "20260929T010203Z".into()
    }
    fn sleep(&self, s: u64) {
        self.now.set(self.now.get() + s.max(1));
        if let Some(f) = self.on_sleep.borrow().as_ref() {
            f();
        }
    }
}

#[derive(Default)]
struct FEnv {
    vars: RefCell<BTreeMap<String, String>>,
    resolved: RefCell<BTreeMap<String, String>>,
    stdin: RefCell<String>,
    /// Config keys that do not resolve.
    unresolved: RefCell<BTreeSet<String>>,
}

impl Env for FEnv {
    fn var(&self, k: &str) -> Option<String> {
        self.vars.borrow().get(k).cloned()
    }
    fn pid(&self) -> u32 {
        4242
    }
    fn read_stdin(&self) -> Result<String, String> {
        Ok(self.stdin.borrow().clone())
    }
    fn read_file(&self, p: &Path) -> Result<String, String> {
        fs::read_to_string(p).map_err(|e| e.to_string())
    }
    fn config(&self, k: &str) -> Result<String, String> {
        if self.unresolved.borrow().contains(k) {
            return Err(format!("{k} is not declared"));
        }
        Ok(self
            .resolved
            .borrow()
            .get(k)
            .cloned()
            .or_else(|| self.var(k))
            .unwrap_or_default())
    }
}

#[derive(Default)]
struct Cap {
    out: RefCell<String>,
    err: RefCell<String>,
}

impl Emit for Cap {
    fn out(&self, s: &str) {
        self.out.borrow_mut().push_str(s);
    }
    fn err(&self, s: &str) {
        self.err.borrow_mut().push_str(s);
    }
}

struct T {
    dir: testkit::TempDir,
    git: FGit,
    bd: FBd,
    lib: FLib,
    scripts: FScripts,
    forge: FForge,
    lc: FLc,
    config: FConfig,
    clock: FClock,
    env: FEnv,
    io: Cap,
    rounds: crate::round_bus::RoundBus,
}

const REPO: &str = "repo";

impl T {
    fn new(mode: LandMode) -> T {
        let dir = tmpdir("ops");
        let s = Settings {
            home: dir.join("home"),
            run: dir.join("run"),
            queue_dir: dir.join("run/queue"),
            releases: Some(dir.join("releases")),
            forge: dir.join("home/forge.sh"),
            repo_map: None,
            batcher_bin: None,
            batcher_off: false,
            lc_bin: None,
            submitted_label: "spira-submitted".into(), // literal-ok: fixture vocabulary
            // The harness repository: not "spira", so the default fixture's landings are of an
            // ordinary queue.local repository and publish no release; the §8 D13 tests make
            // "spira" the harness (harness_world).
            home_repo: "harness".into(),
            db: "db".into(),
            transition_pollsec: 5,
            transition_maxsec: 30,
            preflight_wall_secs: 240,
            verdict: VerdictSettings::default(),
            git_name: "spira".into(),
            git_email: "spira@spira.invalid".into(),
            mailbox: "concierge".into(),
            round_wall_secs: 900,
            results_retention: spira_config::results_retention::Retention { keep_days: 7, max: 50 },
            round_ami_wait_secs: 5400,
            round_spill: String::new(),
            rounds_max: 1,
            text_only: vec!["law/".into(), "docs/".into()],
            publish_red_priority: "1".into(),
            publish_red_escalate_after: 3,
            publish_defer_max: Some(3),
            publish_no_run_secs: Some(600),
            gate_timeout: Some(2700),
            gate_lock_wait: Some("0".into()),
        };
        fs::create_dir_all(s.queue_dir.join("spira")).unwrap();
        let landref = match mode {
            LandMode::QueueLocal => "local/main",
            _ => "origin/main",
        };
        let r = RepoCtx {
            name: "spira".into(),
            path: Some(PathBuf::from(REPO)),
            mode,
            landref: Some(landref.into()),
            map_land: mode.config_word().into(),
            map_base: landref.into(),
            publish: Some(("origin".into(), "main".into())),
            unpublished: false,
            publish_mode: spira_config::PublishMode::Pr,
            remotes: vec!["origin".into()],
        };
        let git = FGit::default();
        git.fetch_ok.set(true);
        let lib = FLib {
            mails: RefCell::default(),
            s,
            r: RefCell::new(r),
            calls: RefCell::default(),
            toml: None,
            readback: RefCell::default(),
            diverged: Cell::new(false),
            cannot_check: Cell::new(false),
            push_ok: Cell::new(true),
            bug_id: RefCell::new(Some("sp-fix1".into())),
            amend_ok: Cell::new(true),
            dead_beads: RefCell::new(Vec::new()),
            pf_rc: Cell::new(0),
            repos: RefCell::new(Ok(vec!["spira".into()])),
            by_name: RefCell::default(),
            close_fail: Cell::new(false),
            reopen_fails: RefCell::default(),
        };
        let scripts = FScripts::default();
        scripts.fence_ok.set(true);
        let forge = FForge::default();
        *forge.pr.borrow_mut() = Some("77".into());
        let lc = FLc::default();
        lc.available.set(true);
        let lc = FLc {
            activated: scripts.activated.clone(),
            ..lc
        };
        T {
            dir,
            git,
            bd: FBd::default(),
            lib,
            scripts,
            forge,
            lc,
            config: FConfig::default(),
            clock: FClock {
                now: std::rc::Rc::new(Cell::new(1_000)),
                extra_ms: Default::default(),
                on_sleep: RefCell::default(),
            },
            env: FEnv::default(),
            io: Cap::default(),
            rounds: crate::round_bus::RoundBus::off(),
        }
    }

    fn run(&self, args: &[&str]) -> i32 {
        self.run_on(&self.git, args)
    }

    /// `f` on the world a verb would run in, for a library call below the verbs.
    fn with_world<R>(&self, f: impl FnOnce(&World) -> R) -> R {
        f(&World {
            git: &self.git,
            bd: &self.bd,
            lib: &self.lib,
            scripts: &self.scripts,
            forge: &self.forge,
            lc: &self.lc,
            config: &self.config,
            clock: &self.clock,
            env: &self.env,
            io: &self.io,
            rounds: &self.rounds,
        })
    }

    /// The args as given: no actor is added to a round verb.
    fn run_bare(&self, args: &[&str]) -> i32 {
        self.run_argv(&self.git, args.iter().map(|s| s.to_string()).collect())
    }

    /// As given, with `--by batcher` on a round verb that needs an actor and names none.
    fn run_on(&self, git: &dyn Git, args: &[&str]) -> i32 {
        let mut argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let acted = [
            "open",
            "certify",
            "eject",
            "land",
            "abandon",
            "stage",
            "stage-test",
            "promote",
            "discard",
        ];
        if argv.first().is_some_and(|a| a == "round")
            && argv.get(1).is_some_and(|v| acted.contains(&v.as_str()))
            && !argv.iter().any(|a| a == "--by" || a.starts_with("--by="))
        {
            argv.extend(["--by".to_string(), "batcher".to_string()]);
        }
        self.run_argv(git, argv)
    }

    fn run_argv(&self, git: &dyn Git, argv: Vec<String>) -> i32 {
        let cmd = match cli::parse(&argv) {
            Ok(c) => c,
            Err(cli::Usage(m)) => {
                self.io.err(&format!("{m}\n"));
                return 2;
            }
        };
        let w = World {
            git,
            bd: &self.bd,
            lib: &self.lib,
            scripts: &self.scripts,
            forge: &self.forge,
            lc: &self.lc,
            config: &self.config,
            clock: &self.clock,
            env: &self.env,
            io: &self.io,
            rounds: &self.rounds,
        };
        crate::dispatch(&w, &cmd)
    }

    fn s(&self) -> &Settings {
        &self.lib.s
    }
    fn qfile(&self, n: &str) -> PathBuf {
        self.s().queue_dir.join("spira").join(n)
    }
    fn out(&self) -> String {
        self.io.out.borrow().clone()
    }
    fn err(&self) -> String {
        self.io.err.borrow().clone()
    }
    /// A spira-lc bead row entered at `since`.
    fn lc_row(&self, id: &str, state: &str, tip: &str, since: u64) {
        let row = LcBeadRow {
            bead_id: id.into(),
            state: state.into(),
            tip: Some(tip.into()),
            since: Some(since),
            blocked_by: Vec::new(),
            reason: None,
            holds: Vec::new(),
            stack: Default::default(),
            ejected_red_tip: None,
            holder: None,
        };
        self.lc.rows.borrow_mut().as_mut().unwrap().push(row);
    }
    /// A SUBMITTED row whose reason says its gate certifies nothing.
    fn lc_row_certifies_nothing(&self, id: &str, tip: &str, since: u64) {
        self.lc_row(id, "SUBMITTED", tip, since);
        let mut rows = self.lc.rows.borrow_mut();
        rows.as_mut().unwrap().last_mut().unwrap().reason = Some("gate-certifies-nothing".into());
    }
    /// `id` is IN_DELIVERY, and the abandon requeues it to `state`.
    fn lc_requeues_to(&self, id: &str, tip: &str, reason: Option<&str>, state: &str) {
        self.lc_row(id, "IN_DELIVERY", tip, 1);
        let mut rows = self.lc.rows.borrow_mut();
        let row = rows.as_mut().unwrap().last_mut().unwrap();
        row.reason = reason.map(Into::into);
        let mut after = row.clone();
        after.state = state.into();
        self.lc.requeue_on_abandon.borrow_mut().insert(id.into(), after);
    }
    fn open_record(&self, text: &str) {
        fs::write(self.qfile("open"), text).unwrap();
    }
    fn var(&self, k: &str, v: &str) {
        self.env.vars.borrow_mut().insert(k.into(), v.into());
    }
    fn landing_log(&self) -> String {
        fs::read_to_string(self.s().run.join("landing.log")).unwrap_or_default()
    }
    fn hold_lock(&self) -> crate::lock::Guard {
        match crate::lock::try_lock(&self.s().queue_dir, "spira") {
            crate::lock::Acquire::Held(g) => g,
            _ => panic!("lock"),
        }
    }
}

// ------------------------------------------------------------------------------ submit

// covers: UC-landing-merge-queue-27
#[test_id(t18lvra)]
#[test]
fn submit_queue_mode_refuses_a_branch_outside_spira() {
    let t = T::new(LandMode::QueueLocal);
    assert_eq!(t.run(&["submit", "feature/x"]), 1);
    assert!(t
        .err()
        .contains("queue mode requires a branch under spira/"));
    assert!(t.scripts.calls.borrow().is_empty());
}

/// A queued change is a bead on spira/<id>; a bead-less branch, certified only in a queue
/// record, is refused.
// covers: UC-landing-merge-queue-27
#[test_id(tjgd231)]
#[test]
fn submit_refuses_a_bead_less_branch() {
    for mode in [LandMode::Queue, LandMode::QueueLocal] {
        let t = T::new(mode);
        t.git
            .set("refs/heads/spira-adhoc/test-a-20260101T000000Z", "t1");
        assert_eq!(
            t.run(&["submit", "spira-adhoc/test-a-20260101T000000Z"]),
            1
        );
        assert!(
            t.err()
                .contains("queue mode requires a branch under spira/"),
            "{}",
            t.err()
        );
        assert!(t.scripts.calls.borrow().is_empty(), "no gate ran");
        assert!(!t.lc.has("certify"));
        assert!(
            !t.s().queue_dir.join("spira-adhoc").exists(),
            "no queue record"
        );
    }
}

// covers: UC-landing-merge-queue-27
#[test_id(t6z4ivk)]
#[test]
fn submit_red_gate_logs_caught_and_propagates_the_gate_code() {
    let t = T::new(LandMode::Queue);
    t.git.set("refs/heads/spira/sp-a", "t1");
    t.scripts.gate_rc.set(75);
    assert_eq!(t.run(&["submit", "spira/sp-a"]), 75);
    assert!(t
        .err()
        .contains("queue.sh submit: spira/sp-a failed the gate (NO_VERDICT)"));
    assert!(t.err().contains("gate says"));
    assert!(t.landing_log().contains("QUEUE CAUGHT 1000 branch=sp-a"));
    assert!(!t.lc.has("certify") && !t.lc.has("event"));
}

#[test_id(tc19qs5)]
#[test]
fn submit_of_a_tip_that_fails_its_lint_is_refused_naming_file_and_line_before_the_gate() {
    let t = T::new(LandMode::Queue);
    t.git.set("refs/heads/spira/sp-a", "t1");
    t.scripts
        .lint_red
        .borrow_mut()
        .insert("t1".into(), "src/binder.rs:222:9".into());
    assert_eq!(t.run(&["submit", "spira/sp-a"]), 1);
    assert!(t.err().contains("src/binder.rs:222:9"), "{}", t.err());
    assert!(
        !t.scripts
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("gate ")),
        "no gate ran"
    );
    assert!(!t.lc.has("certify"));

    t.scripts.lint_red.borrow_mut().clear();
    assert_eq!(t.run(&["submit", "spira/sp-a"]), 0, "{}", t.err());
    assert!(t.lc.has("certify sp-a t1"));
}

#[test_id(thbmh59)]
#[test]
fn submit_push_mode_rebases_pushes_and_closes() {
    let t = T::new(LandMode::Push);
    t.submitted(&["sp-a"]);
    t.git.set("refs/heads/spira/sp-a", "t1");
    assert_eq!(t.run(&["submit", "spira/sp-a"]), 0);
    assert!(t.lib.has("rebase spira/sp-a origin/main"));
    assert!(t.lib.has("push origin spira/sp-a:main"));
    assert!(t.lc.has("certify sp-a t1"));
    assert!(t.lib.has("bead_close sp-a t1"));
    assert!(t.out().contains("landed spira/sp-a (push)"));
}

#[test_id(tcvm0p6)]
#[test]
fn submit_pr_mode_certifies_and_names_the_mode() {
    let t = T::new(LandMode::Pr);
    t.git.set("refs/heads/topic", "t9");
    assert_eq!(t.run(&["submit", "topic"]), 0);
    assert!(t.out().contains("queue.sh submit: certified topic (pr)"));
}

#[test_id(t6i7qtt)]
#[test]
fn submit_missing_branch_is_refused() {
    let t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["submit", "spira/sp-zz"]), 1);
    assert!(t.err().contains("branch not found: spira/sp-zz"));
}

// --------------------------------------------------------------- protect/flush/step

#[test_id(tontkra)]
#[test]
fn protect_writes_the_receipt_only_for_queue_forge() {
    let t = T::new(LandMode::QueueLocal);
    assert_eq!(t.run(&["protect"]), 1);
    assert!(t
        .err()
        .contains("repo is not in queue mode (mode=queue.local)"));
    let t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["protect"]), 0);
    assert_eq!(
        fs::read_to_string(t.s().run.join("queue-protected-spira")).unwrap(),
        "main\n"
    );
    assert_eq!(t.out().lines().count(), 5);
}

#[test_id(tp1gv7v)]
#[test]
fn flush_with_the_batcher_switched_off_cuts_nothing_and_succeeds() {
    let mut t = T::new(LandMode::Queue);
    t.lib.s.batcher_off = true;
    assert_eq!(t.run(&["flush"]), 0);
    assert!(t.out().contains("SPIRA_BATCHER_ENABLE=0"), "{}", t.out());
    let calls = t.scripts.calls.borrow().clone();
    assert!(!calls.iter().any(|c| c.starts_with("cut ")), "{calls:?}");
}

#[test_id(t22by65)]
#[test]
fn step_queue_local_publishes_with_stderr_folded_into_stdout() {
    let t = T::new(LandMode::QueueLocal);
    t.git.set("refs/remotes/origin/main", "f0");
    t.git.set("refs/heads/local/main", "f0");
    let rc = t.run(&["step", "spira"]);
    assert_eq!(rc, 0);
    assert!(
        t.scripts.calls.borrow().is_empty(),
        "{:?}",
        t.scripts.calls.borrow()
    );
    assert!(t.out().contains("nothing to publish for spira"));
}

#[test_id(t32uufo)]
#[test]
fn step_never_cuts_a_round_even_with_a_batcher_program() {
    for mode in [LandMode::QueueLocal, LandMode::Queue] {
        let mut t = T::new(mode);
        t.lib.s.batcher_bin = Some(std::path::PathBuf::from("batcher"));
        t.run(&["step", "spira"]);
        t.run(&["step", "--all"]);
        let calls = t.scripts.calls.borrow().clone();
        assert!(!calls.iter().any(|c| c.starts_with("cut ")), "{calls:?}");
    }
}

#[test_id(tdwtl4n)]
#[test]
fn step_on_queue_forge_is_status_zero_whatever_the_cut_did() {
    let t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["step", "spira"]), 0);
}

// ------------------------------------------------------------------------ step --all

impl T {
    /// Each id is an open bead carrying the submitted label (and its repo:/branch: labels):
    /// the shape a landed member's close acts on.
    fn submitted(&self, ids: &[&str]) {
        for id in ids {
            self.bd.rows.borrow_mut().push(BeadRow {
                id: (*id).into(),
                status: Some("open".into()),
                labels: vec![
                    "spira-submitted".into(),
                    "repo:spira".into(),
                    format!("branch:spira/{id}"),
                ], // literal-ok: fixture vocabulary
                ..Default::default()
            });
        }
    }
}

impl T {
    /// Register a repository `name` in `mode` for step --all.
    fn repo(&self, name: &str, mode: LandMode) {
        let mut r = self.lib.r.borrow().clone();
        r.name = name.into();
        r.mode = mode;
        if mode == LandMode::QueueLocal {
            r.landref = Some("local/main".into());
            r.map_base = "local/main".into();
        }
        // A queue.forge repository gets an open batch named after it, so the in-process
        // verdict's check-status call shows which repositories a step reached.
        if mode == LandMode::Queue {
            let d = self.lib.s.queue_dir.join(name);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("open"), format!("pr={name}\nhead=h\nbase=b\nmembers=sp-x:tx\nopened=1000\nbranch=spira/queue/{name}\n")).unwrap();
        }
        self.lib.by_name.borrow_mut().insert(name.into(), Ok(r));
    }
    /// The queue.forge repositories whose verdict ran (see `repo`).
    fn verdicts(&self) -> Vec<String> {
        self.forge
            .calls
            .borrow()
            .iter()
            .filter_map(|c| c.strip_prefix("check-status "))
            .map(|c| format!("verdict {}", c.split(' ').next().unwrap_or("")))
            .collect()
    }
}

#[test_id(tbemhup)]
#[test]
fn step_all_parses_and_refuses_a_repository_beside_it() {
    let p = |a: &[&str]| cli::parse(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert_eq!(p(&["step", "--all"]), Ok(Cmd::StepAll));
    assert_eq!(
        p(&["step", "spira"]),
        Ok(Cmd::Step {
            repo: "spira".into()
        })
    );
    assert_eq!(
        p(&["step", "--all", "spira"]),
        Err(cli::Usage(
            "queue.sh step: --all takes no repository".into()
        ))
    );
    assert_eq!(
        p(&["step", "--bogus"]),
        Err(cli::Usage("queue.sh step: unknown option: --bogus".into()))
    );
    assert_eq!(
        p(&["step"]),
        Err(cli::Usage("queue.sh step: repo required".into()))
    );
    assert!(cli::USAGE.contains("queue.sh step --all"));
}

#[test_id(th9xu4w)]
#[test]
fn step_all_steps_every_queue_mode_repo_in_order_and_skips_the_rest() {
    let t = T::new(LandMode::Queue);
    t.repo("spira", LandMode::QueueLocal);
    t.repo("svc", LandMode::Queue);
    t.repo("pushy", LandMode::Push);
    t.repo("held", LandMode::Hold);
    t.lib
        .by_name
        .borrow_mut()
        .insert("broken".into(), Err("seam exited 1".into()));
    *t.lib.repos.borrow_mut() = Ok(vec![
        "spira".into(),
        "pushy".into(),
        "broken".into(),
        "svc".into(),
        "held".into(),
    ]);
    t.git.set("refs/remotes/origin/main", "f0");
    t.git.set("refs/heads/local/main", "f0");
    assert_eq!(t.run(&["step", "--all"]), 0);
    // svc's open batch was judged (pending); spira (queue.local) had no publish record
    assert_eq!(t.verdicts(), vec!["verdict svc"]);
    assert!(
        t.out().contains("verdict svc: PR svc pending (run age 0s)"),
        "{}",
        t.out()
    );
    let out = t.out();
    assert!(
        out.contains("queue.sh step --all: spira\n") && out.contains("queue.sh step --all: svc\n")
    );
    assert!(!out.contains("step --all: pushy") && !out.contains("step --all: held"));
    // queue.local still publishes inside the step
    assert!(out.contains("nothing to publish for spira"));
    assert!(t
        .err()
        .contains("cannot resolve the harness configuration: seam exited 1"));
    assert!(out.ends_with("queue.sh step --all: stepped=2 busy=0 unresolved=1 stepped:spira,svc unresolved:broken\n"), "{out}");
}

#[test_id(tnr2bbu)]
#[test]
fn step_all_does_not_propagate_one_repos_failed_step() {
    let t = T::new(LandMode::Queue);
    // queue.local with no local base: the publish inside the step refuses (status 1)
    t.repo("spira", LandMode::QueueLocal);
    t.repo("svc", LandMode::Queue);
    *t.lib.repos.borrow_mut() = Ok(vec!["spira".into(), "svc".into()]);
    // positive control in its own world (its own lock files): the single step does fail
    let c = T::new(LandMode::Queue);
    c.repo("spira", LandMode::QueueLocal);
    assert_ne!(c.run(&["step", "spira"]), 0);
    assert_eq!(t.run(&["step", "--all"]), 0);
    assert_eq!(t.verdicts(), vec!["verdict svc"]);
}

#[test_id(tadz1n4)]
#[test]
fn step_all_without_a_repository_list_is_a_failure_not_an_empty_pass() {
    let t = T::new(LandMode::Queue);
    *t.lib.repos.borrow_mut() = Err("lib.sh repos seam exited 1 with 0 name(s)".into());
    assert_eq!(t.run(&["step", "--all"]), 1);
    assert!(t
        .err()
        .contains("queue.sh step --all: cannot list repositories: lib.sh repos seam exited 1"));
    assert!(t.verdicts().is_empty());
}

#[test_id(tq615o5)]
#[test]
fn step_all_skips_a_repo_another_stepper_holds_and_steps_the_others() {
    let t = T::new(LandMode::Queue);
    t.repo("spira", LandMode::Queue);
    t.repo("svc", LandMode::Queue);
    *t.lib.repos.borrow_mut() = Ok(vec!["spira".into(), "svc".into()]);
    let held = crate::lock::try_step_lock(&t.lib.s.queue_dir, "svc");
    assert!(matches!(held, crate::lock::Acquire::Held(_)));
    assert_eq!(t.run(&["step", "--all"]), 0);
    assert_eq!(t.verdicts(), vec!["verdict spira"]);
    assert!(t
        .err()
        .contains("queue.sh step: another step holds the step lock for svc — skipped"));
    assert!(t
        .out()
        .ends_with("stepped=1 busy=1 unresolved=0 stepped:spira busy:svc\n"));
    drop(held);
    t.forge.calls.borrow_mut().clear();
    assert_eq!(t.run(&["step", "--all"]), 0);
    assert_eq!(
        t.verdicts(),
        vec!["verdict spira", "verdict svc"],
        "{}",
        t.err()
    );
}

#[test_id(tndl0al)]
#[test]
fn step_all_probes_spira_lc_and_refuses_loudly_when_unreachable() {
    let t = T::new(LandMode::Queue);
    t.repo("spira", LandMode::Queue);
    *t.lib.repos.borrow_mut() = Ok(vec!["spira".into()]);
    assert_eq!(t.run(&["step", "--all"]), 0);
    assert!(t.lc.calls.borrow().contains(&"probe".to_string()));
    for args in [
        &["step", "--all"][..],
        &["step", "spira"][..],
        &["flush"][..],
    ] {
        let t = T::new(LandMode::Queue);
        *t.lc.rows.borrow_mut() = Err("dolt down".into());
        t.repo("spira", LandMode::Queue);
        *t.lib.repos.borrow_mut() = Ok(vec!["spira".into()]);
        assert_eq!(t.run(args), 1, "{args:?}");
        assert!(
            t.err()
                .contains("spira-lc is unreachable (dolt down) — refused, nothing changed"),
            "{}",
            t.err()
        );
        assert!(t.scripts.calls.borrow().is_empty(), "{args:?} ran a child");
    }
}

#[test_id(teul82l)]
#[test]
fn a_step_never_holds_the_queue_lock_verdict_and_batch_take() {
    let t = T::new(LandMode::Queue);
    t.repo("spira", LandMode::Queue);
    // the verdict and batch.sh take <repo>/lock themselves; the step must leave it free
    let _q = crate::lock::try_lock(&t.lib.s.queue_dir, "spira");
    assert!(matches!(_q, crate::lock::Acquire::Held(_)));
    assert_eq!(t.run(&["step", "spira"]), 0);
    // the verdict waited SPIRA_QUEUE_LOCK_WAIT for the lock, then skipped its turn
    assert!(
        t.out()
            .contains("verdict spira: another queue operation holds the lock"),
        "{}",
        t.out()
    );
    assert!(t.verdicts().is_empty());
    drop(_q);
    assert_eq!(t.run(&["step", "spira"]), 0);
    assert_eq!(t.verdicts(), vec!["verdict spira"]);
}

// ------------------------------------------------------------------------------- eject

fn open_batch_record(t: &T) {
    t.open_record("pr=12\nhead=h1\nbase=b0\nmembers=sp-a:ta sp-b:tb\nopened=5\nbranch=spira/queue/x\nowner=batcher\n");
}

#[test_id(tscy395)]
#[test]
fn eject_with_a_locked_lifecycle_store_exits_nonzero_naming_the_lock() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    t.lc.event_refused.set(Some("database is locked"));
    assert_eq!(t.run(&["eject", "sp-a"]), 1);
    assert!(
        t.err().contains("database is locked") && t.err().contains("sp-a"),
        "{}",
        t.err()
    );
    assert!(
        !t.out().contains("ejected sp-a from spira batch"),
        "{}",
        t.out()
    );
}

// covers: UC-landing-merge-queue-29
#[test_id(t1ovqwy)]
#[test]
fn eject_member_records_the_harness_cause_and_returns_it_to_rework_on_spira_lc() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    assert_eq!(t.run(&["eject", "sp-a", "--reason", "needs rebase"]), 0);
    assert!(t.lib.has("cause_event sp-a eject"));
    let calls = t.lc.calls.borrow().clone();
    let d = calls
        .iter()
        .position(|c| c == "event bead sp-a CERTIFIED 3 \"Deliver\"")
        .unwrap_or_else(|| panic!("no Deliver: {calls:?}"));
    let r = calls
        .iter()
        .position(|c| {
            c == "event bead sp-a IN_DELIVERY 4 {\"Returned\":{\"reason\":\"batch-ejected\"}}"
        })
        .unwrap_or_else(|| panic!("no Returned: {calls:?}"));
    assert!(d < r);
    assert!(t.lib.has("release_claim sp-a"));
    assert!(!t.lib.has("bead_reopen"));
    assert!(t.forge.calls.borrow().contains(&"pr-close 12".to_string()));
    assert!(!t.qfile("open").exists());
    assert!(t.lib.has("notify spira sp-a ejected (queue.sh eject)"));
    assert!(t
        .out()
        .contains("queue.sh eject: ejected sp-a from spira batch"));
}

#[test_id(tkaj389)]
#[test]
fn eject_red_records_eject_red() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    assert_eq!(t.run(&["eject", "sp-a", "--suites", "test-x.sh"]), 0);
    assert!(t.lib.has("cause_event sp-a eject-red"));
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    assert_eq!(
        t.run(&["eject", "sp-a", "--red", "--suites", "test-x.sh"]),
        0
    );
    assert!(t.lib.has("cause_event sp-a eject-red"));
    let calls = t.lc.calls.borrow().clone();
    assert!(
        calls.contains(
            &"event bead sp-a IN_DELIVERY 4 {\"Returned\":{\"reason\":\"batch-ejected-red\"}}"
                .to_string()
        ),
        "{calls:?}"
    );
}

#[test_id(tivwf8h)]
#[test]
fn eject_of_a_certified_unbatched_bead_reopens_with_the_cause() {
    let t = T::new(LandMode::Queue);
    t.lc_row("sp-c", "CERTIFIED", "tc", 5);
    assert_eq!(
        t.run(&[
            "eject",
            "sp-c",
            "--red",
            "--suites",
            "test-x.sh",
            "--reason-file",
            "-"
        ]),
        0
    );
    assert!(t.lib.has("bead_reopen sp-c eject-red "));
    assert!(t
        .out()
        .contains("ejected sp-c (certified, not yet batched) for spira (withdrawn)"));
}

#[test_id(tk63quq)]
#[test]
fn eject_of_a_certified_unbatched_bead_exits_0_when_the_reopen_could_not_tell_but_the_row_reads_rework() {
    let t = T::new(LandMode::Queue);
    t.lc_row("sp-c", "CERTIFIED", "tc", 5);
    t.lib.reopen_fails.borrow_mut().insert("sp-c".into());
    assert_eq!(
        t.run(&["eject", "sp-c", "--red", "--suites", "test-x.sh", "--reason-file", "-"]),
        0,
        "{}",
        t.err()
    );
    assert!(t.out().contains("ejected sp-c"), "{}", t.out());
    assert!(t.err().contains("bead_reopen did not complete for sp-c"), "{}", t.err());
    let calls = t.lc.calls.borrow().clone();
    assert!(calls.iter().any(|c| c.contains("\"Deliver\"")), "{calls:?}");
    assert!(calls.iter().any(|c| c.contains("batch-ejected-red")), "{calls:?}");
}

#[test_id(t2lv7zz)]
#[test]
fn eject_exits_1_naming_the_state_read_when_the_reopen_failed_and_deliver_is_refused() {
    let t = T::new(LandMode::Queue);
    t.lc_row("sp-s", "SUBMITTED", "ts", 5);
    t.lib.reopen_fails.borrow_mut().insert("sp-s".into());
    t.lc.event_refused.set(Some("refused"));
    assert_eq!(t.run(&["eject", "sp-s", "--red", "--suites", "test-x.sh", "--reason-file", "-"]), 1);
    assert!(t.err().contains("not returned to REWORK on spira-lc"), "{}", t.err());
}

#[test_id(tvklyvs)]
#[test]
fn eject_of_a_submitted_unbatched_bead_withdraws_it_to_rework_at_its_red_tip() {
    let t = T::new(LandMode::Queue);
    t.lc_row("sp-s", "SUBMITTED", "ts", 5);
    assert_eq!(
        t.run(&[
            "eject",
            "sp-s",
            "--red",
            "--suites",
            "test-x.sh",
            "--reason-file",
            "-"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(t.lib.has("bead_reopen sp-s eject-red "));
    let calls = t.lc.calls.borrow().clone();
    assert!(
        calls
            .iter()
            .any(|c| c.contains("sp-s ") && c.contains("Deliver")),
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c.contains("sp-s IN_DELIVERY") && c.contains("batch-ejected-red")),
        "{calls:?}"
    );
}

#[test_id(t23pnk7)]
#[test]
fn eject_of_a_certified_unbatched_bead_is_not_blocked_by_an_unrelated_open_batch() {
    let t = T::new(LandMode::Queue);
    let rec = "pr=12\nmembers=sp-a:ta,sp-b:tb\nbatch_id=spira-1\nversion=3\n";
    t.open_record(rec);
    t.lc_row("sp-c", "CERTIFIED", "tc", 5);
    assert_eq!(
        t.run(&[
            "eject",
            "sp-c",
            "--red",
            "--suites",
            "test-x.sh",
            "--reason-file",
            "-"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(t.lib.has("bead_reopen sp-c eject-red "));
    assert_eq!(fs::read_to_string(t.qfile("open")).unwrap(), rec);
    assert!(!t.lc.has("abandon-batch"));
}

#[test_id(tqha8am)]
#[test]
fn ejecting_a_non_member_returns_it_to_rework_and_leaves_the_open_batch_and_its_pr_alone() {
    let t = T::new(LandMode::Queue);
    let rec = "pr=460\nmembers=sp-a:ta,sp-b:tb\nbatch_id=spira-1\nversion=3\n";
    t.open_record(rec);
    t.lc_row("sp-c", "CERTIFIED", "tc", 5);
    assert_eq!(
        t.run(&[
            "eject",
            "sp-c",
            "--red",
            "--suites",
            "test-x.sh",
            "--reason",
            "withdrawn"
        ]),
        0,
        "{}",
        t.err()
    );
    let calls = t.lc.calls.borrow().join("\n");
    assert!(
        calls.contains("event bead sp-c") && calls.contains("Returned"),
        "{calls}"
    );
    assert!(
        !calls.contains("abandon-batch") && !calls.contains("eject-member"),
        "{calls}"
    );
    assert_eq!(fs::read_to_string(t.qfile("open")).unwrap(), rec);
    assert!(
        !t.forge
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("pr-close")),
        "{:?}",
        t.forge.calls.borrow()
    );
}

#[test_id(t7kvq4s)]
#[test]
fn eject_takes_a_comma_separated_suites_list_and_still_refuses_a_bad_name_in_it() {
    let t = T::new(LandMode::Queue);
    t.lc_row("sp-c", "CERTIFIED", "tc", 5);
    assert_eq!(
        t.run(&["eject", "sp-c", "--suites", "test-x.sh,test-y.sh"]),
        0
    );
    assert!(t.lib.has("bead_reopen sp-c eject-red test-x.sh,test-y.sh"));
    let t = T::new(LandMode::Queue);
    t.lc_row("sp-c", "CERTIFIED", "tc", 5);
    assert_ne!(
        t.run(&["eject", "sp-c", "--suites", "test-x.sh,bad name.sh"]),
        0
    );
    assert!(!t.lib.has("bead_reopen"));
    let t = T::new(LandMode::Queue);
    t.lc_row("sp-c", "CERTIFIED", "tc", 5);
    assert_ne!(
        t.run(&["eject", "sp-c", "--suites", "test-x.sh,"]),
        0,
        "an empty name in the list is refused"
    );
}

// covers: UC-landing-merge-queue-29
#[test_id(twkqyrt)]
#[test]
fn eject_of_a_stranger_names_the_members_and_changes_nothing() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    assert_eq!(t.run(&["eject", "sp-z"]), 1);
    assert!(t.err().contains(
        "sp-z is not a member of the open batch for spira and is not CERTIFIED or SUBMITTED"
    ));
    assert!(t.err().contains("batch members: sp-a sp-b"));
    assert!(t.qfile("open").exists());
    assert!(t.lib.calls.borrow().is_empty());
}

#[test_id(t6l9jen)]
#[test]
fn eject_refuses_a_concierge_owned_batch_unless_overridden() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=12\nmembers=sp-a:ta\nowner=concierge\n");
    assert_eq!(t.run(&["eject", "sp-a"]), 1);
    assert!(t.err().contains("queue.sh eject: refused — this batch is claimed by concierge; override with SPIRA_QUEUE_OWNER_OVERRIDE=1"));
    t.var("SPIRA_QUEUE_OWNER_OVERRIDE", "1");
    assert_eq!(t.run(&["eject", "sp-a"]), 0);
}

#[test_id(thnjqkc)]
#[test]
fn eject_with_a_batch_id_ejects_on_spira_lc_too() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=12\nmembers=sp-a:ta\nbatch_id=spira-1\nversion=3\n");
    assert_eq!(t.run(&["eject", "sp-a", "--reason", "multi\nline"]), 0);
    assert!(t
        .lc
        .calls
        .borrow()
        .contains(&"eject-member spira-1 sp-a CI_RUNNING 4 multi line".to_string()));
}

#[test_id(tqkdf5m)]
#[test]
fn eject_cancels_the_closed_prs_runs_and_only_its_own_branch() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    assert_eq!(t.run(&["eject", "sp-a", "--reason", "x"]), 0, "{}", t.err());
    let calls = t.lib.calls.borrow().clone();
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.starts_with("cancel_runs"))
            .collect::<Vec<_>>(),
        ["cancel_runs spira/queue/x"],
        "{calls:?}"
    );
}

#[test_id(tosgk6b)]
#[test]
fn eject_of_a_record_with_no_branch_cancels_nothing() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=12\nmembers=sp-a:ta\nbatch_id=spira-1\nversion=3\n");
    assert_eq!(t.run(&["eject", "sp-a", "--reason", "x"]), 0, "{}", t.err());
    assert!(!t.lib.has("cancel_runs"));
}

#[test_id(tgnztfm)]
#[test]
fn a_forge_eject_returns_the_member_to_rework_only_after_eject_member_requeues_it() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=12\nmembers=sp-a:ta sp-b:tb\nbatch_id=spira-1\nversion=3\n");
    t.lc.bead_rows
        .borrow_mut()
        .insert("sp-a".into(), ("IN_DELIVERY".into(), "7".into()));
    assert_eq!(
        t.run(&[
            "eject",
            "sp-a",
            "--red",
            "--suites",
            "test-x.sh",
            "--reason",
            "PR red"
        ]),
        0,
        "{}",
        t.err()
    );
    let calls = t.lc.calls.borrow().clone();
    let e = calls
        .iter()
        .position(|c| c.starts_with("eject-member spira-1 sp-a"))
        .unwrap_or_else(|| panic!("{calls:?}"));
    let r = calls
        .iter()
        .position(|c| c.contains("event bead sp-a") && c.contains("Returned"))
        .unwrap_or_else(|| panic!("no Returned: {calls:?}"));
    assert!(e < r, "{calls:?}");
}

#[test_id(t7lrnwn)]
#[test]
fn a_forge_eject_whose_return_write_is_refused_exits_nonzero_naming_the_write() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=12\nmembers=sp-a:ta\nbatch_id=spira-1\nversion=3\n");
    t.lc.bead_rows
        .borrow_mut()
        .insert("sp-a".into(), ("IN_DELIVERY".into(), "7".into()));
    t.lc.event_refused.set(Some("stale version"));
    assert_ne!(
        t.run(&[
            "eject",
            "sp-a",
            "--red",
            "--suites",
            "test-x.sh",
            "--reason",
            "PR red"
        ]),
        0
    );
    assert!(t.err().contains("Returned refused"), "{}", t.err());
}

#[test_id(tybqoqy)]
#[test]
fn a_forge_harness_fault_eject_leaves_the_member_certified() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=12\nmembers=sp-a:ta sp-b:tb\nbatch_id=spira-1\nversion=3\n");
    assert_eq!(
        t.run(&[
            "eject",
            "sp-a",
            "--harness-fault",
            "--reason",
            "runner died"
        ]),
        0,
        "{}",
        t.err()
    );
    let calls = t.lc.calls.borrow().join("\n");
    assert!(calls.contains("eject-member spira-1 sp-a"), "{calls}");
    assert!(!calls.contains("event bead sp-a"), "{calls}");
}

#[test_id(tc71epr)]
#[test]
fn eject_leaving_a_member_abandons_the_batch_row_so_the_survivor_is_not_stranded() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=12\nmembers=sp-a:ta sp-b:tb\nbatch_id=spira-1\nversion=3\n");
    t.lc_requeues_to("sp-b", "tb", Some("delivered-from-submitted"), "SUBMITTED");
    assert_eq!(
        t.run(&["eject", "sp-a", "--reason", "bad"]),
        0,
        "{}",
        t.err()
    );
    let out = t.out();
    assert!(out.contains("queue.sh eject: sp-b returned to SUBMITTED"), "{out}");
    assert!(!out.contains("CERTIFIED"), "{out}");
    let mails = format!("{:?}", t.lib.mails.borrow());
    assert!(mails.contains("Survivors requeued: sp-b:SUBMITTED"), "{mails}");
    assert!(!mails.contains("CERTIFIED"), "{mails}");
    let calls = t.lc.calls.borrow().clone();
    let e = calls
        .iter()
        .position(|c| c.starts_with("eject-member spira-1 sp-a"))
        .unwrap_or_else(|| panic!("{calls:?}"));
    let a = calls
        .iter()
        .position(|c| c.starts_with("abandon-batch spira-1"))
        .unwrap_or_else(|| panic!("no abandon: {calls:?}"));
    assert!(e < a);
    assert!(t.forge.calls.borrow().contains(&"pr-close 12".to_string()));
    assert!(!t.qfile("open").exists());
}

#[test_id(tcq9nx7)]
#[test]
fn eject_of_a_batch_with_no_lifecycle_row_sweeps_the_orphans_it_left() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=12\nmembers=sp-a:ta sp-b:tb\nbatch_id=spira-1\nversion=3\n");
    t.lc.no_batch_row.set(true);
    assert_eq!(
        t.run(&["eject", "sp-a", "--reason", "bad"]),
        1,
        "{}",
        t.err()
    );
    assert!(
        t.err().contains("no readable lifecycle row"),
        "POSITIVE CONTROL: the abandon was refused: {}",
        t.err()
    );
    assert!(t.lc.has("requeue-orphans"), "{:?}", t.lc.calls.borrow());
}

#[test_id(tc6ra23)]
#[test]
fn an_eject_that_settles_runs_no_sweep() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=12\nmembers=sp-a:ta sp-b:tb\nbatch_id=spira-1\nversion=3\n");
    assert_eq!(
        t.run(&["eject", "sp-a", "--reason", "bad"]),
        0,
        "{}",
        t.err()
    );
    assert!(!t.lc.has("requeue-orphans"), "{:?}", t.lc.calls.borrow());
}

// covers: UC-landing-merge-queue-29
#[test_id(tgwx30u)]
#[test]
fn eject_of_the_last_member_abandons_the_emptied_batch_row() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=12\nmembers=sp-a:ta\nbatch_id=spira-1\nversion=3\n");
    assert_eq!(t.run(&["eject", "sp-a"]), 0);
    assert!(t.lc.has("abandon-batch spira-1"));
    assert!(!t.qfile("open").exists());
}

#[test_id(t314z0p)]
#[test]
fn eject_of_the_last_member_retires_the_mender_beads_filed_for_the_batch() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=12\nmembers=sp-a:ta\nbatch_id=spira-1\nversion=3\n");
    let rounds = t.s().run.join("rounds");
    fs::create_dir_all(&rounds).unwrap();
    fs::write(rounds.join("spira-1.mender-1-gate-lint"), "sp-m1\n").unwrap();
    fs::write(rounds.join("other-9.mender-1-gate-lint"), "sp-m2\n").unwrap();
    assert_eq!(t.run(&["eject", "sp-a"]), 0, "{}", t.err());
    assert!(t.lib.has("bead_close sp-m1"), "{:?}", t.lib.calls.borrow());
    assert!(!t.lib.has("bead_close sp-m2"), "{:?}", t.lib.calls.borrow());
    assert!(rounds.join("spira-1.mender-1-gate-lint.retired").exists());
}

#[test_id(t8zi0eh)]
#[test]
fn spira_lc_unreachable_refuses_loudly_and_changes_nothing() {
    for args in [
        &["eject", "sp-a"][..],
        &["abandon", "--reason", "x"][..],
        &["open-batch", "--skip-pregate"][..],
    ] {
        let t = T::new(LandMode::Queue);
        *t.lc.rows.borrow_mut() = Err("Access denied".into());
        open_batch_record(&t);
        assert_eq!(t.run(args), 1, "{args:?}");
        assert!(
            t.err()
                .contains("spira-lc is unreachable (Access denied) — refused, nothing changed"),
            "{}",
            t.err()
        );
        assert!(
            t.lib.calls.borrow().is_empty(),
            "{args:?} changed something"
        );
        assert!(t.qfile("open").exists());
    }
    // no binary at all is the same refusal
    let t = T::new(LandMode::Queue);
    t.lc.available.set(false);
    open_batch_record(&t);
    assert_eq!(t.run(&["eject", "sp-a"]), 1);
    assert!(t.err().contains("no spira-lc program"));
}

#[test_id(te1cthx)]
#[test]
fn eject_of_a_member_already_in_delivery_returns_without_a_second_deliver() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    t.lc.bead_rows
        .borrow_mut()
        .insert("sp-a".into(), ("IN_DELIVERY".into(), "7".into()));
    assert_eq!(t.run(&["eject", "sp-a"]), 0);
    let calls = t.lc.calls.borrow().join("\n");
    assert!(!calls.contains("\"Deliver\""), "{calls}");
    assert!(
        calls.contains(
            "event bead sp-a IN_DELIVERY 7 {\"Returned\":{\"reason\":\"batch-ejected\"}}"
        ),
        "{calls}"
    );
}

#[test_id(t0q34ih)]
#[test]
fn eject_of_a_certified_unbatched_bead_delivers_then_returns_on_spira_lc() {
    let t = T::new(LandMode::Queue);
    t.lc_row("sp-c", "CERTIFIED", "tc", 1);
    assert_eq!(t.run(&["eject", "sp-c"]), 0, "{}", t.err());
    let calls = t.lc.calls.borrow().clone();
    assert!(
        calls.contains(&"event bead sp-c CERTIFIED 3 \"Deliver\"".to_string()),
        "{calls:?}"
    );
    assert!(
        calls.contains(
            &"event bead sp-c IN_DELIVERY 4 {\"Returned\":{\"reason\":\"batch-ejected\"}}"
                .to_string()
        ),
        "{calls:?}"
    );
}

#[test_id(tiohn96)]
#[test]
fn a_lock_held_past_the_deadline_refuses_naming_its_recorded_holder() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    let _g = match crate::lock::try_lock_as(&t.s().queue_dir, "spira", "round certify") {
        crate::lock::Acquire::Held(g) => g,
        _ => panic!("lock"),
    };
    let start = t.clock.now();
    assert_eq!(t.run(&["eject", "sp-a"]), crate::ops::LOCK_BUSY);
    let want = "verb=round certify".to_string();
    assert!(
        t.err().contains(&want) && t.err().contains("pid="),
        "{}",
        t.err()
    );
    assert_eq!(
        t.clock.now() - start,
        t.s().verdict.lock_wait,
        "waited the whole deadline"
    );
}

#[test_id(tya038z)]
#[test]
fn eject_refuses_while_another_queue_operation_holds_the_lock() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    let _g = t.hold_lock();
    assert_eq!(t.run(&["eject", "sp-a"]), crate::ops::LOCK_BUSY);
    assert!(t
        .err()
        .contains("another queue operation holds the lock for spira"));
}

#[test_id(t5pheks)]
#[test]
fn eject_refuses_an_id_that_is_not_an_identifier() {
    let t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["eject", "sp-a;rm"]), 1);
    assert!(t.err().contains("refused"));
}

#[test_id(t9ciz6x)]
#[test]
fn czar_fence_refusal_stops_eject_before_anything() {
    let t = T::new(LandMode::Queue);
    t.var("SPIRA_RUNE", "czar");
    t.var("SPIRA_CZAR_CLASS", "deadlock");
    t.scripts.fence_ok.set(false);
    open_batch_record(&t);
    assert_eq!(t.run(&["eject", "sp-a"]), 1);
    assert_eq!(t.scripts.calls.borrow()[0], "czar deadlock");
    assert!(t.qfile("open").exists());
}

// ----------------------------------------------------------------------------- abandon

// covers: UC-landing-merge-queue-29
#[test_id(tg6faku)]
#[test]
fn abandon_requires_a_reason() {
    let t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["abandon"]), 2);
    assert!(t.err().contains("--reason is required"));
}

#[test_id(tkvtgmf)]
#[test]
fn abandon_with_lifecycle_on_abandons_the_batch_on_spira_lc() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=12\nmembers=sp-a:ta\nbatch_id=spira-1\nversion=3\n");
    assert_eq!(t.run(&["abandon", "--reason", "dirty"]), 0);
    assert!(t
        .lc
        .calls
        .borrow()
        .contains(&"abandon-batch spira-1 CI_RUNNING 4 dirty".to_string()));
    assert!(t.out().contains("spira-1 abandoned on spira-lc"));
}

#[test_id(tpq4ng5)]
#[test]
fn abandon_refuses_a_batch_whose_pr_is_merged() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=12\nmembers=sp-a:ta\nbatch_id=spira-1\nversion=3\n");
    *t.forge.pr_state.borrow_mut() = Some("merged".into());
    assert_eq!(t.run(&["abandon", "--reason", "dirty"]), 1);
    assert!(t.err().contains("PR 12 is already merged"), "{}", t.err());
    assert!(!t.lc.has("abandon-batch"));
    assert!(t.qfile("open").exists());
}

#[test_id(t5hh7jv)]
#[test]
fn abandon_dry_run_prints_the_audit_line_and_changes_nothing() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    assert_eq!(t.run(&["abandon", "--reason=why", "--dry-run"]), 0);
    assert!(t
        .out()
        .contains("dry-run: audit line (landing.log): QUEUE ABANDON"));
    assert!(t.out().contains("dry-run: sp-a: requeue at ta"), "{}", t.out());
    assert!(t.out().contains("members=sp-a:REQUEUED,sp-b:REQUEUED"), "{}", t.out());
    assert!(t.qfile("open").exists());
    assert!(t.landing_log().is_empty());
}

// ------------------------------------------------------------------------ claim/release

#[test_id(te46pcp)]
#[test]
fn claim_and_release_hand_the_batch_back() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    assert_eq!(t.run(&["claim", "--reason", "hand edit"]), 0);
    let rec = fs::read_to_string(t.qfile("open")).unwrap();
    assert!(
        rec.ends_with("owner=concierge\npre_claim_owner=batcher\nclaim_reason=hand edit\n"),
        "{rec}"
    );
    assert_eq!(t.run(&["claim", "--reason", "again"]), 1);
    assert!(t
        .err()
        .contains("already claimed by concierge — pass --force to reclaim"));
    assert_eq!(t.run(&["release"]), 0);
    assert!(t.out().contains("released back to batcher"));
    assert!(fs::read_to_string(t.qfile("open"))
        .unwrap()
        .ends_with("owner=batcher\n"));
    assert_eq!(t.run(&["release"]), 1);
    assert!(t.err().contains("is not claimed by concierge"));
}

#[test_id(tfrvw0q)]
#[test]
fn claim_without_reason_is_usage() {
    let t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["claim"]), 2);
}

// -------------------------------------------------------------------------- open-batch

#[test_id(trar1ya)]
#[test]
fn open_batch_leaves_a_submitted_bead_out_whose_gate_has_not_passed() {
    let t = T::new(LandMode::Queue);
    t.git.set("origin/main", "b0");
    *t.git.branches.borrow_mut() = vec![("spira/sp-s".into(), "ts".into())];
    t.lc_row("sp-s", "SUBMITTED", "ts", 1);
    assert_eq!(
        t.run(&["open-batch", "--members", "sp-s", "--skip-pregate"]),
        1
    );
    assert!(
        t.out()
            .contains("sp-s: SUBMITTED — its gate has not passed at ts; not admitted"),
        "{}",
        t.out()
    );
    assert!(
        t.forge
            .calls
            .borrow()
            .iter()
            .all(|c| !c.starts_with("pr-create")),
        "no batch is cut"
    );
    assert!(!t.qfile("open").exists());
}

fn certifies_nothing_world() -> T {
    let t = T::new(LandMode::Queue);
    t.git.set("origin/main", "b0");
    t.git.set("refs/heads/spira/sp-s", "ts");
    *t.git.branches.borrow_mut() = vec![("spira/sp-s".into(), "ts".into())];
    t
}

fn cut_a_batch(t: &T) -> bool {
    t.forge.calls.borrow().iter().any(|c| c.starts_with("pr-create"))
}

#[test_id(tiq9oji)]
#[test]
fn open_batch_admits_a_named_submitted_bead_whose_gate_certifies_nothing() {
    let t = certifies_nothing_world();
    t.lc_row_certifies_nothing("sp-s", "ts", 1);
    assert_eq!(t.run(&["open-batch", "--members", "sp-s", "--skip-pregate"]), 0, "{}\n{}", t.out(), t.err());
    assert!(cut_a_batch(&t));
}

#[test_id(trbzc61)]
#[test]
fn open_batch_cuts_a_certifies_nothing_bead_from_the_pool() {
    let t = certifies_nothing_world();
    t.lc_row_certifies_nothing("sp-s", "ts", 1);
    assert_eq!(t.run(&["open-batch", "--skip-pregate"]), 0, "{}\n{}", t.out(), t.err());
    assert!(cut_a_batch(&t));
}

#[test_id(tw06c8s)]
#[test]
fn open_batch_ranks_certifies_nothing_beads_after_certified_ones() {
    let t = certifies_nothing_world();
    t.git.set("refs/heads/spira/sp-c", "tc");
    t.git.branches.borrow_mut().push(("spira/sp-c".into(), "tc".into()));
    t.lc_row_certifies_nothing("sp-s", "ts", 1);
    t.lc_row("sp-c", "CERTIFIED", "tc", 2);
    assert_eq!(t.run(&["open-batch", "--dry-run"]), 0, "{}\n{}", t.out(), t.err());
    let out = t.out();
    let at = |id: &str| out.find(&format!("  {id}")).unwrap_or_else(|| panic!("{id} is a member: {out}"));
    assert!(at("sp-c") < at("sp-s"), "{out}");
}

#[test_id(t1frbiu)]
#[test]
fn open_batch_leaves_out_a_certifies_nothing_bead_whose_row_holds_another_tip() {
    let t = certifies_nothing_world();
    t.lc_row_certifies_nothing("sp-s", "tm", 1);
    assert_eq!(t.run(&["open-batch", "--members", "sp-s:ts", "--skip-pregate"]), 1);
    assert!(t.out().contains("sp-s: named at ts, but its tip is tm — not admitted"), "{}", t.out());
    assert!(!cut_a_batch(&t));
}

#[test_id(tzqou5s)]
#[test]
fn open_batch_leaves_out_a_marked_row_whose_tip_moved_after_the_pool_was_listed() {
    let t = certifies_nothing_world();
    t.lc_row_certifies_nothing("sp-s", "ts", 1);
    let mut moved = t.lc.rows.borrow().as_ref().unwrap()[0].clone();
    moved.tip = Some("tm".into());
    t.lc.moved_rows.borrow_mut().insert("sp-s".into(), moved);
    assert_eq!(t.run(&["open-batch", "--skip-pregate"]), 1);
    assert!(t.out().contains("sp-s: SUBMITTED — its gate has not passed at ts; not admitted"), "{}", t.out());
    assert!(!cut_a_batch(&t));
}

#[test_id(tfy199e)]
#[test]
fn open_batch_leaves_out_a_held_certifies_nothing_bead() {
    let t = certifies_nothing_world();
    t.lc_row_certifies_nothing("sp-s", "ts", 1);
    t.lc.rows.borrow_mut().as_mut().unwrap().last_mut().unwrap().holds = vec!["manual".into()];
    assert_eq!(t.run(&["open-batch", "--members", "sp-s", "--skip-pregate"]), 1);
    assert!(t.out().contains("sp-s: held (manual) — not admitted"), "{}", t.out());
    assert!(!cut_a_batch(&t));
}

#[test_id(tophf9x)]
#[test]
fn open_batch_leaves_out_a_bead_a_red_verdict_returned_to_rework() {
    let t = certifies_nothing_world();
    t.lc_row("sp-s", "REWORK", "ts", 1);
    assert_eq!(t.run(&["open-batch", "--members", "sp-s", "--skip-pregate"]), 1);
    assert!(!cut_a_batch(&t));
}

#[test_id(towy1ca)]
#[test]
fn open_batch_admits_a_named_certified_bead() {
    let t = T::new(LandMode::Queue);
    t.git.set("origin/main", "b0");
    t.git.set("refs/heads/spira/sp-c", "tc");
    *t.git.branches.borrow_mut() = vec![("spira/sp-c".into(), "tc".into())];
    t.lc_row("sp-c", "CERTIFIED", "tc", 1);
    assert_eq!(
        t.run(&["open-batch", "--members", "sp-c", "--skip-pregate"]),
        0,
        "{}\n{}",
        t.out(),
        t.err()
    );
    assert!(t
        .forge
        .calls
        .borrow()
        .iter()
        .any(|c| c.starts_with("pr-create")));
}

// covers: UC-landing-merge-queue-41
#[test_id(t2ebi3y)]
#[test]
fn open_batch_pr_body_lists_id_and_title_per_member() {
    let t = T::new(LandMode::Queue);
    t.git.set("origin/main", "b0");
    t.submitted(&["sp-a", "sp-b"]);
    for (id, title) in [("sp-a", "first fix"), ("sp-b", "second fix")] {
        t.bd.rows.borrow_mut().iter_mut().find(|r| r.id == id).unwrap().title = Some(title.into());
        t.git.set(&format!("refs/heads/spira/{id}"), "ts");
        t.lc_row(id, "CERTIFIED", "ts", 1);
    }
    *t.git.branches.borrow_mut() = vec![("spira/sp-a".into(), "ts".into()), ("spira/sp-b".into(), "ts".into())];
    assert_eq!(t.run(&["open-batch", "--skip-pregate"]), 0, "{}\n{}", t.out(), t.err());
    let pr = t.forge.calls.borrow().iter().find(|c| c.starts_with("pr-create")).cloned().unwrap_or_default();
    assert!(pr.contains("- sp-a — first fix\n") && pr.contains("- sp-b — second fix"), "{pr}");
}

#[test_id(tb14pm9)]
#[test]
fn open_batch_names_the_paths_a_member_conflicts_with_base_on() {
    let t = T::new(LandMode::Queue);
    t.git.set("origin/main", "b0");
    t.submitted(&["sp-a", "sp-x"]);
    for (id, tip) in [("sp-a", "ta"), ("sp-x", "tx")] {
        t.git.set(&format!("refs/heads/spira/{id}"), tip);
        t.lc_row(id, "CERTIFIED", tip, 1);
    }
    *t.git.branches.borrow_mut() = vec![("spira/sp-a".into(), "ta".into()), ("spira/sp-x".into(), "tx".into())];
    t.git.merge_fail.borrow_mut().insert("tx".into());
    t.git.conflict_with_base.borrow_mut().insert("tx".into(), vec!["a.rs".into(), "b.rs".into()]);
    assert_eq!(t.run(&["open-batch", "--skip-pregate"]), 0, "{}\n{}", t.out(), t.err());
    assert!(t.out().contains("sp-x: conflicts with base — paths: a.rs b.rs"), "{}", t.out());

    let t = T::new(LandMode::Queue);
    t.git.set("origin/main", "b0");
    t.submitted(&["sp-x"]);
    t.git.set("refs/heads/spira/sp-x", "tx");
    t.lc_row("sp-x", "CERTIFIED", "tx", 1);
    *t.git.branches.borrow_mut() = vec![("spira/sp-x".into(), "tx".into())];
    t.git.merge_fail.borrow_mut().insert("tx".into());
    t.git.conflict_with_base.borrow_mut().insert("tx".into(), vec![]);
    t.run(&["open-batch", "--skip-pregate"]);
    assert!(t.out().contains("sp-x: conflicts with base — paths: unknown"), "{}", t.out());
}

#[test_id(tsykdse)]
#[test]
fn open_batch_refuses_while_a_batch_is_open() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=1\n");
    assert_eq!(t.run(&["open-batch"]), 1);
    assert!(t
        .err()
        .contains("a batch is already open for spira (owner=batcher)"));
}

#[test_id(tg451tb)]
#[test]
fn open_batch_replace_requires_a_reason_and_leaves_the_open_batch_alone() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=1\n");
    assert_eq!(t.run(&["open-batch", "--replace"]), 2);
    assert!(
        t.err().contains("--replace requires --reason"),
        "{}",
        t.err()
    );
    assert!(t.qfile("open").exists());
}

#[test_id(t6c7oxs)]
#[test]
fn open_batch_does_not_admit_when_the_lifecycle_row_cannot_be_read() {
    let t = T::new(LandMode::Queue);
    t.git.set("origin/main", "b0");
    *t.git.branches.borrow_mut() = vec![("spira/sp-a".into(), "ta".into())];
    t.lc_row("sp-a", "CERTIFIED", "ta", 1);
    t.lc.row_fails.set(true);
    assert_eq!(t.run(&["open-batch", "--members", "sp-a"]), 1);
    assert!(
        t.out()
            .contains("sp-a: no lifecycle row (spira-lc could not say) — not admitted"),
        "{}",
        t.out()
    );
    assert!(t.out().contains("no cut — nothing admissible for spira"));
}

// -------------------------------------------------------------------------- land-local

/// h1's tree, and a tree nothing certified (the pre-merge branch's, in the incident).
const T1: &str = "1111111111111111111111111111111111111111";
const T_BRANCH: &str = "2222222222222222222222222222222222222222";

fn local_repo(t: &T) {
    uncertified_local_repo(t);
    t.certify("spira", T1, gate::cert::Source::Round, "-");
}

/// local/main at b0, h1 fast-forwards from it with tree T1 — and no certificate anywhere.
fn uncertified_local_repo(t: &T) {
    t.git.set("refs/heads/local/main", "b0");
    t.git.ancestor("b0", "h1");
    for tip in ["ta", "tb", "tc"] {
        t.git.ancestor(tip, "h1");
    }
    t.git.set("refs/remotes/origin/main", "f0");
    t.git.trees.borrow_mut().insert("h1".into(), T1.into());
}

impl T {
    /// A tree certificate as the gate or batcher-cut writes it (gate::cert, §8 D12).
    fn certify(&self, repo: &str, tree: &str, source: gate::cert::Source, harness: &str) {
        let c = gate::cert::Cert {
            source,
            tree: tree.into(),
            repo: repo.into(),
            rev: "judged-rev".into(),
            branch: "spira/sp-a".into(),
            by: "test".into(),
            when: "2026-09-29T18:00:00Z".into(),
            at: 1,
            harness: harness.into(),
            suites: "-".into(),
        };
        gate::cert::write(&self.s().run.join("verdicts"), &c).unwrap();
    }
    fn landed_ref(&self) -> Option<String> {
        self.git.get("refs/heads/local/main")
    }
}

#[test_id(t6alp1r)]
#[test]
fn land_local_refuses_a_tree_no_gate_or_round_certified() {
    let t = T::new(LandMode::QueueLocal);
    uncertified_local_repo(&t);
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]),
        1
    );
    let e = t.err();
    assert!(
        e.contains(&format!("no round GREEN for h1's tree {T1} in spira")),
        "{e}"
    );
    assert!(
        e.contains("SPIRA_LAND_UNGATED=<reason>") && e.contains("refused, nothing changed"),
        "{e}"
    );
    assert_eq!(t.landed_ref().as_deref(), Some("b0"), "nothing moved");
    assert!(!t.lc.has("certify") && !t.lc.has("event"));
    assert!(!t.qfile("round-seq").exists());
    assert!(t.git.get("refs/archive/rounds/1").is_none());
}

#[test_id(trclmg0)]
#[test]
fn land_local_refuses_a_budgeted_gate_pass_for_the_head_tree() {
    let t = T::new(LandMode::QueueLocal);
    uncertified_local_repo(&t);
    t.certify("spira", T1, gate::cert::Source::Gate, "harness-now");
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]),
        1
    );
    let e = t.err();
    assert!(
        e.contains("only a budgeted gate PASS exists") && e.contains("no round GREEN"),
        "{e}"
    );
    assert_eq!(t.landed_ref().as_deref(), Some("b0"), "nothing moved");
    assert!(!t.lib.has("land_mark"));
}

#[test_id(tg0g2y5)]
#[test]
fn land_local_accepts_a_round_green_for_the_head_tree() {
    let t = T::new(LandMode::QueueLocal);
    uncertified_local_repo(&t);
    t.certify("spira", T1, gate::cert::Source::Round, "-");
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(t.landed_ref().as_deref(), Some("h1"));
    assert!(
        t.err()
            .contains(&format!("tree {T1} certified by round GREEN")),
        "{}",
        t.err()
    );
    assert!(
        t.scripts
            .calls
            .borrow()
            .contains(&"rebase-waiting-start spira".to_string()),
        "a landing starts the rebase of the waiting queue: {:?}",
        t.scripts.calls.borrow()
    );
}

#[test_id(tietvda)]
#[test]
fn land_local_refuses_while_another_operation_holds_the_lock() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    let _g = t.hold_lock();
    let start = t.clock.now();
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]),
        crate::ops::LOCK_BUSY
    );
    assert!(
        t.err()
            .contains("another queue operation holds the lock for spira"),
        "{}",
        t.err()
    );
    assert_eq!(t.landed_ref().as_deref(), Some("b0"), "nothing moved");
    assert!(!t.lc.has("certify") && !t.lc.has("event"));
    assert!(!t.qfile("round-seq").exists());
    assert!(t.git.get("refs/archive/rounds/1").is_none());
    assert_eq!(
        t.clock.now() - start,
        t.s().verdict.lock_wait,
        "waited the whole deadline on the fake clock"
    );
}

#[test_id(tj6vjes)]
#[test]
fn land_local_syncs_the_boundary_wiki_page_only_for_the_home_repo() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        !t.scripts
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("boundary-sync")),
        "{:?}",
        t.scripts.calls.borrow()
    );
    let mut t = T::new(LandMode::QueueLocal);
    t.lib.s.home_repo = "spira".into();
    local_repo(&t);
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.scripts
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("boundary-sync")),
        "{:?}",
        t.scripts.calls.borrow()
    );
}

#[test_id(trv8qys)]
#[test]
fn land_local_ignores_a_pass_for_another_tree_or_repo() {
    // The incident: the branch passed alone (its own tree); the merge that landed is h1.
    let t = T::new(LandMode::QueueLocal);
    uncertified_local_repo(&t);
    t.certify("spira", T_BRANCH, gate::cert::Source::Gate, "harness-now");
    t.certify("other", T1, gate::cert::Source::Gate, "harness-now");
    // A file at the right path that claims another tree is not a certificate for this one.
    let p = gate::cert::path(&t.s().run.join("verdicts"), "spira", T1).unwrap();
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    let forged = fs::read_to_string(
        gate::cert::path(&t.s().run.join("verdicts"), "spira", T_BRANCH).unwrap(),
    )
    .unwrap();
    fs::write(&p, forged).unwrap();
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]),
        1
    );
    assert!(t.err().contains("no round GREEN"), "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
    // A red verdict is not a certificate either.
    fs::write(
        &p,
        fs::read_to_string(&p)
            .unwrap()
            .replace(T_BRANCH, T1)
            .replace("verdict=PASS", "verdict=FAIL"),
    )
    .unwrap();
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]),
        1
    );
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
}

#[test_id(talicwu)]
#[test]
fn land_local_honours_spira_verdicts_as_the_gate_does() {
    let t = T::new(LandMode::QueueLocal);
    uncertified_local_repo(&t);
    let v = t.dir.join("elsewhere");
    t.var("SPIRA_VERDICTS", &v.display().to_string());
    let c = gate::cert::Cert {
        source: gate::cert::Source::Round,
        tree: T1.into(),
        repo: "spira".into(),
        rev: "r".into(),
        branch: "b".into(),
        by: "x".into(),
        when: "w".into(),
        at: 1,
        harness: "-".into(),
        suites: "-".into(),
    };
    gate::cert::write(&v, &c).unwrap();
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
}

#[test_id(tmfkly8)]
#[test]
fn land_local_ungated_override_lands_and_logs_the_reason() {
    let t = T::new(LandMode::QueueLocal);
    uncertified_local_repo(&t);
    t.var("SPIRA_LAND_UNGATED", "");
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]),
        1,
        "an empty reason is no override"
    );
    t.var(
        "SPIRA_LAND_UNGATED",
        "operator: gate host down\nsecond line",
    );
    t.var("SPIRA_LAND_UNGATED_BY", "operator");
    t.var("SPIRA_LAND_UNGATED_EVIDENCE", "b0");
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta,sp-b"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(t.landed_ref().as_deref(), Some("h1"));
    let reason = "operator: gate host down second line";
    assert!(
        t.err().contains(&format!(
            "UNGATED LANDING of h1 (tree {T1}) onto local/main"
        )),
        "{}",
        t.err()
    );
    assert!(
        t.err().contains(&format!(
            "SPIRA_LAND_UNGATED={reason} by=operator evidence=b0"
        )),
        "{}",
        t.err()
    );
    assert!(t.landing_log().contains(&format!("QUEUE UNGATED 1000 repo=spira head=h1 tree={T1} by=operator evidence=b0 reason={reason}")), "{}", t.landing_log());
    let mails = t.lib.mails.borrow();
    let landing: Vec<_> = mails.iter().filter(|(s, _)| s.starts_with("local landing")).collect();
    assert_eq!(landing.len(), 1, "{mails:?}");
    assert!(landing[0].1.contains("UNGATED"), "{}", landing[0].1);
}

#[test_id(tejbqzd)]
#[test]
fn land_local_ungated_override_refuses_without_by_or_evidence_on_the_tip() {
    let t = T::new(LandMode::QueueLocal);
    uncertified_local_repo(&t);
    t.git.set("elsewhere", "x9");
    t.var("SPIRA_LAND_UNGATED", "gate host down");
    for (by, ev, want) in [
        (
            None,
            Some("b0"),
            "SPIRA_LAND_UNGATED_BY must be concierge or operator",
        ),
        (
            Some("dwarf"),
            Some("b0"),
            "SPIRA_LAND_UNGATED_BY must be concierge or operator",
        ),
        (Some("operator"), None, "SPIRA_LAND_UNGATED_EVIDENCE"),
        (Some("operator"), Some("nope"), "does not resolve"),
        (Some("concierge"), Some("elsewhere"), "is not on the tip"),
    ] {
        t.var("SPIRA_LAND_UNGATED_BY", by.unwrap_or(""));
        t.var("SPIRA_LAND_UNGATED_EVIDENCE", ev.unwrap_or(""));
        assert_eq!(
            t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]),
            1
        );
        assert!(
            t.err().contains(want) && t.err().contains("refused, nothing changed"),
            "{want}: {}",
            t.err()
        );
        assert_eq!(t.landed_ref().as_deref(), Some("b0"), "nothing moved");
    }
}

#[test_id(tdc1dol)]
#[test]
fn land_local_delivers_each_member_at_its_version() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta,sp-b"]),
        0,
        "{}",
        t.err()
    );
    let calls = t.lc.calls.borrow().clone();
    let ev = |c: &str| {
        calls
            .iter()
            .position(|x| x == c)
            .unwrap_or_else(|| panic!("missing {c}: {calls:?}"))
    };
    let a = (
        ev("event bead sp-a CERTIFIED 3 \"Deliver\""),
        ev("event bead sp-a IN_DELIVERY 4 {\"Delivered\":{\"merge_sha\":\"ta\",\"proof\":\"ancestry\"}}"),
    );
    assert!(a.0 < a.1);
    ev("event bead sp-b CERTIFIED 3 \"Deliver\"");
    ev("event bead sp-b IN_DELIVERY 4 {\"Delivered\":{\"merge_sha\":\"h1\",\"proof\":\"ancestry\"}}");
}

#[test_id(tmmblny)]
#[test]
fn land_local_with_lifecycle_on_resumes_a_member_already_in_delivery() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    t.git.ancestor("ta", "h1");
    t.lc.bead_rows
        .borrow_mut()
        .insert("sp-a".into(), ("IN_DELIVERY".into(), "7".into()));
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let calls = t.lc.calls.borrow().join("\n");
    assert!(!calls.contains("\"Deliver\""), "{calls}");
    assert!(
        calls.contains("event bead sp-a IN_DELIVERY 7 {\"Delivered\""),
        "{calls}"
    );
}

#[test_id(tm1j2ev)]
#[test]
fn land_local_with_lifecycle_on_does_not_deliver_a_tip_that_is_not_in_the_head() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-a:tz"]),
        1
    );
    assert!(
        t.err().contains("sp-a: tip tz is not an ancestor of h1"),
        "{}",
        t.err()
    );
    assert!(!t
        .lc
        .calls
        .borrow()
        .iter()
        .any(|c| c.starts_with("event bead")));
}

#[test_id(tugtx1b)]
#[test]
fn land_local_with_spira_lc_unreachable_lands_nothing() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    t.lc.available.set(false);
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]),
        1
    );
    assert!(
        t.err()
            .contains("spira-lc is unreachable (no spira-lc program) — refused, nothing changed"),
        "{}",
        t.err()
    );
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
    assert!(!t.lib.has("bead_close"));
}

#[test_id(t3o5zsj)]
#[test]
fn land_local_refuses_a_head_that_does_not_fast_forward() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    t.git.set("h9", "h9");
    assert_eq!(
        t.run(&["land-local", "--head", "h9", "--members", "sp-a:ta"]),
        1
    );
    assert!(t
        .err()
        .contains("does not fast-forward from local/main (b0) — refused, nothing changed"));
    assert_eq!(t.git.get("refs/heads/local/main").as_deref(), Some("b0"));
}

#[test_id(totsorm)]
#[test]
fn land_local_refuses_a_remote_tracking_base_and_other_modes() {
    let t = T::new(LandMode::QueueLocal);
    t.lib.r.borrow_mut().landref = Some("origin/main".into());
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "a:b"]),
        1
    );
    assert!(t
        .err()
        .contains("resolves to a remote-tracking ref (origin/main) — not a queue.local base"));
    let t = T::new(LandMode::Queue);
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "a:b"]),
        1
    );
    assert!(t
        .err()
        .contains("repo is not in queue.local mode (mode=queue)"));
}

fn release_in_force(t: &T) {
    let rel = t.s().releases.clone().unwrap();
    fs::create_dir_all(rel.join("r1")).unwrap();
    std::os::unix::fs::symlink(rel.join("r1"), rel.join("current")).unwrap();
}

/// Over-broad-scopes #3 (sp-ipwlic): land-local releases the queue lock before it activates the
/// release, so a claim or a round verb does not wait behind the activation; and an activation
/// whose head a later landing has moved past is not run.
#[test_id(txbfzpy)]
#[test]
fn land_local_activates_its_release_with_the_queue_lock_released() {
    let mut t = T::new(LandMode::QueueLocal);
    t.lib.s.home_repo = "spira".into();
    local_repo(&t);
    release_in_force(&t);
    let bins = testkit::TempDir::new("queue-land-bins");
    *t.scripts.lock_probe.borrow_mut() = Some(t.s().queue_dir.clone());
    // The round worktree is at h1's tree (the fake answers HEAD for any repository path).
    t.git.set("HEAD", "h1");
    std::fs::create_dir_all(bins.path().join("target/release")).unwrap();
    testkit::write_exe(bins.path().join("target/release/release"), "#!/bin/sh\n");
    let wt = bins.path().display().to_string();
    assert_eq!(
        t.run(&[
            "land-local",
            "--head",
            "h1",
            "--members",
            "sp-a:ta",
            "--worktree",
            &wt
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(
        release_argv(&t).iter().any(|a| a.starts_with("activate ")),
        "{:?}",
        release_argv(&t)
    );
    assert_eq!(
        *t.scripts.lock_free_at_activate.borrow(),
        [true],
        "the queue lock is free while the release activates"
    );
}

/// sp-ia61tg: a round build that ships other binaries but not `release` is refused before the
/// ref moves, never after the landing is recorded.
#[test_id(tun642y)]
#[test]
fn land_local_refuses_before_landing_when_the_round_build_lacks_release() {
    let mut t = T::new(LandMode::QueueLocal);
    t.lib.s.home_repo = "spira".into();
    local_repo(&t);
    release_in_force(&t);
    let bins = testkit::TempDir::new("queue-land-bins-norelease");
    t.git.set("HEAD", "h1");
    std::fs::create_dir_all(bins.path().join("target/release")).unwrap();
    testkit::write_exe(bins.path().join("target/release/other"), "#!/bin/sh\n");
    let wt = bins.path().display().to_string();
    assert_ne!(
        t.run(&[
            "land-local",
            "--head",
            "h1",
            "--members",
            "sp-a:ta",
            "--worktree",
            &wt
        ]),
        0
    );
    assert!(t.err().contains("no release binary at"), "{}", t.err());
    assert_eq!(
        t.git.get("refs/heads/local/main").as_deref(),
        Some("b0"),
        "the ref did not move"
    );
    assert!(
        !release_argv(&t).iter().any(|a| a.starts_with("activate ")),
        "{:?}",
        release_argv(&t)
    );
}

/// sp-6ugk85: a landing whose activation runs after the round's build directory is gone (a
/// deferred activation, the round VM's build) activates the release from that release's own
/// `bin/release`, which `release build` copied from the same tested build.
#[test_id(teiqfhv)]
#[test]
fn activation_runs_the_built_releases_own_binary_when_the_round_build_has_none() {
    let t = T::new(LandMode::QueueLocal);
    let rel = t.s().releases.clone().unwrap();
    let sha = "0ed1c65f3aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    fs::create_dir_all(rel.join(sha).join("bin")).unwrap();
    testkit::write_exe(rel.join(sha).join("bin/release"), "#!/bin/sh\n");
    let gone = testkit::TempDir::new("queue-round-bins-gone");
    let plan = crate::ops::deploy::Plan {
        repo: PathBuf::from("repo"),
        landref: "local/main".into(),
        releases: rel.clone(),
        run: t.s().run.clone(),
        bins: Some(gone.path().join("target/release")),
        db: "db".into(),
    };
    let r = t.with_world(|w| crate::ops::deploy::activate(w, &plan, sha));
    assert_eq!(r, Ok(()), "the release dir carries the activator");
    let calls = t.scripts.release_calls.borrow();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(
        calls[0].0,
        rel.join(sha).join("bin/release").display().to_string()
    );
    assert!(
        calls[0].1.starts_with(&format!("activate {sha} ")),
        "{calls:?}"
    );
}

fn activating_from_unit(restarted_caller: Option<&str>) -> (Vec<String>, usize) {
    let t = T::new(LandMode::QueueLocal);
    let rel = t.s().releases.clone().unwrap();
    let sha = "0ed1c65f3aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    fs::create_dir_all(rel.join(sha).join("bin")).unwrap();
    testkit::write_exe(rel.join(sha).join("bin/release"), "#!/bin/sh\n");
    *t.scripts.caller_unit.borrow_mut() = restarted_caller.map(str::to_string);
    let plan = crate::ops::deploy::Plan {
        repo: PathBuf::from("repo"),
        landref: "local/main".into(),
        releases: rel,
        run: t.s().run.clone(),
        bins: None,
        db: "db".into(),
    };
    let r = t.with_world(|w| crate::ops::deploy::activate(w, &plan, sha));
    assert_eq!(r, Ok(()));
    let keys = t.scripts.detached_keys.borrow().clone();
    let n = t.scripts.release_calls.borrow().len();
    (keys, n)
}

#[test_id(t7upvye)]
#[test]
fn an_activation_called_from_a_unit_it_restarts_runs_in_a_unit_of_its_own() {
    let (keys, calls) = activating_from_unit(Some("spira-runtime-prod.service"));
    assert_eq!(keys, vec!["0ed1c65f3aaa".to_string()]);
    assert_eq!(calls, 1);
}

#[test_id(t0vwak4)]
#[test]
fn an_activation_called_from_a_unit_it_leaves_alone_runs_as_a_child() {
    let (keys, calls) = activating_from_unit(None);
    assert!(keys.is_empty(), "{keys:?}");
    assert_eq!(calls, 1);
}

#[test_id(td5bfr2)]
#[test]
fn land_local_of_another_repository_publishes_no_release() {
    // The fixture's harness is "harness"; landing "spira" is some other queue.local repo.
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    release_in_force(&t);
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(t.landed_ref().as_deref(), Some("h1"));
    assert!(
        t.scripts.release_calls.borrow().is_empty(),
        "no release step for a repository that is not the harness"
    );
}
#[test_id(t3c0or0)]
#[test]
fn land_local_skips_its_lock_only_when_the_caller_holds_it() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    let _g = t.hold_lock();
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]),
        crate::ops::LOCK_BUSY
    );
    assert!(t.err().contains("another queue operation holds the lock"));
    t.var("SPIRA_QUEUE_LOCK_HELD", "1");
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]),
        0
    );
}

#[test_id(tk90td3)]
#[test]
fn land_local_reads_members_from_stdin() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    *t.env.stdin.borrow_mut() = "sp-a:ta\nsp-b:tb\n".into();
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members-file", "-"]),
        0
    );
    assert!(t.lc.has("event bead sp-b CERTIFIED 3 \"Deliver\""));
}

// ------------------------------------------ land-local: the landing publishes a release (§8 D13)

fn release_argv(t: &T) -> Vec<String> {
    t.scripts
        .release_calls
        .borrow()
        .iter()
        .map(|(_, a, _)| a.clone())
        .collect()
}

const _: () = assert!(
    crate::ops::deploy::LAND_DRAIN_WAIT_SECS > 0
        && crate::ops::deploy::LAND_DRAIN_WAIT_SECS < 15 * 60
);

// ----------------------------------------------------------------------------- publish

fn full_suite_passed(t: &T, sha: &str) {
    spira_config::local_pass::record(
        &t.lib.s.run,
        spira_config::local_pass::Kind::FullSuite,
        sha,
        "test",
        "0",
    )
    .unwrap();
}

fn publishable(t: &T) {
    full_suite_passed(t, "m3");
    t.git.set("refs/heads/local/main", "m3");
    t.git.set("refs/remotes/origin/main", "f0");
    t.git.ancestor("f0", "m3");
    for (tip, id) in [("t1", "sp-a"), ("t2", "sp-b")] {
        t.git.ancestor(tip, "m3");
        let _ = id;
    }
    *t.git.log.borrow_mut() = vec![
        RangeCommit {
            sha: "m3".into(),
            parents: vec!["m2".into(), "t2".into()],
            subject: "spira: land sp-b — B".into(),
        },
        RangeCommit {
            sha: "r".into(),
            parents: vec!["m1".into()],
            subject: "round 9: fix".into(),
        },
        RangeCommit {
            sha: "m1".into(),
            parents: vec!["f0".into(), "t1".into()],
            subject: "spira: land sp-a".into(),
        },
    ];
}

#[test_id(tjkuvh6)]
#[test]
fn publish_prefers_the_lifecycle_row_tip_when_it_is_in_range() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    t.git.ancestor("tx", "m3");
    t.lc_row("sp-a", "LANDED", "tx", 5);
    assert_eq!(t.run(&["publish"]), 0);
    assert!(fs::read_to_string(t.qfile("publish"))
        .unwrap()
        .contains("members=sp-a:tx sp-b:t2"));
}

#[test_id(t6ou33t)]
#[test]
fn publish_with_nothing_new_is_success() {
    let t = T::new(LandMode::QueueLocal);
    t.git.set("refs/heads/local/main", "f0");
    t.git.set("refs/remotes/origin/main", "f0");
    assert_eq!(t.run(&["publish"]), 0);
    assert!(t
        .out()
        .contains("nothing to publish for spira (origin/main already at f0)"));
    assert!(!t.qfile("publish").exists());
}

#[test_id(tq6hzli)]
#[test]
fn a_stopped_publish_lane_opens_no_pr_and_another_lane_does_not_stop_it() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    let run = t.s().run.clone();
    let lanes = |l: &str| spira_config::scoped_stop::parse_lanes(l).unwrap();
    spira_config::scoped_stop::pause_repo(
        &run,
        "spira",
        &lanes("claims,rounds"),
        "concierge",
        "hand fix",
        "fix lands",
        1,
    )
    .unwrap();
    assert_eq!(t.run(&["publish"]), 0);
    assert!(
        !t.out().contains("stopped on purpose"),
        "a claims/rounds stop leaves publish running"
    );
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    let run = t.s().run.clone();
    spira_config::scoped_stop::pause_repo(
        &run,
        "spira",
        &lanes("publish"),
        "concierge",
        "hand fix",
        "fix lands",
        1,
    )
    .unwrap();
    assert_eq!(t.run(&["publish"]), 0);
    assert!(t
        .out()
        .contains("publish lane stopped on purpose by concierge"));
    assert!(!t.qfile("publish").exists(), "no publish PR record");
}

#[test_id(tbr9znd)]
#[test]
fn publish_does_not_wait_on_a_closed_fix_forward_that_never_moved_the_head() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    fs::write(t.qfile("publish-red"), "head=m3\nfix_forward=sp-fix\n").unwrap();
    t.lib.dead_beads.borrow_mut().push("sp-fix".into());
    full_suite_passed(&t, "m3");
    let _ = t.run(&["publish"]);
    assert!(t.out().contains("no fix is in flight"), "{}", t.out());
    assert!(t.landing_log().contains("QUEUE PUBLISH_FIX_FORWARD_DEAD"));
    assert!(!t.landing_log().contains("QUEUE PUBLISH_WAIT"));
}

#[test_id(ttmb0vd)]
#[test]
fn publish_waits_on_an_unmoved_red_head_then_republishes_once_local_main_moves() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    fs::write(t.qfile("publish-red"), "head=m3\nfix_forward=sp-fix\n").unwrap();
    assert_eq!(t.run(&["publish"]), 0, "{}", t.err());
    assert!(t.out().contains("waiting on fix-forward sp-fix for spira"));
    assert!(t.landing_log().contains("QUEUE PUBLISH_WAIT"));
    assert!(t.landing_log().contains("fix_forward=sp-fix"));
    assert!(!t.lib.has("push"));
    assert!(t.forge.calls.borrow().is_empty());
    assert!(!t.qfile("publish").exists());
    // the marker survives — the head still has not moved
    assert!(t.qfile("publish-red").exists());

    // local/main moves past the red head: the marker no longer matches and is cleared,
    // and this publish proceeds normally.
    full_suite_passed(&t, "m4");
    t.git.set("refs/heads/local/main", "m4");
    t.git.ancestor("m4", "m4");
    t.git.ancestor("f0", "m4");
    *t.git.log.borrow_mut() = vec![RangeCommit {
        sha: "m4".into(),
        parents: vec!["m3".into()],
        subject: "spira: land sp-c".into(),
    }];
    assert_eq!(t.run(&["publish"]), 0, "{}", t.err());
    assert!(t.qfile("publish").exists());
    assert!(!t.qfile("publish-red").exists());
}

// -------------------------------------------------------------------------- transitions

fn forgeable(t: &mut T) {
    publishable(t);
    t.lib.toml = Some(t.dir.join("cfg"));
    t.config
        .rows
        .borrow_mut()
        .insert("spira".into(), ("queue.local".into(), "local/main".into()));
    let map = t.dir.join("legacy-map");
    fs::write(&map, "x").unwrap();
    t.lib.s.repo_map = Some(map);
}

#[test_id(t8wgrrw)]
#[test]
fn to_forge_waits_for_the_final_publish_then_refuses_if_the_forge_did_not_catch_up() {
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    // pending, then green (the in-process settle fast-forwards and removes the record)...
    t.forge
        .status
        .borrow_mut()
        .extend([Some("pending\n".into()), Some("green\n".into())]);
    // ...but the forge ref this fake fetches never moved to local/main's m3.
    assert_eq!(t.run(&["to-forge"]), 1);
    assert!(t.out().contains("running the final publish for spira"));
    assert!(
        t.out().contains("queue.sh publish: PR 77 opened"),
        "the final publish's output is on stdout"
    );
    assert!(t
        .out()
        .contains("waiting for publish PR 77 to settle green"));
    assert_eq!(
        t.forge
            .calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("check-status"))
            .count(),
        2
    );
    assert!(
        t.out()
            .contains("verdict spira: publish PR 77 green — origin/main fast-forwarded to m3"),
        "{}",
        t.out()
    );
    assert!(t
        .err()
        .contains("origin/main (f0) and local/main (m3) differ — refused, nothing changed"));
    assert!(t.config.legacy.borrow().is_empty());
    assert_eq!(t.config.rows.borrow()["spira"].0, "queue.local");
}

#[test_id(t18jxwr)]
#[test]
fn to_forge_happy_path_flips_archives_and_deletes_the_local_branch() {
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    // nothing to publish: forge already at local/main
    t.git.set("refs/remotes/origin/main", "m3");
    *t.lib.readback.borrow_mut() = ("queue".into(), "origin/main".into());
    assert_eq!(t.run(&["to-forge"]), 0, "{}", t.err());
    assert_eq!(
        t.config.rows.borrow()["spira"],
        ("queue.forge".to_string(), "origin/main".to_string())
    );
    assert_eq!(
        t.config.legacy.borrow()["spira"],
        ("queue.forge".to_string(), "origin/main".to_string())
    );
    assert_eq!(t.git.get("refs/archive/local/main").as_deref(), Some("m3"));
    assert!(t.git.get("refs/heads/local/main").is_none());
    assert!(t.out().contains("queue.sh to-forge: spira is now queue.forge (base=origin/main); local/main archived at refs/archive/local/main"));
}

#[test_id(tet9kz2)]
#[test]
fn to_forge_refuses_on_a_red_final_publish() {
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    t.forge
        .status
        .borrow_mut()
        .push(Some("red\nred-suite: test-a.sh\n".into()));
    assert_eq!(t.run(&["to-forge"]), 1);
    // red is refused, not settled: the record stays for the normal fix-forward recovery
    assert!(t.qfile("publish").exists());
    assert!(t
        .err()
        .contains("the final publish (PR 77) is red — refused, nothing changed"));
    assert_eq!(t.config.rows.borrow()["spira"].0, "queue.local");
}

#[test_id(trmxvfb)]
#[test]
fn to_forge_times_out_on_a_pending_publish() {
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    assert_eq!(t.run(&["to-forge"]), 1);
    assert!(t
        .err()
        .contains("timed out waiting for publish PR 77 to settle — refused, nothing changed"));
}

#[test_id(t4n5dp9)]
#[test]
fn transitions_refuse_when_the_two_stores_already_disagree() {
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    t.config
        .rows
        .borrow_mut()
        .insert("spira".into(), ("queue".into(), "origin/main".into()));
    assert_eq!(t.run(&["to-forge"]), 1);
    assert!(t.err().contains("already disagree about spira"));
    assert!(t.err().contains("queue.local|local/main"));
    assert!(t.err().contains("queue|origin/main"));
}

#[test_id(tgl079c)]
#[test]
fn to_local_derives_the_local_branch_and_restores_the_config_if_the_legacy_map_fails() {
    let mut t = T::new(LandMode::Queue);
    t.lib.toml = Some(t.dir.join("cfg"));
    t.config
        .rows
        .borrow_mut()
        .insert("spira".into(), ("queue.forge".into(), "origin/main".into()));
    let map = t.dir.join("legacy-map");
    fs::write(&map, "x").unwrap();
    t.lib.s.repo_map = Some(map);
    t.git.set("refs/remotes/origin/main", "f5");
    t.config.legacy_fail.set(true);
    assert_eq!(t.run(&["to-local"]), 1);
    assert!(t.err().contains("legacy map write failed"));
    assert_eq!(
        t.config.rows.borrow()["spira"],
        ("queue.forge".to_string(), "origin/main".to_string()),
        "restored"
    );
    assert!(
        t.git.get("refs/heads/local/main").is_none(),
        "the new branch is deleted again"
    );

    t.config.legacy_fail.set(false);
    *t.lib.readback.borrow_mut() = ("queue.local".into(), "local/main".into());
    assert_eq!(t.run(&["to-local"]), 0, "{}", t.err());
    assert_eq!(t.git.get("refs/heads/local/main").as_deref(), Some("f5"));
    assert_eq!(
        t.config.rows.borrow()["spira"],
        ("queue.local".to_string(), "local/main".to_string())
    );
    assert!(t
        .out()
        .contains("spira is now queue.local (base=local/main, synced to origin/main)"));
}

#[test_id(tb1acto)]
#[test]
fn to_local_refuses_an_archive_the_forge_does_not_descend_from() {
    let t = T::new(LandMode::Queue);
    t.git.set("refs/remotes/origin/main", "f5");
    t.git.set("refs/archive/local/main", "old");
    assert_eq!(t.run(&["to-local"]), 1);
    assert!(t
        .err()
        .contains("is not an ancestor of origin/main (f5) — refs differ, refused"));
}

// ---------------------------------------------------------------------- rollback-local

#[test_id(ttm8it2)]
#[test]
fn rollback_local_reactivates_the_previous_rounds_release_without_rebuilding() {
    let t = T::new(LandMode::QueueLocal);
    assert_eq!(t.run(&["rollback-local"]), 1);
    assert!(t
        .err()
        .contains("round-seq is 0 — no landed round to roll back to"));
    fs::write(t.qfile("round-seq"), "2\n").unwrap();
    t.git.set("refs/archive/rounds/1", "p1");
    t.git.set("refs/heads/local/main", "h2");
    assert_eq!(t.run(&["rollback-local"]), 1);
    assert!(t.err().contains("no release is in force"), "{}", t.err());
    release_in_force(&t);
    assert_eq!(t.run(&["rollback-local"]), 1);
    assert!(t.err().contains("no release p1 in"), "{}", t.err());
    fs::create_dir_all(t.s().releases.clone().unwrap().join("p1")).unwrap();
    t.scripts
        .release_rc
        .borrow_mut()
        .insert("verify".into(), (1, "release: p1 FAILS verify\n".into()));
    assert_eq!(t.run(&["rollback-local"]), 1);
    assert!(
        t.err()
            .contains("could not re-activate release p1; nothing changed"),
        "{}",
        t.err()
    );
    assert_eq!(t.git.get("refs/heads/local/main").as_deref(), Some("h2"));
    t.scripts.release_rc.borrow_mut().clear();
    t.scripts.release_calls.borrow_mut().clear();
    assert_eq!(t.run(&["rollback-local"]), 0, "{}", t.err());
    let argv = release_argv(&t);
    assert_eq!(argv.len(), 2);
    assert!(
        argv[0].starts_with("verify p1 ")
            && argv[1].starts_with(&format!(
                "activate p1 --repo {REPO} --landed-ref local/main"
            )),
        "{argv:?}"
    );
    assert_eq!(t.git.get("refs/heads/local/main").as_deref(), Some("p1"));
    assert!(t
        .out()
        .contains("queue.sh rollback-local: activated release p1, local/main reset to p1"));
}

// ------------------------------------------------------------------------------- misc

#[test_id(tquxhah)]
#[test]
fn stats_reads_the_landing_log() {
    let t = T::new(LandMode::Queue);
    fs::write(t.s().run.join("landing.log"), "QUEUE CAUGHT 1 branch=x\n").unwrap();
    assert_eq!(t.run(&["stats"]), 0);
    assert!(t.out().starts_with("caught:          1\n"));
}

#[test_id(t743cqi)]
#[test]
fn stats_reads_lines_that_exist_only_in_a_rotated_landing_log() {
    let t = T::new(LandMode::Queue);
    fs::write(t.s().run.join("landing.log.1"), "QUEUE CAUGHT 1 branch=x\n").unwrap();
    fs::write(t.s().run.join("landing.log"), "QUEUE CAUGHT 2 branch=y\n").unwrap();
    assert_eq!(t.run(&["stats"]), 0);
    assert!(t.out().starts_with("caught:          2\n"), "{}", t.out());
}

#[test_id(tqxg7aa)]
#[test]
fn landing_log_text_reads_kept_files_oldest_first_then_the_live_one() {
    let t = T::new(LandMode::Queue);
    let run = t.s().run.clone();
    assert_eq!(crate::ops::landing_log_text(&run), None);
    fs::write(run.join("landing.log.2"), "a\n").unwrap();
    fs::write(run.join("landing.log.10"), "0\n").unwrap();
    fs::write(run.join("landing.log.1"), "b\n").unwrap();
    fs::write(run.join("landing.log"), "c\n").unwrap();
    assert_eq!(
        crate::ops::landing_log_text(&run).as_deref(),
        Some("0\na\nb\nc\n")
    );
}

#[test_id(tdx4oen)]
#[test]
fn rotation_keeps_the_live_file_in_the_count_and_ignores_names_that_are_not_numbers() {
    let t = T::new(LandMode::Queue);
    let run = t.s().run.clone();
    let put = |n: &str, v: &str| fs::write(run.join(n), v).unwrap();
    let read = |n: &str| fs::read_to_string(run.join(n)).ok();
    put("landing.log", "live");
    put("landing.log.1", "one");
    put("landing.log.2", "two");
    put("landing.log.bak", "bak");
    assert!(!crate::ops::rotate_landing_log(&run, 100, 3).unwrap());
    assert!(crate::ops::rotate_landing_log(&run, 1, 3).unwrap());
    assert_eq!(read("landing.log.1").as_deref(), Some("live"));
    assert_eq!(read("landing.log.2").as_deref(), Some("one"));
    assert_eq!(read("landing.log.3"), None);
    assert_eq!(read("landing.log"), None);
    assert_eq!(read("landing.log.bak").as_deref(), Some("bak"));
    put("landing.log", "again");
    assert!(crate::ops::rotate_landing_log(&run, 1, 1).unwrap());
    assert_eq!(read("landing.log.1"), None);
    assert_eq!(read("landing.log.2"), None);
}

#[test_id(tho8ghw)]
#[test]
fn usage_errors_exit_two() {
    let t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["bogus"]), 2);
    assert_eq!(t.run(&["abandon", "--nope"]), 2);
    assert!(matches!(cli::parse(&["stats".to_string()]), Ok(Cmd::Stats)));
}

// ------------------------------------------------------------------- publish-settle

use crate::ops::publish::{ci_of, decide, Ci, Pr, Settle};

#[test_id(tvrsugq)]
#[test]
fn settle_decision_table() {
    let open = |ci, behind| {
        Some(Pr::Open {
            ci,
            behind,
            tries: 0,
        })
    };
    let tried = |tries| {
        Some(Pr::Open {
            ci: Ci::Untested,
            behind: false,
            tries,
        })
    };
    assert_eq!(decide(None), Settle::Open);
    assert_eq!(decide(open(Ci::Green, true)), Settle::Wait);
    assert_eq!(decide(open(Ci::Green, false)), Settle::Wait);
    assert_eq!(decide(open(Ci::Pending, true)), Settle::Wait);
    assert_eq!(decide(open(Ci::Red, true)), Settle::Supersede);
    assert_eq!(decide(open(Ci::Untested, true)), Settle::Supersede);
    assert_eq!(decide(open(Ci::Red, false)), Settle::Wait);
    assert_eq!(decide(open(Ci::Untested, false)), Settle::Rerun);
    let no_run = |past, started, behind| open(Ci::NoRun { past, started }, behind);
    assert_eq!(decide(no_run(true, false, true)), Settle::Supersede);
    assert_eq!(decide(no_run(true, false, false)), Settle::StartCi);
    assert_eq!(decide(no_run(false, false, true)), Settle::Wait);
    assert_eq!(decide(no_run(false, true, false)), Settle::Wait);
    assert_eq!(decide(no_run(true, true, false)), Settle::Parked);
    assert_eq!(
        ci_of("pending\nno-run: Gate\n"),
        Ci::NoRun {
            past: false,
            started: false
        }
    );
    assert_eq!(decide(tried(1)), Settle::Alarm);
    assert_eq!(decide(tried(2)), Settle::Parked);
    assert_eq!(decide(Some(Pr::Gone)), Settle::Retire);
    assert_eq!(decide(Some(Pr::Unknown)), Settle::Wait);
    assert_eq!(ci_of("provision_fault\n"), Ci::Untested);
    assert_eq!(ci_of("harness_fault"), Ci::Untested);
    assert_eq!(ci_of("red\nred-suite: a"), Ci::Red);
    assert_eq!(ci_of(""), Ci::Pending);
}

fn stale_publish(t: &T, status: &str) {
    publishable(t);
    full_suite_passed(t, "m4");
    t.git.set("refs/heads/local/main", "m4");
    t.git.ancestor("f0", "m4");
    t.git.ancestor("t1", "m4");
    t.git.ancestor("t2", "m4");
    fs::write(
        t.qfile("publish"),
        "pr=77\nhead=m3\nbase=f0\nmembers=sp-a:t1\nopened=900\nbranch=spira/publish/OLD\nremote=origin\nforge_branch=main\n",
    )
    .unwrap();
    t.forge.status.borrow_mut().push(Some(status.into()));
    *t.forge.pr.borrow_mut() = Some("78".into());
}

#[test_id(tibfk6m)]
#[test]
fn settle_supersedes_a_red_or_untested_publish_that_local_has_moved_past() {
    for status in ["red\n", "provision_fault\n"] {
        let t = T::new(LandMode::QueueLocal);
        stale_publish(&t, status);
        assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
        let calls = t.forge.calls.borrow().clone();
        assert!(calls.contains(&"pr-close 77".to_string()), "{calls:?}");
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("pr-create spira/publish/20260929T010203Z main")),
            "{calls:?}"
        );
        assert!(fs::read_to_string(t.qfile("publish"))
            .unwrap()
            .starts_with("pr=78\nhead=m4\n"));
    }
}

#[test_id(tz9yawu)]
#[test]
fn settle_skips_a_repository_declared_unpublished_and_still_fails_a_fetch_that_cannot_run() {
    let t = T::new(LandMode::QueueLocal);
    t.lib.r.borrow_mut().unpublished = true;
    t.git.fetch_ok.set(false);
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert!(
        t.out()
            .contains("publish-settle spira: declared unpublished — skipped"),
        "{}",
        t.out()
    );
    assert_eq!(t.run(&["publish"]), 0, "{}", t.err());
    assert!(t.forge.calls.borrow().is_empty());

    let t = T::new(LandMode::QueueLocal);
    t.git.fetch_ok.set(false);
    assert_ne!(
        t.run(&["publish-settle"]),
        0,
        "positive control: a published repository whose fetch fails is red"
    );
    assert!(
        t.err().contains("could not fetch origin/main"),
        "{}",
        t.err()
    );
}

#[test_id(tuo650x)]
#[test]
fn settle_waits_on_a_green_or_pending_publish_and_on_an_unmoved_red_one() {
    for (status, moved) in [("green\n", true), ("pending\n", true), ("red\n", false)] {
        let t = T::new(LandMode::QueueLocal);
        stale_publish(&t, status);
        if !moved {
            t.git.set("refs/heads/local/main", "m3");
        }
        assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
        let calls = t.forge.calls.borrow().clone();
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with("pr-close") || c.starts_with("pr-create")),
            "{calls:?}"
        );
        assert!(fs::read_to_string(t.qfile("publish"))
            .unwrap()
            .starts_with("pr=77\n"));
    }
}

const NO_RUN: &str = "pending\nno-run: Gate\n";

fn count_calls(t: &T, call: &str) -> usize {
    t.forge
        .calls
        .borrow()
        .iter()
        .filter(|c| c.as_str() == call)
        .count()
}

#[test_id(t7nnzyv)]
#[test]
fn settle_supersedes_a_publish_with_no_run_past_the_bound_when_local_is_ahead() {
    let t = T::new(LandMode::QueueLocal);
    stale_publish(&t, NO_RUN);
    t.clock.now.set(900 + 600);
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    let calls = t.forge.calls.borrow().clone();
    assert!(calls.contains(&"pr-close 77".to_string()), "{calls:?}");
    assert!(calls.iter().any(|c| c.starts_with("pr-create")), "{calls:?}");
    assert_eq!(count_calls(&t, "dispatch spira/publish/OLD"), 0);
    assert!(t.lib.has("notify spira publish PR 77 for spira has no CI run"));
}

#[test_id(tu6si6q)]
#[test]
fn settle_records_the_no_run_page_before_the_supersede_closes_the_pr() {
    let t = T::new(LandMode::QueueLocal);
    stale_publish(&t, NO_RUN);
    *t.forge.record_at_close.borrow_mut() = Some((t.qfile("publish"), String::new()));
    t.clock.now.set(900 + 600);
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    let seen = t.forge.record_at_close.borrow().clone().unwrap().1;
    assert!(seen.contains("no_run_paged="), "a failed close must not page again: {seen:?}");
}

#[test_id(tka7y9p)]
#[test]
fn settle_starts_ci_once_for_a_publish_with_no_run_past_the_bound() {
    let t = T::new(LandMode::QueueLocal);
    stale_publish(&t, NO_RUN);
    t.git.set("refs/heads/local/main", "m3");
    t.clock.now.set(900 + 600);
    for _ in 0..3 {
        t.forge.status.borrow_mut().push(Some(NO_RUN.into()));
        assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    }
    assert_eq!(count_calls(&t, "dispatch spira/publish/OLD"), 1);
    assert!(fs::read_to_string(t.qfile("publish"))
        .unwrap()
        .contains("ci_started=1500"));
    t.clock.now.set(1500 + 600);
    t.forge.status.borrow_mut().push(Some(NO_RUN.into()));
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert_eq!(count_calls(&t, "dispatch spira/publish/OLD"), 1, "parked");
    assert!(t.out().contains("-> Parked"), "{}", t.out());
    let pages = t
        .lib
        .calls
        .borrow()
        .iter()
        .filter(|l| l.starts_with("notify spira publish PR 77 for spira has no CI run"))
        .count();
    assert_eq!(pages, 1);
}

#[test_id(tkky3le)]
#[test]
fn settle_refused_dispatch_fails_and_writes_nothing() {
    let t = T::new(LandMode::QueueLocal);
    stale_publish(&t, NO_RUN);
    t.git.set("refs/heads/local/main", "m3");
    t.clock.now.set(900 + 600);
    *t.forge.dispatch_refused.borrow_mut() = true;
    assert_ne!(t.run(&["publish-settle"]), 0, "{}", t.out());
    let rec = fs::read_to_string(t.qfile("publish")).unwrap();
    assert!(!rec.contains("ci_started=") && !rec.contains("no_run_paged"), "{rec}");
    assert!(!t.lib.has("notify spira publish PR 77 for spira has no CI run"));
}

#[test_id(tl0s1ko)]
#[test]
fn settle_waits_on_a_publish_with_no_run_inside_the_bound() {
    let t = T::new(LandMode::QueueLocal);
    stale_publish(&t, NO_RUN);
    t.clock.now.set(900 + 599);
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert!(t.out().contains("waiting for a gate run on spira/publish/OLD"), "{}", t.out());
    let calls = t.forge.calls.borrow().clone();
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with("dispatch") || c.starts_with("pr-close")),
        "{calls:?}"
    );
    assert!(!t.lib.has("notify spira publish PR 77 for spira has no CI run"));
}

#[test_id(tiv3t67)]
#[test]
fn settle_waits_on_a_publish_whose_run_is_in_progress() {
    let t = T::new(LandMode::QueueLocal);
    stale_publish(&t, "pending\n");
    t.clock.now.set(900 + 6000);
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    let calls = t.forge.calls.borrow().clone();
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with("dispatch") || c.starts_with("pr-close")),
        "{calls:?}"
    );
}

#[test_id(tns050k)]
#[test]
fn settle_refuses_no_run_when_the_bound_does_not_resolve() {
    let mut t = T::new(LandMode::QueueLocal);
    t.lib.s.publish_no_run_secs = None;
    stale_publish(&t, NO_RUN);
    t.clock.now.set(900 + 6000);
    assert_ne!(t.run(&["publish-settle"]), 0, "{}", t.out());
    assert!(t.err().contains("SPIRA_PUBLISH_NO_RUN_SECS"), "{}", t.err());
    assert_eq!(count_calls(&t, "dispatch spira/publish/OLD"), 0);
}

#[test_id(tgxggxl)]
#[test]
fn settle_reruns_an_untested_publish_once_then_alarms_once() {
    let t = T::new(LandMode::QueueLocal);
    stale_publish(&t, "harness_fault\n");
    t.git.set("refs/heads/local/main", "m3");
    *t.forge.run.borrow_mut() = Some("4242".into());
    t.forge
        .status
        .borrow_mut()
        .push(Some("harness_fault\n".into()));
    t.forge
        .status
        .borrow_mut()
        .push(Some("harness_fault\n".into()));
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert_eq!(
        t.forge
            .calls
            .borrow()
            .iter()
            .filter(|c| c.as_str() == "workflow-rerun 4242")
            .count(),
        1,
        "{:?}",
        t.forge.calls.borrow()
    );
    assert!(fs::read_to_string(t.qfile("publish"))
        .unwrap()
        .contains("rerun=4242"));
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert_eq!(
        t.forge
            .calls
            .borrow()
            .iter()
            .filter(|c| c.as_str() == "workflow-rerun 4242")
            .count(),
        1
    );
    assert!(fs::read_to_string(t.qfile("publish"))
        .unwrap()
        .contains("alarmed="));
    assert!(t
        .lib
        .has("notify spira publish PR 77 untested after a rerun"));
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert_eq!(
        t.forge
            .calls
            .borrow()
            .iter()
            .filter(|c| c.as_str() == "workflow-rerun 4242")
            .count(),
        1
    );
}

#[test_id(t5amzfd)]
#[test]
fn settle_opens_a_publish_when_none_is_open_and_skips_other_modes() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    *t.forge.pr.borrow_mut() = Some("78".into());
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert!(fs::read_to_string(t.qfile("publish"))
        .unwrap()
        .starts_with("pr=78\nhead=m3\n"));
    let t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["publish-settle"]), 0);
    assert!(t.forge.calls.borrow().is_empty());
}

#[test_id(tzttahy)]
#[test]
fn settle_defers_the_cut_while_a_round_pass_runs_and_cuts_after_it() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    *t.forge.pr.borrow_mut() = Some("78".into());
    fs::write(t.qfile("round"), "batch_id=b-1\n").unwrap();
    t.lc.batch_states
        .borrow_mut()
        .insert("b-1".into(), ("CI_RUNNING".into(), "4".into()));
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert!(
        t.out().contains("a round pass holds the hypervisor"),
        "{}",
        t.out()
    );
    assert!(
        !t.forge
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("pr-create")),
        "{:?}",
        t.forge.calls.borrow()
    );
    t.lc.batch_states
        .borrow_mut()
        .insert("b-1".into(), ("GREEN".into(), "5".into()));
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert!(
        t.forge
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("pr-create")),
        "{:?}",
        t.forge.calls.borrow()
    );
}

#[test_id(tx9t9vd)]
#[test]
fn settle_asks_only_the_open_round_whether_a_pass_holds_the_hypervisor() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    *t.forge.pr.borrow_mut() = Some("78".into());
    fs::write(t.qfile("round"), "batch_id=b-landed\nopened=1\n").unwrap();
    fs::write(t.qfile("round.b-settled"), "batch_id=b-settled\nopened=2\n").unwrap();
    fs::write(t.qfile("round.b-1"), "batch_id=b-1\nopened=3\n").unwrap();
    for (b, s) in [
        ("b-landed", "LANDED"),
        ("b-settled", "SETTLED"),
        ("b-1", "CI_RUNNING"),
    ] {
        t.lc.batch_states
            .borrow_mut()
            .insert(b.into(), (s.into(), "4".into()));
    }
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert!(
        t.out().contains("a round pass holds the hypervisor"),
        "{}",
        t.out()
    );
    t.lc.batch_states
        .borrow_mut()
        .insert("b-1".into(), ("GREEN".into(), "5".into()));
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert!(
        t.forge
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("pr-create")),
        "ended records hold nothing: {:?}",
        t.forge.calls.borrow()
    );
}

#[test_id(tc17wv3)]
#[test]
fn settle_defers_the_cut_while_round_progress_is_live_and_fresh() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    *t.forge.pr.borrow_mut() = Some("78".into());
    fs::create_dir_all(t.dir.join("run")).unwrap();
    let prog = t.dir.join("run/round-progress.json");
    fs::write(&prog, "{\"phase\":\"suites\",\"updated_at\":900}\n").unwrap();
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert!(
        t.out().contains("a round pass holds the hypervisor"),
        "{}",
        t.out()
    );
    assert!(
        !t.forge
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("pr-create")),
        "{:?}",
        t.forge.calls.borrow()
    );
    fs::write(&prog, "{\"phase\":\"suites\",\"updated_at\":100}\n").unwrap();
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert!(
        t.forge
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("pr-create")),
        "a stale file does not hold the cut: {:?}",
        t.forge.calls.borrow()
    );
}

fn live_progress(t: &T, place: &str, updated_at: u64) {
    fs::create_dir_all(t.dir.join("run")).unwrap();
    fs::write(
        t.dir.join("run/round-progress.json"),
        format!("{{\"phase\":\"suites\",\"place\":\"{place}\",\"updated_at\":{updated_at}}}\n"),
    )
    .unwrap();
}

fn reruns(t: &T) -> usize {
    t.forge
        .calls
        .borrow()
        .iter()
        .filter(|c| c.as_str() == "workflow-rerun 4242")
        .count()
}

fn red_publish_awaiting_rerun(t: &T) {
    stale_publish(t, "harness_fault\n");
    t.git.set("refs/heads/local/main", "m3");
    *t.forge.run.borrow_mut() = Some("4242".into());
    t.forge
        .status
        .borrow_mut()
        .push(Some("harness_fault\n".into()));
}

#[test_id(tr54mzb)]
#[test]
fn settle_reruns_in_the_same_tick_while_the_live_round_pass_runs_on_ec2() {
    let t = T::new(LandMode::QueueLocal);
    red_publish_awaiting_rerun(&t);
    fs::write(t.qfile("round"), "batch_id=b-1\n").unwrap();
    t.lc.batch_states
        .borrow_mut()
        .insert("b-1".into(), ("CI_RUNNING".into(), "4".into()));
    live_progress(&t, "ec2", 900);
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert!(!t.out().contains("holds the hypervisor"), "{}", t.out());
    assert_eq!(reruns(&t), 1, "{:?}", t.forge.calls.borrow());
}

#[test_id(tkwn4v5)]
#[test]
fn settle_supersedes_in_the_same_tick_while_the_live_round_pass_runs_on_ec2() {
    let t = T::new(LandMode::QueueLocal);
    stale_publish(&t, "red\n");
    live_progress(&t, "ec2", 900);
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert!(!t.out().contains("holds the hypervisor"), "{}", t.out());
    assert!(
        t.forge
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("pr-close")),
        "{:?}",
        t.forge.calls.borrow()
    );
}

#[test_id(t71jxy3)]
#[test]
fn settle_runs_after_the_deferral_bound_while_local_passes_run_back_to_back() {
    let t = T::new(LandMode::QueueLocal);
    red_publish_awaiting_rerun(&t);
    for _ in 0..3 {
        t.forge
            .status
            .borrow_mut()
            .push(Some("harness_fault\n".into()));
    }
    for tick in 1..=3 {
        live_progress(&t, "proxmox", 900);
        assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
        assert_eq!(reruns(&t), 0, "tick {tick} defers: {}", t.out());
    }
    live_progress(&t, "proxmox", 900);
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert_eq!(reruns(&t), 1, "{}", t.out());
    assert!(t.out().contains("deferred 3 ticks"), "{}", t.out());
}

#[test_id(tkssh0q)]
#[test]
fn settle_refuses_naming_the_key_when_the_deferral_bound_does_not_resolve() {
    let mut t = T::new(LandMode::QueueLocal);
    t.lib.s.publish_defer_max = None;
    red_publish_awaiting_rerun(&t);
    t.forge
        .status
        .borrow_mut()
        .push(Some("harness_fault\n".into()));
    live_progress(&t, "proxmox", 900);
    assert_ne!(t.run(&["publish-settle"]), 0, "{}", t.out());
    assert!(
        t.err().contains("SPIRA_PUBLISH_SETTLE_DEFER_MAX"),
        "{}",
        t.err()
    );
    assert_eq!(reruns(&t), 0);
}

#[test_id(ta8xgzt)]
#[test]
fn a_tick_that_does_not_defer_restarts_the_deferral_count() {
    let t = T::new(LandMode::QueueLocal);
    red_publish_awaiting_rerun(&t);
    for _ in 0..3 {
        t.forge
            .status
            .borrow_mut()
            .push(Some("harness_fault\n".into()));
    }
    live_progress(&t, "proxmox", 900);
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert!(t.qfile("publish-deferred").exists());
    live_progress(&t, "ec2", 900);
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert!(!t.qfile("publish-deferred").exists());
}

#[test_id(t5tfrtp)]
#[test]
fn settle_exits_zero_and_logs_when_the_queue_lock_is_held() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    let _q = crate::lock::try_lock(&t.lib.s.queue_dir, "spira");
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert!(
        t.out()
            .contains("publish-settle spira: another queue operation holds the lock"),
        "{}",
        t.out()
    );
    assert!(t.forge.calls.borrow().is_empty());
}

// covers: UC-landing-merge-queue-27
#[test_id(tnanakq)]
#[test]
fn submit_hands_the_gate_twice_the_gate_timeout_which_an_explicit_wait_may_only_raise() {
    for (explicit, want) in [("0", "5400"), ("9000", "9000")] {
        let mut t = T::new(LandMode::Queue);
        t.git.set("refs/heads/spira/sp-a", "t1");
        t.lib.s.gate_timeout = Some(2700);
        t.lib.s.gate_lock_wait = Some(explicit.into());
        assert_eq!(t.run(&["submit", "spira/sp-a"]), 0);
        assert_eq!(
            t.scripts.calls.borrow()[1],
            format!("gate spira/sp-a spira bead=sp-a lock_wait={want}")
        );
    }
}

#[test_id(tkcl729)]
#[test]
fn submit_refuses_naming_the_key_when_the_gate_timeout_does_not_resolve() {
    let mut t = T::new(LandMode::Queue);
    t.git.set("refs/heads/spira/sp-a", "t1");
    t.lib.s.gate_timeout = None;
    assert_ne!(t.run(&["submit", "spira/sp-a"]), 0);
    assert!(t.err().contains("SPIRA_GATE_TIMEOUT"), "{}", t.err());
    assert_eq!(count_calls(&t, "gate "), 0);
}

// covers: UC-landing-merge-queue-27
#[test_id(t104fmu)]
#[test]
fn submit_green_in_a_queue_mode_certifies_on_spira_lc_and_keeps_no_record_of_its_own() {
    let t = T::new(LandMode::Queue);
    t.git.set("refs/heads/spira/sp-a", "t1");
    assert_eq!(t.run(&["submit", "spira/sp-a"]), 0);
    assert_eq!(t.scripts.calls.borrow()[0], "lint t1 verify=false");
    assert_eq!(
        t.scripts.calls.borrow()[1],
        "gate spira/sp-a spira bead=sp-a lock_wait=5400"
    );
    assert!(t.lc.has("certify sp-a t1"));
    assert!(
        !t.s().queue_dir.join("sp-a").exists(),
        "the lifecycle row is the only certification record"
    );
    assert!(t
        .landing_log()
        .contains("QUEUE GATE_COST 1000 branch=sp-a seconds=0"));
    assert!(t.out().contains("queue.sh submit: certified spira/sp-a"));
}

// covers: UC-landing-merge-queue-27
#[test_id(tomc4fl)]
#[test]
fn submit_does_not_certify_when_spira_lc_refuses() {
    let t = T::new(LandMode::Queue);
    t.git.set("refs/heads/spira/sp-a", "t1");
    t.lc.certify_refused.set(true);
    assert_eq!(t.run(&["submit", "spira/sp-a"]), 1);
    assert!(t.err().contains("spira-lc certify refused for sp-a (rc=3)"));
    assert!(!t.s().queue_dir.join("sp-a").exists());
}

#[test_id(tl8r99n)]
#[test]
fn eject_dry_run_writes_nothing() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    t.bd.rows.borrow_mut().push(BeadRow {
        id: "sp-a".into(),
        ..Default::default()
    });
    assert_eq!(t.run(&["eject", "sp-a", "--dry-run"]), 0);
    assert!(t.out().contains("dry-run: would record cause eject"));
    assert!(t
        .out()
        .contains("dry-run: would return bead sp-a to spira-lc via a Returned event"));
    assert!(t.out().contains("dry-run: would requeue survivors: sp-b"), "{}", t.out());
    assert!(!t.lc.has("event") && !t.lc.has("eject-member"));
    assert!(t.qfile("open").exists());
    assert!(t.lib.calls.borrow().is_empty());
}

#[test_id(tfwjnkm)]
#[test]
fn a_concierge_claim_binds_every_actor_but_the_concierge() {
    use crate::ops::concierge_claimed;
    assert!(!concierge_claimed("concierge", "concierge"));
    assert!(concierge_claimed("concierge", "verdict"));
    assert!(!concierge_claimed("operator", "verdict"));
    assert!(!concierge_claimed("batcher", "verdict"));
}

// covers: UC-landing-merge-queue-29
#[test_id(tg7dkmt)]
#[test]
fn abandon_keeps_reworked_members_archives_and_audits() {
    let t = T::new(LandMode::Queue);
    open_batch_record(&t);
    t.lc_row("sp-a", "REWORK", "ta", 5);
    t.var("SPIRA_QUEUE_ACTOR", "op");
    assert_eq!(t.run(&["abandon", "--reason", "dirty\nPR"]), 0);
    assert!(t.out().contains("queue.sh abandon: sp-a: left as REWORK"));
    assert!(t
        .out()
        .contains("queue.sh abandon: sp-b: state unknown (spira-lc could not say)"));
    assert!(t.lib.has("cancel_runs spira/queue/x"));
    assert!(t
        .forge
        .calls
        .borrow()
        .iter()
        .any(|c| c == "pr-comment 12 Batch abandoned. Reason: dirty PR"));
    let archive = t.qfile("closed-pr12-20260929T010203Z");
    let a = fs::read_to_string(&archive).unwrap();
    assert!(a.ends_with("reason=dirty PR\nactor=op\n"), "{a}");
    assert!(!t.qfile("open").exists());
    assert!(t.landing_log().contains("QUEUE ABANDON 1000 repo=spira pr=12 actor=op members=sp-a:REWORK,sp-b:UNKNOWN reason=dirty PR"));
    assert!(t
        .lib
        .has("event queue.abandoned abandoned PR 12 for spira (actor=op)"));
}

#[test_id(tzwrw5a)]
#[test]
fn abandon_reports_each_member_in_the_state_spira_lc_requeued_it_to() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=12\nhead=h1\nbase=b0\nmembers=sp-a:ta sp-b:tb sp-c:tc sp-d:td\nbatch_id=spira-1\nversion=3\nbranch=spira/queue/x\nowner=batcher\n");
    t.lc_row("sp-a", "REWORK", "ta", 5);
    t.lc_requeues_to("sp-b", "tb", None, "CERTIFIED");
    t.lc_requeues_to("sp-c", "tc", Some("delivered-from-submitted"), "SUBMITTED");
    // REWORK only once requeued: it was returned there, not left there.
    t.lc_requeues_to("sp-d", "td", None, "REWORK");
    t.var("SPIRA_QUEUE_ACTOR", "op");
    assert_eq!(t.run(&["abandon", "--reason", "x"]), 0, "{}", t.err());
    let (out, log) = (t.out(), t.landing_log());
    assert!(out.contains("sp-b: returned to CERTIFIED"), "{out}");
    assert!(out.contains("sp-c: returned to SUBMITTED"), "{out}");
    assert!(out.contains("sp-d: returned to REWORK"), "{out}");
    assert!(log.contains("members=sp-a:REWORK,sp-b:CERTIFIED,sp-c:SUBMITTED,sp-d:REWORK"), "{log}");
    for text in [&out, &log] {
        assert!(!text.contains("sp-c:CERTIFIED"), "{text}");
        assert!(!text.contains("sp-c: returned to CERTIFIED"), "{text}");
    }
}

#[test_id(tnac5da)]
#[test]
fn land_local_lands_and_archives_and_delivers_on_spira_lc() {
    let t = T::new(LandMode::QueueLocal);
    t.submitted(&["sp-a", "sp-b"]);
    local_repo(&t);
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta,sp-b"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(t.git.get("refs/heads/local/main").as_deref(), Some("h1"));
    assert_eq!(t.git.get("refs/archive/rounds/1").as_deref(), Some("h1"));
    assert_eq!(fs::read_to_string(t.qfile("round-seq")).unwrap(), "1\n");
    assert!(
        t.lib.has("divergence f0 b0"),
        "the cached forge ref is checked, never fetched"
    );
    assert!(!t.git.calls.borrow().iter().any(|c| c.starts_with("fetch")));
    assert!(t.lc.has("event bead sp-a CERTIFIED 3 \"Deliver\""));
    assert!(t.lib.has("bead_close sp-b h1"));
    assert!(t.out().contains("queue.sh land-local: local/main fast-forwarded to h1 (round 1, archived at refs/archive/rounds/1)"));
}

#[test_id(t9y2usn)]
#[test]
fn land_local_lands_through_a_forge_divergence_and_an_open_publish() {
    let t = T::new(LandMode::QueueLocal);
    t.submitted(&["sp-a"]);
    local_repo(&t);
    t.lib.diverged.set(true);
    fs::write(t.qfile("publish"), "pr=9\n").unwrap();
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    assert!(t.lib.has("divergence f0 b0"));
    assert_eq!(t.git.get("refs/heads/local/main").as_deref(), Some("h1"));
}

#[test_id(tjmdqzp)]
#[test]
fn publish_members_come_from_land_commits_even_when_lifecycle_rows_are_missing() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    t.bd.rows.borrow_mut().push(BeadRow {
        id: "sp-b".into(),
        title: Some("B".into()),
        ..Default::default()
    });
    assert_eq!(t.run(&["publish"]), 0, "{}", t.err());
    assert!(t.git.calls.borrow().contains(&"log f0..m3".to_string()));
    let rec = fs::read_to_string(t.qfile("publish")).unwrap();
    assert_eq!(
        rec,
        "pr=77\nhead=m3\nbase=f0\nmembers=sp-a:t1 sp-b:t2\nopened=1000\nbranch=spira/publish/20260929T010203Z\nremote=origin\nforge_branch=main\n"
    );
    assert!(t
        .lib
        .has("push origin m3:refs/heads/spira/publish/20260929T010203Z"));
    let pr = t.forge.calls.borrow()[0].clone();
    assert!(pr
        .starts_with("pr-create spira/publish/20260929T010203Z main publish: 2 bead(s) for spira"));
    assert!(pr.contains("- sp-a — (title unavailable)") && pr.contains("- sp-b — B"));
    assert!(t
        .landing_log()
        .contains("QUEUE PUBLISH 1000 repo=spira pr=77 members=2"));
    assert!(t
        .out()
        .contains("queue.sh publish: PR 77 opened — 2 bead(s) since last publish"));
}

fn hand_landed(t: &T, subjects: &[(&str, &str)]) {
    full_suite_passed(t, "m3");
    t.git.set("refs/heads/local/main", "m3");
    t.git.set("refs/remotes/origin/main", "f0");
    t.git.ancestor("f0", "m3");
    *t.git.log.borrow_mut() = subjects
        .iter()
        .map(|(sha, subject)| RangeCommit {
            sha: sha.to_string(),
            parents: vec!["p".into()],
            subject: subject.to_string(),
        })
        .collect();
}

#[test_id(tyz78to)]
#[test]
fn publish_names_beads_hand_landed_with_no_land_commit() {
    let t = T::new(LandMode::QueueLocal);
    hand_landed(&t, &[("m3", "sp-b: x"), ("m2", "stacked base: merge sp-a (y)"), ("m1", "sp-a: y")]);
    for (id, tip) in [("sp-a", "m1"), ("sp-b", "m3")] {
        t.git.ancestor(tip, "m3");
        t.lc_row(id, "LANDED", tip, 5);
    }
    assert_eq!(t.run(&["publish"]), 0, "{}", t.err());
    let rec = fs::read_to_string(t.qfile("publish")).unwrap();
    assert!(rec.contains("members=sp-a:m1 sp-b:m3"), "{rec}");
}

#[test_id(t8hsth7)]
#[test]
fn publish_names_a_land_commit_and_a_hand_landed_bead_together() {
    let t = T::new(LandMode::QueueLocal);
    hand_landed(&t, &[("m3", "sp-b: x"), ("m1", "spira: land sp-a")]);
    t.git.ancestor("m3", "m3");
    t.lc_row("sp-b", "LANDED", "m3", 5);
    assert_eq!(t.run(&["publish"]), 0, "{}", t.err());
    let rec = fs::read_to_string(t.qfile("publish")).unwrap();
    assert!(rec.contains("members=sp-a:m1 sp-b:m3"), "{rec}");
}

#[test_id(tev89h3)]
#[test]
fn publish_refuses_when_the_store_cannot_say_which_hand_landed_bead_landed() {
    let t = T::new(LandMode::QueueLocal);
    hand_landed(&t, &[("m3", "sp-b: x"), ("m1", "spira: land sp-a")]);
    *t.lc.rows.borrow_mut() = Err("dolt down".into());
    assert_ne!(t.run(&["publish"]), 0);
    assert!(t.err().contains("dolt down"), "{}", t.err());
    assert!(!t.lib.has("push"));
}

#[test_id(twzunvn)]
#[test]
fn publish_still_refuses_a_range_that_landed_no_bead() {
    let t = T::new(LandMode::QueueLocal);
    hand_landed(&t, &[("m3", "sp-b: x")]);
    t.lc_row("sp-b", "CERTIFIED", "m3", 5);
    assert_ne!(t.run(&["publish"]), 0);
    assert!(t.err().contains("none landed a bead"), "{}", t.err());
}

#[test_id(t8begho)]
#[test]
fn transitions_refuse_while_work_is_in_delivery() {
    // an open batch record
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    t.open_record("pr=5\n");
    assert_eq!(t.run(&["to-forge"]), 1);
    assert!(t
        .err()
        .contains("has work in delivery (open batch PR 5) — refused, nothing changed"));
    assert!(!t.lib.has("push"), "no final publish");

    // spira-lc says IN_DELIVERY
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    t.lc_row("sp-r", "IN_DELIVERY", "t2", 1);
    assert_eq!(t.run(&["to-forge"]), 1);
    assert!(t.err().contains("(sp-r)"));

    // spira-lc unreachable: loud refusal
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    *t.lc.rows.borrow_mut() = Err("dolt down".into());
    assert_eq!(t.run(&["to-forge"]), 1);
    assert!(t.err().contains("spira-lc is unreachable (dolt down)"));

    // another repository's IN_DELIVERY bead does not block
    let mut t = T::new(LandMode::QueueLocal);
    forgeable(&mut t);
    t.git.set("refs/remotes/origin/main", "m3");
    *t.lib.readback.borrow_mut() = ("queue".into(), "origin/main".into());
    t.lc_row("xx-1", "IN_DELIVERY", "elsewhere", 1);
    assert_eq!(t.run(&["to-forge"]), 0, "{}", t.err());
}

// ------------------------------------------------------------------ close on land (sp-du6dl)
// The queue closes a landed member itself, in-process: no landing-pass oracle and no second
// ledger — LANDED lives on spira-lc, written by the queue's own delivery.

#[test_id(tthkdl2)]
#[test]
fn a_landed_submitted_member_is_closed_citing_the_sha_and_its_branch_reaped() {
    let t = T::new(LandMode::Push);
    t.submitted(&["sp-a"]);
    t.git.set("refs/heads/spira/sp-a", "t1");
    assert_eq!(t.run(&["submit", "spira/sp-a"]), 0, "{}", t.err());
    assert!(t.lib.has("bead_close sp-a t1"));
    assert!(t.lib.has("reap sp-a spira spira/sp-a landed at t1"));
    assert!(t
        .out()
        .contains("land-close sp-a: closed at t1 (submitted -> landed)"));
    assert!(t
        .out()
        .contains("land-close sp-a: reaped branch spira/sp-a"));
}

#[test_id(t84njeb)]
#[test]
fn close_on_land_leaves_a_never_submitted_bead_alone_whatever_bd_status_says() {
    for row in [
        BeadRow {
            id: "sp-a".into(),
            status: Some("closed".into()),
            labels: vec!["repo:spira".into()],
            ..Default::default()
        }, // literal-ok: fixture vocabulary
        BeadRow {
            id: "sp-a".into(),
            status: Some("open".into()),
            labels: vec!["repo:spira".into()],
            ..Default::default()
        },
    ] {
        let t = T::new(LandMode::Push);
        t.bd.rows.borrow_mut().push(row);
        t.git.set("refs/heads/spira/sp-a", "t1");
        assert_eq!(t.run(&["submit", "spira/sp-a"]), 0, "{}", t.err());
        assert!(!t.lib.has("bead_close"), "{:?}", t.lib.calls.borrow());
        assert!(!t.lib.has("reap"));
    }
}

#[test_id(tomcu79)]
#[test]
fn the_land_close_reason_cites_the_sha_or_unknown() {
    assert!(crate::ops::helpers::land_close_reason("abc")
        .contains("work landed at abc (law-closed-is-not-landed)"));
    assert!(crate::ops::helpers::land_close_reason("").contains("work landed at unknown"));
}

#[test_id(tz7dg96)]
#[test]
fn publish_goes_ahead_on_a_named_override_and_logs_it() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    fs::remove_file(t.lib.s.run.join("local-pass/full-suite/m3")).unwrap();
    *t.forge.pr.borrow_mut() = Some("78".into());
    t.env.vars.borrow_mut().insert(
        spira_config::local_pass::OVERRIDE_ENV.into(),
        "local round impossible, box is down".into(),
    );
    assert_eq!(t.run(&["publish"]), 0, "{}", t.err());
    assert!(t.qfile("publish").exists());
    let log = fs::read_to_string(t.lib.s.run.join("local-pass/overrides.log")).unwrap();
    assert!(
        log.contains("sha=m3") && log.contains("box is down"),
        "{log}"
    );
}

#[test_id(tjr2qf6)]
#[test]
fn settle_adopts_a_green_forge_publish_the_record_does_not_know_then_opens_the_next() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    t.git.set("refs/remotes/origin/spira/publish/OLD", "m2");
    t.git.ancestor("f0", "m2");
    *t.forge.publish_prs.borrow_mut() = vec![("587".into(), "spira/publish/OLD".into())];
    t.forge.status.borrow_mut().push(Some("green\n".into()));
    *t.forge.pr.borrow_mut() = Some("588".into());
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    let calls = t.forge.calls.borrow().clone();
    assert!(calls.contains(&"pr-close 587".to_string()), "{calls:?}");
    assert!(!t.lib.has("notify spira publish PR 587 merged"));
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("pr-create spira/publish/")),
        "{calls:?}"
    );
    assert!(fs::read_to_string(t.qfile("publish"))
        .unwrap()
        .starts_with("pr=588\nhead=m3\n"));
    assert!(
        t.out().contains("adopted open publish PR 587"),
        "{}",
        t.out()
    );
}

#[test_id(tej1iq2)]
#[test]
fn publish_adopts_an_open_pr_at_the_same_head_instead_of_opening_a_second() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    t.git
        .set("refs/remotes/origin/spira/publish/1794422613", "m3");
    *t.forge.publish_prs.borrow_mut() = vec![("1086".into(), "spira/publish/1794422613".into())];
    *t.forge.pr.borrow_mut() = Some("1087".into());
    assert_eq!(t.run(&["publish"]), 0, "{}", t.err());
    let calls = t.forge.calls.borrow().clone();
    assert!(
        !calls.iter().any(|c| c.starts_with("pr-create")),
        "{calls:?}"
    );
    assert!(!t.lib.has("push"), "{:?}", t.lib.calls.borrow());
    let rec = fs::read_to_string(t.qfile("publish")).unwrap();
    assert!(
        rec.starts_with("pr=1086\nhead=m3\n") && rec.contains("branch=spira/publish/1794422613"),
        "{rec}"
    );
}

#[test_id(t2j4l4g)]
#[test]
fn publish_does_not_adopt_an_open_pr_at_another_head() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    t.git.set("refs/remotes/origin/spira/publish/OLD", "m2");
    *t.forge.publish_prs.borrow_mut() = vec![("587".into(), "spira/publish/OLD".into())];
    *t.forge.pr.borrow_mut() = Some("588".into());
    assert_eq!(t.run(&["publish"]), 0, "{}", t.err());
    assert!(t.forge.calls.borrow().iter().any(|c| c.starts_with("pr-create")));
    assert!(fs::read_to_string(t.qfile("publish")).unwrap().starts_with("pr=588\n"));
}

// ------------------------------------------------------------------------------ auto-stack

fn waiting_fixture(t: &T, beads: &[(&str, &str, &[&str])]) {
    t.var("SPIRA_STACK_SHARED_FILES", "Cargo.lock spira/conf.d/");
    t.var("SPIRA_STACK_MAX_DEPTH", "4");
    t.git.set("local/main", "m1");
    for (n, (id, tip, files)) in beads.iter().enumerate() {
        t.lc_row(id, "SUBMITTED", tip, 10 + n as u64);
        t.git.set(&format!("refs/heads/spira/{id}"), tip);
        t.git
            .branches
            .borrow_mut()
            .push((format!("spira/{id}"), tip.to_string()));
        t.git
            .bases
            .borrow_mut()
            .insert(("m1".into(), tip.to_string()), "m1".into());
        let stat: Vec<(&str, u32, u32)> = files.iter().map(|f| (*f, 1, 0)).collect();
        t.git.stat("m1", tip, &stat);
    }
}

fn restacks(t: &T) -> Vec<String> {
    t.lc.calls
        .borrow()
        .iter()
        .filter(|c| c.contains("\"Restack\""))
        .cloned()
        .collect()
}

#[test_id(t8t03b4)]
#[test]
fn three_beads_that_each_add_a_dependency_are_stacked_into_one_chain() {
    let t = T::new(LandMode::QueueLocal);
    waiting_fixture(
        &t,
        &[
            ("sp-a", "ta", &["Cargo.lock", "a/Cargo.toml"]),
            ("sp-b", "tb", &["Cargo.lock", "b/Cargo.toml"]),
            ("sp-c", "tc", &["Cargo.lock", "c/Cargo.toml"]),
        ],
    );
    assert_eq!(t.run(&["rebase-waiting"]), 0);
    let r = restacks(&t);
    assert_eq!(r.len(), 2, "{r:?}");
    assert!(
        r[0].contains("event bead sp-b")
            && r[0].contains("\"stack\":{\"sp-a\":\"ta\"}")
            && r[0].contains("\"stack_depth\":1")
            && r[0].contains("rebased-tb"),
        "{}",
        r[0]
    );
    assert!(
        r[1].contains("event bead sp-c")
            && r[1].contains("\"stack\":{\"sp-b\":\"rebased-tb\"}")
            && r[1].contains("\"stack_depth\":2"),
        "{}",
        r[1]
    );
    assert_eq!(
        t.git.get("refs/heads/spira/sp-b").as_deref(),
        Some("rebased-tb")
    );
    assert_eq!(
        t.git.get("refs/heads/spira/sp-a").as_deref(),
        Some("ta"),
        "the bottom bead is not moved"
    );
}

#[test_id(t0jox8b)]
#[test]
fn beads_that_share_no_shared_file_are_left_alone() {
    let t = T::new(LandMode::QueueLocal);
    waiting_fixture(
        &t,
        &[
            ("sp-a", "ta", &["Cargo.lock"]),
            ("sp-b", "tb", &["b/src/lib.rs"]),
            ("sp-c", "tc", &["c/src/lib.rs", "Cargo.toml"]),
        ],
    );
    assert_eq!(t.run(&["rebase-waiting"]), 0);
    assert!(restacks(&t).is_empty(), "{:?}", restacks(&t));
}

#[test_id(t4tknez)]
#[test]
fn an_empty_shared_file_list_turns_auto_stack_off() {
    let t = T::new(LandMode::QueueLocal);
    waiting_fixture(
        &t,
        &[
            ("sp-a", "ta", &["Cargo.lock"]),
            ("sp-b", "tb", &["Cargo.lock"]),
        ],
    );
    t.var("SPIRA_STACK_SHARED_FILES", "");
    assert_eq!(t.run(&["rebase-waiting"]), 0);
    assert!(restacks(&t).is_empty());
}

#[test_id(tlkrd1y)]
#[test]
fn the_depth_cap_stops_a_chain() {
    let t = T::new(LandMode::QueueLocal);
    waiting_fixture(
        &t,
        &[
            ("sp-a", "ta", &["Cargo.lock"]),
            ("sp-b", "tb", &["Cargo.lock"]),
            ("sp-c", "tc", &["Cargo.lock"]),
        ],
    );
    t.var("SPIRA_STACK_MAX_DEPTH", "1");
    assert_eq!(t.run(&["rebase-waiting"]), 0);
    assert_eq!(restacks(&t).len(), 1);
    assert!(
        t.out()
            .contains("sp-c: not-stacked: depth 2 over SPIRA_STACK_MAX_DEPTH 1"),
        "{}",
        t.out()
    );
}

#[test_id(tm1dqta)]
#[test]
fn a_stacked_bead_is_recut_when_its_prerequisite_tip_moved() {
    let t = T::new(LandMode::QueueLocal);
    waiting_fixture(
        &t,
        &[
            ("sp-a", "ta2", &["Cargo.lock"]),
            ("sp-b", "tb", &["Cargo.lock"]),
        ],
    );
    t.lc.rows.borrow_mut().as_mut().unwrap()[1].stack =
        [("sp-a".to_string(), "ta1".to_string())].into();
    assert_eq!(t.run(&["rebase-waiting"]), 0);
    let r = restacks(&t);
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(
        r[0].contains("event bead sp-b") && r[0].contains("\"stack\":{\"sp-a\":\"ta2\"}"),
        "{}",
        r[0]
    );
    assert!(
        !t.scripts
            .calls
            .borrow()
            .iter()
            .any(|c| c.contains("rebase-stale sp-b")),
        "a stacked bead is cut onto its prerequisite, not the landing ref"
    );
    assert!(t.out().contains("sp-b: recut onto sp-a"), "{}", t.out());
}

#[test_id(tjjgaa2)]
#[test]
fn landing_an_integration_lands_its_members_and_closes_the_record() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    fs::create_dir_all(t.s().queue_dir.join("spira")).unwrap();
    fs::write(t.qfile("integrations"), "sp-int sp-a sp-b\nsp-other sp-c\n").unwrap();
    t.lc.bead_rows
        .borrow_mut()
        .insert("sp-b".into(), ("LANDED".into(), "5".into()));
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-int:ta"]),
        0,
        "{}",
        t.err()
    );
    let calls = t.lc.calls.borrow().clone();
    let landed: Vec<_> = calls
        .iter()
        .filter(|c| c.starts_with("event bead sp-a ") && c.contains("integration:sp-int@h1"))
        .collect();
    assert_eq!(landed.len(), 1, "{calls:?}");
    assert!(
        !calls.iter().any(|c| c.starts_with("event bead sp-b ")),
        "a member already landed is left as is: {calls:?}"
    );
    assert_eq!(
        fs::read_to_string(t.qfile("integrations")).unwrap(),
        "sp-other sp-c\n"
    );
}

#[test_id(t0uqmgn)]
#[test]
fn landing_an_integration_clears_the_carrier_of_each_member_record() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    fs::create_dir_all(t.s().queue_dir.join("spira")).unwrap();
    fs::write(t.qfile("integrations"), "sp-int sp-a sp-b\nsp-other sp-c\n").unwrap();
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-int:ta"]),
        0,
        "{}",
        t.err()
    );
    let calls = t.lc.calls.borrow().clone();
    assert!(calls.iter().any(|c| c == "set-carrier sp-a sp-b --clear"), "{calls:?}");
}

#[test_id(tgvy17f)]
#[test]
fn queue_starts_no_spira_child_that_is_not_a_named_fork_edge() {
    let got = forge::fork_scan::scan(&testkit::manifest_dir().join("src"));
    assert!(got.is_empty(), "unmarked children:\n{}", got.join("\n"));
}

fn land_rows(t: &T) -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(tsd::family_path(&t.s().run, tsd::land::FAMILY))
        .unwrap_or_default();
    text.lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test_id(tzd0r6r)]
#[test]
fn land_local_records_each_member_with_the_commit_its_reverts_label_names() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    t.bd.rows.borrow_mut().push(BeadRow {
        id: "sp-b".into(),
        labels: vec!["repo:spira".into(), "reverts:abcdef0123456".into()],
        ..BeadRow::default()
    });
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let rows = land_rows(&t);
    let got: Vec<_> = rows
        .iter()
        .map(|r| {
            (
                r["bead"].as_str().unwrap(),
                r["commit"].as_str().unwrap(),
                r["reverts"].as_str(),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![("sp-a", "ta", None), ("sp-b", "tb", Some("abcdef0123456"))]
    );
}

#[test_id(t8evb4g)]
#[test]
fn land_local_takes_no_reverts_from_a_title_that_says_revert() {
    let t = T::new(LandMode::QueueLocal);
    local_repo(&t);
    t.bd.rows.borrow_mut().push(BeadRow {
        id: "sp-a".into(),
        title: Some("Revert \"x\" abcdef0123456".into()),
        labels: vec!["repo:spira".into()],
        ..BeadRow::default()
    });
    assert_eq!(
        t.run(&["land-local", "--head", "h1", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        land_rows(&t)
            .iter()
            .map(|r| r["reverts"].is_null())
            .collect::<Vec<_>>(),
        vec![true]
    );
}

// ------------------------------------------------------------------- text-only: lands by lint

const T_TEXT: &str = "4444444444444444444444444444444444444444";

fn text_branch(t: &T, files: &[(&str, u32, u32)]) {
    t.git.set("refs/heads/local/main", "b0");
    t.git.set("refs/heads/spira/sp-a", "t1");
    t.git.ancestor("b0", "t1");
    t.git.trees.borrow_mut().insert("t1".into(), T_TEXT.into());
    t.git.stat("local/main", "t1", files);
}

#[test_id(tg4hanr)]
#[test]
fn submit_of_a_text_only_bead_lands_on_the_ref_by_lint_without_a_gate_or_round() {
    let t = T::new(LandMode::QueueLocal);
    text_branch(&t, &[("law/1-general/x.toml", 3, 0), ("docs/y.md", 1, 1)]);
    assert_eq!(t.run(&["submit", "spira/sp-a"]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("t1"));
    assert!(t.out().contains("sp-a landed at t1"), "{}", t.out());
    assert!(t
        .scripts
        .calls
        .borrow()
        .iter()
        .any(|c| c.starts_with("lint t1")));
    assert!(
        !t.scripts
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("gate ")),
        "no gate ran"
    );
    let p = gate::cert::path(&t.s().run.join("verdicts"), "spira", T_TEXT).unwrap();
    let cert = gate::cert::certifies(&fs::read_to_string(p).unwrap(), "spira", T_TEXT).unwrap();
    assert_eq!(cert.source, gate::cert::Source::LintOnly);
}

fn release_in_force_at(t: &T, name: &str) {
    let rel = t.s().releases.clone().unwrap();
    fs::create_dir_all(rel.join(name).join("bin")).unwrap();
    testkit::write_exe(rel.join(name).join("bin/release"), "#!/bin/sh\n");
    std::os::unix::fs::symlink(rel.join(name), rel.join("current")).unwrap();
}

#[test_id(tytrgiu)]
#[test]
fn submit_of_a_text_only_harness_bead_lands_with_the_release_in_forces_binaries() {
    let mut t = T::new(LandMode::QueueLocal);
    t.lib.s.home_repo = "spira".into();
    t.lib.s.text_only.push("README.md".into());
    text_branch(&t, &[("README.md", 1, 0)]);
    t.git.stat("b0", "t1", &[("README.md", 1, 0)]);
    release_in_force_at(&t, "b0");
    assert_eq!(t.run(&["submit", "spira/sp-a"]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("t1"));
    let bins = t.s().releases.clone().unwrap().join("b0/bin");
    let calls = t.scripts.calls.borrow().join("\n");
    let rel = release_argv(&t).join("\n");
    assert!(rel.contains("build t1") && rel.contains(&format!("--bin-dir {}", bins.display())), "{rel}");
    assert!(rel.contains("activate t1"), "{rel}");
    assert!(!calls.contains("gate "), "{calls}");
}

#[test_id(t302ji5)]
#[test]
fn a_text_only_harness_tip_whose_release_in_force_is_not_its_text_ancestor_rides_the_next_round() {
    let mut t = T::new(LandMode::QueueLocal);
    t.lib.s.home_repo = "spira".into();
    t.lib.s.text_only.push("README.md".into());
    text_branch(&t, &[("README.md", 1, 0)]);
    release_in_force_at(&t, "r1");
    assert_eq!(t.run(&["submit", "spira/sp-a"]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
    assert!(t.scripts.calls.borrow().iter().any(|c| c.starts_with("gate ")));
    let all = format!("{}{}", t.out(), t.err());
    assert!(all.contains("cannot land by lint") && all.contains("lands in the next round"), "{all}");
}

#[test_id(tduth5n)]
#[test]
fn submit_of_a_text_only_bead_whose_lint_is_red_lands_nothing() {
    let t = T::new(LandMode::QueueLocal);
    text_branch(&t, &[("law/x.md", 1, 0)]);
    t.scripts
        .lint_red
        .borrow_mut()
        .insert("t1".into(), "law/x.md:1".into());
    assert_eq!(t.run(&["submit", "spira/sp-a"]), 1);
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
}

#[test_id(tukba55)]
#[test]
fn submit_of_a_bead_touching_one_source_file_is_refused_by_the_classifier_and_gated() {
    let t = T::new(LandMode::QueueLocal);
    text_branch(&t, &[("law/x.md", 1, 0), ("queue/src/lib.rs", 1, 0)]);
    assert_eq!(t.run(&["submit", "spira/sp-a"]), 0, "{}", t.err());
    assert_eq!(
        t.landed_ref().as_deref(),
        Some("b0"),
        "nothing landed by lint"
    );
    assert!(
        t.scripts
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("gate ")),
        "the gate ran"
    );
    assert!(t.lc.has("certify sp-a t1"));
}

#[test_id(twwherz)]
#[test]
fn a_text_only_tip_that_does_not_fast_forward_is_not_landed_by_lint() {
    let t = T::new(LandMode::QueueLocal);
    text_branch(&t, &[("law/x.md", 1, 0)]);
    t.git.anc.borrow_mut().clear();
    assert_eq!(t.run(&["submit", "spira/sp-a"]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
    assert!(t
        .scripts
        .calls
        .borrow()
        .iter()
        .any(|c| c.starts_with("gate ")));
}

fn integrations_of(t: &T, repo: &str, text: &str) {
    t.repo(repo, LandMode::QueueLocal);
    fs::create_dir_all(t.s().queue_dir.join(repo)).unwrap();
    fs::write(t.s().queue_dir.join(repo).join("integrations"), text).unwrap();
}

fn set_carrier_calls(t: &T) -> Vec<String> {
    t.lc.calls.borrow().iter().filter(|c| c.starts_with("set-carrier")).cloned().collect()
}

#[test_id(tufavbh)]
#[test]
fn backfill_carriers_apply_sets_only_the_open_integrations_members_and_only_if_unset() {
    let t = T::new(LandMode::QueueLocal);
    integrations_of(&t, "spira", "sp-open sp-a sp-b\nsp-done sp-c\n");
    *t.lib.repos.borrow_mut() = Ok(vec!["spira".into()]);
    t.lc_row("sp-open", "WORKING", "t", 1);
    t.lc_row("sp-done", "LANDED", "t", 1);
    t.lc.carried.borrow_mut().push("sp-b".into());
    assert_eq!(t.run(&["backfill-carriers", "--apply"]), 0, "{}", t.err());
    assert_eq!(set_carrier_calls(&t), vec!["set-carrier sp-a sp-b --by sp-open --only-if-unset"]);
    let out = t.out();
    assert!(out.contains("set sp-a -> sp-open") && out.contains("kept sp-b"), "{out}");
    assert!(!out.contains("sp-c"), "{out}");
    assert!(out.contains("spira: set=1 kept=1 failed=0"), "{out}");
}

#[test_id(twg8pjw)]
#[test]
fn backfill_carriers_without_apply_names_each_member_and_integration_and_writes_nothing() {
    let t = T::new(LandMode::QueueLocal);
    integrations_of(&t, "spira", "sp-open sp-a sp-b\n");
    *t.lib.repos.borrow_mut() = Ok(vec!["spira".into()]);
    t.lc_row("sp-open", "WORKING", "t", 1);
    assert_eq!(t.run(&["backfill-carriers"]), 0, "{}", t.err());
    assert!(set_carrier_calls(&t).is_empty());
    let out = t.out();
    assert!(out.contains("would set sp-a -> sp-open") && out.contains("would set sp-b -> sp-open"), "{out}");
    assert!(out.contains("spira: would set 2"), "{out}");
}

#[test_id(t3svaqj)]
#[test]
fn backfill_carriers_failure_exits_non_zero_and_the_next_repo_is_still_processed() {
    let t = T::new(LandMode::QueueLocal);
    integrations_of(&t, "spira", "sp-bad sp-a\n");
    integrations_of(&t, "svc", "sp-good sp-b\n");
    *t.lib.repos.borrow_mut() = Ok(vec!["spira".into(), "svc".into()]);
    t.lc_row("sp-bad", "WORKING", "t", 1);
    t.lc_row("sp-good", "WORKING", "t", 1);
    t.lc.carrier_refused.borrow_mut().push("sp-bad".into());
    assert_ne!(t.run(&["backfill-carriers", "--apply"]), 0);
    assert_eq!(
        set_carrier_calls(&t),
        vec!["set-carrier sp-a --by sp-bad --only-if-unset", "set-carrier sp-b --by sp-good --only-if-unset"]
    );
    let out = t.out();
    assert!(out.contains("failed sp-a") && out.contains("set sp-b -> sp-good"), "{out}");
}

#[test_id(tfobkri)]
#[test]
fn backfill_carriers_refuses_an_unknown_option() {
    let p = |a: &[&str]| cli::parse(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert_eq!(
        p(&["backfill-carriers", "--bogus"]),
        Err(cli::Usage("queue.sh backfill-carriers: unknown option: --bogus".into()))
    );
    assert_eq!(p(&["backfill-carriers", "--apply"]), Ok(Cmd::BackfillCarriers { apply: true }));
    assert!(cli::USAGE.contains("queue.sh backfill-carriers"));
}

#[test_id(t796ln3)]
#[test]
fn backfill_carriers_partial_failure_fails_only_the_member_the_verb_names() {
    let t = T::new(LandMode::QueueLocal);
    integrations_of(&t, "spira", "sp-bad sp-a sp-ghost\n");
    *t.lib.repos.borrow_mut() = Ok(vec!["spira".into()]);
    t.lc_row("sp-bad", "WORKING", "t", 1);
    t.lc.carrier_refused.borrow_mut().push("sp-bad".into());
    assert_ne!(t.run(&["backfill-carriers", "--apply"]), 0);
    let out = t.out();
    assert!(out.contains("failed sp-ghost") && out.contains("set sp-a -> sp-bad") && !out.contains("failed sp-a"), "{out}");
    assert!(out.contains("spira: set=1 kept=0 failed=1"), "{out}");
}

#[test_id(t1d0nzb)]
#[test]
fn open_batch_whose_cut_answers_cannot_tell_keeps_the_branch() {
    let t = T::new(LandMode::Queue);
    t.git.set("origin/main", "b0");
    t.git.set("refs/heads/spira/sp-c", "tc");
    *t.git.branches.borrow_mut() = vec![("spira/sp-c".into(), "tc".into())];
    t.lc_row("sp-c", "CERTIFIED", "tc", 1);
    t.lc.cut_applies_late.set(true);
    assert_eq!(t.run(&["open-batch", "--members", "sp-c", "--skip-pregate"]), 1);
    assert!(t.err().contains("cannot tell whether"), "{}", t.err());
    assert!(!t.err().contains("cut refused"), "{}", t.err());
    assert!(
        t.git.calls.borrow().iter().all(|c| !c.starts_with("branch -D")),
        "{:?}",
        t.git.calls.borrow()
    );
}
