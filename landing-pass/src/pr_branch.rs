//! `pr-pass-branch.sh`, ported (sp-t4y60): rebase, confine, force-push, open or refresh a
//! pull request, observe merged/closed — for one branch. This module IS what
//! `PrTools::branch_helper` used to reach by spawning the bash script; the per-repo walk
//! (pr.rs) now calls [`run`] in-process instead. The exit-code shape (0–8) is kept
//! identical to the bash script's documented contract, because pr.rs's caller still matches
//! on it (`matches!(rc, 0 | 7 | 8)`) and the log lines below are grepped by operators today.
//!
//! # Exit codes (unchanged from pr-pass-branch.sh)
//! 0 opened/refreshed · 1 push or PR creation failed · 2 confine violation (reopened) ·
//! 3 rebase conflict (reopened) · 4 bead no longer closed (race) · 5 confine inconclusive
//! (deferred) · 6 already submitted, nothing to do · 7 merged, delivered ·
//! 8 closed unmerged, returned.

use crate::model::RepoRow;
use crate::ports::{Beads, Git, Lib, Tools};
use crate::records::Files;
use std::path::Path;

pub struct Ctx<'a> {
    pub lib: &'a dyn Lib,
    pub git: &'a dyn Git,
    pub beads: &'a dyn Beads,
    pub tools: &'a dyn Tools,
    pub files: &'a Files,
    pub pr_refresh_max: u32,
    pub log: &'a dyn Fn(&str),
}

