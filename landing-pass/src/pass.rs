//! The gated pass (`landing-pass land`, DESIGN.md §4): push, hold, queue and queue.local.
//! pr-mode branches are counted and left to the pr pass.

use crate::gateq::{GateQueue, Job as GateJob};
use crate::budget::{gate_fits, gate_lock_wait};
use crate::model::{BeadRow, GateOutcome, GateRun, LandMode, RepoRow, RunRecord, Settings};
use crate::order::{basefail_fix_decision, certify_order, is_base_fix, prior_pass_suites, OrderRow};
use crate::ports::{Beads, Clock, Git, Lib, Procs, Tools};
use crate::records::{Against, Files, Graphs, Question};
use crate::report::Reporter;
use crate::util::{first_line, tail_bytes, tail_lines};
use std::cell::{Cell, RefCell};

/// A hold other than manual stops the gate; unknown kinds fail closed.
pub fn blocks_gate(holds: &[String]) -> bool {
    holds.iter().any(|h| spira_store::lc_state::hold_kind(h).map(|k| k.as_str()) != Some("manual"))
}
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Dead gates on one branch, with no gate dying elsewhere between, that make the branch the cause.
const DEAD_GATES_RED: u32 = 2;

/// A NO_VERDICT that names no host-side cause: the gate ran the branch and never finished.
fn is_dead_gate_reason(reason: &str) -> bool {
    matches!(reason, "died" | "timeout" | "unspecified") || reason == crate::model::DIED_TWICE
}

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
    pub lc_state: std::cell::OnceCell<Result<Vec<crate::lifecycle::Waiting>, String>>,
    pub out: &'a Reporter,
    pub files: Files,
    pub start: u64,
    pub pid: u32,
    /// The sweep's own meter (law-take-the-simple-fix-with-a-meter).
    pub swept: Cell<u64>,
    pub swept_conflict: Cell<u64>,
    /// Branches the walk answered from their settled marker, with no git call.
    pub settled_from_memory: Cell<u64>,
    pub graphs: Graphs,
    /// Repository paths already fetched this pass.
    pub fetched: RefCell<HashSet<PathBuf>>,
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
    /// Skipped as not closed, or as held by a live dwarf: no decision was made, so the
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

fn carries_nothing_mark(bead: &BeadRow) -> bool {
    bead.reason.as_deref() == Some(lifecycle::bead::CERTIFIES_NOTHING_MARK)
}

