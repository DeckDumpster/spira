//! The Hel (DESIGN.md §2): every `spira/*` branch gets exactly one disposition, and
//! only a branch whose every change the base already holds is ever deleted.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::git::Git;
use crate::ports::{Base, Repo, Sent, World};

/// Which repositories one invocation sweeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    All,
    /// The sentinel's per-pass call: queue-mode repos reap at landing (`spira-lc close-on-land`).
    SkipQueue,
    /// The daily straggler sweep: queue-mode repos only.
    QueueOnly,
}

#[derive(Debug, Clone)]
pub struct Opts {
    pub dry: bool,
    pub fetch: bool,
    /// One bead id or branch; None sweeps every branch (and PASS 2).
    pub only: Option<String>,
    pub scope: Scope,
    /// Wall-clock allowance for one invocation; spent, the sweep stops and exits 0 with the
    /// remainder for the next tick (law-calls-have-deadlines).
    pub budget: Option<Duration>,
}

/// One branch's disposition: what may happen to it, and why. Decided by reading only —
/// no ref, worktree or label is touched deciding it (UC-landed-audit-reaping-16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disp {
    SendContentLanded,
    SendOtherPr,
    ReapSupersededSafe,
    ReapSquashMerged,
    KeepSupersededUnsafe,
    KeepUnlanded,
    OrphanNoBead,
    /// The lifecycle row is terminal and the base lacks the branch's work: nothing will
    /// ever land it, so it is parked under refs/archive, never left to stand or deleted.
    ArchiveTerminal,
}

impl Disp {
    /// The shell's `<VERB> <CODE>` spelling (send_disposition's own output), for the tests.
    #[cfg(test)]
    pub fn code(self) -> &'static str {
        match self {
            Disp::SendContentLanded => "SEND content-landed",
            Disp::SendOtherPr => "SEND other-pr",
            Disp::ReapSupersededSafe => "REAP superseded-safe",
            Disp::ReapSquashMerged => "REAP squash-merged",
            Disp::KeepSupersededUnsafe => "KEEP superseded-unsafe",
            Disp::KeepUnlanded => "KEEP unlanded",
            Disp::OrphanNoBead => "ORPHAN no-bead",
            Disp::ArchiveTerminal => "ARCHIVE terminal",
        }
    }
}