/// `pr-pass-branch.sh <repo> <br> <id> <baseref> <name> <tip>`, plus `base_fq` and
/// `base_branch`/`remote` already resolved by the pass's own context load (no per-branch
/// `qualify_base_ref` re-derivation needed — the base does not move mid-pass).
#[allow(clippy::too_many_arguments)]
pub fn run(c: &Ctx, repo: &Path, br: &str, id: &str, baseref: &str, name: &str, _tip: &str, base_fq: &str, repo_row: &RepoRow) -> i32 {
    let remote = repo_row.base_remote.clone().unwrap_or_else(|| "origin".to_string());
    let base_branch = repo_row.base_branch.clone();

    let mut refresh = 0u32;
    match c.tools.forge_pr_state(repo, br).as_deref() {
        Some("merged") => {
            let merge_sha = c.git.rev_parse(repo, base_fq).unwrap_or_else(|| baseref.to_string());
            c.lib.deliver_pr_merged(repo, id, br, &merge_sha);
            (c.log)(&format!("{br}'s pull request is merged in {name} — delivered"));
            return 7;
        }
        Some("closed") => {
            c.lib.deliver_pr_closed(id, "pull request closed unmerged");
            (c.log)(&format!("{br}'s pull request was closed unmerged in {name} — returned"));
            return 8;
        }
        Some("open") if !c.git.is_ancestor(repo, base_fq, br) => {
            refresh = 1;
            (c.log)(&format!("{id}: {br} is behind {baseref} — rebasing its pull request onto it"));
        }
        _ => {}
    }

    // ── rebase ──────────────────────────────────────────────────────────────────────────
    let rb = c.lib.rebase(br, base_fq, repo, name);
    if !rb.ok {
        if rb.failure != "conflict" {
            (c.log)(&format!(
                "{id}: could not attempt a rebase of {br} onto {baseref} ({}) — not a conflict, leaving the bead closed",
                if rb.failure.is_empty() { "unknown" } else { &rb.failure }
            ));
            if rb.failure == "rebase-refused" {
                c.lib.ask_rebase_refused(id, br, name, if rb.refused_reason.is_empty() { "unknown" } else { &rb.refused_reason });
            }
            return 1;
        }
        if c.lib.pr_merged(repo, br) {
            (c.log)(&format!("{br} does not rebase onto {baseref}, but its pull request is merged — landed, not stuck"));
            return 0;
        }
        let reopen_note = c.lib.conflict_note(&[&repo.to_string_lossy(), br, base_fq, name, &rb.conflicts, "landing-pass"]);
        let others = c.lib.other_beads(repo, br, base_fq, &rb.conflicts);
        c.lib.bump_requeue(id, "merge-conflict");
        let rq_n = c.lib.requeues_of(id);
        if rq_n >= 3 {
            c.lib.reopen(id, "rebase-conflict", &reopen_note);
            c.lib.ask_rebase_loop(&[id, br, name, &rq_n.to_string(), if rb.conflicts.is_empty() { "unknown" } else { &rb.conflicts }, &others]);
            (c.log)(&format!("escalated {id} — rebase conflict x{rq_n} on {br}"));
        } else {
            c.lib.reopen(id, "rebase-conflict", &reopen_note);
            (c.log)(&format!("reopened {id} — does not rebase onto {baseref}"));
            c.lib.event(
                "bead.reopened",
                id,
                &format!("reopened {id} — {br} does not rebase onto {baseref} in {name}"),
                &format!("conflicts in {}; the next aeon is handed the rebase", if rb.conflicts.is_empty() { "unknown" } else { &rb.conflicts }),
            );
        }
        return 3;
    }

    // ── confinement ─────────────────────────────────────────────────────────────────────
    let (crc, cout) = c.tools.confine(id, br, repo, base_fq, "");
    if crc == 1 {
        c.lib.reopen(id, "confine-fail", &format!("Reopened by landing-pass: {cout}"));
        (c.log)(cout.lines().next().unwrap_or(""));
        return 2;
    } else if crc != 0 {
        (c.log)(&format!("confine.sh could not evaluate: {}", cout.lines().next().unwrap_or("")));
        return 5;
    }

    // ── re-read before landing: a race with reopen/reclaim since the scan ─────────────────
    let cur_st = c.beads.bead_lc_state(id);
    if !crate::model::handed_on(&cur_st) {
        (c.log)(&format!("bead is now {cur_st} (was closed at scan time) — not landing {br}"));
        return 4;
    }

    // ── open or refresh the pull request ───────────────────────────────────────────────
    if land_pr(c, repo, br, id, &remote, &base_branch) {
        if refresh > 0 {
            (c.log)(&format!("refreshed {br} onto {baseref} in {name} — rebased and force-pushed"));
        } else {
            (c.log)(&format!("opened a pull request for {br} in {name}"));
        }
        0
    } else {
        1
    }
}

/// `land_pr` (lib.sh 9254–9305): force-push-with-lease, open a PR if none exists (after a
/// dedup scan against every other open PR), arm squash auto-merge.
fn land_pr(c: &Ctx, repo: &Path, br: &str, id: &str, remote: &str, base_branch: &str) -> bool {
    if c.lib.force_push(repo, remote, br).is_err() {
        (c.log)(&format!("{id}: could not push {br} to {remote}"));
        return false;
    }
    // The PR may already exist (a prior pass opened it and this is a refresh); ask directly.
    let mut num = pr_number(c, repo, br);
    if num.is_none() {
        // DOES ANOTHER OPEN PR ALREADY CARRY THIS WORK? (sp-pd-ci)
        if let Some(dup) = duplicate_open_pr(c, repo, br, remote) {
            (c.log)(&format!("{id}: #{dup} already carries every commit on {br} — not opening a second pull request"));
            c.lib.note(id, &format!("Not opening a pull request: #{dup} already carries every commit on {br}. Continue the review there rather than splitting it across two threads."));
            return false;
        }
        let title = c.beads.show(&[id.to_string()]).ok().and_then(|v| v.into_iter().next()).map(|b| b.title).unwrap_or_default();
        let title = if title.is_empty() { "Spira".to_string() } else { title };
        let pr_title = format!("{id}: {title}");
        let body = "Filed by Spira for bead ".to_string()
            + id
            + ". The bead is closed in the Spira database; this\npull request is how the work lands, so it is not done until this merges.\n\nAuto-merge is armed — a green run merges it without anyone waiting on it.\n";
        let Some(n) = c.tools.forge_pr_create(repo, br, base_branch, &pr_title, &body) else {
            (c.log)(&format!("{id}: gh pr create failed for {br}"));
            return false;
        };
        num = Some(n);
    }
    if !c.tools.forge_pr_automerge(repo, br) {
        (c.log)(&format!("{id}: pull request {} is open but auto-merge could not be armed", num.map(|n| n.to_string()).unwrap_or_else(|| "?".into())));
    }
    (c.log)(&format!("{id}: pull request {} open on {br} — its CI is the gate now", num.map(|n| n.to_string()).unwrap_or_else(|| "?".into())));
    true
}

