//! The run: decline or claim, build the workspace, render the brief, run the session.
//! verdict.rs judges what the session left; teardown.rs accounts for it.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use crate::bd::{self, BeadRow};
use crate::checkpoint;
use crate::brief::{self, FixtureInfo, Tokens};
use crate::claim::{self, Selection, Selector};
use crate::conf::{self, Conf, Fayth, SystemPrompt};
use crate::decide::{self, WorldStop};
use crate::ledger::{self, Ledger};
use crate::ports::{s, Bd, Env, Exec, Git, Seam};
use crate::restrict;
use crate::seam::Snapshot;
use crate::session::{self, Beat, Heartbeat, Launcher, SessionSpec, Stop};
use crate::stack;
use crate::trace;
use crate::util::{self, Out, Sink};
use crate::worktree;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Claim,
    DryRun,
    Sweep { prompt: Option<String> },
}

/// Why the work phase stopped early. Every variant still runs the teardown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Abort {
    /// aeon.sh's `die`: `FATAL <msg>` on stderr, rc 1.
    Die(String),
    /// A plain `exit <n>` inside the armed region.
    Exit(i32),
    /// TERM/INT, or the heartbeat's own trip: rc 128+sig.
    Signal(i32),
}

pub struct Deps<'a> {
    pub bd: &'a dyn Bd,
    pub seam: &'a dyn Seam,
    pub git: &'a dyn Git,
    pub exec: &'a dyn Exec,
    pub launcher: &'a dyn Launcher,
    pub sink: &'a dyn Sink,
    pub env: &'a Env,
    pub clock: &'a (dyn Fn() -> i64 + Sync),
    /// The summon jitter's sleep (sp-f4ig1): one second at a time, the stop flag checked between.
    pub sleep: &'a (dyn Fn(Duration) + Sync),
}

/// Everything learned along the way — aeon.sh's globals, now fields.
#[derive(Debug, Default)]
pub struct State {
    pub aeon: String,
    /// The claim holder: `aeon-<name>@<pid>.<starttime>`, one per process (spira_config::session).
    pub holder: String,
    pub bead: String,
    pub claimed: Option<BeadRow>,
    pub repo_name: String,
    pub repo: PathBuf,
    pub repo_land: String,
    pub branch: String,
    pub pidfile: Option<PathBuf>,
    pub logf: Option<PathBuf>,
    pub work: Option<PathBuf>,
    pub base: String,
    pub base_branch: String,
    pub base_remote: String,
    pub base_fq: String,
    pub world_was_stopped: bool,
    pub fixture: Option<FixtureInfo>,
    pub fixture_lib: Option<PathBuf>,
    pub session_started: bool,
    /// The claude session id this run launched or resumed (empty before launch).
    pub session_id: String,
    pub session_rc: i32,
    /// The verdict's `committed`; false until the verdict block ran.
    pub committed: bool,
    /// The branch's tip (full hash), read right before the session starts (sp-1zxru-2).
    /// Empty (never read) or `?` (the read failed) both compare unequal to nothing but
    /// themselves — [`decide::tip_moved`] treats either as "cannot tell", not as moved.
    /// Distinct from `committed` (`verdict_committed`'s own job: a commit naming this bead
    /// ANYWHERE in the window, branch or landing refs, for the close/SOP/groom/eviction
    /// checks that must see a prior session's work too): this field answers only "did THIS
    /// session's own turn move the branch" — the Unlanded/NoProgress split needs that
    /// narrower question, or a branch already ahead from an earlier session reads
    /// `committed` forever and never lands in NoProgress no matter how many more sessions
    /// add nothing (sp-iku03, sp-al5ng).
    pub session_start_tip: String,
    pub lc_model_restricted: bool,
    pub requeue_cause: Option<String>,
    pub requeue_why: String,
    pub session_epoch: i64,
    pub rebase_conflicts: String,
    pub groom_lines_before: usize,
    /// The claim's stack proposal (design stacked-dependents-2026-09-28 §1): the certified
    /// tip of every prerequisite this claim was built on, keyed by prerequisite bead id.
    /// Empty for a claim that is not stacked on anything.
    pub stack: std::collections::BTreeMap<String, String>,
}

pub struct Run<'a> {
    pub d: Deps<'a>,
    pub conf: Conf,
    pub fayth: Fayth,
    pub snap: Snapshot,
    pub mode: Mode,
    pub ledger: Ledger,
    pub pid: u32,
    pub own_unit: String,
    pub t0: i64,
    /// `spira-claim`, by name on the launcher's PATH; a field so a test can point it at a fixture.
    pub claim_bin: String,
    pub stop: Arc<Stop>,
    pub hb_shutdown: Arc<AtomicBool>,
    pub hb_done: Arc<AtomicBool>,
    pub fayth_file: PathBuf,
    pub s: State,
}

impl<'a> Run<'a> {
    pub fn now(&self) -> i64 {
        (self.d.clock)()
    }

    pub fn log(&self, msg: &str) {
        self.d.sink.out(&util::log_line(self.now(), msg));
    }

    pub fn die_line(&self, msg: &str) {
        self.d.sink.err(&util::log_line(self.now(), &format!("FATAL {msg}")));
    }

    /// A lib.sh function whose stdout is the answer; its stderr is passed through.
    pub fn sv(&self, f: &str, args: &[String]) -> Out {
        let o = self.d.seam.call(f, args);
        for l in o.stderr.lines() {
            self.d.sink.err(l);
        }
        o
    }

    /// A lib.sh function called for its effect; what it prints is passed through.
    pub fn sdo(&self, f: &str, args: &[String]) -> i32 {
        let o = self.d.seam.call(f, args);
        for l in o.stdout.lines() {
            self.d.sink.out(l);
        }
        for l in o.stderr.lines() {
            self.d.sink.err(l);
        }
        o.code
    }

    pub fn note(&self, text: &str) {
        bd::note(self.d.bd, &self.s.bead, text);
    }

    pub fn release(&self) {
        self.sdo("release_own_claim", &s(&[&self.s.bead]));
    }

    pub fn bead_reopen(&self, cause: &str, note: &str) -> i32 {
        self.sdo("bead_reopen", &s(&[&self.s.bead, cause, note]))
    }

    pub fn bump_requeue(&self, cause: &str) {
        self.sdo("bump_requeue", &s(&[&self.s.bead, cause]));
    }

    pub fn home(&self) -> &Path {
        &self.conf.home
    }

    pub fn run_dir(&self) -> &Path {
        &self.conf.run
    }

    pub fn f(&self) -> &str {
        &self.fayth.name
    }

    /// `ledger_done <rc> <status>`: the line, then tsd and the rapid-recur check.
    pub fn ledger_done(&self, rc: i32, status: &str) {
        let fields = ledger::session_result_fields(self.s.logf.as_deref(), &self.conf.trace_mark());
        self.ledger.done(self.now(), self.f(), &self.s.bead, rc, status, &fields);
        trace::tsd_aeon_session(self.d.exec, &self.run_dir().display().to_string(), &self.s.bead, self.f(), rc, status, &fields);
        self.rapid_recur_check();
    }

    /// `rapid_recur_check`: three (SPIRA_RAPID_RECUR_THRESHOLD) consecutive sub-10s `done`
    /// lines for this bead are a setup loop that recurs identically on every retry
    /// (law-a-retry-must-change-an-input) — park with the ask label and `overseer` rather
    /// than keep re-summoning into the same fault. Reads the ledger the `done` line above
    /// just wrote.
    fn rapid_recur_check(&self) {
        let id = self.s.bead.clone();
        if id.is_empty() {
            return;
        }
        let threshold = self.conf.i("SPIRA_RAPID_RECUR_THRESHOLD").max(0) as usize;
        if threshold == 0 {
            return;
        }
        let Ok(text) = std::fs::read_to_string(self.conf.ledger()) else { return };
        let needle = regex::Regex::new(&format!(r" done \S+ {} ", regex::escape(&id))).unwrap();
        let matches: Vec<&str> = text.lines().filter(|l| needle.is_match(l)).collect();
        let tail = &matches[matches.len().saturating_sub(threshold)..];
        let count = decide::rapid_recur_streak(tail);
        if count < threshold as i64 {
            return;
        }
        // Already parked is the row's ask hold, never a label (sp-psztcc).
        if self.lc_bead(&id).is_some_and(|r| r.held("ask")) {
            return;
        }
        self.log(&format!("{}: {id} RAPID-RECUR: {count} consecutive sub-10s runs — parking, a setup loop cannot be learned from a retry", self.f()));
        let _ = self.d.bd.bd(&s(&["label", "add", &id, "overseer"]));
        // spira-lc's caller verb, as in verdict.rs's eviction-race escalation.
        let _ = self.d.exec.exec("spira-lc", &s(&["hold", &id, "ask", &format!("rapid-recur: {count} consecutive sub-10s aeon summons"), self.f()]), None, None);
        bd::note(
            self.d.bd,
            &id,
            &format!(
                "RAPID-RECUR: {count} consecutive sub-10s aeon runs on {id}. Each summon dies before meaningful work, suggesting a setup loop — the defect recurs identically on every retry. Parked with an ask hold (and the overseer label) instead of only annotated: a fourth summon cannot learn anything the third did not. Check: worktree path, conflicting branches, or box state. Details in aeon-ledger."
            ),
        );
        self.sdo(
            "spira_event",
            &s(&["aeon.rapid", &id, &format!("Rapid-recur: {id} — {count} consecutive sub-10s aeon summons (setup loop) — parked")]),
        );
    }