/// Per-repository walk state.
pub(crate) struct Walk<'r> {
    pub repo: &'r RepoRow,
    pub base: String,
    pub base_fq: String,
    pub land: PathBuf,
    pub beads: HashMap<String, BeadRow>,
    pub enum_tip: HashMap<String, String>,
    /// `spira/*` branches already merged into the base, from one read after the fetch.
    pub merged: HashSet<String>,
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

    /// The waiting rows, read once per pass: reachability and the settle sweep share the one read.
    fn lc_waiting(&self) -> &Result<Vec<crate::lifecycle::Waiting>, String> {
        self.lc_state.get_or_init(|| self.lc.waiting())
    }

    /// Is spira-lc reachable?
    pub(crate) fn lc_ready(&self) -> Result<(), String> {
        self.lc_waiting().as_ref().map(|_| ()).map_err(Clone::clone)
    }

    /// The bead's lifecycle state, re-read live (sp-mve9i: never bd's status).
    pub(crate) fn bead_lc_state(&self, id: &str) -> String {
        self.beads.bead_lc_state(id)
    }

    /// (handed on, the state it was read in) — a re-read before acting on a scan-time row.
    fn handed_on_now(&self, id: &str) -> (bool, String) {
        let st = self.bead_lc_state(id);
        (crate::model::handed_on(&st), st)
    }

    /// A red gate that already wrote REWORK leaves the bead claimable; the reopen's resume
    /// note and requeue bump are then ours to record.
    pub(crate) fn record_rework(&self, id: &str, cause: &str, note: &str) {
        self.log(&format!("CHECK6 {id}: bead is now REWORK (gate red) — recording the resume note and requeue bump, not reopening"));
        self.lib.note(id, note);
        self.lib.bump_requeue(id, cause);
    }

    /// `bd show` rows joined to their lifecycle states. A store or machine that cannot be
    /// read is said loudly: nothing reads as handed on this pass.
    pub(crate) fn show_rows(&self, _repo: &std::path::Path, ids: &[String]) -> Result<Vec<BeadRow>, String> {
        self.beads.show(ids).inspect_err(|why| {
            self.log(&crate::lifecycle::unreachable_line(why, "no bead reads as handed on this pass"));
        })
    }

    /// Record a gate outcome as a lifecycle event; a refusal is loud, and the
    /// caller of a "pass" must not mark CERTIFIED when this returns false.
    fn lc_certify(&self, id: &str, tip: &str, outcome: &str, detail: &str) -> bool {
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

    /// A red verdict recorded with the step it failed at and that step's message.
    fn lc_certify_red(&self, id: &str, tip: &str, reason: &str, g: &GateRun) -> bool {
        let (step, message) = g.red_at();
        match self.lc.certify_red(id, tip, reason, &step, &message) {
            Ok(a) => {
                self.log(&format!("CHECK6 {id}: lifecycle certify red at {tip}: {a}"));
                true
            }
            Err(why) => {
                self.log(&format!("CHECK6 {id}: LIFECYCLE: certify red at {tip} did not happen ({why})"));
                false
            }
        }
    }

    /// A verdict the row refused because it holds another tip: the gate judged the branch, so
    /// the row takes the branch tip and the same verdict is offered once more.
    fn lc_certify_at_tip(&self, id: &str, tip: &str, outcome: &str, detail: &str) -> bool {
        if self.lc_certify(id, tip, outcome, detail) {
            return true;
        }
        match self.lc.resubmit_submitted(id, tip) {
            Ok(a) => {
                self.log(&format!("CHECK6 {id}: lifecycle held another tip — resubmitted {tip}: {a}"));
                self.lc_certify(id, tip, outcome, detail)
            }
            Err(why) => {
                self.log(&format!("CHECK6 {id}: LIFECYCLE: resubmit of {tip} did not happen ({why})"));
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

        let (seen_before, settled_before) = (self.out.branches(), self.settled_from_memory.get());
        for repo in self.rotated() {
            self.settle_landed_rows(repo);
            self.walk_repo(repo);
            // A pr-mode repository's file is the pr pass's.
            if repo.mode != LandMode::Pr {
                self.graphs.write(&self.files, &repo.name);
            }
        }
        self.files.prune_graphs(&names);

        self.log(&format!(
            "landing: walk complete — {} branch(es), {} settled from memory",
            self.out.branches() - seen_before,
            self.settled_from_memory.get() - settled_before
        ));

        for r in self.repos {
            if !r.mode.refreshed() || r.path.as_os_str().is_empty() {
                continue;
            }
            if !r.path.join(".git").exists() {
                continue;
            }
            let o = self.tools.skew_refresh(&r.path, &["--no-fetch"]);
            if !o.is_empty() {
                self.log(&o);
            }
        }

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
        for tool in ["unit-ensure", "target-reap"] {
            match spira_config::release_env::tool(tool) {
                Ok(script) => {
                    for l in self.tools.ensure(Path::new(&script)) {
                        self.log(&l);
                    }
                }
                Err(e) => self.log(&format!("landing: {e}")),
            }
        }
        self.log(&format!(
            "landing: pass complete — {} branch(es) seen, {} movement(s){sweep}",
            self.out.branches(),
            self.out.moved()
        ));
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

    /// A SUBMITTED or CERTIFIED row whose recorded tip is already an ancestor of the landing
    /// ref is settled LANDED, whether or not its branch still exists to be walked.
    fn settle_landed_rows(&self, repo: &RepoRow) {
        let (Some(base_fq), false) = (repo.base_fq.as_deref(), repo.path.as_os_str().is_empty()) else { return };
        if !repo.mode.queued() || !repo.path.join(".git").exists() {
            return;
        }
        let rows: Vec<(String, String)> = match self.lc_waiting() {
            Ok(all) => all.iter().filter(|w| w.repo == repo.name).map(|w| (w.id.clone(), w.tip.clone())).collect(),
            Err(e) => {
                self.log(&format!("CHECK6 {}: waiting rows unreadable ({e}) — none settled against {base_fq} this pass", repo.name));
                return;
            }
        };
        if rows.is_empty() {
            return;
        }
        self.fetch_once(repo);
        let refs = self.git.spira_refs(&repo.path);
        let at = self.against(repo, base_fq);
        let merged = self.graphs.merged(at, &refs);
        for (id, tip) in rows {
            let landed = match refs.iter().find(|(_, t)| *t == tip) {
                Some((br, _)) => merged.contains(br),
                None => self.graphs.answer(at, &tip, Question::Ancestor, || true, || self.git.is_ancestor(&repo.path, &tip, base_fq)),
            };
            if !landed {
                continue;
            }
            let at = self.git.rev_parse(&repo.path, base_fq).unwrap_or_default();
            if self.lib.content_on_base(&id, &format!("settle-landed:{tip} ancestor-of:{base_fq}@{at}")) {
                self.log(&format!("CHECK6 {id}: tip {tip} is an ancestor of {base_fq} — settled LANDED"));
            }
        }
    }

    fn fetch_once(&self, repo: &RepoRow) {
        if let Some(rem) = &repo.base_remote {
            if self.fetched.borrow_mut().insert(repo.path.clone()) {
                self.git.fetch(&repo.path, rem);
            }
        }
    }

    fn against<'b>(&'b self, repo: &'b RepoRow, base_fq: &'b str) -> Against<'b> {
        Against { files: &self.files, git: self.git, name: &repo.name, path: &repo.path, base_fq }
    }

    /// Whether the base already holds `br` at `tip`, asked of git once per (tip, base commit).
    pub(crate) fn content_on_base(&self, repo: &RepoRow, br: &str, tip: &str, base_fq: &str) -> bool {
        self.graphs.content_on_base(self.against(repo, base_fq), br, tip)
    }

    pub fn walk_repo(&self, repo: &RepoRow) {
        let name = &repo.name;
        if repo.path.as_os_str().is_empty() {
            self.log(&format!("CHECK6 {name}: no repository map entry — skipped"));
            return;
        }
        if !repo.path.join(".git").exists() {
            self.log(&format!("CHECK6 {name}: {} is not a git checkout — skipped", repo.path.display()));
            if self.repos.len() == 1 {
                self.lib.ask_repo_unreadable(name, &repo.path);
            }
            return;
        }
        // Ahead of every early return: the pass's refresh of this checkout does not fetch.
        if repo.mode.refreshed() {
            self.fetch_once(repo);
        }
        let refs = self.git.spira_refs(&repo.path);
        if refs.is_empty() {
            return;
        }
        self.out.add_branches(refs.len() as u64);
        if repo.mode == LandMode::Pr {
            return;
        }
        if !self.s.batcher_enable && matches!(repo.mode, LandMode::Push | LandMode::Other(_)) {
            self.log(&format!("{name}: SPIRA_BATCHER_ENABLE=0 — the operator lands by hand; {} branch(es) held", refs.len()));
            return;
        }
        let (Some(base), Some(base_fq)) = (repo.landref.clone(), repo.base_fq.clone()) else {
            self.log(&format!(
                "CHECK6 {name}: cannot resolve the ref its branches land on — skipped. Give it a `base` in the repository map."
            ));
            return;
        };
        self.fetch_once(repo);
        let merged = self.graphs.merged(self.against(repo, &base_fq), &refs);
        let basename = repo.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let land = self.s.run.join("worktree").join(format!(".landing.{basename}"));
        if repo.mode == LandMode::Push {
            if !self.git.tree_ok(&land) {
                self.lib.prune_worktrees(&repo.path);
                self.git.tree_add_detached(&repo.path, &land, &base_fq);
            }
            if self.git.tree_ok(&land) && !self.git.tree_on_landing(&land, &base_fq) {
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
            refs.iter().map(|(b, _)| OrderRow::of(b, beads.get(b.trim_start_matches("spira/")))).collect();
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
            merged,
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

        if w.beads.get(id).is_some_and(|b| b.handed_on()) && w.enum_tip.get(br).is_some_and(|tip| self.files.content_settled(br, tip)) {
            self.settled_from_memory.set(self.settled_from_memory.get() + 1);
            return Screen::Done(Flow::Next);
        }
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
        let st = bead.map(|b| b.state.as_str()).unwrap_or("");
        if !crate::model::handed_on(st) {
            if w.merged.contains(br) {
                return Screen::Done(Flow::Next);
            }
            let shown = if st.is_empty() { "-" } else { st };
            if self.procs.holder_alive(id) {
                self.log(&format!("CHECK6 {id}: {br} not landed — its bead is {shown}, held by a live dwarf"));
            } else {
                self.log(&format!("CHECK6 {id}: {br} not landed — its bead is {shown} and no dwarf holds it"));
            }
            return Screen::Retry;
        }
        let bead = bead.expect("handed on implies present");

        // A branch lands only in the repository its BEAD names.
        let bead_path = self.repos.iter().find(|r| r.name == bead.repo).map(|r| r.path.clone());
        if bead_path.as_deref() != Some(repo.path.as_path()) {
            self.log(&format!("CHECK6 {id}: {br} is in {name} but the bead names repo:{} — not landing it here", bead.repo));
            return Screen::Done(Flow::Next);
        }
        if bead.superseded {
            self.log(&format!(
                "CHECK6 {id}: {br} is superseded — its work landed under the successor's id; leaving it for the Hel to reap"
            ));
            return Screen::Done(Flow::Next);
        }
        if bead.has_label(&self.s.cutover_label) {
            self.log(&format!("CHECK6 {id}: {br} is labelled {} — leaving it for the cutover round", self.s.cutover_label));
            return Screen::Done(Flow::Next);
        }
        // A queued repository's terminal bead off the base is the batcher's: no content read.
        // Push and other modes keep it, because `push_or_hold` relies on it there.
        if !w.merged.contains(br) && repo.mode.queued() && spira_store::lc_state::is_terminal(&bead.state) {
            self.log(&format!("CHECK6 {id}: {br} is {} — the batcher's now, not certified again", bead.state));
            return Screen::Done(Flow::Next);
        }
        let tip = w.enum_tip.get(br).map(String::as_str).unwrap_or_default();
        if w.merged.contains(br) || self.content_on_base(repo, br, tip, &w.base_fq) {
            self.settle_content_on_base(w, br, id, bead);
            return Screen::Done(Flow::Next);
        }
        if repo.mode.queued() && bead.state == "IN_DELIVERY" && self.beads.in_round(id) {
            self.gate_provisional(w, br, id, bead);
            return Screen::Done(Flow::Next);
        }
        // A queued repository only certifies: a SUBMITTED row is the hand-over, and a later
        // Submit at a new tip replaces the job, so the live builder need not be waited for.
        let hand_over = repo.mode.queued() && bead.state == "SUBMITTED";
        if !hand_over && self.procs.holder_alive(id) {
            self.log(&format!("CHECK6 {id}: a live dwarf still holds {br} — deferring the land"));
            return Screen::Retry;
        }
        if repo.mode.queued() {
            // sp-vn19s: a queued repository certifies SUBMITTED only. Past that (CERTIFIED,
            // IN_DELIVERY, ...) the bead is the batcher's; walking it again would re-gate a
            // certified tip every pass. A push or hold repository lands its own CERTIFIED beads.
            if bead.state != "SUBMITTED" {
                self.log(&format!("CHECK6 {id}: {br} is {} — the batcher's now, not certified again", bead.state));
                return Screen::Done(Flow::Next);
            }
            return Screen::Queue(bead);
        }
        Screen::Push(bead)
    }

    /// §4.1 step 5: the base already holds every change on `br`. Said and settled once per
    /// tip; a bead not yet terminal is closed LANDED, but only when the branch did work for
    /// it (hel's evidence rule) — an empty diff alone proves nothing.
    fn settle_content_on_base(&self, w: &Walk, br: &str, id: &str, bead: &BeadRow) {
        let repo = &w.repo.path;
        let Some(tip) = w.enum_tip.get(br).cloned().or_else(|| self.git.rev_parse(repo, br)) else {
            self.log(&format!("{} already contains every change on {br} — nothing to land", w.base));
            return;
        };
        if self.files.content_settled(br, &tip) {
            return;
        }
        if self.files.announce_content(br, &tip) {
            self.log(&format!("{} already contains every change on {br} — nothing to land", w.base));
        }
        if spira_store::lc_state::is_terminal(&bead.state) {
            self.files.settle_content(br, &tip);
            return;
        }
        let ahead = self.git.count(repo, &format!("{}..{br}", w.base_fq)).unwrap_or(0);
        let own = || {
            self.git
                .log_subjects(repo, &format!("{}..{br}", w.base_fq), &[])
                .is_some_and(|s| s.lines().any(|l| names_bead(l, id)))
        };
        let stacked = || {
            self.git
                .log_subjects(repo, &format!("{}..{br}", w.base_fq), &[])
                .is_some_and(|s| !s.trim().is_empty() && s.lines().all(|l| l.starts_with("stacked base: merge ")))
        };
        let on_base = || {
            self.git
                .log_grep(repo, id, std::slice::from_ref(&w.base_fq))
                .is_some_and(|s| s.lines().filter_map(|l| l.split_once('\t')).any(|(_, subj)| names_bead(subj, id)))
        };
        if hel::sweep::needs_evidence(ahead, own, stacked, on_base) {
            self.log(&format!("CHECK6 {id}: {br} has {ahead} commit(s) not in {}, none naming the bead — an empty diff is not evidence of landing, not closing it", w.base));
            self.files.settle_content(br, &tip);
            return;
        }
        let at = self.git.rev_parse(repo, &w.base_fq).unwrap_or_default();
        if !self.lib.content_on_base(id, &format!("merge-tree:{at}")) {
            return;
        }
        self.lib.note(id, &format!("tip {tip}, {} at {at}: no tree diff, closed LANDED by landing-pass", w.base));
        self.out.progress(&format!("landing-pass: {id} closed LANDED — {br} at {tip} adds nothing to {} ({at})", w.base));
        self.files.settle_content(br, &tip);
    }

    /// Queue-mode certification (DESIGN.md §4.2).
    fn certify(&self, w: &Walk, br: &str, id: &str, bead: &BeadRow) -> Flow {
        let tip = match self.certify_screen(w, br, id, bead) {
            Ok(tip) => tip,
            Err(f) => return f,
        };
        let holds = self.holds_after_expired_wait(id, bead);
        if blocks_gate(&holds) {
            self.log(&format!("CHECK6 {id}: {br} is held ({}) — not gated until the hold lifts", holds.join(", ")));
            return Flow::Next;
        }
        if !holds.is_empty() {
            self.log(&format!("CHECK6 {id}: {br} is held (manual) — gated; the hold keeps it out of the cut"));
        }
        if self.beads.in_round(id) {
            self.files.clear_unrounded(br);
            self.gate_provisional(w, br, id, bead);
            return Flow::Next;
        }
        // A move that rewrote the reason (a closed batch's requeue) took the mark: only a new verdict restores it.
        if self.files.marked(br, &tip) && !carries_nothing_mark(bead) {
            self.log(&format!("CHECK6 {id}: {br} at {tip} no longer carries its certifies-nothing mark — gating it again"));
            self.files.forget_marked(br);
            self.files.clear_backoff(br);
        }
        if self.s.gate_worker {
            return self.certify_queued(w, br, id, bead, &tip);
        }
        self.drop_foreign_backoff(br, id, &tip);
        if self.files.backing_off(br, &tip, self.clock.now()) {
            self.log(&format!("CHECK6 {id}: {br} at {tip} found no room for a gate lately — not gated again yet"));
            return Flow::Next;
        }
        let g = self.run_gate(&w.repo.name, br, id);
        self.certify_judge(w, br, id, bead, &tip, g, &Cell::new(false))
    }

    fn drop_foreign_backoff(&self, br: &str, id: &str, tip: &str) {
        if self.files.take_foreign_backoff(br, tip, self.clock.now()) {
            self.log(&format!("CHECK6 {id}: {br} at {tip} carries a back-off from another release — gating it again"));
        }
    }

    /// The row's holds once an expired timed wait is lifted; a refused lift is logged and the hold stays.
    fn holds_after_expired_wait(&self, id: &str, bead: &BeadRow) -> Vec<String> {
        let expired = bead.reason.as_deref().is_some_and(|r| spira_store::lc_state::wait_expired(r, self.clock.now() as i64));
        if !expired || !bead.holds.iter().any(|h| h == "wait") {
            return bead.holds.clone();
        }
        match self.lc.unhold(id, "wait") {
            Ok(_) => bead.holds.iter().filter(|h| *h != "wait").cloned().collect(),
            Err(e) => {
                self.log(&format!("CHECK6 {id}: could not lift its expired wait hold ({e}) — stays held"));
                bead.holds.clone()
            }
        }
    }

    /// A member a round holds has no gate of its own: the round's pass on the merged tree is its
    /// test verdict. A red already on file is marked once; a cancelled run is taken and charges
    /// nothing; a queued job is dropped and a claimed one cancelled. Nothing is ever queued.
    fn gate_provisional(&self, w: &Walk, br: &str, id: &str, _bead: &BeadRow) {
        let name = &w.repo.name;
        let tip = self.git.rev_parse(&w.repo.path, br).unwrap_or_default();
        if tip.is_empty() || self.files.gated(br, &tip) {
            return;
        }
        if !self.s.gate_worker {
            self.log(&format!("CHECK6 {id}: {br} is in a round — the round judges it, no per-bead gate"));
            return;
        }
        let q = GateQueue::new(&self.s.run);
        if q.peek_done(name, br, &tip).is_some_and(|d| d.run.unjudged()) {
            q.forget_done(name, br, &tip);
        }
        if let Some(d) = q.peek_done(name, br, &tip).filter(|d| d.run.outcome == GateOutcome::Fail) {
            drop(q.take_done(name, br, &tip));
            self.files.note_gated(br, &tip);
            let reason = d.run.reason_or("unspecified");
            self.log(&format!("CHECK6 {id}: provisional {br} failed its gate at {tip} ({reason}) — marked red for the round"));
            let _ = self.lc_certify_red(id, &tip, &reason, &d.run);
            return;
        }
        let withdrawn = |what: &str| self.log(&format!("CHECK6 {id}: {br} is held by a round — its gate is withdrawn ({what})"));
        if q.peek_done(name, br, &tip).is_some_and(|d| d.run.outcome == GateOutcome::NoVerdict && d.run.reason.as_deref() == Some("cancelled")) {
            drop(q.take_done(name, br, &tip));
            withdrawn("cancelled run taken, nothing charged");
            return;
        }
        if q.withdraw(name, br, &tip) {
            withdrawn("dropped queued job");
        } else if q.find(name, br, &tip) == Some(crate::gateq::Where::Claimed) {
            let (rc, out) = self.tools.gate_cancel(id, &format!("a round holds {id}: the round's pass is its test verdict"));
            if rc == 0 {
                withdrawn("cancel requested");
            } else {
                self.log(&format!("CHECK6 {id}: {br} is held by a round — cancel refused ({}), asking again next pass", out.trim()));
            }
        } else {
            withdrawn("nothing queued");
        }
    }

    /// Hand the gate to `gate-worker`: apply the verdict it filed for this tip, or queue the
    /// branch and move on. The pass never waits on a gate, so no budget can cut it short.
    fn certify_queued(&self, w: &Walk, br: &str, id: &str, bead: &BeadRow, tip: &str) -> Flow {
        let name = &w.repo.name;
        let q = GateQueue::new(&self.s.run);
        if q.peek_done(name, br, tip).is_some_and(|d| d.run.unjudged()) {
            drop(q.take_done(name, br, tip));
            self.log(&format!("CHECK6 {id}: gate-worker filed no verdict for {br} at {tip} — gating it again"));
        } else if let Some(d) = q.take_done(name, br, tip) {
            self.log(&format!("CHECK6 {id}: gate-worker's verdict for {br} at {tip} is in — applying it"));
            self.files.note_gated(br, tip);
            let regate = Cell::new(false);
            let flow = self.certify_judge(w, br, id, bead, tip, d.run, &regate);
            if !regate.get() {
                return flow;
            }
        }
        self.drop_foreign_backoff(br, id, tip);
        if self.files.backing_off(br, tip, self.clock.now()) {
            self.log(&format!("CHECK6 {id}: {br} at {tip} found no room for a gate lately — not queued again yet"));
            return Flow::Next;
        }
        let fix = is_base_fix(bead.external_ref.as_deref(), name);
        match q.enqueue(&GateJob::new(name, br, id, tip, fix).with_priority(bead.priority).with_express(bead.express)) {
            Ok(true) => {
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
        if self.files.pass_refused(br, &tip) {
            self.log(&format!("CHECK6 {id}: lifecycle already refused GatePass for {br} at {tip} — not offered again"));
            return Err(Flow::Next);
        }
        if !self.s.gate_worker && !self.budget_allows(bead, &repo.name, id) {
            return Err(Flow::BudgetCut);
        }
        Ok(tip)
    }

    /// §4.2 after the gate: the one decision this pass makes for this branch.
    #[allow(clippy::too_many_arguments)]
    fn certify_judge(&self, w: &Walk, br: &str, id: &str, bead: &BeadRow, tip: &str, g: GateRun, regate: &Cell<bool>) -> Flow {
        let repo = w.repo;
        let name = &repo.name;
        let tip = tip.to_string();
        if g.outcome == GateOutcome::NoVerdict && g.reason_or("unspecified") == spira_config::gate_source::CERTIFIES_NOTHING {
            let mut window = self.s.noverdict_class_window;
            if repo.mode == LandMode::Queue {
                if self.git.rev_parse(&repo.path, br).unwrap_or_default() != tip {
                    self.log(&format!("CHECK6 {id}: {br} moved during the gate — verdict on {tip} not recorded"));
                    return Flow::Next;
                }
                let already = self.files.marked(br, &tip) && carries_nothing_mark(bead);
                if already || self.lc_certify_at_tip(id, &tip, "nothing", spira_config::gate_source::CERTIFIES_NOTHING) {
                    self.files.note_marked(br, &tip);
                    self.log(&format!("CHECK6 {id}: {br} in {name} at {tip} certifies nothing — admitted to its batch provisionally; the batch PR's CI is its check"));
                } else {
                    window = self.s.gate_backoff;
                }
            } else {
                self.log(&format!("CHECK6 {id}: {br} in {name} at {tip} certifies nothing — its repository declares tests its gate does not run; the bead stays SUBMITTED"));
            }
            self.files.back_off(br, &tip, self.clock.now() + window.max(0) as u64);
            return Flow::Next;
        }
        self.files.forget_marked(br);
        if g.outcome != GateOutcome::Pass {
            let reason = g.reason_or("unspecified");
            self.log(&format!("CHECK6 {id}: certification gate {} on {br} in {name} ({reason})", g.outcome.word()));
            let moved = matches!(g.outcome, GateOutcome::Fail | GateOutcome::NoVerdict)
                && self.git.rev_parse(&repo.path, br).unwrap_or_default() != tip;
            let dead_run = (g.outcome == GateOutcome::NoVerdict && !moved && is_dead_gate_reason(&reason))
                .then(|| if reason == crate::model::DIED_TWICE { DEAD_GATES_RED.max(self.files.note_dead(br)) } else { self.files.note_dead(br) })
                .filter(|n| *n >= DEAD_GATES_RED);
            if reason == crate::model::DIED_TWICE && dead_run.is_some() && !moved {
                self.lib.event("gate.died-twice", id, &format!("{id}: the gate died at its cap twice on {tip}"), &tail_lines(&g.out, 3));
            }
            if g.outcome == GateOutcome::Fail {
                self.files.note_gate_judged(br);
                self.settle_unjudged(name, br, &tip, "fail");
            }
            if moved {
                self.log(&format!("CHECK6 {id}: {br} moved during the gate — verdict on {tip} not recorded"));
            } else {
                match g.outcome {
                    GateOutcome::Fail => drop(self.lc_certify_red(id, &tip, &reason, &g)),
                    GateOutcome::NoVerdict if dead_run.is_some() => drop(self.lc_certify(id, &tip, "red", "timeout")),
                    GateOutcome::NoVerdict => drop(self.lc_certify(id, &tip, "infra", "")),
                    _ => {}
                }
            }
            match g.outcome {
                GateOutcome::BaseFail => {
                    self.settle_unjudged(name, br, &tip, "base-fail");
                    self.log(&format!("CHECK6 {id}: held — the base fails its own gate (suite {})", g.suite));
                    self.base_fail(w, br, id, bead, &tip, &g);
                }
                GateOutcome::NoVerdict if dead_run.is_none() && reason == "gate-not-found" && self.awaiting_first_cut(br) => {
                    self.log(&format!("CHECK6 {id}: {br} has no per-bead gate and no round yet — not counted or escalated until the batcher has passed it over {}s", self.s.unrounded_grace));
                }
                GateOutcome::NoVerdict if dead_run.is_none() => {
                    self.lib.noverdict(id, br, name, &reason, g.outcome.word(), &g.out);
                    let mut held = false;
                    if !moved {
                        self.files.back_off(br, &tip, self.clock.now() + self.s.gate_backoff.max(0) as u64);
                        let seen = self.files.note_exhaust(br, &tip, &reason);
                        if seen.len() >= self.s.noverdict_max.max(1) as usize {
                            held = self.stop_regating_unjudged(w, br, id, &tip, &seen, &g.out);
                        }
                    }
                    regate.set(!moved && !held);
                }
                _ => {
                    let (closed, st) = self.handed_on_now(id);
                    if !closed && st != "REWORK" {
                        self.log(&format!("CHECK6 {id}: bead is now {st} (was closed at scan time) — not reopening {br}"));
                        return Flow::Next;
                    }
                    let n = self.count_or_q(&repo.path, &format!("{}..{br}", w.base));
                    let scope = self.scope_note(name, br, &g.suite);
                    let scope_part = if scope.is_empty() { String::new() } else { format!("\n{scope}") };
                    let (red_step, red_message) = g.red_at();
                    let red_part = match (red_step.as_str(), red_message.as_str()) {
                        ("", "") => String::new(),
                        (st, m) => format!("\nFailed step: {}\nMessage: {m}", if st.is_empty() { "-" } else { st }),
                    };
                    let why = match dead_run {
                        Some(k) => format!("{name}'s certification gate could not finish this branch: it died {k} times running ({reason}) while no other branch's gate did, so the branch is the cause. Look at the phase and test the tail below was in."),
                        None => format!("branch {br} failed {name}'s certification gate."),
                    };
                    let note = format!(
                        "Reopened by sentinel: {why} The branch carries {n} commit(s) from the previous session — the next dwarf should resume from the existing work, not restart.{red_part}\n{scope_part}\n\n{}",
                        tail_lines(&g.out, 20)
                    );
                    let cause = crate::model::red_cause("cert-gate-red", &reason);
                    if !closed {
                        self.record_rework(id, &cause, &note);
                        return Flow::Next;
                    }
                    self.lib.reopen(id, &cause, &note);
                    self.out.progress(&format!("reopened {id} — failed the certification gate"));
                    self.lib.event(
                        "bead.reopened",
                        id,
                        &format!("reopened {id} — {br} failed {name}'s certification gate"),
                        &tail_lines(&g.out, 3),
                    );
                }
            }
            return Flow::Next;
        }
        if g.reason.as_deref() == Some("cached") {
            self.log(&format!("CHECK6 {id}: certification PASS on {br} in {name} — this tree had already passed"));
        }
        self.files.clear_noverdict(br);
        self.files.note_gate_judged(br);
        self.settle_unjudged(name, br, &tip, "pass");
        let (closed, st) = self.handed_on_now(id);
        if !closed {
            self.log(&format!("CHECK6 {id}: bead is now {st} (was closed at scan time) — not certifying {br}"));
            return Flow::Next;
        }
        if !self.lc_certify_at_tip(id, &tip, "pass", "gate") {
            self.files.refuse_pass(br, &tip);
            self.log(&format!("CHECK6 {id}: lifecycle refused GatePass at {tip} — {br} stays uncertified"));
            return Flow::Next;
        }
        self.out.progress(&format!("certified {br} in {name} — gate passed, round and CI are the remaining judges"));
        Flow::Next
    }

    /// A verdict contradicts "the gate cannot judge": close every such incident filed for `repo`.
    /// An id whose close fails stays recorded for the next verdict.
    fn settle_unjudged(&self, repo: &str, br: &str, tip: &str, verdict: &str) {
        let at = crate::util::iso_utc(self.clock.now());
        let evidence = format!("{repo}'s gate reached a verdict ({verdict}) on {br} at {tip} at {at}: the gate can judge again");
        for b in self.files.unjudged_branches(repo) {
            self.files.clear_backoff(&b);
        }
        self.files.forget_unjudged_branches(repo);
        for inc in self.files.unjudged_incidents(repo) {
            match self.lib.close_incident(&inc, &evidence) {
                Ok(()) => {
                    self.files.forget_unjudged_incident(repo, &inc);
                    self.log(&format!("CHECK6 {br}: closed incident {inc} — {repo}'s gate reached a verdict ({verdict})"));
                }
                Err(e) => self.log(&format!("CHECK6 {br}: incident {inc} not closed, kept for the next verdict: {e}")),
            }
        }
    }

    /// The gate could not judge this tip `reasons.len()` times: stop gating it until the window
    /// ends, a verdict lands in the repo, or the tip moves, and file one incident. A harness
    /// fault, so no rework, no attempt charged, and no lifecycle hold: the bead stays in the pool.
    fn stop_regating_unjudged(&self, w: &Walk, br: &str, id: &str, tip: &str, reasons: &[String], out: &str) -> bool {
        let repo = &w.repo.name;
        let n = reasons.len();
        let joined = reasons.join(", ");
        let reason = reasons.last().map(String::as_str).unwrap_or("unknown");
        let title = format!("{repo}'s gate cannot judge: {reason}");
        let body = [
            format!("{repo}'s certification gate ended without a verdict on {id} ({br} at {tip}) {n} times running."),
            format!("Reasons, in order: {joined}"),
            format!("Each run was followed by a backoff of {}s before the tip was gated again.", self.s.gate_backoff),
            "A gate that cannot judge is a harness fault, not the bead's: the bead was not reopened and no attempt was charged.".into(),
            format!("The tip is not gated again until the {}s window ends, the repo's gate reaches a verdict on any branch, or the branch gets a new tip. The bead is not held and stays in the round pool.", self.s.noverdict_class_window),
            String::new(),
            "--- the last run's output -------------------------------------------------------------".into(),
            tail_bytes(out, 6000),
        ]
        .join("\n");
        let filing = crate::model::IncidentFiling {
            cause: "gate-unjudged".into(),
            repo: repo.to_string(),
            reference: format!("gate-unjudged:{repo}:{reason}"),
            title,
            body,
        };
        let inc = match self.lib.file_incident(&filing) {
            Ok(id) => id,
            Err(e) => {
                self.log(&format!("CHECK6 {id}: the gate-unjudged incident was not filed: {e}"));
                String::new()
            }
        };
        if !inc.is_empty() {
            self.files.note_unjudged_incident(repo, &inc);
        }
        let inc = if inc.is_empty() { "not filed".to_string() } else { inc };
        let until = self.clock.now() + self.s.noverdict_class_window.max(0) as u64;
        self.files.back_off(br, tip, until);
        self.files.note_unjudged_branch(repo, br);
        self.files.clear_exhaust(br);
        self.log(&format!("CHECK6 {id}: {br} at {tip} not gated again until {} — the gate could not judge it {n} time(s): {joined}; incident {inc}; it stays in the round pool", crate::util::iso_utc(until)));
        true
    }

    fn awaiting_first_cut(&self, br: &str) -> bool {
        (self.files.unrounded_age(br, self.clock.now()) as i64) <= self.s.unrounded_grace
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
                let wait = gate_lock_wait(self.s.gate_timeout, self.s.gate_lock_wait.as_deref());
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
            let _ = self.certify_judge(w, &job.br, id, &job.bead, &job.tip, GateRun::parse(rc, out), &Cell::new(false));
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
            w.beads.get(id).is_some_and(|x| x.handed_on()) && !self.procs.holder_alive(id)
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
            .map(|b| OrderRow::of(b, w.beads.get(b.trim_start_matches("spira/"))))
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

    pub(crate) fn run_gate(&self, name: &str, br: &str, id: &str) -> GateRun {
        self.set_run(name, br, "gate");
        let wait = gate_lock_wait(self.s.gate_timeout, self.s.gate_lock_wait.as_deref());
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
            let (closed, _) = self.handed_on_now(id);
            if closed {
                self.log(&format!("CHECK6 {id}: base-fix: {br} is green on {name}'s red suite {suite} — certifying"));
                if !self.lc_certify_at_tip(id, tip, "pass", "base-fix") {
                    self.files.refuse_pass(br, tip);
                    self.log(&format!("CHECK6 {id}: lifecycle refused GatePass at {tip} — {br} stays uncertified"));
                    return;
                }
                self.out.progress(&format!("certified {br} in {name} — base-fix (suite {suite})"));
            }
            return;
        }
        self.lib.bump_requeue(id, crate::model::BASE_RED_CAUSE);
        if !w.basefail_filed.get() {
            w.basefail_filed.set(true);
            let sha = self.git.rev_parse(&w.repo.path, &w.base_fq).unwrap_or_default();
            self.base_incident(name, &g.suite, &g.reason_or("base-red"), br, &w.base, &sha, &g.out);
        }
    }

    /// File the base's own red through the in-process intake (deduped on repository + suite).
    #[allow(clippy::too_many_arguments)]
    fn base_incident(&self, name: &str, suite: &str, reason: &str, br: &str, base: &str, sha: &str, out: &str) {
        let named = if suite == "-" { "- (the gate named none; read its output below)".to_string() } else { suite.to_string() };
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
        match self.lib.file_incident(&base_red_filing(name, &key, &title, &payload)) {
            Ok(id) => self.log(&format!("CHECK6 {name}: the base's own red is {id} (suite {suite})")),
            Err(e) => self.log(&format!("CHECK6 {name}: the base's own red was not filed: {e}")),
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
                self.log(&format!("CHECK6 {id}: a dwarf took {br} while this pass ran — leaving its rebase to it"));
                continue;
            }
            let tip = self.git.rev_parse(&repo.path, br).unwrap_or_default();
            if self.content_on_base(repo, br, &tip, &w.base_fq) {
                self.log(&format!("CHECK6 {id}: {} now contains every change on {br} — nothing left to rebase", w.base));
                continue;
            }
            let rb = self.lib.rebase(br, &w.base_fq, &repo.path, name);
            if rb.ok {
                self.swept.set(self.swept.get() + 1);
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
            self.lib.bump_requeue(id, "merge-conflict");
            let n = self.lib.requeues_of(id).max(1);
            let rc = self.lib.recut(br, &w.base_fq, &repo.path, name);
            if rc.ok {
                self.swept.set(self.swept.get() + 1);
                self.log(&format!(
                    "CHECK6 {id}: re-cut {br} onto {} after this pass's landings ({} commit(s)) — still landable",
                    w.base, rc.applied
                ));
                continue;
            }
            self.swept_conflict.set(self.swept_conflict.get() + 1);
            let conflicts = first_nonempty(&[&rc.conflicts, &rb.conflicts, "unknown"]);
            let others = self.lib.other_beads(&repo.path, br, &w.base_fq, first_nonempty(&[&rc.conflicts, &rb.conflicts]));
            let n_s = n.to_string();
            let path_s = repo.path.to_string_lossy().into_owned();
            self.lib.ask_rebase_loop(&[id, br, name, &n_s, conflicts, &others, &path_s, &w.base_fq]);
            self.out.progress(&format!(
                "escalated {id} — re-cut conflicted on {br} after {n} attempt(s); {} commit(s) moved to {}",
                rc.applied, w.base
            ));
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

/// `subject` names `id` as a whole word: `sp-ab` is not `sp-abc`.
fn names_bead(subject: &str, id: &str) -> bool {
    subject
        .match_indices(id)
        .any(|(i, _)| !subject[i + id.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '.' || c == '-'))
}

pub(crate) fn base_red_filing(name: &str, key: &str, title: &str, payload: &str) -> crate::model::IncidentFiling {
    crate::model::IncidentFiling { cause: "base-suite-red".into(), repo: name.into(), reference: key.into(), title: title.into(), body: payload.into() }
}