fn labels(b: &Value) -> Vec<String> {
    b.get("labels").and_then(Value::as_array).map(|a| a.iter().filter_map(|l| l.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// bd list and bd show spell the dependency type differently; both count
/// (law-absence-needs-a-positive-control).
fn superseded(b: &Value) -> bool {
    b.get("dependencies").and_then(Value::as_array).is_some_and(|deps| {
        deps.iter().any(|d| d.get("dependency_type").or_else(|| d.get("type")).and_then(Value::as_str) == Some("supersedes"))
    })
}

pub struct Ctx<'a> {
    pub w: &'a dyn World,
    pub repo: &'a Path,
    pub name: &'a str,
    pub base: &'a Base,
    pub submitted_label: &'a str,
}

/// The facts a disposition reads, each asked only when the rules reach it: the forge is asked
/// after every local check has said no. The live sweep answers from git and the world; the
/// scheduler's `hel.sweep` answers from its snapshot.
pub trait Facts {
    fn content_on_base(&self) -> bool;
    fn has_bead(&self) -> bool;
    fn superseded(&self) -> bool;
    fn ahead(&self) -> Option<u64>;
    fn merges_clean(&self) -> bool;
    fn terminal(&self) -> bool;
    /// The bead carries the submitted label, or its lifecycle row is past the builder.
    fn submitted(&self) -> bool;
    fn tip(&self) -> Option<String>;
    /// The merged PR's head for the branch (the network call).
    fn pr_merged_tip(&self) -> Option<String>;
    fn landed(&self) -> bool;
    fn cherry_unapplied(&self) -> bool;
    fn landed_by_subject(&self) -> bool;
}

/// The rules of send_disposition, in order, over [`Facts`].
pub fn judge(f: &dyn Facts) -> Disp {
    if f.content_on_base() {
        return Disp::SendContentLanded;
    }
    let has_bead = f.has_bead();

    // A SUPERSEDED BEAD'S BRANCH: its work landed under the successor's id. The edge is a
    // claim, not a proof — reap only when merging it would conflict (nothing of its own is
    // missing from the base), or when it carries nothing at all.
    if has_bead && f.superseded() {
        let n = f.ahead();
        if n != Some(0) && f.merges_clean() {
            if f.terminal() {
                return Disp::ArchiveTerminal;
            }
            return Disp::KeepSupersededUnsafe;
        }
        return Disp::ReapSupersededSafe;
    }

    // SQUASH-MERGED: a merged PR whose head is still the branch tip captured every commit.
    // The network call, reached only here.
    if has_bead && f.submitted() {
        let tip = f.tip();
        if let Some(pr) = f.pr_merged_tip() {
            if !pr.is_empty() && tip.as_deref() == Some(pr.as_str()) {
                return Disp::ReapSquashMerged;
            }
        }
    }

    // LANDED BY OTHER PR: the lifecycle record says the bead LANDED (`spira-lc state`, the one
    // record — never a commit-subject search) — but that landing may be about a PRIOR push of
    // this branch, so every commit unique to it must already be on the base (git cherry), or
    // this would delete work under a LANDED that is true about the past. A branch whose proof
    // fails is archived, not kept: the commits stay reachable at refs/archive.
    if has_bead && f.landed() {
        if f.cherry_unapplied() && !f.landed_by_subject() {
            return Disp::ArchiveTerminal;
        }
        return Disp::SendOtherPr;
    }

    if has_bead {
        if f.terminal() {
            return Disp::ArchiveTerminal;
        }
        return Disp::KeepUnlanded;
    }
    // No bead, and not an ancestor (that one was sent at the first check): real commits
    // with no owner to land them — archived, never plain-deleted.
    Disp::OrphanNoBead
}

/// A content-landed branch with commits, none of which names the bead, is kept: an empty
/// diff is not evidence of landing.
/// Each fact is asked only when the ones before it have not settled the answer.
pub fn needs_evidence(ahead: u64, has_own_commit: impl FnOnce() -> bool, only_stacked_base: impl FnOnce() -> bool, base_names: impl FnOnce() -> bool) -> bool {
    ahead > 0 && !has_own_commit() && !only_stacked_base() && !base_names()
}

/// The live facts of one branch: git on the checkout, the bead read once, the world.
struct Live<'a, 'b> {
    c: &'a Ctx<'b>,
    id: &'a str,
    br: &'a str,
    bead: std::cell::OnceCell<Option<Value>>,
}

impl Live<'_, '_> {
    fn g(&self) -> Git<'_> {
        Git(self.c.repo)
    }
    fn bead(&self) -> Option<&Value> {
        self.bead.get_or_init(|| self.c.w.bead(self.id)).as_ref()
    }
}

impl Facts for Live<'_, '_> {
    fn content_on_base(&self) -> bool {
        self.g().content_on_base(self.br, &self.c.base.landref)
    }
    fn has_bead(&self) -> bool {
        self.bead().is_some()
    }
    fn superseded(&self) -> bool {
        self.bead().is_some_and(superseded)
    }
    fn ahead(&self) -> Option<u64> {
        self.g().ahead(&self.c.base.landref, self.br)
    }
    fn merges_clean(&self) -> bool {
        self.g().merges_clean(&self.c.base.landref, self.br)
    }
    fn terminal(&self) -> bool {
        self.c.w.lc_terminal(self.id)
    }
    fn submitted(&self) -> bool {
        self.bead().is_some_and(|b| labels(b).iter().any(|l| l == self.c.submitted_label)) || self.c.w.lc_past_builder(self.id)
    }
    fn tip(&self) -> Option<String> {
        self.g().rev_parse(self.br)
    }
    fn pr_merged_tip(&self) -> Option<String> {
        self.c.w.pr_merged_tip(self.c.repo, self.br)
    }
    fn landed(&self) -> bool {
        self.c.w.lc_landed(self.id)
    }
    fn cherry_unapplied(&self) -> bool {
        self.g().cherry_unapplied(&self.c.base.landref, self.br)
    }
    fn landed_by_subject(&self) -> bool {
        self.g().landed_by_subject(&self.c.base.landref, self.br, self.id, 2000)
    }
}

/// send_disposition. The bead is read once; the forge is asked only after every local
/// check has said no.
pub fn disposition(c: &Ctx, id: &str, br: &str) -> Disp {
    judge(&Live { c, id, br, bead: std::cell::OnceCell::new() })
}

/// One branch's facts as a pass reads them, all of them, in-process: what the scheduler's
/// `hel.sweep` observes so it can judge without the world.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchScan {
    pub id: String,
    pub br: String,
    pub tip: String,
    pub ahead: Option<u64>,
    pub content_on_base: bool,
    pub merges_clean: bool,
    pub cherry_unapplied: bool,
    pub landed_by_subject: bool,
    pub has_own_commit: bool,
    pub only_stacked_base: bool,
    pub base_names: bool,
    pub worktree: Option<std::path::PathBuf>,
    pub labels: Vec<String>,
    pub superseded: bool,
    /// Asked only when the rules would reach the forge (a submitted bead not otherwise settled).
    pub pr_merged_tip: Option<String>,
    /// Why somebody may be home, from the holder witnesses.
    pub held: Option<String>,
}