    fn check_stop(&self) -> Result<(), Abort> {
        match self.stop.signalled() {
            Some(sig) => Err(Abort::Signal(sig)),
            None => Ok(()),
        }
    }

    /// `capacity_paused`, in-process now (wave 4.26 — family K's home is this crate; aeon
    /// is the probe's one owner). Shared by `decline()` and `sweep()`.
    pub(crate) fn capacity_check(&self) -> crate::capacity::Verdict {
        let v = crate::capacity::check_and_probe(&self.conf, self.d.exec, self.now());
        for l in &v.log {
            self.log(l);
        }
        if v.clear_file {
            let _ = std::fs::remove_file(self.conf.capacity_pause());
        }
        v
    }

    fn take_name(&mut self) {
        // In-process (wave 4.23, sp-0ffox): this crate is aeon_name_take's own owning
        // crate — the ONLY caller besides this one is sweep.rs's own copy of the same
        // call, so there is no cross-crate reason left to round-trip through lib.sh.
        let name = crate::naming::aeon_name_take(self.run_dir(), self.f());
        let actor = format!("aeon-{name}");
        let holder = spira_config::session::own_holder(&actor);
        let env = self.d.env;
        env.set("SPIRA_AEON", &name);
        env.set("BEADS_ACTOR", &holder);
        env.set("GIT_AUTHOR_NAME", &actor);
        env.set("GIT_AUTHOR_EMAIL", &format!("{actor}@spira.local"));
        env.set("GIT_COMMITTER_NAME", &actor);
        env.set("GIT_COMMITTER_EMAIL", &format!("{actor}@spira.local"));
        self.s.aeon = name;
        self.s.holder = holder;
    }

    // ==== the whole run ================================================================

    pub fn main(&mut self) -> i32 {
        if let Err(lines) = conf::fenced(self.f(), &self.fayth.labels, &self.conf.s("SPIRA_SCOPE_LABEL")) {
            for l in lines {
                self.log(&l);
            }
            self.die_line(&format!("{}: refusing to claim behind an unfenced predicate", self.f()));
            return 1;
        }
        self.ledger.trim();
        self.ledger.born(self.now(), self.f(), self.pid);
        if let Mode::Sweep { prompt } = self.mode.clone() {
            return self.sweep(prompt);
        }
        if let Some(code) = self.decline() {
            return code;
        }
        if self.mode == Mode::DryRun {
            return self.dry_run();
        }
        if let Err(code) = self.claim() { return code }
        // ---- armed: every exit from here runs the teardown ----
        std::thread::scope(|sc| {
            let rc = match self.work(sc) {
                Ok(()) => 0,
                Err(Abort::Die(m)) => {
                    self.die_line(&m);
                    1
                }
                Err(Abort::Exit(n)) => n,
                Err(Abort::Signal(sig)) => 128 + sig,
            };
            self.teardown(rc)
        })
    }

    /// The capacity / halted / draining / paused declines: Some(exit code) to stop.
    fn decline(&mut self) -> Option<i32> {
        let own = self.own_unit.clone();
        let have: i64 = self.sv("aeon_count", &s(&[self.f(), &own])).text().trim().parse().unwrap_or(0);
        let mut limit = self.fayth.max_concurrent as i64;
        let mut pool = String::new();
        if self.fayth.elastic {
            // SPIRA_MAX_AEONS is registered (a real spira/conf.d default, 4) — reached
            // through `Conf::i`, never `self.conf.v` directly: that raw map read used to
            // let an absent/unparseable value silently skip this whole branch instead of
            // naming the key (per Ryan 2026-10-05, round 2: a missing key is an error).
            let max = self.conf.i("SPIRA_MAX_AEONS");
            limit = max;
            pool = (if max > have { max - have } else { 0 }).to_string();
        }
        let free = self.sv("fayth_free", &s(&[self.f(), &pool, &own])).text();
        if free.trim().parse::<i64>().ok() == Some(0) {
            self.log(&format!("{}: at capacity ({have}/{limit}), not summoning", self.f()));
            self.ledger.awake(self.now(), self.f(), "capacity");
            return Some(0);
        }
        if self.run_dir().join("world.halted").is_file() {
            self.log(&format!("{}: halted — claiming nothing (world.sh start to lift)", self.f()));
            self.ledger.awake(self.now(), self.f(), "halted");
            return Some(0);
        }
        if self.run_dir().join("world.draining").is_file() {
            self.log(&format!("{}: draining — claiming nothing (world.sh resume to lift)", self.f()));
            self.ledger.awake(self.now(), self.f(), "draining");
            return Some(0);
        }
        match self.capacity_check().state {
            crate::capacity::Paused::Open => {}
            crate::capacity::Paused::Paused(left) => {
                self.log(&format!("{}: the account is out of capacity for another {left}s — claiming nothing", self.f()));
                self.ledger.awake(self.now(), self.f(), "paused");
                return Some(0);
            }
            crate::capacity::Paused::Unknown => {
                self.log(&format!("{}: the account's capacity pause file could not be read — claiming nothing (failing closed)", self.f()));
                self.ledger.awake(self.now(), self.f(), "paused");
                return Some(0);
            }
        }
        None
    }

    /// The ready set, as JSON: spira-claim's `fayth-ready --json`, the lifecycle machine's
    /// claimable rows in this fayth's partition — the rows CHECK 7 counted when it summoned
    /// this aeon; bd supplies only their content.
    fn ready_set(&self, tries: u32, delay: Duration) -> Result<String, String> {
        let tries = tries.max(1);
        let mut last = Out::default();
        for i in 1..=tries {
            last = self.d.seam.call("_aeon_ready_set", &[]);
            if last.success() {
                return Ok(util::json_only(&last.stdout));
            }
            if i < tries {
                (self.d.sleep)(delay);
            }
        }
        Err(format!("fayth-ready: query failed after {tries} attempt(s): {}", last.first_err_line()))
    }

    /// `spira-claim stack <id>` — the stack this claim would carry, or an empty, unstacked
    /// proposal when the binary is missing or its answer does not parse: a claim never
    /// blocks on this the way it blocks on `lc_claim_bead` itself, since the worst case is
    /// only that this round misses the speed the stack would have bought it.
    fn stack_proposal(&self, id: &str, who: &str) -> stack::Proposal {
        let claim_bin = self.claim_bin.clone();
        let o = self.d.exec.exec(&claim_bin, &s(&["stack", id]), None, None);
        match stack::parse_proposal(&o.stdout) {
            Some(p) => p,
            None => {
                self.log(&format!("{who}: {id} — spira-claim stack did not answer ({}) — proceeding unstacked", o.first_err_line()));
                stack::Proposal::default()
            }
        }
    }

    fn dry_run(&self) -> i32 {
        self.log(&format!("{}: dry run — candidates:", self.f()));
        let rows: serde_json::Value = self.ready_set(1, Duration::ZERO).ok().and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
        for r in rows.as_array().into_iter().flatten().take(10) {
            self.d.sink.out(r["id"].as_str().unwrap_or("?"));
        }
        0
    }

    /// A WORKING bead this name holds under another session that is still running: claiming
    /// a second would silently double-hold. A dead session's row is the reaper's, not a refusal.
    fn double_hold(&self, mine: &str) -> Option<String> {
        let o = self.d.exec.exec("spira-lc", &s(&["list-all"]), None, None);
        if o.code != 0 {
            return None;
        }
        o.stdout.lines().find_map(|l| {
            let mut f = l.split('\t');
            let (id, state, holder) = (f.next()?, f.next()?, f.next()?);
            let live = !spira_config::session::session_gone(holder, &spira_config::admission::RealProcs);
            (state == "WORKING" && live && spira_config::session::same_name_other_session(holder, mine)).then(|| id.to_string())
        })
    }

