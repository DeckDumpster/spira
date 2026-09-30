//! The run: decline or claim, build the workspace, render the brief, run the session.
//! verdict.rs judges what the session left; teardown.rs accounts for it.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use crate::bd::{self, BeadRow};
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
    pub session_rc: i32,
    /// The verdict's `committed`; false until the verdict block ran.
    pub committed: bool,
    pub lc_model_restricted: bool,
    pub requeue_cause: Option<String>,
    pub requeue_why: String,
    pub session_epoch: i64,
    pub rebase_conflicts: String,
    pub sop_before: Option<String>,
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
    pub enforce: bool,
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
        self.sdo("_tsd_aeon_session", &s(&[&self.s.bead, self.f(), &rc.to_string(), status, &fields.render()]));
        self.sdo("_aeon_rapid_recur", &s(&[&self.s.bead, &self.conf.ledger().display().to_string()]));
    }

    fn check_stop(&self) -> Result<(), Abort> {
        match self.stop.signalled() {
            Some(sig) => Err(Abort::Signal(sig)),
            None => Ok(()),
        }
    }

    fn capacity_paused(&self) -> Option<String> {
        let o = self.d.seam.call("_aeon_capacity_paused", &[]);
        for l in o.stderr.lines() {
            self.d.sink.out(l);
        }
        (o.code == 0).then_some(o.stdout)
    }

    fn take_name(&mut self) {
        let name = self.sv("aeon_name_take", &s(&[self.f()])).text();
        let actor = format!("aeon-{name}");
        let env = self.d.env;
        env.set("SPIRA_AEON", &name);
        env.set("BEADS_ACTOR", &actor);
        env.set("GIT_AUTHOR_NAME", &actor);
        env.set("GIT_AUTHOR_EMAIL", &format!("{actor}@spira.local"));
        env.set("GIT_COMMITTER_NAME", &actor);
        env.set("GIT_COMMITTER_EMAIL", &format!("{actor}@spira.local"));
        self.s.aeon = name;
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
            if let Some(max) = self.conf.v.get("SPIRA_MAX_AEONS").and_then(|m| m.trim().parse::<i64>().ok()) {
                limit = max;
                pool = (if max > have { max - have } else { 0 }).to_string();
            }
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
        if let Some(left) = self.capacity_paused() {
            self.log(&format!("{}: the account is out of capacity for another {left}s — claiming nothing", self.f()));
            self.ledger.awake(self.now(), self.f(), "paused");
            return Some(0);
        }
        None
    }

    fn ready_args(&self) -> Vec<String> {
        let mut a = self.snap.ready_args.clone();
        a.extend(s(&["--label", &self.fayth.labels, "--exclude-label", &self.snap.claim_exclude]));
        a
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
        let o = self.d.bd.bd(&self.ready_args());
        for l in util::strip_bd_hints(&o.stdout).lines().take(10) {
            self.d.sink.out(l);
        }
        0
    }

    fn claim(&mut self) -> Result<(), i32> {
        self.take_name();
        let tries = self.conf.n("SPIRA_CLAIM_RETRIES", 3).max(1) as u32;
        let delay = Duration::from_secs(self.conf.n("SPIRA_CLAIM_RETRY_DELAY_S", 1).max(0) as u64);
        let ready = match claim::claim_retry(self.d.bd, &self.ready_args(), tries, delay) {
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
        let sel = Selector { exec: self.d.exec, seam: self.d.seam, git: self.d.git, claim_bin: &claim_bin, fayth: &self.fayth.name, scratch: &self.conf.run, pid: self.pid };
        let (ids, resumable, tier) = match sel.select(&ready) {
            Selection::ClaimError { log, ledger } => {
                self.log(&log);
                self.ledger.awake(self.now(), self.f(), &ledger);
                return Err(1);
            }
            Selection::Ranked { ids, resumable, tier } => (ids, resumable, tier),
        };
        let (claimed, logs) = claim::claim_loop(self.d.bd, &ids, &resumable, tier.as_deref(), &who, tries, delay);
        for l in logs {
            self.log(&l);
        }
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

        // Read-after-claim: the predicate and the claim are not atomic.
        if bd::show(self.d.bd, &c.id).is_some_and(|r| r.has_label("spira-poison")) {
            self.release();
            self.log(&format!("{who}: {} carries spira-poison — released immediately after claim (race with the label)", c.id));
            self.ledger_done(0, "poison-raced");
            return Err(0);
        }

        // ---- lifecycle: lifecycle_enforce alone decides (sp-74gzo) ----
        if self.enforce {
            let holder = format!("aeon-{}", self.s.aeon);
            let until = self.now() + self.fayth.lease_seconds();
            let proposal = self.stack_proposal(&c.id, &who);
            self.s.stack = proposal.stack.clone();
            let rc = self.sdo(
                "lc_claim_bead",
                &s(&[
                    &c.id,
                    &holder,
                    &until.to_string(),
                    &stack::stack_json(&proposal.stack),
                    &proposal.stack_depth.to_string(),
                    &proposal.stack_max_depth.to_string(),
                ]),
            );
            if rc != 0 {
                self.release();
                if rc == 3 {
                    self.log(&format!(
                        "{who}: {} — the lifecycle machine refused this claim (not READY/REWORK, or its stack exceeds stack_max_depth) — released",
                        c.id
                    ));
                    self.ledger_done(0, "lc-claim-refused");
                } else {
                    self.log(&format!("{who}: {} — the lifecycle machine could not be reached for this claim — released", c.id));
                    self.ledger_done(0, "lc-claim-unreachable");
                }
                return Err(0);
            }
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
        self.s.repo_name = if c.repo.is_empty() { self.sv("spira_home_repo", &[]).text() } else { c.repo.clone() };
        let root = self.sv("repo_root", &s(&[&self.s.repo_name]));
        let repo = PathBuf::from(root.text());
        if !root.success() || !repo.join(".git").exists() {
            self.log(&format!("{}: {} names repo:{}, which repo-map does not resolve to a checkout", self.f(), c.id, self.s.repo_name));
            self.sdo("park_unmapped", &s(&[&c.id, &self.s.repo_name]));
            self.ledger_done(1, "unmapped-repo");
            // DESIGN.md §8.2: the world stopped for this bead is started again even here.
            self.restore_world();
            return Err(1);
        }
        self.s.repo = repo;
        self.s.repo_land = self.sv("repo_land", &s(&[&self.s.repo_name])).text();
        self.d.env.set("SPIRA_INCIDENT_REPO", &self.s.repo_name);
        self.log(&format!("{}: {} works repo:{} at {} (land={})", self.f(), c.id, self.s.repo_name, self.s.repo.display(), self.s.repo_land));

        // ---- the stacked base: a conflict here is a claim refusal, not a work-session
        // failure, so it is decided now, before work() ever arms the session (an unresolvable
        // base ref itself is left to work()'s own base block below, which refuses it exactly
        // as it always has — this check only ever fires when there is a stack to merge).
        if !self.s.stack.is_empty() {
            let b = self.sv("_aeon_base", &s(&[&self.s.repo.display().to_string()]));
            let base = b.stdout.lines().next().unwrap_or("").to_string();
            if b.success() && !base.is_empty() {
                let mut base_fq = self.sv("qualify_base_ref", &s(&[&base, &self.s.repo.display().to_string()])).text();
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
        let b = self.sv("_aeon_base", &s(&[&self.s.repo.display().to_string()]));
        if !b.success() {
            self.log(&format!("{}: {bead} names repo:{}, whose land ref cannot be resolved", self.f(), self.s.repo_name));
            let map = self.conf.s("SPIRA_REPO_MAP");
            self.note(&format!("Released by aeon.sh: repo:{} has no resolvable default branch — {map} declares no `base` for it, its remote publishes no HEAD, and it is not a local-only repository. Give it a base column. Refusing to guess: a branch cut from a guessed base rebases onto a ref nobody chose, and `main` is a guess that is wrong wherever a repository still uses `master`.", self.s.repo_name));
            return Err(Abort::Exit(1));
        }
        let mut lines = b.stdout.lines();
        self.s.base = lines.next().unwrap_or("").to_string();
        self.s.base_branch = lines.next().unwrap_or("").to_string();
        self.s.base_remote = lines.next().unwrap_or("").to_string();
        if !self.s.base_remote.is_empty() && !self.d.git.git(&self.s.repo, &["fetch", "-q", &self.s.base_remote]).success() {
            self.log(&format!("{}: fetch of {} failed — basing on a possibly stale {}", self.f(), self.s.base_remote, self.s.base));
        }
        if self.s.stack.is_empty() {
            self.s.base_fq = self.sv("qualify_base_ref", &s(&[&self.s.base, &self.s.repo.display().to_string()])).text();
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
        self.write_prompt(&work, fixture_ms, &resume, &slain, &rebase_text, &dirty);

        // ---- the closing rule's shelf, and the groom log, before ----
        self.s.session_epoch = self.now();
        self.d.env.set("SESSION_EPOCH", &self.s.session_epoch.to_string());
        if self.fayth.sop_required {
            let sop = "sop.sh";
            if !self.d.exec.exec(sop, &s(&["ledger-init"]), None, None).success() {
                self.log(&format!("{}: could not create the SOP applications ledger — the closing rule cannot be judged this run", self.f()));
            }
            let dg = self.d.exec.exec(sop, &s(&["digest"]), None, None);
            if dg.success() {
                self.s.sop_before = Some(dg.text());
            } else {
                self.log(&format!("{}: could not read the SOP shelf before the session — the closing rule cannot be judged this run", self.f()));
            }
        }
        if self.fayth.groom_escalation_check {
            self.s.groom_lines_before = std::fs::read_to_string(self.run_dir().join("groom.log")).map(|t| t.lines().count()).unwrap_or(0);
        }
        self.check_stop()?;

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

    fn write_prompt(&mut self, work: &Path, fixture_ms: u64, resume: &str, slain: &str, rebase: &str, dirty: &[String]) {
        let bead = self.s.bead.clone();
        let wdisp = work.display().to_string();
        let overlay = self.conf.overlay();
        let db = self.conf.db();
        let landing = brief::landing_brief(&self.s.repo_land, &self.s.branch, &self.s.base_branch, &self.s.repo_name, &self.s.base);
        let park = brief::park_brief(&self.s.repo_land, &self.s.branch, &self.s.repo_name, &self.s.base_branch);
        let finish = brief::finish_brief(self.enforce, &bead, &db);
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
        let keep = self.conf.n("SPIRA_BRIEF_KEEP_RECURRENCES", 5).max(0) as usize;
        let max = self.conf.n("SPIRA_BRIEF_NOTES_MAX_CHARS", 8000).max(0) as usize;
        let mut body = brief::bound_bead_notes(&format!("{body}\n"), keep, max).trim_end_matches('\n').to_string();
        let paths: Vec<String> = self.sv("bead_named_paths", &s(&[&body, &self.s.repo.display().to_string()])).stdout.lines().filter(|l| !l.is_empty()).map(String::from).collect();
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
        let home_repo = self.sv("spira_home_repo", &[]).text();
        let tokens = Tokens {
            single: vec![
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
                ("SOP", "sop.sh".into()),
                ("INCIDENT", "incident.sh".into()),
                ("ASK", "mail.sh".into()),
                ("SUITES", format!("{} suites", brief::TESTENV)),
                ("TESTENV", brief::TESTENV.into()),
                ("FOLLOWUP", brief::followup_brief(self.enforce, &bead, &self.s.repo_name)),
                ("GROOM", "groomer".into()),
                ("DEP", "bead.sh dep add".into()),
                ("SPIRA_HOME", home.clone()),
                ("RUN", self.run_dir().display().to_string()),
                ("MAX_BEADS", self.conf.s("SPIRA_MAECHEN_MAX_BEADS")),
                ("REMEDY_LABEL", self.conf.s("SPIRA_MAECHEN_REMEDY_LABEL")),
                ("SCOPE", if scope.is_empty() { String::new() } else { format!("{scope},") }),
            ],
            bead: body,
            park: blocks[0].clone(),
            fixture: blocks[1].clone(),
            deadline: blocks[2].clone(),
            finish: blocks[3].clone(),
        };
        let prompt = brief::render_prompt(&chamber, &tokens, banner.as_deref());
        let statutes = self.statutes();
        let (sys, mut task) = brief::split(&statutes, &prompt);
        task.push_str(&brief::dirty_brief(dirty));
        task.push_str(resume);
        task.push_str(slain);
        task.push_str(&brief::already_done_brief(&self.s.base, &bead, &wdisp, &db));
        task.push_str(&brief::close_brief(&self.s.base, &wdisp, &self.s.base_remote));
        task.push_str(rebase);
        let _ = std::fs::write(self.run_dir().join(format!("{bead}.system.md")), sys);
        let _ = std::fs::write(self.run_dir().join(format!("{bead}.task.md")), task);
    }

    /// `render_memories "${FAYTH_MEMORY_PREFIXES:-law-}" "" "${FAYTH_STATUTE_CORE:-}"`.
    pub fn statutes(&self) -> String {
        let cache = self.conf.s("SPIRA_MEMORIES_CACHE");
        let age = self.conf.n("SPIRA_MEMORIES_CACHE_AGE", 300);
        let mut json = String::new();
        if !cache.is_empty() && Path::new(&cache).is_file() {
            let m = session::mtime(Path::new(&cache));
            if self.now() - m < age {
                json = std::fs::read_to_string(&cache).unwrap_or_default();
            }
        }
        if json.trim().is_empty() {
            let cmd = self.conf.s("SPIRA_MEMORIES_CMD");
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
        let harness = self.conf.or("SPIRA_REPO", "<harness>");
        brief::render_memories(&json, &self.fayth.memory_prefixes, 120_000, &core, &harness)
    }

    /// `aeon_claude_argv <flag> <file>`.
    pub fn claude_argv(&self, sys_file: &Path) -> Vec<String> {
        let flag = match self.fayth.system_prompt {
            SystemPrompt::Replace => "--system-prompt-file",
            SystemPrompt::Append => "--append-system-prompt-file",
        };
        let toml = self.conf.s("SPIRA_TOML_FILE");
        let model = conf::persona_model(self.f(), (!toml.is_empty()).then(|| Path::new(&toml)));
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
        match spira_config::build::wrapper(&path, setting.as_deref()) {
            Ok(w) => {
                let vars = match &admit {
                    Some(a) => w.admitted_env(a, &run_dir, &bead),
                    None => {
                        self.log(&format!("{}: spira-admit not on PATH — this session's builds are not admitted", self.f()));
                        w.env()
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
        let max = self.conf.n(spira_config::admission::JITTER_ENV, spira_config::admission::JITTER_DEFAULT as i64).max(0) as u64;
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
        let argv = self.claude_argv(&sys_file);
        self.s.session_started = true;
        let agent = self.agent_bin();
        let child = env.child();
        let (prog, args, spec_env) = if self.enforce {
            // The model runs bound to this bead under the restricted environment (design
            // §3.5; replaces the work-env.sh wrapper process, sp-zpaq0): same allow-list,
            // now applied in-process rather than through a subprocess and `env -i`.
            self.s.lc_model_restricted = true;
            let path = child.get("PATH").cloned().unwrap_or_default();
            let Some(work_dir) = restrict::work_bin_dir(&path, |p| is_executable(p)) else {
                return Err(Abort::Die("work-env: work is not on PATH — the launcher sets PATH to a release".to_string()));
            };
            (agent, argv, restrict::restricted_env(&bead, &child, &work_dir))
        } else {
            (agent, argv, child)
        };
        let spec = SessionSpec { prog, args, stdin_file: task_file, log: logf, cwd: work.to_path_buf(), env: spec_env, timeout: self.fayth.timeout_seconds };
        let rc = self.d.launcher.run(&spec, &self.stop);
        // Interrupted: bash's trap ran before `SESSION_RC=$rc`, so SESSION_RC stays 0.
        self.check_stop()?;
        self.s.session_rc = rc;
        self.log(&format!("{}: {bead} session exited rc={rc}", self.f()));
        Ok(())
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
            wall_min: self.conf.n("SPIRA_THRASH_MINUTES", 20),
        };
        let beat = RealBeat {
            logf: self.s.logf.clone().unwrap_or_default(),
            bead: self.s.bead.clone(),
            work: self.run_dir().join("worktree").join(&self.s.bead),
            repo_name: self.s.repo_name.clone(),
            seam: self.d.seam,
            bd: self.d.bd,
            sink: self.d.sink,
            clock: self.d.clock,
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
    pub repo_name: String,
    pub seam: &'a dyn Seam,
    pub bd: &'a dyn Bd,
    pub sink: &'a dyn Sink,
    pub clock: &'a (dyn Fn() -> i64 + Sync),
}

impl Beat for RealBeat<'_> {
    fn now(&self) -> i64 {
        (self.clock)()
    }
    fn trace_mtime(&self) -> i64 {
        session::mtime(&self.logf)
    }
    fn fuse(&self) -> String {
        self.seam.call("aeon_fuse_minutes", &s(&[&self.bead, &self.work.display().to_string(), &self.repo_name])).text()
    }
    fn trace_last(&self, n: usize) -> String {
        let t = self.seam.call("trace_last", &s(&[&self.logf.display().to_string()])).stdout;
        let b = t.as_bytes();
        String::from_utf8_lossy(&b[..b.len().min(n)]).into_owned()
    }
    fn bd_heartbeat(&self) -> bool {
        self.bd.bd(&s(&["heartbeat", &self.bead])).success()
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
    for p in [home.join("hooks").join("aeon-fence.sh"), home.join("bd-close-unacked-guard.sh")] {
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
        use std::os::unix::fs::PermissionsExt;
        let fence = d.join("hooks/aeon-fence.sh");
        std::fs::write(&fence, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&fence, std::fs::Permissions::from_mode(0o755)).unwrap();
        let s1 = aeon_settings(&d);
        assert!(s1.contains("\"PreToolUse\": [{\"hooks\": [{\"type\": \"command\", \"command\": \""));
        let v: serde_json::Value = serde_json::from_str(&s1).unwrap();
        assert_eq!(v["env"]["CLAUDE_CODE_DISABLE_BACKGROUND_TASKS"], "1");
    }
}
