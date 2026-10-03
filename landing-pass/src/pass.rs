//! The gated pass (`landing-pass land`, DESIGN.md §4): push, hold, queue and queue.local.
//! pr-mode branches are counted and left to the pr pass.

use crate::gateq::{GateQueue, Job as GateJob};
use crate::budget::{gate_fits, gate_lock_wait};
use crate::model::{BeadRow, GateOutcome, GateRun, LandMode, RepoRow, RunRecord, Settings};
use crate::order::{basefail_fix_decision, certify_order, is_base_fix, prior_pass_suites, OrderRow};
use crate::ports::{Beads, Clock, Git, Lib, Procs, Tools};
use crate::records::Files;
use crate::report::Reporter;
use crate::util::{first_line, tail_bytes, tail_lines};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub struct Pass<'a> {
    pub s: &'a Settings,
    pub repos: &'a [RepoRow],
    pub beads: &'a dyn Beads,
    pub git: &'a dyn Git,
    pub lib: &'a dyn Lib,
    pub tools: &'a dyn Tools,
    pub procs: &'a dyn Procs,
    pub clock: &'a dyn Clock,
    /// Consulted only with the lifecycle switch ON (DESIGN.md §9).
    pub lc: &'a dyn crate::lifecycle::Lc,
    pub lc_state: std::cell::OnceCell<Result<(), String>>,
    pub out: &'a Reporter,
    pub files: Files,
    pub start: u64,
    pub pid: u32,
    /// The sweep's own meter (law-take-the-simple-fix-with-a-meter).
    pub swept: Cell<u64>,
    pub swept_conflict: Cell<u64>,
}

/// Why the walk stopped at a branch.
pub(crate) enum Flow {
    Next,
    BudgetCut,
}

/// What §4.1's steps 1–6 made of a branch.
pub(crate) enum Screen<'w> {
    /// Handled (skipped, reopened or recorded) — nothing more this pass.
    Done(Flow),
    /// Skipped as not closed, or as held by a live aeon: no decision was made, so the
    /// concurrent walk may look at it again when a refresh finds it ready (D14 (h)).
    Retry,
    Queue(&'w BeadRow),
    Push(&'w BeadRow),
}

/// A gate the concurrent walk has started and not yet decided.
struct Job {
    ticket: u64,
    br: String,
    bead: BeadRow,
    tip: String,
    fix: bool,
}

/// Per-repository walk state.
pub(crate) struct Walk<'r> {
    pub repo: &'r RepoRow,
    pub base: String,
    pub base_fq: String,
    pub land: PathBuf,
    pub beads: HashMap<String, BeadRow>,
    pub enum_tip: HashMap<String, String>,
    /// Closed, rebased and still unlanded this pass (push/hold), in the order judged.
    pub judged: RefCell<Vec<String>>,
    pub basefail_filed: Cell<bool>,
    pub landed_any: Cell<bool>,
}

impl<'r> Walk<'r> {
    pub fn judge(&self, br: &str) {
        let mut j = self.judged.borrow_mut();
        if !j.iter().any(|b| b == br) {
            j.push(br.to_string());
        }
    }
    pub fn unjudge(&self, br: &str) {
        self.judged.borrow_mut().retain(|b| b != br);
    }
}

impl<'a> Pass<'a> {
    fn log(&self, m: &str) {
        self.out.log(m);
    }

    fn set_run(&self, repo: &str, branch: &str, phase: &str) {
        self.files.write_run(&RunRecord {
            pid: self.pid.to_string(),
            started: self.start.to_string(),
            repo: repo.into(),
            branch: branch.into(),
            phase: phase.into(),
        });
    }

    /// With the switch ON, is spira-lc reachable? Probed once per pass, never with it OFF.
    pub(crate) fn lc_ready(&self) -> Result<(), String> {
        debug_assert!(self.s.lifecycle_enforce);
        self.lc_state.get_or_init(|| self.lc.probe()).clone()
    }

    pub(crate) fn land_status(&self, id: &str) -> String {
        let st = self.beads.land_status(id);
        if st != "closed" && self.s.lifecycle_enforce && self.lc.submitted().is_ok_and(|m| m.contains_key(id)) {
            return "closed".into();
        }
        st
    }

    fn status_closed(&self, id: &str) -> (bool, String) {
        let st = self.land_status(id);
        (st == "closed", st)
    }

    /// `bd show` rows, with the lifecycle machine's SUBMITTED-at-the-branch-tip rows read as
    /// closed: under `lifecycle_enforce` the label is a projection, never the source.
    pub(crate) fn show_rows(&self, repo: &std::path::Path, ids: &[String]) -> Result<Vec<BeadRow>, String> {
        let mut rows = self.beads.show(ids)?;
        if !self.s.lifecycle_enforce {
            return Ok(rows);
        }
        let submitted = match self.lc.submitted() {
            Ok(m) => m,
            Err(why) => {
                self.log(&crate::lifecycle::unreachable_line(&why, "no bead reads as submitted this pass"));
                return Ok(rows);
            }
        };
        for r in rows.iter_mut().filter(|r| r.status != "closed") {
            let Some(tip) = submitted.get(&r.id) else { continue };
            if self.git.rev_parse(repo, &format!("spira/{}", r.id)).as_deref() == Some(tip.as_str()) {
                r.status = "closed".into();
            }
        }
        Ok(rows)
    }