/// One orphaned worktree: registered under the run's worktree directory, its branch gone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrphanScan {
    pub id: String,
    pub path: std::path::PathBuf,
    pub br: String,
    pub held: Option<String>,
}

/// One repository's reads for a pass: its base, every `spira/*` branch's facts (after the
/// fetch of the base's own remote, only when there is a branch), and its orphaned worktrees.
/// `Err` names why the repository is skipped.
pub fn scan_repo(w: &dyn World, root: &Path, submitted_label: &str, fetch: bool) -> Result<(Base, Vec<BranchScan>, Vec<OrphanScan>), String> {
    if !root.join(".git").exists() {
        return Err(format!("{} is not a git checkout", root.display()));
    }
    let g = Git(root);
    let brs = g.spira_branches();
    let base = w.base(root).ok_or("cannot resolve the ref it lands on — configure its `base`")?;
    if let Some(rem) = base.remote.as_deref() {
        if fetch && !brs.is_empty() {
            g.fetch(rem);
        }
    }
    if !brs.is_empty() {
        w.prefetch();
    }
    let lr = base.landref.as_str();
    let mut branches = vec![];
    for br in &brs {
        let id = br.strip_prefix("spira/").unwrap_or(br).to_string();
        let held = if id.starts_with("round-") { None } else { w.witness(&id) };
        let bead = w.bead(&id);
        let content = g.content_on_base(br, lr);
        let superseded_ = bead.as_ref().is_some_and(superseded);
        let labels_ = bead.as_ref().map(labels).unwrap_or_default();
        let submitted = bead.is_some() && (labels_.iter().any(|l| l == submitted_label) || w.lc_past_builder(&id));
        let pr_merged_tip = if held.is_none() && !content && !superseded_ && submitted { w.pr_merged_tip(root, br) } else { None };
        branches.push(BranchScan {
            tip: g.rev_parse(br).unwrap_or_default(),
            ahead: g.ahead(lr, br),
            content_on_base: content,
            merges_clean: g.merges_clean(lr, br),
            cherry_unapplied: g.cherry_unapplied(lr, br),
            landed_by_subject: g.landed_by_subject(lr, br, &id, 2000),
            has_own_commit: g.has_own_commit(lr, br, &id),
            only_stacked_base: g.only_stacked_base(lr, br),
            base_names: g.base_names(lr, &id, 2000),
            worktree: g.worktree_of(br),
            labels: labels_,
            superseded: superseded_,
            pr_merged_tip,
            held,
            br: br.clone(),
            id,
        });
    }
    let wts = w.worktrees();
    let mut orphans = vec![];
    for (path, br) in g.worktrees() {
        if !path.starts_with(&wts) || path == wts {
            continue;
        }
        let Some(id) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
        if id.starts_with('.') {
            continue;
        }
        let Some(br) = br else { continue };
        if g.branch_exists(&br) {
            continue;
        }
        let held = w.witness(&id);
        orphans.push(OrphanScan { id, path, br, held });
    }
    Ok((base, branches, orphans))
}