    /// The lifecycle claim: a Claim event per ranked candidate, carrying its stack proposal,
    /// until one applies. bd is read afterwards for the bead's content only. Ok(None): idle.
    fn lc_claim(&mut self, ids: &[String], resumable: &[String], tier: Option<&str>, who: &str) -> Result<Option<claim::Claimed>, i32> {
        let holder = self.s.holder.clone();
        if let Some(held) = self.double_hold(&holder) {
            self.log(&format!("{who}: refusing to claim — {held} is already WORKING under another live session of {}", self.s.aeon));
            return Err(1);
        }
        let until = (self.now() + self.fayth.lease_seconds()).to_string();
        let mut stacks = std::collections::BTreeMap::new();
        let (won, logs, unreachable) = claim::lc_claim_loop(ids, resumable, tier, who, |id| {
            let p = self.stack_proposal(id, who);
            let rc = self.sdo(
                "lc_claim_bead",
                &s(&[id, &holder, &until, &stack::stack_json(&p.stack), &p.stack_depth.to_string(), &p.stack_max_depth.to_string(), &self.fayth.name]),
            );
            stacks.insert(id.to_string(), p.stack);
            rc
        });
        for l in logs {
            self.log(&l);
        }
        let Some(id) = won else {
            if unreachable {
                self.log(&format!("{}: claim-error the lifecycle machine answered no claim — not reporting idle for a claim that never completed", self.f()));
                self.ledger.awake(self.now(), self.f(), "claim-error lifecycle machine unreachable");
                return Err(1);
            }
            return Ok(None);
        };
        self.s.stack = stacks.remove(&id).unwrap_or_default();
        let raw = bd::json(self.d.bd, &["show", &id]);
        match bd::first_row(&raw) {
            Some(row) if row.id == id => {
                let repo = row.label_value("repo:").unwrap_or_default();
                Ok(Some(claim::Claimed { id, repo, row, raw }))
            }
            _ => {
                self.s.bead = id.clone();
                self.release();
                self.log(&format!("{who}: claimed {id}, but bd returned no record of it — released"));
                self.ledger.awake(self.now(), self.f(), &format!("claim-error no bd record for {id}"));
                Err(1)
            }
        }
    }

    /// The holds on a just-claimed row that make it unclaimable: the Claim event does not
    /// read holds, so a hold applied between the ready read and the claim is caught here.
    fn blocking_holds(&self, id: &str) -> Vec<String> {
        let o = self.d.exec.exec("spira-lc", &s(&["holds", id]), None, None);
        o.stdout.lines().map(str::trim).filter(|h| !h.is_empty() && *h != "wait").map(String::from).collect()
    }

    fn claim(&mut self) -> Result<(), i32> {
        self.take_name();
        let tries = self.conf.i("SPIRA_CLAIM_RETRIES").max(1) as u32;
        let delay = Duration::from_secs(self.conf.i("SPIRA_CLAIM_RETRY_DELAY_S").max(0) as u64);
        let ready = match self.ready_set(tries, delay) {
            Ok(j) => j,
            Err(e) => {
                let e = if e.is_empty() { "bd gave no reason".to_string() } else { e };
                self.log(&format!("{}: claim-error {e} — retries exhausted, not reporting idle for a query that never completed", self.f()));
                self.ledger.awake(self.now(), self.f(), &format!("claim-error {e}"));
                return Err(1);
            }
        };
        let ready = if ready.trim().is_empty() { "[]".to_string() } else { ready };
        let claim_bin = self.claim_bin.clone();
        let who = format!("{}/{}", self.f(), self.s.aeon);
        let sel = Selector { exec: self.d.exec, git: self.d.git, claim_bin: &claim_bin, fayth: &self.fayth.name, scratch: &self.conf.run, pid: self.pid, repos: &self.conf.repos };
        let (ids, resumable, tier) = match sel.select(&ready) {
            Selection::ClaimError { log, ledger } => {
                self.log(&log);
                self.ledger.awake(self.now(), self.f(), &ledger);
                return Err(1);
            }
            Selection::Ranked { ids, resumable, tier } => (ids, resumable, tier),
        };
        let claimed = self.lc_claim(&ids, &resumable, tier.as_deref(), &who)?;
        let Some(c) = claimed else {
            self.log(&format!("{}: nothing ready to claim", self.f()));
            self.ledger.awake(self.now(), self.f(), "idle");
            return Err(0);
        };
        self.s.bead = c.id.clone();
        self.s.claimed = Some(c.row.clone());
        self.log(&format!("{who}: claimed {}", c.id));
        self.ledger.awake(self.now(), self.f(), &c.id);
        self.sdo("spira_event", &s(&["aeon.claimed", &c.id, &format!("{} claimed {}", self.s.aeon, c.id), &format!("summoned from the {} fayth", self.f())]));

        // The baseline verdict compares the description against at close. A metadata write, so
        // not a description edit as bdq's live-claim fence reads it.
        if let Some(h) = bead::claimdesc::desc_hash(&bd::json(self.d.bd, &["show", &c.id])) {
            let _ = self.d.bd.bd(&s(&["update", &c.id, "--set-metadata", &format!("{}={h}", bead::claimdesc::HASH_KEY)]));
        }

        // Read-after-claim: the predicate and the claim are not atomic.
        let held = self.blocking_holds(&c.id);
        if !held.is_empty() {
            self.release();
            self.log(&format!("{who}: {} carries a {} hold — released immediately after claim (race with the hold)", c.id, held.join(",")));
            self.ledger_done(0, "hold-raced");
            return Err(0);
        }

        // ---- world-stop fence ----
        let label = self.conf.s("SPIRA_WORLD_STOP_LABEL");
        let has = bd::show(self.d.bd, &c.id).is_some_and(|r| r.has_label(&label));
        let live = self.live_peers();
        let skip = self.conf.set_nonempty("SPIRA_WORLD_STOP_SKIP");
        match decide::world_stop(has, &live, skip) {
            WorldStop::Refuse => {
                self.release();
                self.log(&format!("{who}: {} carries {label} — live aeons present ({live}) — released. Set SPIRA_WORLD_STOP_SKIP=1 to override.", c.id));
                self.note(&format!("Released by aeon.sh: this bead carries {label} and requires the world halted while it runs. Live aeons are present ({live}) and the world was not stopped. Wait for them to finish, or set SPIRA_WORLD_STOP_SKIP=1 to proceed with live aeons."));
                self.ledger_done(0, "world-stop-fence"); // literal-ok: ledger status name, not a label
                return Err(0);
            }
            WorldStop::Stop => {
                let extra = if live.is_empty() { String::new() } else { format!(" (SPIRA_WORLD_STOP_SKIP set, live: {live})") };
                self.log(&format!("{who}: {} carries {label} — stopping the world before this session{extra}", c.id));
                if !self.d.exec.exec("world.sh", &s(&["stop", "--why", &format!("{label} bead {}", c.id)]), None, None).success() {
                    self.log(&format!("{who}: {} world.sh stop returned non-zero — proceeding", c.id));
                }
                self.s.world_was_stopped = true;
            }
            WorldStop::None => {}
        }

        // ---- the repository comes from the bead; an unknown name is refused ----
        // spira_config::repos (sp-37rmg, "wave 4.11") in-process, instead of a bash seam
        // call per lookup — repo_root/repo_land/spira_home_repo were the most-called family
        // in the whole decomposition.
        self.s.repo_name = if c.repo.is_empty() { self.conf.repos.home_repo().to_string() } else { c.repo.clone() };
        let root = self.conf.repos.root(&self.s.repo_name);
        let repo = root.clone().map(PathBuf::from).unwrap_or_default();
        if root.is_none() || !repo.join(".git").exists() {
            self.log(&format!("{}: {} names repo:{}, which repo-map does not resolve to a checkout", self.f(), c.id, self.s.repo_name));
            self.sdo("park_unmapped", &s(&[&c.id, &self.s.repo_name]));
            self.ledger_done(1, "unmapped-repo");
            // DESIGN.md §8.2: the world stopped for this bead is started again even here.
            self.restore_world();
            return Err(1);
        }
        self.s.repo = repo;
        self.s.repo_land = self.conf.repos.land(&self.s.repo_name);
        self.d.env.set("SPIRA_INCIDENT_REPO", &self.s.repo_name);
        self.log(&format!("{}: {} works repo:{} at {} (land={})", self.f(), c.id, self.s.repo_name, self.s.repo.display(), self.s.repo_land));

        // ---- the stacked base: a conflict here is a claim refusal, not a work-session
        // failure, so it is decided now, before work() ever arms the session (an unresolvable
        // base ref itself is left to work()'s own base block below, which refuses it exactly
        // as it always has — this check only ever fires when there is a stack to merge).
        if !self.s.stack.is_empty() {
            // spira_config::repos (sp-o88bx, "wave 4.12") in-process, instead of the
            // _aeon_base/qualify_base_ref bash seam.
            let base = spira_config::repos::landref(&self.conf.repos, &self.s.repo.display().to_string()).unwrap_or_default();
            if !base.is_empty() {
                let mut base_fq = spira_config::repos::qualify_base_ref(&base, &self.s.repo.display().to_string());
                if base_fq.is_empty() {
                    base_fq = base.clone();
                }
                match stack::build_stacked_base(self.d.git, &self.s.repo, &base_fq, &self.s.stack) {
                    Ok(merged) => self.s.base_fq = merged,
                    Err(conflict) => {
                        self.release();
                        let note = conflict.note();
                        for prereq in [&conflict.a, &conflict.b] {
                            if self.s.stack.contains_key(prereq) {
                                bd::note(
                                    self.d.bd,
                                    prereq,
                                    &format!(
                                        "{note} — a dependent's claim of {} could not merge this prerequisite's tip with another's cleanly, and was refused. The stack stays held until one of the two conflicting prerequisites changes.",
                                        c.id
                                    ),
                                );
                            }
                        }
                        self.log(&format!("{who}: {} — {note} — claim refused, stack stays held", c.id));
                        self.note(&format!("Released by aeon.sh: {note} while building this claim's stacked base. The stack stays held; it becomes claimable again once one of the conflicting prerequisites changes."));
                        self.ledger_done(0, "stack-conflict");
                        return Err(0);
                    }
                }
            }
        }

        // ---- the branch is recorded on the bead ----
        let st = self.d.bd.bd(&s(&["state", &c.id, "branch"]));
        let mut br = if st.success() { st.text() } else { String::new() };
        if br.starts_with('(') {
            br.clear();
        }
        if br.is_empty() {
            br = format!("spira/{}", c.id);
            let _ = self.d.bd.bd(&s(&["set-state", &c.id, &format!("branch={br}")]));
            self.log(&format!("{who}: {} takes branch {br}", c.id));
        } else {
            self.log(&format!("{who}: {} resumes recorded branch {br}", c.id));
        }
        self.s.branch = br;

        // ---- identity on disk, and the attempt's segment ----
        let pidfile = self.run_dir().join(format!("aeon-{}-{}.pid", self.f(), c.id));
        let logf = self.run_dir().join(format!("{}.log", c.id));
        let _ = std::fs::write(&pidfile, format!("{}\n", self.pid));
        let _ = std::fs::write(pidfile.with_extension("name"), &self.s.aeon);
        let mark = ledger::trace_mark_line(&logf, &self.s.aeon, &self.conf.trace_mark(), self.now());
        append(&logf, &format!("{mark}\n"));
        self.s.pidfile = Some(pidfile);
        self.s.logf = Some(logf);
        Ok(())
    }