    /// Record a gate outcome as a lifecycle event (enforce only); a refusal is loud, and the
    /// caller of a "pass" must not mark CERTIFIED when this returns false.
    fn lc_certify(&self, id: &str, tip: &str, outcome: &str, detail: &str) -> bool {
        if !self.s.lifecycle_enforce {
            return true;
        }
        match self.lc.certify(id, tip, outcome, detail) {
            Ok(a) => {
                self.log(&format!("CHECK6 {id}: lifecycle certify {outcome} at {tip}: {a}"));
                true
            }
            Err(why) => {
                self.log(&format!("CHECK6 {id}: LIFECYCLE: certify {outcome} at {tip} did not happen ({why})"));
                false
            }
        }
    }

    /// The whole pass after the lock and the run record (DESIGN.md §4 steps 3–10). Returns
    /// nothing: every outcome is in the log, the mailbox and the records.
    pub fn run(&self) {
        let names: Vec<&str> = self.repos.iter().map(|r| r.name.as_str()).collect();
        let listing: String = names.iter().fold(String::new(), |mut acc, n| {
            acc.push_str(n);
            acc.push(' ');
            acc
        });
        self.log(&format!("landing: starting a pass over [{listing}]"));
        Files::prune_verdicts(&self.s.verdicts, self.s.verdict_ttl, self.clock.now());

        self.queue_step("queue early: ");

        for repo in self.rotated() {
            self.walk_repo(repo);
        }

        crate::prune::prune_landstate(self);

        self.queue_step("queue late: ");

        for r in self.repos {
            if !matches!(r.mode, LandMode::Push | LandMode::Queue) || r.path.as_os_str().is_empty() {
                continue;
            }
            if !r.path.join(".git").exists() {
                continue;
            }
            let o = self.tools.skew_refresh(&r.path);
            if !o.is_empty() {
                self.log(&o);
            }
        }

        self.lib.gh_unlanded_scan();
        let sweep = if self.swept.get() + self.swept_conflict.get() > 0 {
            format!(
                ", {} survivor(s) rebased after a landing, {} conflicted",
                self.swept.get(),
                self.swept_conflict.get()
            )
        } else {
            String::new()
        };
        // unit-ensure (sp-31dm0: a bare-name binary now, replacing systemd/unit-ensure.sh),
        // then the reaper of finished work's build output (sp-z61hj): the `target/` of every
        // worktree whose bead is closed. It took the slot of land-build-ensure.sh, whose
        // post-landing `cargo build --release --workspace` was redundant once a landing
        // publishes a release from its tested binaries.
        for script in [PathBuf::from("unit-ensure"), PathBuf::from("target-reap")] {
            for l in self.tools.ensure(&script) {
                self.log(&l);
            }
        }
        self.log(&format!(
            "landing: pass complete — {} branch(es) seen, {} movement(s){sweep}",
            self.out.branches(),
            self.out.moved()
        ));
    }

    /// `queue step` for every queued repository, its lines relabelled into this log.
    fn queue_step(&self, prefix: &str) {
        for r in self.repos.iter().filter(|r| r.mode.queued()) {
            match self.tools.queue_step(&r.name) {
                Ok(lines) => {
                    for l in lines {
                        self.log(&format!("{prefix}{l}"));
                    }
                }
                Err(e) => self.log(&format!("{prefix}{}: {e}", r.name)),
            }
        }
    }