fn pr_number(c: &Ctx, repo: &Path, br: &str) -> Option<u64> {
    c.tools.forge_pr_list_open(repo).into_iter().find(|(_, h)| h == br).map(|(n, _)| n)
}

/// Another open PR whose head already carries every commit on `br` — a parallel duplicate,
/// catching the sp-pd-ci case (#114 carried all nineteen of #113's commits). Uses
/// `content_on_base`'s merge-tree equivalence (this crate's own primitive for exactly "does
/// X already contain every change on Y") rather than bash's per-commit SHA-ancestor loop —
/// more robust to the candidate branch having been rebased or amended since it diverged
/// (a named difference from bash's `land_pr`, DESIGN.md). The candidate ref is qualified
/// with the caller's own resolved remote, not the literal `origin` bash hardcoded here —
/// the same rule `land_pr` already applies to its own push and base.
fn duplicate_open_pr(c: &Ctx, repo: &Path, br: &str, remote: &str) -> Option<u64> {
    for (n, head) in c.tools.forge_pr_list_open(repo) {
        if head == br {
            continue;
        }
        let candidate_ref = format!("{remote}/{head}");
        if c.git.content_on_base(repo, br, &candidate_ref) {
            return Some(n);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LandMode, Rebase};
    use crate::ports::Beads;
    use crate::records::Files;
    use crate::testutil::tmpdir;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[derive(Default)]
    struct FLib {
        calls: RefCell<Vec<String>>,
        rebase: RefCell<Option<Rebase>>,
        pr_merged: Cell<bool>,
        requeues: Cell<u32>,
        force_push_err: RefCell<Option<String>>,
    }
    impl FLib {
        fn rec(&self, s: String) {
            self.calls.borrow_mut().push(s);
        }
        fn has(&self, prefix: &str) -> bool {
            self.calls.borrow().iter().any(|c| c.starts_with(prefix))
        }
    }
    impl Lib for FLib {
        fn reopen(&self, id: &str, cause: &str, _note: &str) {
            self.rec(format!("reopen {id} {cause}"));
        }
        fn event(&self, kind: &str, id: &str, _title: &str, _detail: &str) {
            self.rec(format!("event {kind} {id}"));
        }
        fn noverdict(&self, _: &str, _: &str, _: &str, _: &str, _: &str, _: &str) {}
        fn incident(&self, _: &str, _: &str, _: &str, _: &str, _: &str) -> Result<String, i32> {
            Ok(String::new())
        }
        fn ask_rebase_loop(&self, a: &[&str]) {
            self.rec(format!("ask_rebase_loop {}", a.join(" ")));
        }
        fn ask_red_recurring(&self, _: &str, _: &str, _: &str, _: &str, _: &str) {}
        fn ask_rebase_refused(&self, id: &str, _: &str, _: &str, reason: &str) {
            self.rec(format!("ask_rebase_refused {id} {reason}"));
        }
        fn ask_budget_deferred(&self, _: &str, _: &str, _: u32) {}
        fn rebase(&self, br: &str, _: &str, _: &Path, _: &str) -> Rebase {
            self.rec(format!("rebase {br}"));
            self.rebase.borrow().clone().unwrap_or(Rebase { ok: true, ..Default::default() })
        }
        fn recut(&self, _: &str, _: &str, _: &Path, _: &str) -> crate::model::Recut {
            Default::default()
        }
        fn bump_requeue(&self, id: &str, reason: &str) {
            self.rec(format!("bump_requeue {id} {reason}"));
        }
        fn requeues_of(&self, _: &str) -> u32 {
            self.requeues.get()
        }
        fn conflict_note(&self, _a: &[&str]) -> String {
            "note".into()
        }
        fn other_beads(&self, _: &Path, _: &str, _: &str, _: &str) -> String {
            String::new()
        }
        fn pr_merged(&self, _: &Path, _: &str) -> bool {
            self.pr_merged.get()
        }
        fn note(&self, id: &str, text: &str) {
            self.rec(format!("note {id} {text}"));
        }
        fn push(&self, _: &Path, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn land_subject(&self, id: &str) -> String {
            format!("spira: land {id}")
        }
        fn deliver_delivered(&self, _: &str, _: &str) {}
        fn deliver_requeued(&self, _: &str, _: &str) {}
        fn deliver_returned(&self, _: &str, _: &str) {}
        fn closeout(&self, _: &str, _: &str, _: &Path) {}
        fn close_on_land(&self, _: &str, _: &str) {}
        fn prune_worktrees(&self, _: &Path) {}
        fn gh_unlanded_scan(&self) {}
        fn ask_refresh_loop(&self, _: &Path, _: &str, br: &str, id: &str, _: &str, n: u32) {
            self.rec(format!("ask_refresh_loop {id} {br} {n}"));
        }
        fn deliver_pr_merged(&self, _: &Path, id: &str, br: &str, sha: &str) {
            self.rec(format!("deliver_pr_merged {id} {br} {sha}"));
        }
        fn deliver_pr_closed(&self, id: &str, reason: &str) {
            self.rec(format!("deliver_pr_closed {id} {reason}"));
        }
        fn force_push(&self, _: &Path, remote: &str, br: &str) -> Result<(), String> {
            self.rec(format!("force_push {remote} {br}"));
            match self.force_push_err.borrow().clone() {
                Some(e) => Err(e),
                None => Ok(()),
            }
        }
    }

    #[derive(Default)]
    struct FGit {
        /// (a, b): is_ancestor(a, b) is true.
        ancestors: RefCell<Vec<(String, String)>>,
        tips: RefCell<HashMap<String, String>>,
    }
    impl Git for FGit {
        fn spira_refs(&self, _: &Path) -> Vec<(String, String)> {
            Vec::new()
        }
        fn branch_exists(&self, _: &Path, _: &str) -> bool {
            true
        }
        fn rev_parse(&self, _: &Path, rev: &str) -> Option<String> {
            self.tips.borrow().get(rev).cloned().or_else(|| Some(format!("{rev}-sha")))
        }
        fn is_ancestor(&self, _: &Path, a: &str, b: &str) -> bool {
            self.ancestors.borrow().iter().any(|(x, y)| x == a && y == b)
        }
        fn content_on_base(&self, _: &Path, _: &str, _: &str) -> bool {
            false
        }
        fn count(&self, _: &Path, _: &str) -> Option<u64> {
            Some(1)
        }
        fn fetch(&self, _: &Path, _: &str) {}
        fn tree_ok(&self, _: &Path) -> bool {
            false
        }
        fn tree_add_detached(&self, _: &Path, _: &Path, _: &str) {}
        fn tree_checkout_landing(&self, _: &Path, _: &str) -> bool {
            false
        }
        fn tree_merge(&self, _: &Path, _: &str, _: &str, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn tree_head(&self, _: &Path) -> Option<String> {
            None
        }
        fn tree_reset_hard(&self, _: &Path, _: &str) {}
        fn tree_merge_abort(&self, _: &Path) {}
        fn delete_branch(&self, _: &Path, _: &str) -> bool {
            true
        }
        fn refs_matching(&self, _: &Path, _: &str) -> Vec<String> {
            Vec::new()
        }
        fn log_grep(&self, _: &Path, _: &str, _: &[String]) -> Option<String> {
            None
        }
        fn merge_base(&self, _: &Path, _: &str, _: &str) -> Option<String> {
            None
        }
        fn log_subjects(&self, _: &Path, _: &str, _: &[&str]) -> Option<String> {
            None
        }
        fn commit_body(&self, _: &Path, _: &str) -> Option<String> {
            None
        }
    }

    #[derive(Default)]
    struct FBeads {
        status: RefCell<HashMap<String, String>>,
    }
    impl Beads for FBeads {
        fn show(&self, ids: &[String]) -> Result<Vec<crate::model::BeadRow>, String> {
            Ok(ids
                .iter()
                .map(|id| crate::model::BeadRow {
                    id: id.clone(),
                    state: "SUBMITTED".into(),
                    repo: "spira".into(),
                    labels: Vec::new(),
                    superseded: false,
                    closed_at: "2026-09-30".into(),
                    priority: 1,
                    external_ref: None,
                    title: "a bead".into(),
                    notes: Vec::new(),
                })
                .collect())
        }
        fn bead_lc_state(&self, id: &str) -> String {
            self.status.borrow().get(id).cloned().unwrap_or_else(|| "SUBMITTED".into())
        }
        fn ask_open(&self, _label: &str, _subject: &str) -> bool {
            false
        }
        fn context(&self, id: &str, _now: i64) -> String {
            format!("(fixture context for {id})")
        }
    }

    #[derive(Default)]
    struct FTools {
        confine: Cell<i32>,
        pr_state: RefCell<Option<String>>,
        pr_create_n: Cell<Option<u64>>,
        pr_list_open: RefCell<Vec<(u64, String)>>,
        automerge_ok: Cell<bool>,
        forge_calls: RefCell<Vec<String>>,
    }
    impl Tools for FTools {
        fn gate(&self, _: &str, _: &str, _: &str, _: &str) -> (i32, String) {
            (0, String::new())
        }
        fn gate_start(&self, _: &str, _: &str, _: &str, _: &str) -> u64 {
            0
        }
        fn gate_wait_any(&self) -> Option<(u64, i32, String)> {
            None
        }
        fn gate_slots_free(&self, _: usize) -> Option<usize> {
            None
        }
        fn gate_status(&self, _: &str, _: &str) -> Option<String> {
            None
        }
        fn confine(&self, _: &str, _: &str, _: &Path, _: &str, _: &str) -> (i32, String) {
            (self.confine.get(), "confine says no\n".into())
        }
        fn queue_step(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }
        fn skew_refresh(&self, _: &Path) -> String {
            String::new()
        }
        fn ensure(&self, _: &Path) -> Vec<String> {
            Vec::new()
        }
        fn rebase_stale(&self, _: &str, _: &str) -> i32 {
            3
        }
        fn forge_pr_state(&self, _: &Path, selector: &str) -> Option<String> {
            self.forge_calls.borrow_mut().push(format!("pr-state {selector}"));
            self.pr_state.borrow().clone()
        }
        fn forge_pr_create(&self, _: &Path, head: &str, base: &str, title: &str, _: &str) -> Option<u64> {
            self.forge_calls.borrow_mut().push(format!("pr-create {head} {base} {title}"));
            self.pr_create_n.get()
        }
        fn forge_pr_list_open(&self, _: &Path) -> Vec<(u64, String)> {
            self.forge_calls.borrow_mut().push("pr-list-open".into());
            self.pr_list_open.borrow().clone()
        }
        fn forge_pr_automerge(&self, _: &Path, selector: &str) -> bool {
            self.forge_calls.borrow_mut().push(format!("pr-automerge {selector}"));
            self.automerge_ok.get()
        }
    }

    fn repo_row() -> RepoRow {
        RepoRow {
            name: "spira".into(),
            path: PathBuf::from("/repo"),
            mode: LandMode::Pr,
            landref: Some("origin/main".into()),
            base_fq: Some("refs/remotes/origin/main".into()),
            base_remote: Some("origin".into()),
            base_branch: "main".into(),
            forge_ref: Some("refs/remotes/origin/main".into()),
        }
    }

    struct Fixture {
        lib: FLib,
        git: FGit,
        beads: FBeads,
        tools: FTools,
        files: Files,
        _dir: crate::testutil::TmpDir,
        logs: RefCell<Vec<String>>,
    }
    impl Fixture {
        fn new() -> Fixture {
            let dir = tmpdir("prbr");
            Fixture {
                lib: FLib::default(),
                git: FGit::default(),
                beads: FBeads::default(),
                tools: FTools::default(),
                files: Files::new(&dir),
                _dir: dir,
                logs: RefCell::new(Vec::new()),
            }
        }
        fn run(&self, br: &str, id: &str, tip: &str) -> i32 {
            let row = repo_row();
            let log = |m: &str| self.logs.borrow_mut().push(m.to_string());
            let ctx = Ctx { lib: &self.lib, git: &self.git, beads: &self.beads, tools: &self.tools, files: &self.files, pr_refresh_max: 5, log: &log };
            run(&ctx, Path::new("/repo"), br, id, "origin/main", "spira", tip, "refs/remotes/origin/main", &row)
        }
    }

    #[test]
    fn a_fresh_branch_opens_a_pull_request() {
        let f = Fixture::new();
        f.tools.pr_create_n.set(Some(9));
        f.git.tips.borrow_mut().insert("spira/sp-a".into(), "t1".into());
        let rc = f.run("spira/sp-a", "sp-a", "t1");
        assert_eq!(rc, 0);
        assert!(f.lib.has("force_push origin spira/sp-a"));
        assert!(f.logs.borrow().iter().any(|l| l.contains("opened a pull request")));
    }

    #[test]
    fn a_conflicting_rebase_reopens_and_marks_red() {
        let f = Fixture::new();
        *f.lib.rebase.borrow_mut() = Some(Rebase { ok: false, failure: "conflict".into(), conflicts: "a.rs".into(), refused_reason: String::new() });
        let rc = f.run("spira/sp-a", "sp-a", "t1");
        assert_eq!(rc, 3);
        assert!(f.lib.has("reopen sp-a rebase-conflict"));
    }

    /// Regression (sp-li2pv, same defect class as sp-cgklh in landing.sh's CHECK6 loop):
    /// past `SPIRA_REBASE_ESCALATE_AT` the escalate branch must STILL reopen the bead, not
    /// only ask. The historical bug called `spira_ask_rebase_loop` without ever calling
    /// `bead_reopen`, leaving the bead closed with an unlandable branch no aeon could claim.
    /// Ported from test-landing-pass.sh TEST 4, retired with pr-pass-branch.sh (sp-t4y60).
    #[test]
    fn escalating_past_the_rebase_threshold_still_reopens_the_bead() {
        let f = Fixture::new();
        *f.lib.rebase.borrow_mut() = Some(Rebase { ok: false, failure: "conflict".into(), conflicts: "shared.txt".into(), refused_reason: String::new() });
        f.lib.requeues.set(3); // at SPIRA_REBASE_ESCALATE_AT (3)
        let rc = f.run("spira/sp-a", "sp-a", "t1");
        assert_eq!(rc, 3);
        assert!(f.lib.has("reopen sp-a rebase-conflict"), "the escalation ask is not a substitute for reopening");
        assert!(f.lib.has("ask_rebase_loop"), "and the escalation ask still fires");
    }

    /// Ported from test-landing-pass.sh TEST 2 (retired with pr-pass-branch.sh, sp-t4y60):
    /// the PR opens against the repository's own base branch, never the literal `main` —
    /// three repositories in this tree default to `master`.
    #[test]
    fn opens_the_pull_request_against_the_repositorys_own_base_branch_not_a_literal_main() {
        let f = Fixture::new();
        f.tools.pr_create_n.set(Some(1));
        let row = RepoRow {
            name: "spira".into(),
            path: PathBuf::from("/repo"),
            mode: LandMode::Pr,
            landref: Some("origin/master".into()),
            base_fq: Some("refs/remotes/origin/master".into()),
            base_remote: Some("origin".into()),
            base_branch: "master".into(),
            forge_ref: Some("refs/remotes/origin/master".into()),
        };
        let log = |m: &str| f.logs.borrow_mut().push(m.to_string());
        let ctx = Ctx { lib: &f.lib, git: &f.git, beads: &f.beads, tools: &f.tools, files: &f.files, pr_refresh_max: 5, log: &log };
        let rc = run(&ctx, Path::new("/repo"), "spira/sp-a", "sp-a", "origin/master", "spira", "t1", "refs/remotes/origin/master", &row);
        assert_eq!(rc, 0);
        assert!(f.tools.forge_calls.borrow().iter().any(|c| c == "pr-create spira/sp-a master sp-a: a bead"), "{:?}", f.tools.forge_calls.borrow());
    }

    #[test]
    fn a_conflicting_rebase_whose_pr_already_merged_is_not_reopened() {
        let f = Fixture::new();
        *f.lib.rebase.borrow_mut() = Some(Rebase { ok: false, failure: "conflict".into(), conflicts: String::new(), refused_reason: String::new() });
        f.lib.pr_merged.set(true);
        let rc = f.run("spira/sp-a", "sp-a", "t1");
        assert_eq!(rc, 0);
        assert!(!f.lib.has("reopen"), "a merged PR's stuck rebase is landed, not a defect");
    }

    #[test]
    fn a_rebase_refusal_asks_rather_than_reopens() {
        let f = Fixture::new();
        *f.lib.rebase.borrow_mut() = Some(Rebase { ok: false, failure: "rebase-refused".into(), conflicts: String::new(), refused_reason: "dirty worktree".into() });
        let rc = f.run("spira/sp-a", "sp-a", "t1");
        assert_eq!(rc, 1);
        assert!(f.lib.has("ask_rebase_refused sp-a dirty worktree"));
        assert!(!f.lib.has("reopen"), "not a conflict — the bead stays closed");
    }

    #[test]
    fn a_confine_violation_reopens_and_never_lands() {
        let f = Fixture::new();
        f.tools.confine.set(1);
        f.git.tips.borrow_mut().insert("spira/sp-a".into(), "t1".into());
        let rc = f.run("spira/sp-a", "sp-a", "t1");
        assert_eq!(rc, 2);
        assert!(f.lib.has("reopen sp-a confine-fail"));
    }

    #[test]
    fn a_confine_that_cannot_evaluate_is_deferred_not_a_violation() {
        let f = Fixture::new();
        f.tools.confine.set(2);
        let rc = f.run("spira/sp-a", "sp-a", "t1");
        assert_eq!(rc, 5);
        assert!(!f.lib.has("reopen"), "inconclusive is deferred, never charged to the bead");
    }

    #[test]
    fn a_bead_reclaimed_since_the_scan_is_not_landed() {
        let f = Fixture::new();
        f.beads.status.borrow_mut().insert("sp-a".into(), "WORKING".into());
        let rc = f.run("spira/sp-a", "sp-a", "t1");
        assert_eq!(rc, 4);
    }

    #[test]
    fn a_duplicate_pull_request_is_noted_and_not_reopened_as_a_second_pr() {
        let f = Fixture::new();
        // content_on_base defaults false in FGit, so make the candidate branch look like it
        // already carries br's work by overriding content_on_base via a second fixture git.
        struct DupGit(FGit);
        impl Git for DupGit {
            fn spira_refs(&self, r: &Path) -> Vec<(String, String)> {
                self.0.spira_refs(r)
            }
            fn branch_exists(&self, r: &Path, b: &str) -> bool {
                self.0.branch_exists(r, b)
            }
            fn rev_parse(&self, r: &Path, rev: &str) -> Option<String> {
                self.0.rev_parse(r, rev)
            }
            fn is_ancestor(&self, r: &Path, a: &str, b: &str) -> bool {
                self.0.is_ancestor(r, a, b)
            }
            fn content_on_base(&self, _: &Path, branch: &str, base: &str) -> bool {
                branch == "spira/sp-a" && base == "origin/spira/sp-b"
            }
            fn count(&self, r: &Path, x: &str) -> Option<u64> {
                self.0.count(r, x)
            }
            fn fetch(&self, r: &Path, x: &str) {
                self.0.fetch(r, x)
            }
            fn tree_ok(&self, r: &Path) -> bool {
                self.0.tree_ok(r)
            }
            fn tree_add_detached(&self, r: &Path, t: &Path, a: &str) {
                self.0.tree_add_detached(r, t, a)
            }
            fn tree_checkout_landing(&self, t: &Path, b: &str) -> bool {
                self.0.tree_checkout_landing(t, b)
            }
            fn tree_merge(&self, t: &Path, s: &str, b: &str, n: &str, e: &str) -> Result<(), String> {
                self.0.tree_merge(t, s, b, n, e)
            }
            fn tree_head(&self, t: &Path) -> Option<String> {
                self.0.tree_head(t)
            }
            fn tree_reset_hard(&self, t: &Path, x: &str) {
                self.0.tree_reset_hard(t, x)
            }
            fn tree_merge_abort(&self, t: &Path) {
                self.0.tree_merge_abort(t)
            }
            fn delete_branch(&self, r: &Path, b: &str) -> bool {
                self.0.delete_branch(r, b)
            }
            fn refs_matching(&self, r: &Path, p: &str) -> Vec<String> {
                self.0.refs_matching(r, p)
            }
            fn log_grep(&self, r: &Path, g: &str, refs: &[String]) -> Option<String> {
                self.0.log_grep(r, g, refs)
            }
            fn merge_base(&self, r: &Path, a: &str, b: &str) -> Option<String> {
                self.0.merge_base(r, a, b)
            }
            fn log_subjects(&self, r: &Path, range: &str, paths: &[&str]) -> Option<String> {
                self.0.log_subjects(r, range, paths)
            }
            fn commit_body(&self, r: &Path, sha: &str) -> Option<String> {
                self.0.commit_body(r, sha)
            }
        }
        let dup_git = DupGit(FGit::default());
        f.tools.pr_list_open.borrow_mut().push((113, "spira/sp-b".into()));
        let row = repo_row();
        let log = |m: &str| f.logs.borrow_mut().push(m.to_string());
        let ctx = Ctx { lib: &f.lib, git: &dup_git, beads: &f.beads, tools: &f.tools, files: &f.files, pr_refresh_max: 5, log: &log };
        let rc = run(&ctx, Path::new("/repo"), "spira/sp-a", "sp-a", "origin/main", "spira", "t1", "refs/remotes/origin/main", &row);
        assert_eq!(rc, 1, "not opening a duplicate PR is a no-op this pass, not a defect");
        assert!(f.lib.has("note sp-a"));
        assert!(f.tools.pr_create_n.get().is_none() || !f.logs.borrow().iter().any(|l| l.contains("opened a pull request")));
    }

}
