//! The teardown (dwarf.sh's `cleanup`, DESIGN.md §4.3-4.4): runs once, on every exit path
//! after the claim. Charging is default-deny — `decide::disposition` holds the precedence;
//! this module gathers its inputs lazily (a marker a higher row would match is never
//! consumed early) and performs the side effects the verdict names.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::bd;
use crate::decide::{self, DispositionIn, NoteKey};
use crate::ledger;
use crate::ports::s;
use crate::run::{Run, FIXTURE_DROP};
use crate::util;

/// Two refused submits from different beads inside this many seconds are one page.
const SUBMIT_BLOCKED_WINDOW_S: i64 = 600;
const SUBMIT_REFUSED: &str = "SUBMIT REFUSED";
const SUBMIT_BLOCKED: &str = "SUBMIT BLOCKED";

/// The dwarf's own last word when it names a refused `work submit`: its first line.
fn submit_refusal(reason: &str) -> Option<&str> {
    let first = reason.trim().lines().next()?.trim();
    let low = first.to_lowercase();
    (low.contains("work submit") && low.contains("refus")).then_some(first)
}

/// The beads, other than `bead`, with a refused-submit line stamped at or after `since` in the
/// inbox text; ISO UTC stamps of one width order as strings.
fn others_refused(inbox: &str, since: &str, bead: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in inbox.lines() {
        let Some((stamp, rest)) = line.split_once(' ') else { continue };
        let Some(who) = rest.split_once(SUBMIT_REFUSED).and_then(|(_, r)| r.trim().split(':').next()) else { continue };
        if stamp >= since && who != bead && !who.is_empty() && !out.iter().any(|o| o == who) {
            out.push(who.to_string());
        }
    }
    out
}

fn remove_path(p: &Path) -> bool {
    match p.symlink_metadata() {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p).is_ok(),
        Ok(_) => std::fs::remove_file(p).is_ok(),
        Err(_) => true,
    }
}

/// Removes `p`; a staged release is read-only on purpose, so a first failure makes the
/// directories inside writable and tries once more.
fn remove_tree(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fn open_up(p: &Path) {
        let Ok(m) = p.symlink_metadata() else { return };
        if m.is_dir() {
            let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(m.permissions().mode() | 0o700));
            if let Ok(rd) = std::fs::read_dir(p) {
                rd.flatten().for_each(|e| open_up(&e.path()));
            }
        }
    }
    remove_path(p) || {
        open_up(p);
        remove_path(p)
    }
}

/// Removes `dir` and nothing beside it; returns the bytes it held.
pub fn remove_session_tmpdir(dir: &Path) -> u64 {
    let bytes = runtime::usage::dir_bytes(dir);
    if remove_tree(dir) {
        bytes
    } else {
        0
    }
}

/// What became of the fast-tier output a reopen note refers to.
enum Kept {
    Entry(String),
    Failed(String),
}

impl std::fmt::Display for Kept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Kept::Entry(id) => write!(f, "Full output: work logs {id} fast-tier.out"),
            Kept::Failed(e) => write!(f, "full output not kept: {e}"),
        }
    }
}