    /// Start from the repository where the last pass cut its budget, so none is always last.
    pub fn rotated(&self) -> Vec<&'a RepoRow> {
        let all: Vec<&RepoRow> = self.repos.iter().collect();
        let Some(c) = self.files.cursor_repo() else { return all };
        match all.iter().position(|r| r.name == c) {
            Some(i) => all[i..].iter().chain(all[..i].iter()).copied().collect(),
            None => all,
        }
    }

    pub fn walk_repo(&self, repo: &RepoRow) {
        let name = &repo.name;
        if repo.path.as_os_str().is_empty() {
            self.log(&format!("CHECK6 {name}: no repository map entry — skipped"));
            return;
        }
        if !repo.path.join(".git").exists() {
            self.log(&format!("CHECK6 {name}: {} is not a git checkout — skipped", repo.path.display()));
            return;
        }
        let refs = self.git.spira_refs(&repo.path);
        if refs.is_empty() {
            return;
        }
        self.out.add_branches(refs.len() as u64);
        if repo.mode == LandMode::Pr {
            return;
        }
        let (Some(base), Some(base_fq)) = (repo.landref.clone(), repo.base_fq.clone()) else {
            self.log(&format!(
                "CHECK6 {name}: cannot resolve the ref its branches land on — skipped. Give it a `base` in the repository map."
            ));
            return;
        };
        if let Some(rem) = &repo.base_remote {
            self.git.fetch(&repo.path, rem);
        }
        let basename = repo.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let land = self.s.run.join("worktree").join(format!(".landing.{basename}"));
        if repo.mode == LandMode::Push {
            if !self.git.tree_ok(&land) {
                self.lib.prune_worktrees(&repo.path);
                self.git.tree_add_detached(&repo.path, &land, &base_fq);
            }
            if self.git.tree_ok(&land) {
                self.git.tree_checkout_landing(&land, &base_fq);
            }
        }

        let ids: Vec<String> = refs.iter().map(|(b, _)| b.trim_start_matches("spira/").to_string()).collect();
        let beads: HashMap<String, BeadRow> = match self.show_rows(&repo.path, &ids) {
            Ok(rows) => rows.into_iter().map(|b| (b.id.clone(), b)).collect(),
            Err(e) => {
                self.log(&format!("CHECK6 {name}: the bead store could not be read ({e}) — every branch reads as not closed this pass"));
                HashMap::new()
            }
        };
        let rows: Vec<OrderRow> =
            refs.iter().map(|(b, _)| OrderRow::of(b, beads.get(b.trim_start_matches("spira/")), &self.s.express_label)).collect();
        let order = certify_order(name, &rows);
        let by_branch: HashMap<&str, &OrderRow> = rows.iter().map(|r| (r.branch.as_str(), r)).collect();
        let fix_front: Vec<&str> =
            order.iter().take_while(|b| is_base_fix(by_branch[b.as_str()].external_ref.as_deref(), name)).map(|s| s.as_str()).collect();
        if !fix_front.is_empty() {
            self.log(&format!("CHECK6 {name}: base-fix branch(es) at front of queue: {}", fix_front.join(" ")));
        }
        let express: Vec<&str> = order
            .iter()
            .skip(fix_front.len())
            .take_while(|b| by_branch[b.as_str()].express)
            .map(|s| s.as_str())
            .collect();
        if !express.is_empty() {
            let list: String = express.iter().fold(String::new(), |mut acc, b| {
                acc.push(' ');
                acc.push_str(b);
                acc
            });
            self.log(&format!("CHECK6 {name}: express branch(es) certified first:{list}"));
        }

        let mut w = Walk {
            repo,
            base,
            base_fq,
            land,
            beads,
            enum_tip: refs.into_iter().collect(),
            judged: RefCell::new(Vec::new()),
            basefail_filed: Cell::new(false),
            landed_any: Cell::new(false),
        };

        // CONCURRENT CERTIFICATION (DESIGN.md §8 D14): queued repositories only, and only
        // when SPIRA_CERTIFY_PAR > 1. At 1 the serial walk below runs unchanged.
        if repo.mode.queued() && self.s.certify_par > 1 && !self.s.gate_worker {
            self.walk_concurrent(&mut w, order);
            return;
        }

        let mut cut_at: Option<usize> = None;
        for (i, br) in order.iter().enumerate() {
            self.set_run(name, br, "");
            if let Flow::BudgetCut = self.branch(&w, br) {
                cut_at = Some(i);
                break;
            }
        }

        match cut_at {
            Some(k) => {
                let left = self.s.land_maxsec - (self.clock.now() as i64 - self.start as i64);
                self.log(&format!(
                    "landing: budget cut at {} — {left}s left, {} branch(es) deferred in {name}",
                    order[k],
                    order.len() - k
                ));
                self.files.set_cursor(name);
                for (i, br) in order.iter().enumerate() {
                    if i >= k {
                        let n = self.files.bump_deferred(br, name, self.clock.now());
                        if n >= self.s.deferral_escalate_at {
                            self.lib.ask_budget_deferred(br, name, n);
                        }
                    } else {
                        self.files.clear_deferred(br);
                    }
                }
            }
            None => {
                for br in &order {
                    self.files.clear_deferred(br);
                }
            }
        }

        // THE SURVIVORS ARE REBASED ONCE PER PASS (sp-4hs0i): after the whole walk, and only
        // if the base moved under them here. Per-landing sweeps replayed every survivor after
        // every landing — k landings × n survivors.
        if w.landed_any.get() && !w.judged.borrow().is_empty() {
            let survivors = w.judged.borrow().clone();
            self.rebase_survivors(&w, &survivors);
        }
    }

    /// One branch (DESIGN.md §4.1 steps 1–7).
    fn branch(&self, w: &Walk, br: &str) -> Flow {
        let id = br.trim_start_matches("spira/");
        match self.screen(w, br) {
            Screen::Done(f) => f,
            Screen::Retry => Flow::Next,
            Screen::Queue(bead) => self.certify(w, br, id, bead),
            Screen::Push(bead) => crate::push::push_or_hold(self, w, br, id, bead),
        }
    }

    /// §4.1 steps 1–6: everything before the branch's own mode takes over.
    fn screen<'w>(&self, w: &'w Walk, br: &str) -> Screen<'w> {
        let repo = w.repo;
        let name = &repo.name;
        let id = br.trim_start_matches("spira/");

        if !self.git.branch_exists(&repo.path, br) {
            match w.enum_tip.get(br) {
                Some(was) if self.git.is_ancestor(&repo.path, was, &w.base_fq) => self.log(&format!(
                    "CHECK6 {id}: {br} is gone since this pass began and {was} is on {} — landed and reaped, not reopening",
                    w.base
                )),
                Some(was) => self.log(&format!(
                    "CHECK6 {id}: {br} is gone since this pass began and {was} is NOT on {} — reaped or slain, not reopening",
                    w.base
                )),
                None => self.log(&format!("CHECK6 {id}: {br} is gone since this pass began — landed or reaped elsewhere, not reopening")),
            }
            return Screen::Done(Flow::Next);
        }
        let bead = w.beads.get(id);
        let st = bead.map(|b| b.status.as_str()).unwrap_or("");
        if st != "closed" {
            let shown = if st.is_empty() { "-" } else { st };
            if self.procs.holder_alive(id) {
                self.log(&format!("CHECK6 {id}: {br} not landed — its bead is {shown}, held by a live aeon"));
            } else {
                self.log(&format!("CHECK6 {id}: {br} not landed — its bead is {shown} and no aeon holds it"));
            }
            return Screen::Retry;
        }
        let bead = bead.expect("closed implies present");

        // A branch lands only in the repository its BEAD names.
        let bead_path = self.repos.iter().find(|r| r.name == bead.repo).map(|r| r.path.clone());
        if bead_path.as_deref() != Some(repo.path.as_path()) {
            self.log(&format!("CHECK6 {id}: {br} is in {name} but the bead names repo:{} — not landing it here", bead.repo));
            return Screen::Done(Flow::Next);
        }
        if bead.superseded {
            self.log(&format!(
                "CHECK6 {id}: {br} is superseded — its work landed under the successor's id; leaving it for the Sending to reap"
            ));
            return Screen::Done(Flow::Next);
        }
        if bead.has_label(&self.s.cutover_label) {
            self.log(&format!("CHECK6 {id}: {br} is labelled {} — leaving it for the cutover round", self.s.cutover_label));
            return Screen::Done(Flow::Next);
        }
        if let Some(ls) = self.files.land_state(id) {
            if ls.state == "EJECTED" {
                self.log(&format!("CHECK6 {id}: closed but landstate is EJECTED — reopening so the aeon can fix the batch gate failure"));
                let tip = if ls.tip.is_empty() { "none".to_string() } else { ls.tip.clone() };
                self.lib.land_mark(id, "RED", &tip, "ejected-not-requeued");
                self.lib.reopen(id, "batch-eject", "Reopened by sentinel: batch gate failure recorded but bead closed before aeon could fix it.");
                self.out.progress(&format!("reopened {id} — ejected-not-requeued"));
                return Screen::Done(Flow::Next);
            }
            if ls.state == "LANDED" {
                self.log(&format!(
                    "CHECK6 {id}: closed with landstate LANDED but {br} is not on {} (landed by another route, e.g. cherry-pick) — leaving the record for the Sending to reap, not gating",
                    w.base
                ));
                return Screen::Done(Flow::Next);
            }
        }
        if self.git.content_landed(&repo.path, br, &w.base_fq) {
            self.log(&format!("{} already contains every change on {br} — nothing to land", w.base));
            let tip = self.git.rev_parse(&repo.path, br).unwrap_or_else(|| "none".into());
            self.lib.land_mark(id, "CONTENT", &tip, "");
            self.files.drop_ejected(id);
            return Screen::Done(Flow::Next);
        }
        if self.procs.holder_alive(id) {
            self.log(&format!("CHECK6 {id}: a live aeon still holds {br} — deferring the land"));
            return Screen::Retry;
        }
        if repo.mode.queued() {
            return Screen::Queue(bead);
        }
        Screen::Push(bead)
    }

    /// Queue-mode certification (DESIGN.md §4.2).
    fn certify(&self, w: &Walk, br: &str, id: &str, bead: &BeadRow) -> Flow {
        let tip = match self.certify_screen(w, br, id, bead) {
            Ok(tip) => tip,
            Err(f) => return f,
        };
        if self.s.gate_worker {
            return self.certify_queued(w, br, id, bead, &tip);
        }
        let g = self.run_gate(&w.repo.name, br, id, true, &tip);
        self.certify_judge(w, br, id, bead, &tip, g)
    }

    /// Hand the gate to `gate-worker`: apply the verdict it filed for this tip, or queue the
    /// branch and move on. The pass never waits on a gate, so no budget can cut it short.
    fn certify_queued(&self, w: &Walk, br: &str, id: &str, bead: &BeadRow, tip: &str) -> Flow {
        let name = &w.repo.name;
        let q = GateQueue::new(&self.s.run);
        if let Some(d) = q.take_done(name, br, tip) {
            self.log(&format!("CHECK6 {id}: gate-worker's verdict for {br} at {tip} is in — applying it"));
            return self.certify_judge(w, br, id, bead, tip, d.run);
        }
        let fix = is_base_fix(bead.external_ref.as_deref(), name);
        match q.enqueue(&GateJob::new(name, br, id, tip, fix)) {
            Ok(true) => {
                self.lib.land_mark(id, "GATING", tip, "");
                self.log(&format!("CHECK6 {id}: {br} queued for gate-worker at {tip}"));
            }
            Ok(false) => self.log(&format!("CHECK6 {id}: {br} is already with gate-worker at {tip}")),
            Err(e) => self.log(&format!("CHECK6 {id}: could not queue {br} for gate-worker ({e}) — it stays unjudged this pass")),
        }
        Flow::Next
    }

    /// §4.2 before the gate: one judgement per tip, then the budget. Ok(the tip to gate).
    fn certify_screen(&self, w: &Walk, br: &str, id: &str, bead: &BeadRow) -> Result<String, Flow> {
        let repo = w.repo;
        let tip = self.git.rev_parse(&repo.path, br).unwrap_or_default();
        if let Some(ls) = self.files.land_state(id) {
            if ls.state == "WITHDRAWN" && ls.tip == tip {
                self.log(&format!("CHECK6 {id}: withdrawn at {tip} — staying WITHDRAWN until the tip changes"));
                return Err(Flow::Next);
            }
            if ls.state == "CERTIFIED" && ls.tip == tip {
                return Err(Flow::Next);
            }
        }
        if !self.s.gate_worker && !self.budget_allows(bead, &repo.name, id) {
            return Err(Flow::BudgetCut);
        }
        Ok(tip)
    }

    /// §4.2 after the gate: the one decision this pass makes for this branch.
    fn certify_judge(&self, w: &Walk, br: &str, id: &str, bead: &BeadRow, tip: &str, g: GateRun) -> Flow {
        let repo = w.repo;
        let name = &repo.name;
        let tip = tip.to_string();
        if g.outcome != GateOutcome::Pass {
            let reason = g.reason_or("unspecified");
            self.log(&format!("CHECK6 {id}: certification gate {} on {br} in {name} ({reason})", g.outcome.word()));
            self.lib.land_mark(id, "GATED", &tip, &format!("{}:{reason}", g.outcome.word()));
            match g.outcome {
                GateOutcome::Fail => drop(self.lc_certify(id, &tip, "red", &reason)),
                GateOutcome::NoVerdict => drop(self.lc_certify(id, &tip, "infra", "")),
                _ => {}
            }
            match g.outcome {
                GateOutcome::BaseFail => {
                    self.log(&format!("CHECK6 {id}: held — the base fails its own gate (suite {})", g.suite));
                    self.base_fail(w, br, id, bead, &tip, &g);
                }
                GateOutcome::NoVerdict if reason == "conflict" => {
                    // STALE, NOT RED (gate/DESIGN.md): the branch no longer merges onto the
                    // landing ref. rebase-stale rebases it mechanically and re-certifies, or
                    // returns it to an aeon with the hunks quoted; only when it could not even
                    // attempt it does this fall back to the ordinary no-verdict record.
                    let rc = self.tools.rebase_stale(id, name);
                    self.log(&format!("CHECK6 {id}: {br} does not merge onto {} — handed to rebase-stale (exit {rc})", w.base));
                    if rc == 3 {
                        self.lib.noverdict(id, br, name, &reason, g.outcome.word(), &g.out);
                    }
                }
                GateOutcome::NoVerdict => {
                    self.lib.noverdict(id, br, name, &reason, g.outcome.word(), &g.out);
                }
                _ => {
                    let (closed, st) = self.status_closed(id);
                    if !closed {
                        self.log(&format!("CHECK6 {id}: bead is now {st} (was closed at scan time) — not reopening {br}"));
                        return Flow::Next;
                    }
                    let n = self.count_or_q(&repo.path, &format!("{}..{br}", w.base));
                    let scope = self.scope_note(name, br, &g.suite);
                    let scope_part = if scope.is_empty() { String::new() } else { format!("\n{scope}") };
                    let note = format!(
                        "Reopened by sentinel: branch {br} failed {name}'s certification gate. The branch carries {n} commit(s) from the previous session — the next aeon should resume from the existing work, not restart.\n{scope_part}\n\n{}",
                        tail_lines(&g.out, 20)
                    );
                    self.lib.reopen(id, "cert-gate-red", &note);
                    self.out.progress(&format!("reopened {id} — failed the certification gate"));
                    self.lib.event(
                        "bead.reopened",
                        id,
                        &format!("reopened {id} — {br} failed {name}'s certification gate"),
                        &tail_lines(&g.out, 3),
                    );
                    self.lib.land_mark(id, "RED", &tip, "gate");
                }
            }
            return Flow::Next;
        }
        if g.reason.as_deref() == Some("cached") {
            self.log(&format!("CHECK6 {id}: certification PASS on {br} in {name} — this tree had already passed"));
        }
        self.files.clear_noverdict(br);
        let (closed, st) = self.status_closed(id);
        if !closed {
            self.log(&format!("CHECK6 {id}: bead is now {st} (was closed at scan time) — not certifying {br}"));
            return Flow::Next;
        }
        if !self.lc_certify(id, &tip, "pass", "gate") {
            self.log(&format!("CHECK6 {id}: lifecycle refused GatePass at {tip} — {br} stays uncertified"));
            return Flow::Next;
        }
        self.lib.land_mark(id, "CERTIFIED", &tip, "");
        self.files.mark_submitted(id, &tip, "certified", self.clock.now());
        self.out.progress(&format!("certified {br} in {name} — gate passed, round and CI are the remaining judges"));
        Flow::Next
    }

    fn is_fix(&self, w: &Walk, br: &str) -> bool {
        let id = br.trim_start_matches("spira/");
        is_base_fix(w.beads.get(id).and_then(|b| b.external_ref.as_deref()), &w.repo.name)
    }

    fn set_run_inflight(&self, name: &str, inflight: &[Job]) {
        let brs: Vec<&str> = inflight.iter().map(|j| j.br.as_str()).collect();
        self.set_run(name, &brs.join(","), if brs.is_empty() { "" } else { "gate" });
    }

    /// The queued walk with up to `certify_par` gates in flight (DESIGN.md §8 D14). Screening,
    /// starting and deciding all happen on this thread; only the gates run beside it.
    fn walk_concurrent(&self, w: &mut Walk, order: Vec<String>) {
        let name = w.repo.name.clone();
        let par = self.s.certify_par.max(1);
        let mut pending: Vec<String> = order;
        let mut inflight: Vec<Job> = Vec::new();
        let mut screened: Vec<String> = Vec::new();
        let mut retry: Vec<String> = Vec::new();
        // Started or decided this pass: never put back into `pending`.
        let mut taken: HashSet<String> = HashSet::new();
        let mut cut: Option<(String, i64)> = None;
        loop {
            while cut.is_none() && inflight.len() < par && !pending.is_empty() {
                // (e) a base fix runs alone: nothing starts beside it, and it starts only
                // into an idle walk.
                if inflight.iter().any(|j| j.fix) {
                    break;
                }
                if !inflight.is_empty() && self.is_fix(w, &pending[0]) {
                    break;
                }
                // (f) never start a gate into a full admission pool, unless it is the only one.
                if !inflight.is_empty() && self.tools.gate_slots_free(par) == Some(0) {
                    break;
                }
                let br = pending.remove(0);
                let id = br.trim_start_matches("spira/").to_string();
                screened.push(br.clone());
                taken.insert(br.clone());
                self.set_run(&name, &br, "");
                let bead = match self.screen(w, &br) {
                    Screen::Done(_) => continue,
                    Screen::Retry => {
                        taken.remove(&br);
                        retry.push(br);
                        continue;
                    }
                    // A queued repository never screens to push; decided as the serial walk would.
                    Screen::Push(b) => {
                        let _ = crate::push::push_or_hold(self, w, &br, &id, b);
                        continue;
                    }
                    Screen::Queue(b) => b.clone(),
                };
                let tip = match self.certify_screen(w, &br, &id, &bead) {
                    Ok(t) => t,
                    Err(Flow::Next) => continue,
                    Err(Flow::BudgetCut) => {
                        let left = self.s.land_maxsec - (self.clock.now() as i64 - self.start as i64);
                        screened.pop();
                        taken.remove(&br);
                        cut = Some((br, left));
                        break;
                    }
                };
                let (wait, note) = gate_lock_wait(self.s.land_maxsec, self.start, self.s.gate_lock_wait.as_deref(), self.clock.now());
                if let Some(n) = note {
                    self.log(&n);
                }
                self.lib.land_mark(&id, "GATING", &tip, "");
                let ticket = self.tools.gate_start(&br, &name, &wait, &id);
                let fix = is_base_fix(bead.external_ref.as_deref(), &name);
                inflight.push(Job { ticket, br, bead, tip, fix });
                self.set_run_inflight(&name, &inflight);
            }
            if inflight.is_empty() {
                break;
            }
            self.set_run_inflight(&name, &inflight);
            let Some((ticket, rc, out)) = self.tools.gate_wait_any() else {
                self.log(&format!("CHECK6 {name}: lost track of {} running gate(s) — ending the walk", inflight.len()));
                break;
            };
            let Some(pos) = inflight.iter().position(|j| j.ticket == ticket) else { continue };
            let job = inflight.remove(pos);
            self.set_run_inflight(&name, &inflight);
            let id = job.br.trim_start_matches("spira/");
            let _ = self.certify_judge(w, &job.br, id, &job.bead, &job.tip, GateRun::parse(rc, out));
            if cut.is_none() {
                self.refresh(w, &mut pending, &mut retry, &taken);
            }
        }

        match cut {
            Some((at, left)) => {
                let deferred: Vec<String> = std::iter::once(at.clone()).chain(pending).collect();
                self.log(&format!(
                    "landing: budget cut at {at} — {left}s left, {} branch(es) deferred in {name}",
                    deferred.len()
                ));
                self.files.set_cursor(&name);
                for br in &screened {
                    self.files.clear_deferred(br);
                }
                for br in &deferred {
                    let n = self.files.bump_deferred(br, &name, self.clock.now());
                    if n >= self.s.deferral_escalate_at {
                        self.lib.ask_budget_deferred(br, &name, n);
                    }
                }
            }
            None => {
                for br in &screened {
                    self.files.clear_deferred(br);
                }
            }
        }
    }

    /// D14 (h): between completions, one cheap re-read — new branches, and the beads skipped
    /// as not closed or held — so a branch that became ready mid-pass takes the next free slot.
    fn refresh(&self, w: &mut Walk, pending: &mut Vec<String>, retry: &mut Vec<String>, taken: &HashSet<String>) {
        let repo = w.repo;
        let fresh: Vec<(String, String)> =
            self.git.spira_refs(&repo.path).into_iter().filter(|(b, _)| !w.enum_tip.contains_key(b)).collect();
        if fresh.is_empty() && retry.is_empty() {
            return;
        }
        let ids: Vec<String> =
            fresh.iter().map(|(b, _)| b).chain(retry.iter()).map(|b| b.trim_start_matches("spira/").to_string()).collect();
        let Ok(rows) = self.show_rows(&repo.path, &ids) else { return };
        for r in rows {
            w.beads.insert(r.id.clone(), r);
        }
        let ready = |b: &str| {
            let id = b.trim_start_matches("spira/");
            w.beads.get(id).map(|x| x.status == "closed").unwrap_or(false) && !self.procs.holder_alive(id)
        };
        self.out.add_branches(fresh.len() as u64);
        let mut added: Vec<String> = Vec::new();
        for (b, _) in &fresh {
            if taken.contains(b) {
                continue;
            }
            if ready(b) {
                added.push(b.clone());
            } else {
                retry.push(b.clone());
            }
        }
        retry.retain(|b| {
            if added.contains(b) || !ready(b) {
                return true;
            }
            added.push(b.clone());
            false
        });
        for (b, t) in fresh {
            w.enum_tip.insert(b, t);
        }
        if added.is_empty() {
            return;
        }
        pending.extend(added.iter().cloned());
        let rows: Vec<OrderRow> = pending
            .iter()
            .map(|b| OrderRow::of(b, w.beads.get(b.trim_start_matches("spira/")), &self.s.express_label))
            .collect();
        *pending = certify_order(&repo.name, &rows);
        self.log(&format!("CHECK6 {}: candidates refreshed — {} newly ready: {}", repo.name, added.len(), added.join(" ")));
    }

    /// The gate may start only if the pass can finish it; a base-fix branch is exempt.
    pub(crate) fn budget_allows(&self, bead: &BeadRow, name: &str, id: &str) -> bool {
        if gate_fits(self.s.land_maxsec, self.start, self.s.gate_reserve, self.clock.now()) {
            return true;
        }
        if is_base_fix(bead.external_ref.as_deref(), name) {
            self.log(&format!("CHECK6 {id}: base-fix branch — gating despite budget exhaustion"));
            return true;
        }
        false
    }

    pub(crate) fn run_gate(&self, name: &str, br: &str, id: &str, mark_gating: bool, tip: &str) -> GateRun {
        self.set_run(name, br, "gate");
        let (wait, note) = gate_lock_wait(self.s.land_maxsec, self.start, self.s.gate_lock_wait.as_deref(), self.clock.now());
        if let Some(n) = note {
            self.log(&n);
        }
        if mark_gating {
            self.lib.land_mark(id, "GATING", tip, "");
        }
        let (rc, out) = self.tools.gate(br, name, &wait, id);
        self.set_run(name, br, "");
        GateRun::parse(rc, out)
    }

    /// BASE_FAIL: a green base-fix is certified; anything else files the base's own red,
    /// once per repository per pass.
    pub(crate) fn base_fail(&self, w: &Walk<'_>, br: &str, id: &str, bead: &BeadRow, tip: &str, g: &GateRun) {
        let name = &w.repo.name;
        if basefail_fix_decision(bead.external_ref.as_deref(), name, &g.out) {
            let suite = bead.external_ref.as_deref().unwrap_or("").trim_start_matches(&format!("basefail:{name}:")).to_string();
            let (closed, _) = self.status_closed(id);
            if closed {
                self.log(&format!("CHECK6 {id}: base-fix: {br} is green on {name}'s red suite {suite} — certifying"));
                if !self.lc_certify(id, tip, "pass", "base-fix") {
                    self.log(&format!("CHECK6 {id}: lifecycle refused GatePass at {tip} — {br} stays uncertified"));
                    return;
                }
                self.lib.land_mark(id, "CERTIFIED", tip, "");
                self.files.mark_submitted(id, tip, "certified", self.clock.now());
                self.out.progress(&format!("certified {br} in {name} — base-fix (suite {suite})"));
            }
            return;
        }
        if !w.basefail_filed.get() {
            w.basefail_filed.set(true);
            let sha = self.git.rev_parse(&w.repo.path, &w.base_fq).unwrap_or_default();
            self.base_incident(name, &g.suite, &g.reason_or("base-red"), br, &w.base, &sha, &g.out);
        }
    }

    /// File the base's own red through incident.sh (deduped on repository + suite).
    fn base_incident(&self, name: &str, suite: &str, reason: &str, br: &str, base: &str, sha: &str, out: &str) {
        if !crate::util::runnable(&self.s.incident) {
            self.log(&format!("CHECK6 {name}: no intake at {} — the base's own red reaches nobody", self.s.incident.display()));
            return;
        }
        let named = if suite == "-" { "- (the gate named none; read its output below)".to_string() } else { suite.to_string() };
        let labels = if self.s.scope_label.is_empty() { "plan".to_string() } else { format!("{},plan", self.s.scope_label) };
        let title = format!("{name}'s own gate fails against {base} — nothing can land");
        let payload = [
            format!("{name}'s landing gate was run against {base} itself and failed there, so every branch of"),
            "this repository is refused for a condition no branch caused. No bead has been reopened and".into(),
            "no attempt charged: the branches are held, and they land on the pass after this is fixed.".into(),
            String::new(),
            format!("  repository       {name}"),
            format!("  base             {base}"),
            format!("  failing suite    {named}"),
            format!("  gate verdict     BASE_FAIL ({reason})"),
            format!("  first noticed by {br}, which is not at fault"),
            format!("  reproduce        gate.sh {base} {name}"),
            String::new(),
            "The dedupe key is the repository and the suite, so every other branch blocked by this same".into(),
            "red bumps a recurrence on this bead rather than filing another one.".into(),
            String::new(),
            "--- the gate's own output -------------------------------------------------------------".into(),
            tail_bytes(out, 6000),
        ]
        .join("\n");
        let key = if suite == "-" { format!("basefail:{name}:-@{}", if sha.is_empty() { "unknown" } else { sha }) } else { format!("basefail:{name}:{suite}") };
        match self.lib.incident(&labels, name, &key, &title, &payload) {
            Err(_) => self.log(&format!("CHECK6 {name}: the intake could not file the base's red — it stays spooled and drain will retry")),
            Ok(printed) => {
                let id: String = printed
                    .lines()
                    .map(str::trim)
                    .find(|l| !self.s.id_prefix.is_empty() && l.starts_with(&self.s.id_prefix) && !l.contains(char::is_whitespace))
                    .unwrap_or("")
                    .to_string();
                if !id.is_empty() {
                    self.log(&format!("CHECK6 {name}: the base's own red is {id} (suite {suite})"));
                } else {
                    self.log(&format!(
                        "CHECK6 {name}: the intake returned no bead id for the base's red — check {}",
                        self.s.incident.display()
                    ));
                }
            }
        }
    }

    /// The suite set the branch's own recorded PASS covered, beside the one that just failed.
    fn scope_note(&self, name: &str, br: &str, failing: &str) -> String {
        let Some(status) = self.tools.gate_status(br, name) else { return String::new() };
        let suites = prior_pass_suites(&status);
        if suites.is_empty() {
            return String::new();
        }
        format!("The branch held a recorded gate PASS covering: {suites}\nCertification just failed on: {failing}")
    }

    pub(crate) fn count_or_q(&self, repo: &Path, range: &str) -> String {
        self.git.count(repo, range).map(|n| n.to_string()).unwrap_or_else(|| "?".into())
    }

    /// Rebase every judged, still-unlanded branch onto the moved base — once.
    fn rebase_survivors(&self, w: &Walk, survivors: &[String]) {
        let repo = w.repo;
        let name = &repo.name;
        for br in survivors {
            let id = br.trim_start_matches("spira/");
            if self.git.is_ancestor(&repo.path, &w.base_fq, &format!("refs/heads/{br}")) {
                continue;
            }
            if !self.git.branch_exists(&repo.path, br) {
                self.log(&format!("CHECK6 {id}: {br} is gone since this pass judged it — not rebasing it onto {}", w.base));
                continue;
            }
            if self.procs.holder_alive(id) {
                self.log(&format!("CHECK6 {id}: an aeon took {br} while this pass ran — leaving its rebase to it"));
                continue;
            }
            if self.git.content_landed(&repo.path, br, &w.base_fq) {
                self.log(&format!("CHECK6 {id}: {} now contains every change on {br} — nothing left to rebase", w.base));
                continue;
            }
            let old_tip = self.git.rev_parse(&repo.path, br).unwrap_or_default();
            let certified = self.files.land_state(id).is_some_and(|ls| ls.state == "CERTIFIED" && ls.tip == old_tip);
            let rb = self.lib.rebase(br, &w.base_fq, &repo.path, name);
            if rb.ok {
                self.swept.set(self.swept.get() + 1);
                let tip = self.git.rev_parse(&repo.path, br).unwrap_or_default();
                if certified {
                    self.lib.land_mark(id, "CERTIFIED", &tip, "carried");
                } else {
                    self.lib.land_mark(id, "REBASED", &tip, "swept");
                }
                self.log(&format!("CHECK6 {id}: rebased {br} onto {} after this pass's landings — still landable", w.base));
                continue;
            }
            if rb.failure != "conflict" {
                self.log(&format!(
                    "CHECK6 {id}: could not attempt a rebase of {br} onto {} after this pass's landings ({}) — not a conflict, leaving the bead closed",
                    w.base, rb.failure
                ));
                if rb.failure == "rebase-refused" {
                    self.lib.ask_rebase_refused(id, br, name, or(&rb.refused_reason, "unknown"));
                }
                continue;
            }
            if self.lib.pr_merged(&repo.path, br) {
                self.log(&format!("CHECK6 {id}: {br} does not rebase onto {}, but its pull request is merged — landed, not stuck", w.base));
                continue;
            }
            let cur_tip = self.git.rev_parse(&repo.path, br).unwrap_or_default();
            let cur_base = self.git.rev_parse(&repo.path, &w.base_fq).unwrap_or_default();
            if let Some(ls) = self.files.land_state(id) {
                if ls.state == "RED" && ls.tip == cur_tip {
                    self.log(&format!("CHECK6 {id}: tip unchanged since last RED mark — skipping duplicate bump"));
                    continue;
                }
            }
            self.lib.bump_requeue(id, "merge-conflict");
            let n = self.lib.requeues_of(id).max(1);
            let rc = self.lib.recut(br, &w.base_fq, &repo.path, name);
            if rc.ok {
                self.swept.set(self.swept.get() + 1);
                let t = self.git.rev_parse(&repo.path, br).unwrap_or_default();
                self.lib.land_mark(id, "REBASED", &t, "recut-swept");
                self.log(&format!(
                    "CHECK6 {id}: re-cut {br} onto {} after this pass's landings ({} commit(s)) — still landable",
                    w.base, rc.applied
                ));
                continue;
            }
            self.swept_conflict.set(self.swept_conflict.get() + 1);
            let t = self.git.rev_parse(&repo.path, br).unwrap_or_default();
            let conflicts = first_nonempty(&[&rc.conflicts, &rb.conflicts, "unknown"]);
            let others = self.lib.other_beads(&repo.path, br, &w.base_fq, first_nonempty(&[&rc.conflicts, &rb.conflicts]));
            let n_s = n.to_string();
            let path_s = repo.path.to_string_lossy().into_owned();
            self.lib.ask_rebase_loop(&[id, br, name, &n_s, conflicts, &others, &path_s, &w.base_fq]);
            self.out.progress(&format!(
                "escalated {id} — re-cut conflicted on {br} after {n} attempt(s); {} commit(s) moved to {}",
                rc.applied, w.base
            ));
            self.lib.land_mark(id, "RED", &t, &format!("no-rebase@{cur_base}"));
        }
    }
}

pub(crate) fn or<'s>(s: &'s str, d: &'s str) -> &'s str {
    if s.is_empty() { d } else { s }
}

pub(crate) fn first_nonempty<'s>(xs: &[&'s str]) -> &'s str {
    xs.iter().copied().find(|s| !s.is_empty()).unwrap_or("")
}

/// Log the first line of a multi-line text (confine.sh's verdict).
pub(crate) fn head1(s: &str) -> &str {
    first_line(s)
}