    /// Other live aeons' pidfile names; a pidfile whose pid is gone is removed.
    fn live_peers(&self) -> String {
        let mut names = Vec::new();
        let Ok(rd) = std::fs::read_dir(self.run_dir()) else { return String::new() };
        let mut files: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                n.starts_with("aeon-") && n.ends_with(".pid")
            })
            .collect();
        files.sort();
        for pf in files {
            let pid = std::fs::read_to_string(&pf).map(|s| s.trim().to_string()).unwrap_or_default();
            if !util::pid_alive(&pid) {
                let _ = std::fs::remove_file(&pf);
                continue;
            }
            names.push(pf.file_stem().and_then(|n| n.to_str()).unwrap_or("").to_string());
        }
        names.join(", ")
    }

    pub fn restore_world(&mut self) {
        if self.s.world_was_stopped {
            self.s.world_was_stopped = false;
            self.log(&format!("{}: {} {} bead — starting the world", self.f(), self.s.bead, self.conf.s("SPIRA_WORLD_STOP_LABEL")));
            let _ = self.d.exec.exec("world.sh", &s(&["start"]), None, None);
        }
    }

    // ==== the armed region =============================================================

    fn work<'s>(&mut self, sc: &'s std::thread::Scope<'s, '_>) -> Result<(), Abort>
    where
        'a: 's,
    {
        let bead = self.s.bead.clone();
        let mail = self.conf.s("SPIRA_MAIL");
        if !mail.is_empty() {
            for sub in ["new", "cur", "tmp"] {
                let _ = std::fs::create_dir_all(Path::new(&mail).join(format!("aeon-{bead}")).join(sub));
            }
        }
        self.d.env.set("BEAD_ID", &bead);
        self.d.env.set("SPIRA_MAIL", &mail);
        self.d.env.set("SPIRA_MAIL_FROM", &self.fayth.mail_from());
        self.start_heartbeat(sc);
        self.check_stop()?;

        // ---- the base: a freshly fetched remote-tracking ref, never guessed ----
        // spira_config::repos (sp-o88bx, "wave 4.12") in-process, instead of the
        // _aeon_base/qualify_base_ref bash seam.
        let repo_disp = self.s.repo.display().to_string();
        let Some(base) = spira_config::repos::landref(&self.conf.repos, &repo_disp) else {
            self.log(&format!("{}: {bead} names repo:{}, whose land ref cannot be resolved", self.f(), self.s.repo_name));
            let map = self.conf.s("SPIRA_REPO_MAP");
            self.note(&format!("Released by aeon.sh: repo:{} has no resolvable default branch — {map} declares no `base` for it, its remote publishes no HEAD, and it is not a local-only repository. Give it a base column. Refusing to guess: a branch cut from a guessed base rebases onto a ref nobody chose, and `main` is a guess that is wrong wherever a repository still uses `master`.", self.s.repo_name));
            return Err(Abort::Exit(1));
        };
        self.s.base_branch = spira_config::repos::ref_branch(&base);
        self.s.base_remote = spira_config::repos::ref_remote(&base, Some(&repo_disp)).unwrap_or_default();
        self.s.base = base;
        if !self.s.base_remote.is_empty() && !self.d.git.git(&self.s.repo, &["fetch", "-q", &self.s.base_remote]).success() {
            self.log(&format!("{}: fetch of {} failed — basing on a possibly stale {}", self.f(), self.s.base_remote, self.s.base));
        }
        if self.s.stack.is_empty() {
            self.s.base_fq = spira_config::repos::qualify_base_ref(&self.s.base, &repo_disp);
            if self.s.base_fq.is_empty() {
                self.s.base_fq = self.s.base.clone();
            }
        }
        // else: claim() already built base_fq as the landing ref merged with this claim's
        // stack (stacked-dependents-2026-09-28 §1) — every read site below inherits it.

        // ---- one aeon, one worktree, under the sanctioned root ----
        let root = self.run_dir().join("worktree");
        let work = root.join(&bead);
        self.s.work = Some(work.clone());
        match worktree::evict_foreign(self.d.git, &work, &self.s.repo, self.now()) {
            worktree::Evict::Refused => return Err(Abort::Die(format!("{} belongs to another repository and could not be moved aside", work.display()))),
            worktree::Evict::Moved(aside) => {
                self.log(&format!("{}: {} was a worktree of another repository — moved to {}", self.f(), work.display(), aside.display()));
                self.note(&format!("Moved aside by aeon.sh: the worktree at {} belonged to a different repository than this bead's repo:{}. It is preserved at {} — nothing was deleted — and a fresh worktree was cut in the right checkout. A bead whose repo: label is corrected keeps its old worktree path, so without this every later summon would go on working it in the old repository.", work.display(), self.s.repo_name, aside.display()));
            }
            worktree::Evict::Kept => {}
        }
        let mut branch = self.s.branch.clone();
        let repo = self.s.repo.clone();
        let acts = {
            let e = worktree::Ensure { git: self.d.git, repo: &repo, work: &work, bead_id: &bead, root: &root, base: &self.s.base, base_fq: &self.s.base_fq, fayth: &self.fayth.name, now: self.now() };
            let prune = || {
                let _ = self.d.seam.call("spira_prune_worktrees", &s(&[&repo.display().to_string()]));
            };
            worktree::ensure(&e, &mut branch, &prune)
        };
        let acts = acts.map_err(Abort::Die)?;
        for a in acts {
            match a {
                worktree::Act::Log(l) => self.log(&l),
                worktree::Act::Note(n) => self.note(&n),
                worktree::Act::SetBranch(b) => {
                    let _ = self.d.bd.bd(&s(&["set-state", &bead, &format!("branch={b}")]));
                }
            }
        }
        self.s.branch = branch;
        self.check_stop()?;

        // ---- a retry is rebased now, while nothing else can be inside ----
        let mut rebase_text = String::new();
        let rb = self.d.seam.call("_aeon_rebase", &s(&[&self.s.branch, &self.s.base_fq, &repo.display().to_string(), &self.s.repo_name]));
        for l in rb.stderr.lines() {
            self.d.sink.out(l);
        }
        if !rb.success() {
            self.s.rebase_conflicts = rb.stdout.trim_end().to_string();
            let c = if self.s.rebase_conflicts.is_empty() { "unknown".to_string() } else { self.s.rebase_conflicts.clone() };
            self.log(&format!("{}: {} does not rebase onto {} — conflicts in {c}", self.f(), self.s.branch, self.s.base));
            rebase_text = brief::rebase_brief(&self.s.branch, &self.s.base, &work.display().to_string(), &self.s.rebase_conflicts);
        }

        // ---- prior work on the branch ----
        let range = format!("{}..{}", self.s.base_fq, self.s.branch);
        let n = self.d.git.git(&repo, &["rev-list", "--count", &range]);
        let n_prior = if n.success() { n.text().trim().to_string() } else { String::new() };
        let prior_log = match n_prior.as_str() {
            "" | "0" | "?" => String::new(),
            _ => self.d.git.git(&repo, &["log", "--format=  %h %s", "-n", "5", &self.s.branch]).text(),
        };
        let wdisp = work.display().to_string();
        let resume = brief::resume_brief(&self.s.branch, &wdisp, &n_prior, &prior_log);
        let mut slain = String::new();
        if n_prior.parse::<i64>().unwrap_or(0) > 0 {
            let last = self.d.git.git(&repo, &["log", "--format=%s", "-1", &self.s.branch]).text();
            let (mut when, mut ds) = (String::new(), String::new());
            if last.contains(brief::SLAY_MARK) {
                when = self.d.git.git(&repo, &["log", "--format=%ci", "-1", &self.s.branch]).text();
                ds = self.d.git.git(&repo, &["diff", "--stat", &self.s.base_fq, &self.s.branch]).stdout.lines().last().unwrap_or("").to_string();
            }
            slain = brief::slain_brief(&last, &n_prior, &self.s.base, &when, &ds, &self.s.logf.as_ref().map(|p| p.display().to_string()).unwrap_or_default());
        }

        // ---- exported for the commit guard and the session ----
        self.d.env.set("SPIRA_WORK", &wdisp);
        self.d.env.set("SPIRA_FAYTH", self.f());
        if self.f() == "czar" {
            let class = self.s.claimed.as_ref().and_then(|r| r.label_value("czar-class:")).unwrap_or_default();
            self.d.env.set("SPIRA_CZAR_CLASS", &class);
            self.d.env.set("SPIRA_CZAR_TRIGGER_BEAD", &bead);
        }

        // ---- pre-session dirty files and the worktree's hooks ----
        let mut dirty = Vec::new();
        let gd = self.d.git.git(&work, &["rev-parse", "--path-format=absolute", "--git-dir"]);
        if gd.success() && !gd.text().is_empty() {
            let gitdir = PathBuf::from(gd.text());
            let _ = self.d.git.git(&repo, &["config", "extensions.worktreeConfig", "true"]);
            let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            set.extend(self.d.git.git(&work, &["diff", "--name-only", "HEAD"]).stdout.lines().map(String::from));
            set.extend(self.d.git.git(&work, &["ls-files", "--others", "--exclude-standard"]).stdout.lines().map(String::from));
            set.retain(|l| !l.is_empty());
            dirty = set.into_iter().collect::<Vec<_>>();
            let body: String = dirty.iter().map(|l| format!("{l}\n")).collect();
            let _ = std::fs::write(gitdir.join("spira-dirty-before"), body);
            // The worktree's hooks are the RELEASE's (sp-31gtu): the aeon's --home is the
            // release's spira/ (its launcher passes it), worktree-hooks.sh arms the worktree
            // with core.hooksPath naming hooks that run `<home>/hooks/pre-commit` by absolute
            // path, and those call spira-lint by name on the launcher's release PATH — so a
            // fresh worktree commits with no build of its own.
            let hooks_home = self.home().display().to_string();
            let _ = self.d.exec.exec("env", &s(&[&format!("SPIRA_HOME={hooks_home}"), "worktree-hooks.sh", "install", &wdisp]), None, None);
        }

        // ---- one test fixture for the whole session ----
        let fixture_ms = self.build_fixture(&work);
        self.check_stop()?;

        // ---- the brief ----
        self.write_prompt(&work, fixture_ms, &resume, &slain, &rebase_text, &dirty)?;

        // ---- the groom log, before ----
        self.s.session_epoch = self.now();
        if self.fayth.groom_escalation_check {
            self.s.groom_lines_before = std::fs::read_to_string(self.run_dir().join("groom.log")).map(|t| t.lines().count()).unwrap_or(0);
        }
        self.check_stop()?;

        // ---- this session's own starting tip, read right before the model's turn (sp-1zxru-2) ----
        let start_tip = self.d.git.git(&work, &["rev-parse", "HEAD"]);
        self.s.session_start_tip = if start_tip.success() { start_tip.text().trim().to_string() } else { "?".into() };

        // ---- work ----
        self.session(&work)?;
        self.wiki_commit();
        self.verdict();
        Ok(())
    }

    fn build_fixture(&mut self, work: &Path) -> u64 {
        for k in ["TESTDB_SHARED", "TESTDB_NAME", "TESTDB_DIR", "TESTDB_BASELINE", "TESTDB_BIN", "TESTDB_SERVER_INIT_HASH"] {
            self.d.env.unset(k);
        }
        let rel = self.conf.s("SPIRA_TESTDB_LIB");
        let lib = work.join(&rel);
        if rel.is_empty() || !lib.is_file() {
            return 0;
        }
        let name = format!("aeon{}", self.s.bead.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>());
        let t = std::time::Instant::now();
        let mut data = Vec::new();
        for r in [lib.display().to_string(), name] {
            data.extend_from_slice(r.as_bytes());
            data.push(0);
        }
        let o = self.d.exec.exec("bash", &s(&["-c", FIXTURE_UP]), Some(data), None);
        let ms = t.elapsed().as_millis() as u64;
        if !o.stdout.is_empty() {
            let mut l = o.stdout.lines().map(String::from);
            let mut nx = || l.next().unwrap_or_default();
            let f = FixtureInfo { name: nx(), dir: nx(), baseline: nx(), bin: nx(), started_service: nx(), mode: nx(), server_init_hash: nx() };
            let env = self.d.env;
            env.set("TESTDB_SHARED", "1");
            env.set("TESTDB_NAME", &f.name);
            env.set("TESTDB_DIR", &f.dir);
            env.set("TESTDB_BASELINE", &f.baseline);
            env.set("TESTDB_BIN", &f.bin);
            env.set("TESTDB_STARTED_SERVICE", &f.started_service);
            env.set("TESTDB_MODE", &f.mode);
            env.set("TESTDB_SERVER_INIT_HASH", &f.server_init_hash);
            let mode = if f.mode.is_empty() { "embedded" } else { &f.mode };
            self.log(&format!("{}: {} shares one test fixture {} ({mode}), built in {ms}ms", self.f(), self.s.bead, f.name));
            self.s.fixture = Some(f);
            self.s.fixture_lib = Some(lib);
        } else {
            let why: String = o.stderr.replace('\n', " ").chars().take(500).collect();
            self.log(&format!("{}: {} has no shared test fixture — its suites will each build their own: {why}", self.f(), self.s.bead));
        }
        ms
    }

    fn write_prompt(&mut self, work: &Path, fixture_ms: u64, resume: &str, slain: &str, rebase: &str, dirty: &[String]) -> Result<(), Abort> {
        let bead = self.s.bead.clone();
        let wdisp = work.display().to_string();
        let overlay = self.conf.overlay();
        let db = self.conf.db();
        let landing = brief::landing_brief(&self.s.repo_land, &self.s.branch, &self.s.base_branch, &self.s.repo_name, &self.s.base);
        let park = brief::park_brief(&self.s.repo_land, &self.s.branch, &self.s.repo_name, &self.s.base_branch);
        let finish = brief::finish_brief(&bead);
        let fixture = brief::fixture_brief(self.s.fixture.as_ref(), &self.conf.s("SPIRA_TESTDB_LIB"), &self.conf.s("SPIRA_TESTDB_PORT"), fixture_ms);
        let deadline_at = self.fayth.timeout_seconds.map(|t| self.t0 + t as i64);
        let deadline = brief::deadline_brief(deadline_at, self.now(), deadline_at.and_then(util::local_hms_zone));

        let chamber_file = self.home().join("chamber").join(format!("{}.md", self.f()));
        let (chamber, logs) = brief::chamber_with_overlays(self.f(), &chamber_file, &overlay);
        for l in logs {
            self.log(&format!("{}: {bead} {l}", self.f()));
        }
        let mut blocks = Vec::new();
        for (name, text) in [("PARK", park), ("FIXTURE", fixture), ("DEADLINE", deadline), ("FINISH", finish)] {
            let (t, l) = brief::block_overlay(&overlay, name, text);
            if let Some(l) = l {
                self.log(&format!("{}: {bead} {l}", self.f()));
            }
            blocks.push(t);
        }

        // The bead, bounded; then who else holds the files it names.
        let shown = self.d.bd.bd(&s(&["show", &bead]));
        let body = util::strip_bd_hints(&shown.stdout).trim_end_matches('\n').to_string();
        let keep = self.conf.i("SPIRA_BRIEF_KEEP_RECURRENCES").max(0) as usize;
        let max = self.conf.i("SPIRA_BRIEF_NOTES_MAX_CHARS").max(0) as usize;
        let mut body = brief::bound_bead_notes(&format!("{body}\n"), keep, max).trim_end_matches('\n').to_string();
        let tracked = if self.s.repo.join(".git").exists() {
            let o = self.d.git.git(&self.s.repo, &["ls-files"]);
            if o.success() { o.stdout } else { String::new() }
        } else {
            String::new()
        };
        let paths: Vec<String> = trace::bead_named_paths(&body, &tracked);
        if !paths.is_empty() {
            let mut a = s(&["--repo", &self.s.repo_name]);
            a.extend(paths);
            let h = self.d.exec.exec("holds.sh", &a, None, None);
            let hb = brief::holds_brief(&bead, h.code, &h.stdout);
            if !hb.is_empty() {
                body = format!("{body}\n\n{hb}");
            }
        }

        // A thrashed bead leads with its sticking point, while the tip has not moved.
        let mut banner = None;
        let meta = self.sv("_aeon_thrash_meta", &s(&[&bead])).stdout;
        let mut ml = meta.lines();
        let (streak, tip, last) = (ml.next().unwrap_or("").to_string(), ml.next().unwrap_or("").to_string(), ml.next().unwrap_or("").to_string());
        if streak.trim().parse::<i64>().unwrap_or(0) >= 1 {
            let cur = self.d.git.git(work, &["rev-parse", "--short", "HEAD"]);
            let cur = if cur.success() { cur.text() } else { "?".into() };
            if !tip.is_empty() && tip == cur {
                banner = Some(brief::thrash_banner(&streak, &last));
            }
        }

        let scope = self.conf.s("SPIRA_SCOPE_LABEL");
        let home = self.home().display().to_string();
        let home_repo = self.conf.repos.home_repo().to_string();
        let mut single = vec![
            ("BEAD_ID", bead.clone()),
            ("BRANCH", self.s.branch.clone()),
            ("REPO", wdisp.clone()),
            ("REPO_NAME", self.s.repo_name.clone()),
            ("HOME_REPO", home_repo),
            ("LANDING", landing),
            ("DB", db.clone()),
            ("SPIKE_DIR", self.conf.s("SPIRA_SPIKE_DIR")),
            ("SPIKE_PATHS", self.conf.s("SPIRA_SPIKE_PATHS")),
            // Tools by bare name (sp-gypjk): the aeon's environment carries the
            // launcher's PATH, whose first entries are the release's bin/ and spira/.
            ("SUITES", format!("{} suites", brief::TESTENV)),
            ("TESTENV", brief::TESTENV.into()),
            ("FOLLOWUP", brief::followup_brief(&bead)),
            ("NO_BD", brief::no_bd_brief()),
            ("SPIRA_HOME", home.clone()),
            ("RUN", self.run_dir().display().to_string()),
            ("MAX_BEADS", self.conf.s("SPIRA_MAECHEN_MAX_BEADS")),
            ("REMEDY_LABEL", self.conf.s("SPIRA_MAECHEN_REMEDY_LABEL")),
            ("SCOPE", if scope.is_empty() { String::new() } else { format!("{scope},") }),
        ];
        // The tool placeholders (ASK, GROOM, INCIDENT, SOP, DEP): `work` verbs, since the
        // model has no bd-reaching tool (sp-st0mm).
        single.extend(brief::tool_tokens());
        // The model has no bd and no path into the release (sp-st0mm, sp-zf4q3): no prompt
        // names the database or the harness's home, so neither is handed over.
        single.retain(|(k, _)| !brief::WITHHELD.contains(k));
        let tokens = Tokens {
            single,
            bead: body,
            park: blocks[0].clone(),
            fixture: blocks[1].clone(),
            deadline: blocks[2].clone(),
            finish: blocks[3].clone(),
        };
        let prompt = brief::render_prompt(&chamber, &tokens, banner.as_deref());
        let statutes = self.statutes().map_err(Abort::Die)?;
        let (sys, mut task) = brief::split(&statutes, &prompt);
        task.push_str(&brief::dirty_brief(dirty));
        task.push_str(resume);
        task.push_str(slain);
        task.push_str(&brief::already_done_brief(&self.s.base, &bead, &wdisp));
        task.push_str(&brief::close_brief(&self.s.base, &wdisp, &self.s.base_remote));
        task.push_str(rebase);
        let _ = std::fs::write(self.run_dir().join(format!("{bead}.system.md")), sys);
        let _ = std::fs::write(self.run_dir().join(format!("{bead}.task.md")), task);
        Ok(())
    }

    /// `render_memories "${FAYTH_MEMORY_PREFIXES:-law-}" "" "${FAYTH_STATUTE_CORE:-}"`, with
    /// this installation's own `SPIRA_STATUTE_CORE_LOCAL` slugs appended to whatever core the
    /// persona resolves (`brief::with_local_core`). Err when a declared core slug — local or
    /// not — names no memory: the brief would be silently thinned.
    pub fn statutes(&self) -> Result<String, String> {
        let cache = self.conf.s("SPIRA_MEMORIES_CACHE");
        let age = self.conf.i("SPIRA_MEMORIES_CACHE_AGE");
        let mut json = String::new();
        if !cache.is_empty() && Path::new(&cache).is_file() {
            let m = session::mtime(Path::new(&cache));
            if self.now() - m < age {
                json = std::fs::read_to_string(&cache).unwrap_or_default();
            }
        }
        if json.trim().is_empty() {
            // Not a registered config key — `Conf::or`, not the strict `Conf::s`.
            let cmd = self.conf.or("SPIRA_MEMORIES_CMD", "");
            json = if !cmd.is_empty() {
                self.d.exec.exec("bash", &s(&["-c", &cmd]), None, None).stdout
            } else {
                bd::json(self.d.bd, &["memories"])
            };
            if !cache.is_empty() && !json.trim().is_empty() {
                if let Some(p) = Path::new(&cache).parent() {
                    let _ = std::fs::create_dir_all(p);
                }
                let _ = std::fs::write(&cache, format!("{}\n", json.trim_end_matches('\n')));
            }
        }
        let core = if self.fayth.statute_core.is_empty() { self.conf.s("SPIRA_STATUTE_CORE") } else { self.fayth.statute_core.clone() };
        let core = brief::with_local_core(&core, &self.conf.s("SPIRA_STATUTE_CORE_LOCAL"));
        let harness = self.conf.or("SPIRA_REPO", "<harness>");
        let missing = brief::missing_core(&json, &self.fayth.memory_prefixes, &core);
        if !missing.is_empty() {
            return Err(format!("{}: core statute(s) declared but not found: {} — refusing to start with a thinned brief", self.f(), missing.join(", ")));
        }
        Ok(brief::render_memories(&json, &self.fayth.memory_prefixes, 120_000, &core, &harness))
    }

    /// `aeon_claude_argv <flag> <file>`.
    pub fn claude_argv(&self, sys_file: &Path) -> Vec<String> {
        let flag = match self.fayth.system_prompt {
            SystemPrompt::Replace => "--system-prompt-file",
            SystemPrompt::Append => "--append-system-prompt-file",
        };
        let model = conf::persona_model(self.f(), &self.conf.s("SPIRA_TOML")).unwrap_or_else(|e| {
            eprintln!("aeon: FATAL: {e}");
            std::process::exit(1)
        });
        let mut a = s(&["-p", "--output-format", "stream-json", "--verbose", "--include-partial-messages", "--system-prompt-snapshot", "on", flag, &sys_file.display().to_string()]);
        a.extend(s(&["--model", &model, "--allowedTools", &self.fayth.tools, "--dangerously-skip-permissions"]));
        if self.fayth.project_instructions == "none" {
            a.extend(s(&["--setting-sources", "user"]));
        }
        a.extend(s(&["--settings", &aeon_settings(self.home())]));
        a
    }

    fn agent_bin(&self) -> String {
        let agent = self.conf.agent();
        if agent.contains('/') {
            return agent;
        }
        let path = self.d.env.child().get("PATH").cloned().unwrap_or_default();
        for d in path.split(':').filter(|d| !d.is_empty()) {
            let p = Path::new(d).join(&agent);
            if is_executable(&p) {
                return p.display().to_string();
            }
        }
        agent
    }

    fn session(&mut self, work: &Path) -> Result<(), Abort> {
        let bead = self.s.bead.clone();
        let logf = self.s.logf.clone().expect("armed");
        self.log(&format!("{}: working {bead} on {} (log: {})", self.f(), self.s.branch, logf.display()));
        if !work.is_dir() {
            return Err(Abort::Die(format!("worktree missing: {}", work.display())));
        }
        let _ = std::env::set_current_dir(work);
        let gh = self.run_dir().join("aeon-empty-gh");
        let _ = std::fs::create_dir_all(&gh);
        let env = self.d.env;
        env.set("GH_CONFIG_DIR", &gh.display().to_string());
        env.unset("GH_TOKEN");
        env.unset("GITHUB_TOKEN");
        env.set("GIT_SSH_COMMAND", "echo 'aeon: no SSH credentials — landing.sh and the batcher handle forge writes' >&2; exit 1");
        env.set("GIT_TERMINAL_PROMPT", "0");
        env.set("GIT_ASKPASS", "/bin/false");
        // THE BUILD CACHE (sp-z61hj; spira-config/DESIGN-build-cache.md): the agent's cargo
        // compiles through the box's one sccache, so a worktree never builds its dependencies
        // cold. A session is not a build: absent sccache falls back — loudly — to uncached.
        let path = env.child().get("PATH").cloned().unwrap_or_default();
        let setting = env.child().get(spira_config::build::CACHE_ENV).cloned();
        //
        // THE COMPILE POOL (sp-f4ig1; gate/DESIGN-admission.md §3.3): the same compiler, fronted
        // by `spira-admit`, so each cargo the agent starts takes a host-wide compile slot and
        // N agents never compile all at once. Absent spira-admit (an older release) falls back,
        // loudly, to the plain wrapper: a scheduling tool never stops a session.
        let admit = spira_config::build::find_on(&path, spira_config::admission::BIN);
        let run_dir = self.conf.run.display().to_string();
        // THE SHARED STORE (sp-xtdqi): `SPIRA_SCCACHE_DAV_ADDR`, resolved in-process into
        // `self.conf` (`conf::merge_resolved_config`) the same as every other
        // `spira.toml`-only key — never a bare env read, which only ever saw it when an
        // operator's own shell had happened to export it first.
        let store = spira_config::build::Store::from_values(|k| {
            let v = self.conf.s(k);
            (!v.is_empty()).then_some(v)
        });
        match spira_config::build::wrapper(&path, setting.as_deref()) {
            Ok(w) => {
                let vars = match &admit {
                    Some(a) => w.admitted_env(a, &run_dir, &bead, store.as_ref()),
                    None => {
                        self.log(&format!("{}: spira-admit not on PATH — this session's builds are not admitted", self.f()));
                        w.env(store.as_ref())
                    }
                };
                for (k, v) in vars {
                    env.set(&k, &v);
                }
                if w == spira_config::build::Wrapper::Off {
                    self.log(&format!("{}: {}", self.f(), w.describe()));
                }
            }
            Err(e) => self.log(&format!("{}: {e} — this session's builds are UNCACHED", self.f())),
        }
        // THE SUMMON JITTER (sp-f4ig1 §3.4): aeons a pass summoned together start apart.
        let max = self.conf.i(spira_config::admission::JITTER_ENV).max(0) as u64;
        let seed = ((std::process::id() as u64) << 32)
            ^ std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos() as u64).unwrap_or(0);
        let j = spira_config::admission::jitter(max, seed);
        if j > 0 {
            self.log(&format!("{}: summon jitter {j}s", self.f()));
            for _ in 0..j {
                self.check_stop()?;
                (self.d.sleep)(Duration::from_secs(1));
            }
            self.check_stop()?;
        }
        let sys_file = self.run_dir().join(format!("{bead}.system.md"));
        let task_file = self.run_dir().join(format!("{bead}.task.md"));
        let mut argv = self.claude_argv(&sys_file);
        argv.extend(self.checkpoint_args(work, &task_file));
        self.s.session_started = true;
        let agent = self.agent_bin();
        let child = env.child();
        let (prog, args, spec_env) = {
            // The model runs bound to this bead under the restricted environment (design
            // §3.5; replaces the work-env.sh wrapper process, sp-zpaq0): same allow-list,
            // now applied in-process rather than through a subprocess and `env -i`.
            self.s.lc_model_restricted = true;
            // sp-zf4q3: the model's PATH names the release's model-bin/ (only `work`), never
            // the bin/ that holds `work` beside ~40 tools that call bd. Fail-closed.
            let path = child.get("PATH").cloned().unwrap_or_default();
            let model_bin = match restrict::model_bin_dir(&child, &path, |p| is_executable(p)) {
                Ok(d) => d,
                Err(e) => return Err(Abort::Die(format!("work-env: {e}"))),
            };
            (agent, argv, restrict::restricted_env(&bead, &child, &model_bin))
        };
        let spec = SessionSpec { prog, args, stdin_file: task_file, log: logf, cwd: work.to_path_buf(), env: spec_env, timeout: self.fayth.timeout_seconds, halted: Some(self.run_dir().join("world.halted")) };
        let rc = self.d.launcher.run(&spec, &self.stop);
        // Interrupted: bash's trap ran before `SESSION_RC=$rc`, so SESSION_RC stays 0.
        self.check_stop()?;
        self.s.session_rc = rc;
        self.log(&format!("{}: {bead} session exited rc={rc}", self.f()));
        Ok(())
    }

    /// The session-id arguments for this launch. A checkpoint left by a world stop is
    /// resumed (the task file becomes the message that the world was down); one that cannot
    /// be resumed falls back to a cold claim of the kept branch, and the bead note says why.
    fn checkpoint_args(&mut self, work: &Path, task_file: &Path) -> Vec<String> {
        let bead = self.s.bead.clone();
        let who = self.s.aeon.clone();
        let facts = self.d.exec.exec("spira-lc", &s(&["facts", "--ids", &bead, "--kinds", &format!("{},{}", checkpoint::SESSION, checkpoint::CHECKPOINTED)]), None, None);
        let facts = if facts.success() { checkpoint::parse_facts(&facts.stdout) } else { Vec::new() };
        let persona = checkpoint::persona_hash(&std::fs::read(self.home().join("chamber").join(format!("{}.md", self.f()))).unwrap_or_default());
        let cfg = checkpoint::config_dir(&self.d.env.child());
        let plan = checkpoint::plan(&facts, self.f(), &persona, work.is_dir(), |id| checkpoint::transcript_exists(&cfg, id));
        let (id, args) = match plan {
            checkpoint::Plan::Resume { session, stopped_at } => {
                let msg = checkpoint::resume_message(stopped_at, self.now(), &self.s.branch);
                let _ = std::fs::write(task_file, msg);
                self.log(&format!("{}: {bead} resumes session {} after a world stop", self.f(), session.id));
                let args = s(&["--resume", &session.id]);
                (session.id, args)
            }
            plan => {
                if let checkpoint::Plan::Cold { reason } = plan {
                    self.log(&format!("{}: {bead} checkpoint not resumable — claiming cold: {reason}", self.f()));
                    self.note(&format!("Checkpoint not resumed: {reason}. This aeon claimed the kept branch cold, from the bead and its notes, with no attempt charged for the stop."));
                }
                let id = checkpoint::new_session_id();
                let args = s(&["--session-id", &id]);
                (id, args)
            }
        };
        let fact = checkpoint::SessionFact { id: id.clone(), fayth: self.f().to_string(), persona, worktree: work.display().to_string() };
        let wrote = self.d.exec.exec("spira-lc", &s(&["fact", &bead, "--kind", checkpoint::SESSION, "--actor", &who, "--cause", &fact.cause()]), None, None);
        if !wrote.success() {
            self.log(&format!("{}: {bead} could not record session {id} (rc={}): a stop will not be checkpointed", self.f(), wrote.code));
            self.s.session_id.clear();
        } else {
            self.s.session_id = id;
        }
        args
    }

    /// A slain session whose world is halted is held, not abandoned: the bead leaves WORKING
    /// but stays unclaimable until `world start` lifts the checkpoint hold. False when the
    /// session was never recorded or the hold was refused (the plain release then stands).
    pub(crate) fn checkpoint_across_stop(&self) -> bool {
        let id = &self.s.bead;
        if self.s.session_id.is_empty() || !self.run_dir().join("world.halted").is_file() {
            return false;
        }
        let now = self.now();
        let reason = spira_config::lc_state::checkpoint_reason(now + checkpoint::HOLD_SECS);
        let held = self.d.exec.exec("spira-lc", &s(&["hold", id, "wait", &reason, "aeon"]), None, None);
        if !held.success() {
            self.log(&format!("{}: {id} checkpoint hold refused (rc={}): {}", self.f(), held.code, held.stdout.trim()));
            return false;
        }
        let wrote = self.d.exec.exec("spira-lc", &s(&["fact", id, "--kind", checkpoint::CHECKPOINTED, "--actor", &self.s.aeon, "--cause", &now.to_string()]), None, None);
        if !wrote.success() {
            let _ = self.d.exec.exec("spira-lc", &s(&["unhold", id, "wait", "aeon"]), None, None);
            self.log(&format!("{}: {id} checkpoint fact refused (rc={}): hold withdrawn", self.f(), wrote.code));
            return false;
        }
        self.note(&format!("Checkpointed across a world stop: session {} is kept with branch {} and its worktree; `world start` lifts the hold and the next claim resumes it. No attempt charged.", self.s.session_id, self.s.branch));
        true
    }

    fn start_heartbeat<'s>(&self, sc: &'s std::thread::Scope<'s, '_>)
    where
        'a: 's,
    {
        let hb = Heartbeat {
            bead: self.s.bead.clone(),
            fayth: self.fayth.name.clone(),
            run: self.conf.run.clone(),
            lease_s: self.fayth.lease_seconds(),
            every: Duration::from_secs(self.fayth.heartbeat_seconds.max(1)),
            wall_min: self.conf.i("SPIRA_THRASH_MINUTES"),
        };
        let beat = RealBeat {
            logf: self.s.logf.clone().unwrap_or_default(),
            bead: self.s.bead.clone(),
            work: self.run_dir().join("worktree").join(&self.s.bead),
            run: self.conf.run.clone(),
            repo_name: self.s.repo_name.clone(),
            mark: self.conf.trace_mark(),
            seam: self.d.seam,
            git: self.d.git,
            bd: self.d.bd,
            exec: self.d.exec,
            holder: self.s.holder.clone(),
            renew_rc: std::sync::Mutex::new(0),
            sink: self.d.sink,
            clock: self.d.clock,
            repos: self.conf.repos.clone(),
        };
        let stop = Arc::clone(&self.stop);
        let shutdown = Arc::clone(&self.hb_shutdown);
        let done = Arc::clone(&self.hb_done);
        sc.spawn(move || {
            hb.run(&beat, &stop, &shutdown);
            done.store(true, std::sync::atomic::Ordering::SeqCst);
        });
    }
}