impl Run<'_> {
    /// A refused `work submit` ends a session with nothing the dwarf can do: say so in the inbox
    /// (one line for the bead), and page once when a second bead says it inside the window.
    pub(crate) fn report_refused_submit(&self, bead: &str, reason: &str) {
        let Some(refusal) = submit_refusal(reason) else { return };
        let now = self.now();
        let inbox = self.conf.s("SPIRA_CONCIERGE_INBOX");
        let append = |line: String| {
            let o = self.d.exec.exec("inbox-triage", &s(&["append", &format!("[dwarf] {line}")]), None, None);
            if !o.success() {
                self.log(&format!("{}: inbox-triage append refused (rc={}): {}", self.f(), o.code, o.first_err_line()));
            }
        };
        let since = util::iso_utc(now - SUBMIT_BLOCKED_WINDOW_S);
        append(format!("{SUBMIT_REFUSED} {bead}: {refusal}"));
        let seen = std::fs::read_to_string(&inbox).unwrap_or_default();
        let others = others_refused(&seen, &since, bead);
        let paged = seen.lines().any(|l| l.contains(SUBMIT_BLOCKED) && l.split(' ').next().is_some_and(|st| st >= since.as_str()));
        if !others.is_empty() && !paged {
            append(format!("P0 {SUBMIT_BLOCKED}: work submit refused for {bead} and {} within {} minutes: {refusal}", others.join(", "), SUBMIT_BLOCKED_WINDOW_S / 60));
        }
    }

    /// The session's `TMPDIR`: inside its worktree, where the worktree reap already cleans.
    pub(crate) fn session_tmpdir(work: &Path) -> PathBuf {
        work.join(spira_config::RUNTIME_DIR).join(format!("{}{}", spira_config::SESSION_TMP_PREFIX, std::process::id()))
    }

    pub fn make_session_tmpdir(work: &Path) -> Result<PathBuf, (PathBuf, std::io::Error)> {
        let dir = Self::session_tmpdir(work);
        let ignore = work.join(spira_config::RUNTIME_DIR).join(".gitignore");
        std::fs::create_dir_all(&dir)
            .and_then(|()| if ignore.exists() { Ok(()) } else { std::fs::write(&ignore, "*\n") })
            .map(|()| dir.clone())
            .map_err(|e| (dir, e))
    }

    pub(crate) fn export_session_tmpdir(&self, work: &Path) {
        match Self::make_session_tmpdir(work) {
            Ok(tmp) => self.d.env.set("TMPDIR", &tmp.display().to_string()),
            Err((tmp, e)) => self.log(&format!("{}: session TMPDIR {} not created ({e}); scratch falls to the inherited temp root", self.s.bead, tmp.display())),
        }
    }

    pub(crate) fn remove_tmpdir(&self) {
        let Some(work) = self.s.work.as_deref() else { return };
        let dir = Self::session_tmpdir(work);
        if !dir.is_dir() {
            return;
        }
        let bytes = remove_session_tmpdir(&dir);
        self.log(&format!("{}: removed session TMPDIR {} ({} MiB)", self.f(), dir.display(), bytes >> 20));
    }

    fn stop_heartbeat(&self) {
        self.hb_shutdown.store(true, Ordering::SeqCst);
        let t = Instant::now();
        while !self.hb_done.load(Ordering::SeqCst) && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Only a fixture this process built is dropped (a borrower must never drop a lender's).
    fn fixture_drop(&self) {
        let (Some(lib), Some(_)) = (self.s.fixture_lib.as_ref(), self.s.fixture.as_ref()) else { return };
        let lib = if lib.is_file() { lib.clone() } else { self.home().join("testdb.sh") };
        if !lib.is_file() {
            return;
        }
        let mut data = lib.display().to_string().into_bytes();
        data.push(0);
        let _ = self.d.exec.exec("bash", &s(&["-c", FIXTURE_DROP]), Some(data), None);
    }

    pub(crate) fn ask_blocked(&self, id: &str) -> bool {
        let row = bd::json(self.d.bd, &["show", id]);
        let ids = decide::blocks_targets(&row);
        if ids.is_empty() {
            return false;
        }
        let targets = util::json_only(&self.d.bd.bd(&s(&["list", "--all", "--limit", "0", "--id", &ids.join(","), "--json"])).stdout);
        decide::open_ask_blocker(&row, &targets, id, &self.conf.ask_label())
    }

    /// `gate-run.sh --status <branch> <repo>`: (exit code, stdout), by name on the
    /// launcher's PATH (sp-gypjk). A suite fixture scripts it by putting its own
    /// `gate-run.sh` first on PATH.
    fn gate_status(&self) -> Option<(i32, String)> {
        let o = self.d.exec.exec(
            "gate-run.sh",
            &s(&["--status", &self.s.branch, &self.s.repo_name]),
            None,
            None,
        );
        Some((o.code, o.text()))
    }

    /// The in-session fast tier (`fast_tier::red`); a tool that is absent (127) is skipped,
    /// never a red.
    fn fast_tier_red(&self) -> Option<crate::fast_tier::Red> {
        let work = self.s.work.as_deref()?;
        crate::fast_tier::red(self.d.git, self.d.exec, &self.s.repo, work, &self.s.branch, &self.s.base_fq, false)
    }

    fn head_tip(&self) -> String {
        let work = self.s.work.as_deref().unwrap_or(Path::new(system_paths::DEV_NULL));
        let o = self.d.git.git(work, &["rev-parse", "HEAD"]);
        if o.success() { o.text().trim().to_string() } else { String::new() }
    }

    /// The previous fast-tier red's digest when it judged this very tip: a retry must change an
    /// input, and a harness fault records no such fact.
    fn unchanged_tip_red(&self) -> Option<String> {
        let tip = self.head_tip();
        let recorded = self.last_fast_tier_red();
        let (digest, judged) = crate::fast_tier::split_tip(&recorded);
        (!tip.is_empty() && judged == Some(tip.as_str())).then(|| digest.to_string())
    }

    /// Keeps the whole fast-tier output as a `send-back-logs` entry; answers the note line that
    /// names it, or says it was not kept. A failed keep never blocks the reopen.
    fn keep_fast_tier_output(&self, red: &crate::fast_tier::Red) -> Kept {
        let lc = |args: &[String]| -> Result<String, String> {
            let o = self.d.exec.exec("spira-lc", args, None, None);
            if o.success() { Ok(o.stdout) } else { Err(o.stderr.lines().next().unwrap_or("").to_string()) }
        };
        let days = self.conf.i("SPIRA_SENDBACK_KEEP_DAYS") as u32;
        let files = [("fast-tier.out".to_string(), red.text.as_bytes().to_vec())];
        match spira_config::ejection::keep_and_reference(&lc, self.run_dir(), &[&self.s.bead], "fast-tier", &files, days) {
            Ok(id) => Kept::Entry(id),
            Err(e) => {
                self.log(&format!("{}: {} fast-tier output not kept: {e}", self.f(), self.s.bead));
                Kept::Failed(e.to_string())
            }
        }
    }

    /// Refuses the handoff when the fast tier is red: the bead goes back to the graph with the
    /// failure text instead of waiting a certification cycle to learn it.
    fn refuse_handoff(&self, red: &crate::fast_tier::Red) {
        let br = &self.s.branch;
        let digest = crate::fast_tier::digest(&red.text);
        let kept = self.keep_fast_tier_output(red);
        if red.harness {
            self.bead_reopen("fast-tier-harness", &format!("Reopened by dwarf.sh: the in-session fast tier could not run on {br} (a tool failed, not the work) and the handoff was refused. No attempt charged.\n\n{digest}\n\n{kept}"));
            self.log(&format!("{}: {} REOPENED — fast tier harness failure, no attempt charged", self.f(), self.s.bead));
            return;
        }
        self.record_fact(crate::fast_tier::KIND, &crate::fast_tier::with_tip(&digest, &self.head_tip()));
        self.bead_reopen("fast-tier-red", &format!("Reopened by dwarf.sh: {br} failed the in-session fast tier (lint, build fence, rebase check) and was not handed to certification.\n\n{digest}\n\n{kept}"));
        self.log(&format!("{}: {} REOPENED — fast tier red, handoff refused", self.f(), self.s.bead));
        self.ledger.write(self.now(), &format!("reopened {} {} fast-tier-red", self.f(), self.s.bead));
    }

    /// The unlanded prerequisite this claim was stacked on whose certified tip changed a file
    /// the rebase conflict names: the bead waits for it instead of retrying the conflict.
    fn conflicting_blocker(&self, red: &crate::fast_tier::Red) -> Option<(String, String)> {
        let paths = crate::fast_tier::conflict_paths(&red.text);
        if paths.is_empty() {
            return None;
        }
        let repo_disp = self.s.repo.display().to_string();
        let mut main = spira_config::repos::qualify_base_ref(&self.s.base, &repo_disp);
        if main.is_empty() {
            main = self.s.base.clone();
        }
        self.s.stack.iter().find_map(|(prereq, tip)| {
            let o = self.d.git.git(&self.s.repo, &["diff", "--name-only", &format!("{main}...{tip}")]);
            let changed = o.success() && o.stdout.lines().any(|f| paths.iter().any(|p| p == f.trim()));
            changed.then(|| (prereq.clone(), tip.clone()))
        })
    }

    /// A rebase conflict with a blocker that has not landed is waiting, not a fault: the
    /// blocker's marker keeps the bead unclaimable until the blocker's tip moves or it lands,
    /// and the cause is one the attempt census does not charge.
    fn wait_for_blocker(&self, prereq: &str, tip: &str, red: &crate::fast_tier::Red) {
        spira_config::stack_conflict::record(self.run_dir(), prereq, tip);
        let digest = crate::fast_tier::digest(&red.text);
        self.bead_reopen(
            crate::fast_tier::BLOCKER_CONFLICT_CAUSE,
            &format!("Reopened by dwarf.sh: {} conflicts with files changed by its blocker {prereq}, which has not landed. The bead waits for {prereq} to land (one rebase then); no attempt charged.\n\n{digest}", self.s.branch),
        );
        self.log(&format!("{}: {} waits for blocker {prereq} — rebase conflict on its files, no attempt charged", self.f(), self.s.bead));
    }

    /// A session that ends without submitting, when the bead's last fast-tier red named a
    /// compile error and the session never ran `cargo check`, is recorded as a blind rework.
    fn note_blind_rework(&self) {
        let red = self.last_fast_tier_red();
        if !crate::fast_tier::names_compile_error(&red) {
            return;
        }
        let log = self.s.logf.as_deref().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        if log.contains("cargo check") || log.contains("build-fence") {
            return;
        }
        self.record_fact("blind-rework", "ended without submitting; never ran cargo check against the last fast-tier red");
        self.note("Blind rework: the previous closeout's fast tier named a compile error, and this session ended without submitting or ever running cargo check against it. Reproduce the error first.");
    }

    /// Commits naming this bead between the base and the branch.
    fn has_own_commit(&self) -> bool {
        let o = self.d.git.git(&self.s.repo, &["log", "--format=%s%n%b", &format!("{}..{}", self.s.base_fq, self.s.branch)]);
        o.success() && o.stdout.contains(&self.s.bead)
    }

    pub fn teardown(&mut self, rc: i32) -> i32 {
        let mut rc = rc;
        let id = self.s.bead.clone();
        let f = self.rune.name.clone();
        self.stop_heartbeat();
        let lc = self.lc_bead(&id);
        self.s.submitted_at = lc.as_ref().filter(|r| r.state == "SUBMITTED").and_then(|r| r.since).filter(|&at| at >= self.s.claimed_at.unwrap_or(0));
        if lc.as_ref().is_some_and(|r| r.working()) {
            self.phase("teardown");
        }
        self.fixture_drop();
        self.remove_tmpdir();
        let _ = std::fs::remove_file(self.run_dir().join("dwarf").join(format!("{id}.lease")));
        self.restore_world();
        let _ = std::env::set_current_dir(&self.s.repo);

        // The bead's state is its lifecycle row's, never bd's status (design §3.4, sp-mve9i):
        // `st` is the ledger's word. A session hands its bead on only through the work verbs
        // (every session runs restricted since sp-v62vn), which the disposition reads as
        // `submitted` — there is no "the model closed the bead" branch here any more.
        let st = decide::ledger_word(lc.as_ref()).to_string();
        // Operator-wait is the lifecycle record's (sp-v62vn follow-up): the model asks
        // through `work ask`/`work blocked`, whose broker places an `ask` hold on this bead's
        // row. A pool claim releases a bead already carrying a non-wait hold (run.rs
        // blocking_holds), so its ask hold was placed during this session, or raced the claim
        // unseen when the store could not tell (sp-druc64). One a directed claim inherited
        // counts only once it is raised again.
        let asked = lc.as_ref().is_some_and(|r| r.held("ask") && self.s.inherited_ask.is_none_or(|at| r.ask_since != Some(at)));
        let disposition = lc.as_ref().and_then(|r| r.disposition.as_deref());
        let disposition_note = lc.as_ref().and_then(|r| r.disposition_note.clone()).unwrap_or_default();
        let logf = self.s.logf.as_ref().map(|p| p.display().to_string()).unwrap_or_default();

        let mut i = DispositionIn { status: st.clone(), session_rc: self.s.session_rc, committed: false, session_started: self.s.session_started, harness_red: self.s.harness_red.is_some(), ..Default::default() };
        let (mut reset, mut thrash_note, mut thrash_tip, mut streak) = (String::new(), String::new(), String::new(), 0i64);
        let (mut lapsed_quiet, mut lapsed_last, mut gw) = (String::new(), String::new(), String::new());
        let mut unlanded_reason = String::new();
        let work = self.s.work.clone();
        let short_tip = |this: &Self| {
            let o = this.d.git.git(work.as_deref().unwrap_or(Path::new(system_paths::DEV_NULL)), &["rev-parse", "--short", "HEAD"]);
            if o.success() { o.text() } else { "?".into() }
        };
        // Full hash, matching session_start_tip's own format (sp-1zxru-2) — short_tip's
        // truncated form is for notes/thrash-tip identity, not this equality check.
        let full_tip = |this: &Self| {
            let o = this.d.git.git(work.as_deref().unwrap_or(Path::new(system_paths::DEV_NULL)), &["rev-parse", "HEAD"]);
            if o.success() { o.text().trim().to_string() } else { "?".into() }
        };
        let requeue_cause = self.s.requeue_cause.clone();
        // capacity_reset_at, in-process now (wave 4.26). `Some(0)` ("hit, but no
        // resetsAt") still counts as a capacity return — see capacity.rs's own doc.
        let sent_back_why = self.stop.sent_back_why();
        i.sent_back = sent_back_why.is_some();
        let cr = crate::capacity::reset_at(Path::new(&logf), &self.conf.trace_mark());
        if i.sent_back {
            // the row is already back in the queue: nothing to gather, nothing to charge
        } else if let Some(r) = cr {
            i.capacity = true;
            reset = r.to_string();
        } else if disposition == Some("slain") {
            i.slain = true;
        } else if disposition == Some("thrash") {
            i.thrash = true;
            thrash_note = disposition_note.clone();
            thrash_tip = short_tip(self);
            let n = if thrash_note.is_empty() { "?" } else { &thrash_note };
            streak = self.sv("thrash_streak_bump", &s(&[&id, &thrash_tip, n])).text().trim().parse().unwrap_or(0);
            i.thrash_charged = streak >= self.conf.i("SPIRA_THRASH_STREAK_CAP");
        } else if disposition == Some("lapsed") {
            i.lapsed = true;
            match disposition_note.split_once('\t') {
                Some((q, l)) => {
                    lapsed_quiet = q.into();
                    lapsed_last = l.into();
                }
                None => {
                    lapsed_quiet = disposition_note.clone();
                    lapsed_last = disposition_note.clone();
                }
            }
        } else if let Some((2, why)) = self.gate_status() {
            i.gate_unfinished = true;
            gw = why;
        } else if self.ask_blocked(&id) {
            i.decision_blocked = true;
        } else if self.s.session_rc == 124 && !self.s.committed {
            // timeout — decided from session_rc / committed
        } else if requeue_cause.is_some() {
            // a harness requeue — the cause is passed below
        } else if asked {
            i.operator_wait = true;
        } else if self.sv("lc_bead_verified", &s(&[&id])).success() {
            i.submitted = true;
        } else if ledger::trace_segment(self.s.logf.as_deref(), 50_000, &self.conf.trace_mark()).is_some_and(|seg| decide::session_yield_headless(&seg)) {
            i.yield_headless = true;
        } else if !self.s.session_started {
            // pre-session death
        } else {
            let seg = ledger::trace_segment(self.s.logf.as_deref(), 0, &self.conf.trace_mark());
            // The dwarf's own last word, gathered here and used only if the disposition
            // below lands on NoProgress (sp-1zxru) — never consulted for the decision
            // itself, which stays session_outcome's and committed's alone.
            unlanded_reason = decide::last_assistant_text(seg.as_deref()).unwrap_or_default();
            i.outcome = Some(decide::session_outcome(seg.as_deref()).to_string());
        }
        // SESSION_RC, committed and REQUEUE_CAUSE are passed whatever was gathered, as
        // dwarf.sh passed them; the precedence lives in decide::disposition.
        i.committed = self.s.committed;
        // sp-1zxru-2: THIS session's own tip movement — session_start_tip was read
        // right before the model's turn (run.rs::work); read again now, after it.
        i.tip_moved = decide::tip_moved(&self.s.session_start_tip, &full_tip(self));
        i.requeue_cause = requeue_cause;
        let d = decide::disposition(&i);
        let cause = d.requeue_cause.clone().unwrap_or_default();
        let status = d.ledger_status.clone();
        let thrash_minutes = self.conf.i("SPIRA_THRASH_MINUTES");
        match d.note {
            NoteKey::SentBack => {
                let why = sent_back_why.unwrap_or_default();
                self.note(&format!("Session stopped: this bead was sent back ({why}) after `work submit` while the session still ran. Branch spira/{id} is kept; anything uncommitted is left in the worktree."));
                self.log(&format!("{f}: {id} sent back while the session ran — stopped, nothing released, no attempt charged"));
                return self.finish(rc, &status);
            }
            NoteKey::Capacity => {
                let at: i64 = reset.trim().parse().unwrap_or(0);
                if let Some(line) = crate::capacity::pause_set(&self.conf.capacity_pause(), &self.conf.ledger(), self.now(), self.conf.capacity_backoff(), at, &id) {
                    self.log(&line);
                }
                self.release();
                self.bump_requeue(&cause);
                self.note("Returned unchanged by dwarf.sh: the account's capacity window was spent mid-session, so this bead was never judged. No attempt was charged and nothing about the work is implied. Calling is paused until the window reopens.");
                self.log(&format!("{f}: {id} returned unchanged — the account ran out of capacity, no attempt charged"));
                return self.finish(rc, &status);
            }
            NoteKey::Slain => {
                self.release();
                self.bump_requeue(&cause);
                if self.checkpoint_across_stop() {
                    self.log(&format!("{f}: {id} slain by a world stop — checkpointed, no attempt charged"));
                    return self.finish(rc, &status);
                }
                self.log(&format!("{f}: {id} slain — released, no attempt charged"));
                return self.finish(rc, &status);
            }
            NoteKey::ThrashCharged => {
                self.release();
                self.bump_requeue(&cause);
                let n = if thrash_note.is_empty() { "?" } else { &thrash_note };
                self.note(&format!("STICKING POINT: {n}\n\nRequeued (thrash): the deliverable did not move for {thrash_minutes}m while turns advanced, and this is the {streak}th consecutive thrash with branch {} still at {thrash_tip} — nothing has been committed since the last one. An attempt IS charged this time: the sticking point above is the next dwarf's first move, not something to rediscover by reading back through this bead's notes.", self.s.branch));
                self.log(&format!("{f}: {id} thrash-requeued — attempt charged (streak {streak}, tip {thrash_tip} unchanged; last: {n})"));
                return self.finish(rc, &status);
            }
            NoteKey::Thrash => {
                self.release();
                self.bump_requeue(&cause);
                let n = if thrash_note.is_empty() { "?" } else { &thrash_note };
                self.note(&format!("Requeued (thrash): the deliverable did not move for {thrash_minutes}m while turns advanced. Last action: {n}. No attempt charged — the next dwarf should start from this sticking point."));
                self.log(&format!("{f}: {id} thrash-requeued — no attempt charged (streak {streak}, tip {thrash_tip}; last: {n})"));
                return self.finish(rc, &status);
            }
            NoteKey::Lapsed => {
                let q = if lapsed_quiet.is_empty() { "?" } else { &lapsed_quiet };
                let l = if lapsed_last.is_empty() { "?" } else { &lapsed_last };
                self.sdo("bump_lapsed", &s(&[&id, l]));
                let tip = short_tip(self);
                self.sdo("write_lapse_record", &s(&[&id, q, l, &tip]));
                self.note(&format!("Lease lapsed: the trace was silent for {q}s (limit {}s). Last: {l}. Branch spira/{id} preserved. Attempt 2 should start from where attempt 1 wedged.", self.rune.lease_seconds()));
                self.log(&format!("{f}: {id} lease lapsed — attempt charged (quiet {q}s)"));
                self.release();
                return self.finish(rc, &status);
            }
            NoteKey::GateUnfinished => {
                self.release();
                self.bump_requeue(&cause);
                self.note(&format!("Released by dwarf.sh: the session ended while its landing gate was still running, so it never held a verdict about its own work. No attempt was charged and nothing about the work is implied — {gw}. Run the gate through gate-run.sh, which waits in bounded slices, and do not end the session while it is unfinished."));
                self.log(&format!("{f}: {id} released with its gate still running — no attempt charged ({gw})"));
                return self.finish(rc, &status);
            }
            NoteKey::DecisionBlocked => {
                self.release();
                self.bump_requeue(&cause);
                let ask = self.conf.ask_label();
                self.note(&format!("Released by dwarf.sh: blocked on an open decision bead ({ask} label) — waiting for operator input. No attempt charged; the bead becomes ready when the decision is resolved."));
                self.log(&format!("{f}: {id} has open decision blocker — released, no attempt charged"));
                return self.finish(rc, &status);
            }
            NoteKey::Timeout => {
                let t = self.rune.timeout_seconds.map(|t| t.to_string()).unwrap_or_else(|| "?".into());
                self.note(&format!("Timeout: the session was killed by the lane cap ({t}s) with nothing committed. This is the harness's clock ending the turn, not a verdict about the work. No attempt charged."));
                self.log(&format!("{f}: {id} timed out — no attempt charged"));
                self.release();
                self.bump_requeue(&cause);
                return self.finish(rc, &status);
            }
            NoteKey::Requeue => {
                self.bump_requeue(&cause);
                let n = self.sv("requeues_of", &s(&[&id])).text();
                self.note(&format!("Requeue {n} ({cause}): {} The session did the work and closed the bead; the harness put it back. NO attempt was charged and nothing about the work is implied.", self.s.requeue_why));
                self.log(&format!("{f}: {id} requeued by the harness ({cause}, count {n}) — no attempt charged"));
                self.release();
                return self.finish(rc, &status);
            }
            NoteKey::OperatorWait => {
                self.bump_requeue(&cause);
                self.note("Released by dwarf.sh: the session asked the operator a question (work ask) and exited awaiting the answer; the bead's lifecycle row carries the ask hold. No attempt charged; the bead becomes claimable when the operator's reply lifts the hold.");
                self.log(&format!("{f}: {id} operator-wait — asked the operator (ask hold on the row), released, no attempt charged"));
                self.release();
                return self.finish(rc, &status);
            }
            NoteKey::Submitted => {
                if self.has_own_commit() {
                    if let Some(prev) = self.unchanged_tip_red() {
                        let tip = self.head_tip();
                        let red = crate::fast_tier::Red { harness: false, killed_by: None, could_not_run: false, text: format!("resubmitted tip {tip} unchanged since its fast-tier red; change the tip before submitting. That red:\n{prev}") };
                        self.refuse_handoff(&red);
                        return self.finish(rc, "open");
                    }
                    if let Some(red) = self.fast_tier_red() {
                        if crate::fast_tier::certify_anyway(&red) {
                            let first = red.text.lines().next().unwrap_or_default();
                            self.log(&format!("{f}: {id} fast tier cut short — {first}; dwarf signalled: {:?} — handed to certification, no attempt charged", self.stop.signalled()));
                            let kept = self.keep_fast_tier_output(&red);
                            self.note(&format!("Fast tier was killed or could not run, so it did not judge the branch ({first}); certification judges it. {kept}"));
                        } else {
                            if let Some((prereq, tip)) = self.conflicting_blocker(&red) {
                                self.wait_for_blocker(&prereq, &tip, &red);
                                return self.finish(rc, "open");
                            }
                            self.refuse_handoff(&red);
                            return self.finish(rc, "open");
                        }
                    }
                }
                self.note(&format!("{} and marked submitted; the landing pass closes this bead when it lands, citing the merge commit. No attempt charged.", bead::HARNESS_SUBMIT_NOTE));
                self.log(&format!("{f}: {id} submitted — no attempt charged"));
                self.release();
                // The dwarf exits with its own rc (0: the bead was handed on, so the unit never
                // goes FAILED), but the ledger's `done` line records the MODEL's real exit code
                // (UC-dwarf-execution-18) — a session that submitted and then exited 1 is a fact
                // the ledger keeps, not one the dwarf's success erases.
                self.remove_identity();
                self.ledger_done(self.s.session_rc, &status);
                return rc;
            }
            NoteKey::YieldHeadless => {
                self.note("Yield-headless: the session ended its turn waiting for a background task notification. This session runs headless — there is no notification channel, so the session terminated and its background tasks were killed. Attempt charged; commit before any long step rather than backgrounding and yielding.");
                self.log(&format!("{f}: {id} yield-headless — ended turn waiting for background task, attempt charged"));
                self.release();
                return self.finish(rc, &status);
            }
            NoteKey::HarnessRed => {
                let why = self.s.harness_red.clone().unwrap_or_default();
                if rc == 0 {
                    rc = 1;
                }
                self.bump_requeue(&cause);
                self.release();
                let rc = self.finish(rc, &status);
                let threshold = self.conf.i("SPIRA_RAPID_RECUR_THRESHOLD");
                let lines = self.recent_done_lines(&id, threshold.max(0) as usize);
                let streak = decide::harness_red_streak(&lines.iter().map(String::as_str).collect::<Vec<_>>());
                if threshold <= 0 || streak < threshold {
                    self.note(&format!("Harness red (rc={rc}): the dwarf could not start its session — {why}. NO attempt was charged and nothing about the work is implied; this repeats until the harness config is fixed."));
                    self.log(&format!("{f}: {id} harness red (rc={rc}): {why} — no attempt charged"));
                    return rc;
                }
                if self.lc_bead(&id).is_some_and(|r| r.held("ask")) {
                    self.note(&format!("Harness red (rc={rc}): the dwarf could not start its session — {why}. NO attempt was charged. The row already carries an ask hold, so no further ask was filed."));
                    return rc;
                }
                let first: String = why.lines().next().unwrap_or("").trim().chars().take(200).collect();
                let subj = format!("harness red: {f} cannot start {id}: {first}");
                let body = format!("## Note\n{id} failed to start its session {streak} times in a row with a harness red; the last failure was:\n\n{why}\n\nNo attempt was charged for any of them. The bead is held and is not claimable again until the hold is lifted.\n");
                let _ = self.d.exec.exec(
                    "env",
                    &s(&["SPIRA_MAIL_ALLOW_BLOCKING=1", "mail", "send", "concierge", "--from", "Dwarf <dwarf@spira>", "--subject", &subj, "--kind", "question", "--default", "fix the harness fault named above, then lift the hold", "--bead", &id]),
                    Some(body.into_bytes()),
                    None,
                );
                let held = self.hold_unless_terminal(&id, "manual", &format!("harness-red streak {streak}: {first}"), "dwarf");
                if let Some(held) = held.filter(|h| h.code != 0) {
                    self.log(&format!("{f}: {id} manual hold refused (rc={}): {}", held.code, held.stdout.trim()));
                }
                self.note(&format!("Harness red (rc={rc}): the dwarf could not start its session — {why}. After {streak} consecutive harness reds the bead is held and the Concierge asked; it is not claimable again until the hold is lifted. NO attempt was charged."));
                self.log(&format!("{f}: {id} harness red streak {streak}/{threshold}: {first} — held, Concierge asked, no attempt charged"));
                return rc;
            }
            NoteKey::PreSession => {
                if rc == 0 {
                    rc = 1;
                }
                self.note(&format!("Pre-session death (rc={rc}): the dwarf died during setup before its Claude session started. Attempt charged — this failure repeats until the box state changes."));
                self.log(&format!("{f}: {id} pre-session death (rc={rc}) — attempt charged"));
                self.release();
                return self.finish(rc, &status);
            }
            NoteKey::Unlanded => {
                let o = i.outcome.clone().unwrap_or_default();
                self.note_blind_rework();
                self.note(&format!("Unlanded ({o}): the session ran to its own end and left this bead open. That is a verdict about the work; the next claim counts toward the poison threshold via the events trail."));
                self.log(&format!("{f}: {id} not closed ({o}), released"));
                self.release();
            }
            // sp-1zxru: ran to its own end, left the bead in_progress, but nothing it
            // did moved the branch. This is the harness's loop (law-attempts-count-
            // the-harness), not a judged attempt — release() alone would put it right
            // back in front of the next call (dwarf-ledger.log: resumed every ~75s).
            // Reuses the thrash streak (same tip-keyed counter thrash_streak_bump
            // already maintains) to tell "stuck again at the same commit" from "moved
            // on", and bump_requeue's exempt cause so the events fold never charges it
            // or lets CHECK 4 count it toward the attempts ask.
            NoteKey::NoProgress => {
                let o = i.outcome.clone().unwrap_or_default();
                let tip = short_tip(self);
                let reason = {
                    let r = unlanded_reason.trim().lines().next().unwrap_or("").trim();
                    let r: String = r.chars().take(300).collect();
                    if r.is_empty() { "no reason given".to_string() } else { r }
                };
                self.report_refused_submit(&id, &reason);
                let streak: i64 = self.sv("thrash_streak_bump", &s(&[&id, &tip, &reason])).text().trim().parse().unwrap_or(0);
                let cap = self.conf.i("SPIRA_THRASH_STREAK_CAP");
                self.bump_requeue(&cause);
                self.release();
                if streak >= cap {
                    let subj = format!("dwarf cannot progress: {reason}");
                    let body = format!(
                        "## Note\n{id} has made no progress across {streak} consecutive no-progress exits, branch {} stuck at {tip}. Each exit was held for a backoff instead of resumed, and no attempt was charged for any of them. The dwarf's own last word: {reason}\n\nChange the approach, split the bead, or drop it. This bead is blocked on this question until it is closed.\n",
                        self.s.branch
                    );
                    // A question with a blocking edge: the bead is unclaimable until the
                    // Concierge answers (law-a-retry-must-change-an-input).
                    let _ = self.d.exec.exec(
                        "env",
                        &s(&["SPIRA_MAIL_ALLOW_BLOCKING=1", "mail", "send", "concierge", "--from", "Dwarf <dwarf@spira>", "--subject", &subj, "--kind", "question", "--default", "split or drop the bead; it cannot progress with unchanged inputs", "--bead", &id]),
                        Some(body.into_bytes()),
                        None,
                    );
                    let why = format!("no-progress streak {streak} at {tip}: the Concierge lifts this once the bead's inputs have changed ({reason})");
                    let held = self.hold_unless_terminal(&id, "manual", &why, "dwarf");
                    if let Some(held) = held.filter(|h| h.code != 0) {
                        self.log(&format!("{f}: {id} manual hold refused (rc={}): {}", held.code, held.stdout.trim()));
                    }
                    self.note(&format!("{}{o}): {reason}\n\nAfter {streak} consecutive no-progress exits at {tip}, routed to the Concierge as a blocking question: the bead is not claimable again until it is answered. No attempt charged.", bead::HARNESS_NO_PROGRESS_NOTE));
                    self.log(&format!("{f}: {id} no-progress streak {streak}/{cap} at {tip} — routed to the Concierge, no attempt charged"));
                } else {
                    let backoff_min = decide::no_progress_backoff_minutes(streak);
                    let until_epoch = self.now() + backoff_min * 60;
                    let until = util::iso_utc(until_epoch);
                    // A timed `wait` hold on the lifecycle row: claim reads the expiry from its
                    // reason, so nothing has to lift it and no bd status or defer is written.
                    let held = self.hold_unless_terminal(&id, "wait", &format!("{} no-progress: {reason}", spira_store::lc_state::snooze_reason(until_epoch)), "dwarf");
                    if let Some(held) = held.filter(|h| h.code != 0) {
                        self.log(&format!("{f}: {id} snooze hold refused (rc={}): {}", held.code, held.stdout.trim()));
                    }
                    self.note(&format!("{}{o}): {reason}\n\nHeld for {backoff_min}m (no-progress streak {streak}, branch stuck at {tip}) — not re-claimed until {until}. No attempt charged.", bead::HARNESS_NO_PROGRESS_NOTE));
                    self.log(&format!("{f}: {id} no-progress streak {streak} at {tip} — held {backoff_min}m until {until}, no attempt charged"));
                }
            }
            NoteKey::NotJudged => {
                let o = i.outcome.clone().unwrap_or_default();
                self.bump_requeue(&cause);
                let red = if o == "refused" { format!("Harness red (session exit rc={}, no transcript). ", self.s.session_rc) } else { String::new() };
                self.note(&format!("{red}Not judged ({o}): the worker did not survive to judge this bead, so NO attempt was charged and nothing about the work is implied. See {logf}."));
                self.log(&format!("{f}: {id} never judged ({o}) — no attempt charged"));
                self.release();
            }
        }

        let final_rc = self.s.session_rc;
        self.remove_identity();
        self.ledger_done(final_rc, if st.is_empty() { "?" } else { &st });
        final_rc
    }

    fn remove_identity(&self) {
        if let Some(p) = &self.s.pidfile {
            let _ = std::fs::remove_file(p);
            let _ = std::fs::remove_file(strand::probe::lease_file(p));
            let _ = std::fs::remove_file(p.with_extension("name"));
        }
        let mail = self.conf.s("SPIRA_MAIL");
        if !self.s.bead.is_empty() {
            let _ = self.d.exec.exec("bead.sh", &s(&["amend", &self.s.bead, "--settle"]), None, None);
            crate::mailbox::close(&mail, &self.s.bead);
        }
    }

    /// The disposition branches that `exit $rc` inside the case: identity removed LAST, then
    /// the ledger line.
    fn finish(&self, rc: i32, status: &str) -> i32 {
        // dwarf.sh exited inside the case without removing the pidfile or the mailbox; the
        // pidfile is removed here after every bead operation (DESIGN.md §2.10.7).
        self.remove_identity();
        self.ledger_done(rc, status);
        rc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(t2bdp19)]
    #[test]
    fn removing_the_session_tmpdir_leaves_a_newer_sibling_of_the_same_user_alone() {
        let dir = testkit::TempDir::new("sweep");
        let own = dir.path().join("sp-x-123");
        let sibling = dir.path().join("tl-run.7m7bqt");
        std::fs::create_dir_all(own.join("a")).unwrap();
        std::fs::write(own.join("a/f"), vec![0u8; 4096]).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(sibling.join("conf"), "x").unwrap();
        let bytes = remove_session_tmpdir(&own);
        assert!(bytes >= 4096);
        assert!(!own.exists());
        assert!(sibling.join("conf").is_file());
    }

    #[test_id(txkp39h)]
    #[test]
    fn a_read_only_staged_tree_in_the_session_tmpdir_is_removed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = testkit::TempDir::new("ro");
        let staged = dir.path().join("t/db11/bin");
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(staged.join("f"), "x").unwrap();
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o555)).unwrap();
        remove_session_tmpdir(&dir.path().join("t"));
        assert!(!dir.path().join("t").exists());
    }

    #[test_id(tze7y6w)]
    #[test]
    fn a_symlink_in_the_session_tmpdir_is_unlinked_not_followed() {
        let dir = testkit::TempDir::new("link");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep"), "x").unwrap();
        let own = dir.path().join("own");
        std::fs::create_dir_all(&own).unwrap();
        std::os::unix::fs::symlink(&outside, own.join("l")).unwrap();
        remove_session_tmpdir(&own);
        assert!(!own.exists());
        assert!(outside.join("keep").is_file());
    }

    #[test_id(td4vv2b)]
    #[test]
    fn the_session_tmpdir_lies_inside_the_worktree() {
        let w = Path::new("w/wt");
        assert!(Run::session_tmpdir(w).starts_with(w));
    }

    #[test_id(t1vmp6x)]
    #[test]
    fn making_the_session_tmpdir_writes_a_self_ignoring_gitignore_and_keeps_an_existing_one() {
        let dir = testkit::TempDir::new("gi");
        Run::make_session_tmpdir(dir.path()).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join(".runtime/.gitignore")).unwrap(), "*\n");
        let kept = testkit::TempDir::new("gi");
        std::fs::create_dir(kept.path().join(".runtime")).unwrap();
        std::fs::write(kept.path().join(".runtime/.gitignore"), "other\n").unwrap();
        Run::make_session_tmpdir(kept.path()).unwrap();
        assert_eq!(std::fs::read_to_string(kept.path().join(".runtime/.gitignore")).unwrap(), "other\n");
    }

    #[test_id(tabf5za)]
    #[test]
    fn a_session_tmpdir_that_cannot_be_created_is_reported_with_its_path() {
        let dir = testkit::TempDir::new("blocked");
        std::fs::write(dir.path().join(".runtime"), "a file, not a directory").unwrap();
        let (path, _) = Run::make_session_tmpdir(dir.path()).unwrap_err();
        assert!(path.starts_with(dir.path()));
        let ok = testkit::TempDir::new("ok");
        assert!(Run::make_session_tmpdir(ok.path()).unwrap().is_dir());
    }
}