/// send_orphan's first half: park `br`'s tip at refs/archive/<br> and read it back. `Ok` is
/// the tip parked; the branch may then be reaped with caller `archived`.
pub fn archive_tip(root: &Path, br: &str, expect_tip: &str) -> Result<String, String> {
    let g = Git(root);
    let tip = g.rev_parse(br).ok_or_else(|| format!("cannot resolve tip of {br}"))?;
    if !expect_tip.is_empty() && tip != expect_tip {
        return Err(format!("{br} moved from {expect_tip} to {tip} since it was judged — left for the next pass"));
    }
    let archive = format!("refs/archive/{br}");
    if !g.update_ref(&archive, &tip) {
        return Err(format!("update-ref {archive} failed"));
    }
    let back = g.verify(&archive);
    if back.as_deref() != Some(tip.as_str()) {
        return Err(format!("{archive} reads back as {}, expected {tip}", back.as_deref().unwrap_or("nothing")));
    }
    Ok(tip)
}

/// One invocation's tally.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub sent: u32,
    pub failed: u32,
}

pub struct Sweep<'a> {
    pub w: &'a dyn World,
    pub opts: Opts,
    pub submitted_label: String,
    pub tally: Tally,
    pub deadline: Option<Instant>,
    pub truncated: bool,
}

fn q(n: Option<u64>) -> String {
    n.map(|n| n.to_string()).unwrap_or_else(|| "?".into())
}

impl<'a> Sweep<'a> {
    /// The whole invocation: every repository in scope, then the tally line. Returns the
    /// exit status (0 when nothing failed).
    pub fn run(&mut self, repos: &[Repo]) -> i32 {
        self.deadline = self.opts.budget.map(|b| Instant::now() + b);
        for r in repos {
            if self.out_of_time() {
                break;
            }
            match self.opts.scope {
                Scope::SkipQueue if r.forge_queued => continue,
                Scope::QueueOnly if !r.forge_queued => continue,
                _ => {}
            }
            self.repo(r);
        }
        if !self.opts.dry {
            self.w.log(&format!("hel: {} sent, {} failed", self.tally.sent, self.tally.failed));
        }
        i32::from(self.tally.failed != 0)
    }