/// The heartbeat's view of the world.
pub struct RealBeat<'a> {
    pub logf: PathBuf,
    pub bead: String,
    pub work: PathBuf,
    pub run: PathBuf,
    pub repo_name: String,
    pub mark: String,
    pub seam: &'a dyn Seam,
    pub git: &'a dyn Git,
    pub bd: &'a dyn Bd,
    /// `spira-lc renew`'s runner, found by name like every other spira-lc call here.
    pub exec: &'a dyn Exec,
    /// The claim's holder — the name `lc_claim` claimed under (`aeon-<name>`).
    pub holder: String,
    /// The last renewal's exit, so a refusal is logged once per change, not every beat.
    pub renew_rc: std::sync::Mutex<i32>,
    pub sink: &'a dyn Sink,
    pub clock: &'a (dyn Fn() -> i64 + Sync),
    /// spira_config::repos (sp-o88bx, "wave 4.12"): the heartbeat's base-ref read
    /// (family W, spira_landref) in-process, no longer through the seam. Owned, not
    /// borrowed: the heartbeat thread outlives `start_heartbeat`'s own `&self`, and
    /// `Registry` is cheap to clone (a repo-map's rows, a handful of strings).
    pub repos: spira_config::repos::Registry,
}

impl Beat for RealBeat<'_> {
    fn now(&self) -> i64 {
        (self.clock)()
    }
    fn trace_mtime(&self) -> i64 {
        session::mtime(&self.logf)
    }
    fn fuse(&self) -> String {
        // `base` (family W, base refs): spira_config::repos in-process (sp-o88bx, "wave
        // 4.12"), not the spira_landref seam.
        let base: Option<String> =
            if self.repo_name.is_empty() { None } else { spira_config::repos::landref(&self.repos, &self.repo_name) };
        let commit_ahead_ts = base.as_deref().and_then(|b| {
            let o = self.git.git(&self.work, &["log", "--format=%ct", "-1", &format!("{b}..HEAD")]);
            o.success().then(|| o.text()).filter(|t| !t.is_empty()).and_then(|t| t.parse::<i64>().ok())
        });
        trace::aeon_fuse_minutes(&self.bead, &self.work, &self.run, commit_ahead_ts, (self.clock)())
    }
    fn trace_last(&self, n: usize) -> String {
        let t = trace::trace_last(&self.logf, &self.mark);
        let b = t.as_bytes();
        String::from_utf8_lossy(&b[..b.len().min(n)]).into_owned()
    }
    fn renew(&self, lease_until: i64) -> bool {
        // The lifecycle row's lease (sp-2jf0a); there is no bd claim to `bd heartbeat`. Never ends the heartbeat — its
        // lapse and thrash guards must keep watching the session whatever the machine says.
        // A refusal (reaped and handed on, already submitted, the machine unreachable) is
        // logged once per change; a holder that stops renewing simply expires.
        let o = self.exec.exec("spira-lc", &s(&["renew", &self.bead, &self.holder, &lease_until.to_string()]), None, None);
        let mut last = self.renew_rc.lock().unwrap_or_else(|e| e.into_inner());
        if o.code != *last {
            if o.code != 0 {
                self.log(&format!("{}: lifecycle lease renewal refused (rc={}): {}", self.bead, o.code, o.first_err_line()));
            }
            *last = o.code;
        }
        true
    }
    fn log(&self, msg: &str) {
        self.sink.out(&util::log_line((self.clock)(), msg));
    }
}

