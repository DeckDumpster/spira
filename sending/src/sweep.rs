//! The Sending (DESIGN.md §2): every `spira/*` branch gets exactly one disposition, and
//! only a branch whose every change the base already holds is ever deleted.

use std::path::Path;

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
    KeepCherryUnapplied,
    OrphanNoBead,
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
            Disp::KeepCherryUnapplied => "KEEP cherry-unapplied",
            Disp::OrphanNoBead => "ORPHAN no-bead",
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

fn status(b: &Value) -> Option<&str> {
    b.get("status").and_then(Value::as_str)
}

pub struct Ctx<'a> {
    pub w: &'a dyn World,
    pub repo: &'a Path,
    pub name: &'a str,
    pub base: &'a Base,
    pub submitted_label: &'a str,
}

/// send_disposition. The bead is read once; the forge is asked only after every local
/// check has said no.
pub fn disposition(c: &Ctx, id: &str, br: &str) -> Disp {
    let g = Git(c.repo);
    let lr = c.base.landref.as_str();
    if g.content_landed(br, lr) {
        return Disp::SendContentLanded;
    }
    let bead = c.w.bead(id);
    let b = bead.as_ref();

    // A SUPERSEDED BEAD'S BRANCH: its work landed under the successor's id. The edge is a
    // claim, not a proof — reap only when merging it would conflict (nothing of its own is
    // missing from the base), or when it carries nothing at all.
    if b.is_some_and(superseded) {
        let n = g.ahead(lr, br);
        if n != Some(0) && g.merges_clean(lr, br) {
            return Disp::KeepSupersededUnsafe;
        }
        return Disp::ReapSupersededSafe;
    }

    // SQUASH-MERGED: a merged PR whose head is still the branch tip captured every commit.
    // The network call, reached only here.
    if b.is_some_and(|b| status(b) == Some("closed") || labels(b).iter().any(|l| l == c.submitted_label)) {
        let tip = g.rev_parse(br);
        if let Some(pr) = c.w.pr_merged_tip(c.repo, br) {
            if !pr.is_empty() && tip.as_deref() == Some(pr.as_str()) {
                return Disp::ReapSquashMerged;
            }
        }
    }

    // LANDED BY OTHER PR: a landing record names the bead — but that record may be about a
    // PRIOR push of this branch, so every commit unique to it must already be on the base
    // (git cherry), or this would delete work under a landed() that is true about the past.
    if b.is_some() && g.landed(id, &c.base.landrefs) {
        if g.cherry_unapplied(lr, br) {
            return Disp::KeepCherryUnapplied;
        }
        return Disp::SendOtherPr;
    }

    if b.is_some_and(|b| b.get("status").is_some()) {
        return Disp::KeepUnlanded;
    }
    // No bead, and not an ancestor (that one was sent at the first check): real commits
    // with no owner to land them — archived, never plain-deleted.
    Disp::OrphanNoBead
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
}

fn q(n: Option<u64>) -> String {
    n.map(|n| n.to_string()).unwrap_or_else(|| "?".into())
}

impl<'a> Sweep<'a> {
    /// The whole invocation: every repository in scope, then the tally line. Returns the
    /// exit status (0 when nothing failed).
    pub fn run(&mut self, repos: &[Repo]) -> i32 {
        for r in repos {
            match self.opts.scope {
                Scope::SkipQueue if r.queued => continue,
                Scope::QueueOnly if !r.queued => continue,
                _ => {}
            }
            self.repo(r);
        }
        if !self.opts.dry {
            self.w.log(&format!("sending: {} sent, {} failed", self.tally.sent, self.tally.failed));
        }
        i32::from(self.tally.failed != 0)
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
            Disp::KeepCherryUnapplied => self.say(&format!(
                "KEEP   {id}  {} commit(s) not in {lr}; landed() names it but git cherry finds unapplied commits — not safe to reap",
                q(g.ahead(lr, br))
            )),
            Disp::OrphanNoBead => {
                if dry {
                    self.say(&format!("WOULD  {id}  archive orphan branch {br} ({} commit(s) not in {lr}) to refs/archive/{br}", q(g.ahead(lr, br))));
                    return;
                }
                self.orphan(c, id, br);
            }
            Disp::SendContentLanded => {
                if dry {
                    self.say(&format!("WOULD  {id}  send branch {br}{}", wt_suffix()));
                    return;
                }
                // EVIDENCE BEFORE DELETING, and only when there was work to land: the branch
                // is about to vanish, and with it the only proof the bead was not closed on
                // nothing. Zero ahead is a fast-forward (whose own commit names the bead) or
                // empty work; only a branch with commits whose diff is nonetheless on the
                // base is the case this records. ON: a ContentOnBase event. OFF: the
                // `content-landed` label CHECK 5's exemption still reads (the gap sp-arpjt
                // closes — dc3e364bf dropped it for OFF).
                if g.ahead(lr, br).unwrap_or(0) > 0 {
                    if self.w.enforce() {
                        let proof = format!("merge-tree:{}", g.rev_parse(lr).unwrap_or_default());
                        self.w.content_on_base(id, &proof);
                    } else {
                        self.w.label_add(id, "content-landed");
                    }
                }
                self.landed(c, id, br, "SENT");
            }
            Disp::SendOtherPr => {
                if dry {
                    self.say(&format!("WOULD  {id}  send branch {br} (commit on {lr} names it)"));
                    return;
                }
                self.landed(c, id, br, "SENT");
            }
            Disp::ReapSupersededSafe => {
                if dry {
                    self.say(&format!("WOULD  {id}  reap superseded branch {br}{}", wt_suffix()));
                    return;
                }
                let _ = self.branch(c, id, br, "REAPED", "sending");
            }
            Disp::ReapSquashMerged => {
                if dry {
                    self.say(&format!("WOULD  {id}  reap squash-merged branch {br} (PR merged at this tip)"));
                    return;
                }
                self.landed(c, id, br, "REAPED");
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
                // The aeon's session log is KEPT: CHECK 5 tells an aeon-worked bead from a
                // hand-closed one by it.
                self.tally.sent += 1;
                self.say(&format!("{verb} {id}  {} {br}", c.name));
            }
        }
        Ok(())
    }

    /// send_landed: send, then close a submitted work bead at the land ref's sha — in pr and
    /// hold mode this sweep is the first place that sees the work land.
    fn landed(&mut self, c: &Ctx, id: &str, br: &str, verb: &str) {
        if self.branch(c, id, br, verb, "sending").is_err() {
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
        self.w.log(&format!("sending: archived {} {br} at {tip} -> {archive}", c.name));
        let _ = self.branch(c, id, br, "ARCHIVED", "archived");
    }
}