    fn out_of_time(&mut self) -> bool {
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            if !self.truncated {
                self.truncated = true;
                self.w.log("hel: time budget spent — remaining branches and worktrees wait for the next pass");
            }
            return true;
        }
        false
    }

    fn say(&self, s: &str) {
        self.w.emit(s);
    }

    /// sweep_repo: both passes over one repository.
    pub fn repo(&mut self, r: &Repo) {
        let Some(root) = r.root.as_deref() else {
            self.say(&format!("SKIP   {}  no path is configured for it", r.name));
            return;
        };
        if !root.join(".git").exists() {
            self.say(&format!("SKIP   {}  {} is not a git checkout", r.name, root.display()));
            return;
        }
        let g = Git(root);
        let brs = g.spira_branches();
        let Some(base) = self.w.base(root) else {
            self.say(&format!("SKIP   {}  cannot resolve the ref it lands on — configure its `base`", r.name));
            return;
        };
        // The base's OWN remote, and only when there is a branch to judge: the local
        // question first, the round trip only if it is worth asking.
        if let Some(rem) = base.remote.as_deref() {
            if self.opts.fetch && !brs.is_empty() {
                g.fetch(rem);
            }
        }
        let label = self.submitted_label.clone();
        let c = Ctx { w: self.w, repo: root, name: &r.name, base: &base, submitted_label: &label };

        if !brs.is_empty() {
            self.w.prefetch();
        }

        // PASS 1 — every spira/* branch, exactly one disposition.
        for br in &brs {
            if self.out_of_time() {
                break;
            }
            let id = br.strip_prefix("spira/").unwrap_or(br);
            if let Some(only) = self.opts.only.as_deref() {
                if only != id && only != br {
                    continue;
                }
            }
            if id.starts_with("round-") {
                self.say(&format!("SKIP   {id}  round branch, not a bead — swept by the Concierge's own toolchain"));
                continue;
            }
            if let Some(held) = self.w.witness(id) {
                self.say(&format!("HELD   {id}  {held}"));
                continue;
            }
            let d = disposition(&c, id, br);
            self.act(&c, id, br, d);
        }

        // PASS 2 — orphaned worktrees: registered under $SPIRA_RUN/worktree, branch gone.
        if self.opts.only.is_some() {
            return;
        }
        let wts = self.w.worktrees();
        for (w, br) in g.worktrees() {
            if !w.starts_with(&wts) || w == wts {
                continue;
            }
            if self.out_of_time() {
                break;
            }
            let Some(base_name) = w.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
            if base_name.starts_with('.') {
                continue;
            }
            let Some(br) = br else { continue };
            if g.branch_exists(&br) {
                continue;
            }
            let id = base_name;
            if let Some(held) = self.w.witness(&id) {
                self.say(&format!("HELD   {id}  orphaned worktree kept — {held}"));
                continue;
            }
            if self.opts.dry {
                self.say(&format!("WOULD  {id}  remove orphaned worktree {}", w.display()));
                continue;
            }
            if !self.w.destroy_worktree(&id, &w, root, &format!("orphan: branch {br} is gone")) {
                self.say(&format!("FAILED {id}  orphaned worktree {} was not removed — see {}", w.display(), self.w.reaplog()));
                self.tally.failed += 1;
                continue;
            }
            self.tally.sent += 1;
            self.say(&format!("SENT {id}  {} {br}  orphaned worktree (branch was already gone)", r.name));
        }
        if !self.opts.dry {
            self.w.prune(root);
        }
    }

    fn act(&mut self, c: &Ctx, id: &str, br: &str, d: Disp) {
        let g = Git(c.repo);
        let lr = c.base.landref.as_str();
        let dry = self.opts.dry;
        let wt_suffix = || if g.worktree_of(br).is_some() { " and its worktree" } else { "" };
        match d {
            Disp::KeepSupersededUnsafe => {
                let files = g.diff_names(lr, br);
                self.say(&format!(
                    "KEEP   {id}  superseded but {} unlanded commit(s) add content absent from {lr}: {}",
                    q(g.ahead(lr, br)),
                    if files.is_empty() { "unknown files" } else { &files }
                ));
                // The branch stays; its worktree does not — the worktree was never what made
                // the branch unsafe, and the witnesses already said nobody is home.
                if let Some(w) = g.worktree_of(br) {
                    if dry {
                        self.say(&format!("WOULD  {id}  free worktree {} (branch kept)", w.display()));
                    } else {
                        self.w.destroy_worktree(id, &w, c.repo, &format!("branch {id} kept as superseded; worktree freed"));
                    }
                }
            }
            Disp::KeepUnlanded => self.say(&format!("KEEP   {id}  unlanded — {} commit(s) not in {lr}", q(g.ahead(lr, br)))),
            Disp::OrphanNoBead | Disp::ArchiveTerminal => {
                if dry {
                    self.say(&format!("WOULD  {id}  archive orphan branch {br} ({} commit(s) not in {lr}) to refs/archive/{br}", q(g.ahead(lr, br))));
                    return;
                }
                self.orphan(c, id, br);
            }
            Disp::SendContentLanded => {
                // EVIDENCE BEFORE DELETING, and only when there was work to land: the branch
                // is about to vanish, and with it the only proof the bead was not closed on
                // nothing. Zero ahead is a fast-forward (whose own commit names the bead) or
                // empty work; only a branch with commits whose diff is nonetheless on the
                // base is the case this records: a ContentOnBase event.
                let ahead = g.ahead(lr, br).unwrap_or(0);
                if needs_evidence(ahead, || g.has_own_commit(lr, br, id), || g.only_stacked_base(lr, br), || g.base_names(lr, id, 2000)) {
                    self.say(&format!("KEEP   {id}  {ahead} commit(s) not in {lr}, none naming the bead — an empty diff is not evidence of landing"));
                    return;
                }
                if dry {
                    self.say(&format!("WOULD  {id}  send branch {br}{}", wt_suffix()));
                    return;
                }
                if ahead > 0 && !self.w.lc_terminal(id) {
                    let proof = format!("merge-tree:{}", g.rev_parse(lr).unwrap_or_default());
                    self.w.content_on_base(id, &proof);
                }
                self.send_landed(c, id, br, "SENT");
            }
            Disp::SendOtherPr => {
                if dry {
                    self.say(&format!("WOULD  {id}  send branch {br} (commit on {lr} names it)"));
                    return;
                }
                self.send_landed(c, id, br, "SENT");
            }
            Disp::ReapSupersededSafe => {
                if dry {
                    self.say(&format!("WOULD  {id}  reap superseded branch {br}{}", wt_suffix()));
                    return;
                }
                let _ = self.branch(c, id, br, "REAPED", "hel");
            }
            Disp::ReapSquashMerged => {
                if dry {
                    self.say(&format!("WOULD  {id}  reap squash-merged branch {br} (PR merged at this tip)"));
                    return;
                }
                self.send_landed(c, id, br, "REAPED");
            }
        }
    }

    /// send_branch: the recheck and the verified deletion. Ok(()) unless the deletion
    /// failed (a HELD or queued branch is not a failure, and is not counted sent).
    fn branch(&mut self, c: &Ctx, id: &str, br: &str, verb: &str, caller: &str) -> Result<(), ()> {
        match self.w.send(id, br, c.repo, &format!("landed in {}", c.base.landref), caller) {
            Sent::Held(why) => self.say(&format!("HELD   {id}  {why} (mid-send)")),
            Sent::Queued => self.say(&format!("SKIP   {id}  CERTIFIED/BATCHED — waiting for verdict")),
            Sent::Failed(err) => {
                let err = if err.is_empty() { format!("refused, see {}", self.w.reaplog()) } else { err };
                self.say(&format!("FAILED {id}  {err}"));
                self.tally.failed += 1;
                return Err(());
            }
            Sent::Done => {
                // The dwarf's session log is KEPT: CHECK 5 tells a dwarf-worked bead from a
                // hand-closed one by it.
                self.tally.sent += 1;
                self.say(&format!("{verb} {id}  {} {br}", c.name));
            }
        }
        Ok(())
    }

    /// send_landed: send, then close a submitted work bead at the land ref's sha — in pr and
    /// hold mode this sweep is the first place that sees the work land.
    fn send_landed(&mut self, c: &Ctx, id: &str, br: &str, verb: &str) {
        if self.branch(c, id, br, verb, "hel").is_err() {
            return;
        }
        let sha = Git(c.repo).rev_parse(&c.base.landref).unwrap_or_else(|| c.base.landref.clone());
        self.w.close_on_land(id, &sha);
    }

    /// send_orphan: park the tip at refs/archive/<br>, read it back, only then reap.
    fn orphan(&mut self, c: &Ctx, id: &str, br: &str) {
        let g = Git(c.repo);
        let Some(tip) = g.rev_parse(br) else {
            self.say(&format!("FAILED {id}  cannot resolve tip of {br}"));
            self.tally.failed += 1;
            return;
        };
        let archive = format!("refs/archive/{br}");
        if !g.update_ref(&archive, &tip) {
            self.say(&format!("FAILED {id}  update-ref {archive} failed"));
            self.tally.failed += 1;
            return;
        }
        let back = g.verify(&archive);
        if back.as_deref() != Some(tip.as_str()) {
            self.say(&format!("FAILED {id}  {archive} reads back as {}, expected {tip}", back.as_deref().unwrap_or("nothing")));
            self.tally.failed += 1;
            return;
        }
        self.w.log(&format!("hel: archived {} {br} at {tip} -> {archive}", c.name));
        let _ = self.branch(c, id, br, "ARCHIVED", "archived");
    }
}