/// The fixture build: the worktree's own testdb library, in a subshell, with the lib path
/// and the fixture name on stdin. Its seven lines are the whole interface.
pub const FIXTURE_UP: &str = r#"IFS= read -r -d '' __lib || exit 96
IFS= read -r -d '' __name || exit 96
. "$__lib" && testdb_up "$__name" >&2 &&
printf '%s\n%s\n%s\n%s\n%s\n%s\n%s\n' \
    "$TESTDB_NAME" "$TESTDB_DIR" "$TESTDB_BASELINE" "${TESTDB_BIN:-}" \
    "${TESTDB_STARTED_SERVICE:-0}" "${TESTDB_MODE:-embedded}" \
    "${TESTDB_SERVER_INIT_HASH:-}"
"#;

/// The fixture drop: only a fixture this process built (TESTDB_SHARED=0 marks the owner).
pub const FIXTURE_DROP: &str = r#"IFS= read -r -d '' __lib || exit 96
TESTDB_SHARED=0
. "$__lib" && testdb_drop
"#;

pub fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

pub fn append(p: &Path, text: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
        let _ = f.write_all(text.as_bytes());
    }
}

/// `aeon_settings`: the --settings JSON, spelled as python's json.dumps spells it.
pub fn aeon_settings(home: &Path) -> String {
    let q = |p: &Path| serde_json::to_string(&p.display().to_string()).unwrap();
    let hook = |p: &Path| format!("{{\"type\": \"command\", \"command\": {}, \"timeout\": 5}}", q(p));
    let mail = home.join("hooks").join("aeon-mail-deliver.sh");
    let deliver = home.join("bd-unacked-comment-deliver.sh");
    let mut post = vec![hook(&mail)];
    if is_executable(&deliver) {
        post.push(hook(&deliver));
    }
    let mut hooks = format!("\"PostToolUse\": [{{\"hooks\": [{}]}}]", post.join(", "));
    let mut pre = Vec::new();
    for p in [
        home.join("hooks").join("aeon-fence.sh"),
        home.join("bd-close-unacked-guard.sh"),
        home.join("script-running-guard.sh"),
    ] {
        if is_executable(&p) {
            pre.push(hook(&p));
        }
    }
    if !pre.is_empty() {
        hooks.push_str(&format!(", \"PreToolUse\": [{{\"hooks\": [{}]}}]", pre.join(", ")));
    }
    format!(
        "{{\"hooks\": {{{hooks}}}, \"env\": {{\"BASH_DEFAULT_TIMEOUT_MS\": \"1800000\", \"BASH_MAX_TIMEOUT_MS\": \"3600000\", \"CLAUDE_CODE_DISABLE_BACKGROUND_TASKS\": \"1\"}}}}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // test-aeon-settings-guard-allowlist.sh / test-aeon-launch-grammar.sh.
    #[test]
    fn settings_json_matches_python_dumps() {
        let d = testkit::TempDir::new("aeon-settings");
        std::fs::create_dir_all(d.join("hooks")).unwrap();
        let s0 = aeon_settings(&d);
        assert_eq!(
            s0,
            format!(
                "{{\"hooks\": {{\"PostToolUse\": [{{\"hooks\": [{{\"type\": \"command\", \"command\": \"{}/hooks/aeon-mail-deliver.sh\", \"timeout\": 5}}]}}]}}, \"env\": {{\"BASH_DEFAULT_TIMEOUT_MS\": \"1800000\", \"BASH_MAX_TIMEOUT_MS\": \"3600000\", \"CLAUDE_CODE_DISABLE_BACKGROUND_TASKS\": \"1\"}}}}",
                d.display()
            )
        );
        assert!(!s0.contains("aeon-fence.sh"), "no fence installed: --settings does not name it");
        let fence = d.join("hooks/aeon-fence.sh");
        // testkit::write_exe, never fs::write + set_permissions (sp-os3of).
        testkit::write_exe(&fence, "#!/bin/sh\n");
        let s1 = aeon_settings(&d);
        assert!(s1.contains("\"PreToolUse\": [{\"hooks\": [{\"type\": \"command\", \"command\": \""));
        let v: serde_json::Value = serde_json::from_str(&s1).unwrap();
        assert_eq!(v["env"]["CLAUDE_CODE_DISABLE_BACKGROUND_TASKS"], "1");
    }
}
