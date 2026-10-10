//! The teardown (aeon.sh's `cleanup`, DESIGN.md §4.3-4.4): runs once, on every exit path
//! after the claim. Charging is default-deny — `decide::disposition` holds the precedence;
//! this module gathers its inputs lazily (a marker a higher row would match is never
//! consumed early) and performs the side effects the verdict names.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::bd;
use crate::decide::{self, DispositionIn, NoteKey};
use crate::ledger;
use crate::ports::s;
use crate::run::{Run, FIXTURE_DROP};
use crate::util;

impl Run<'_> {
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

    /// Refuses the handoff when the fast tier is red: the bead goes back to the graph with the
    /// failure text instead of waiting a certification cycle to learn it.
    fn refuse_handoff(&self, red: &crate::fast_tier::Red) {
        let br = &self.s.branch;
        let digest = crate::fast_tier::digest(&red.text);
        if red.harness {
            self.bead_reopen("fast-tier-harness", &format!("Reopened by aeon.sh: the in-session fast tier could not run on {br} (a tool failed, not the work) and the handoff was refused. No attempt charged.\n\n{digest}"));
            self.log(&format!("{}: {} REOPENED — fast tier harness failure, no attempt charged", self.f(), self.s.bead));
            return;
        }
        self.record_fact(crate::fast_tier::KIND, &digest);
        self.bead_reopen("fast-tier-red", &format!("Reopened by aeon.sh: {br} failed the in-session fast tier (lint, build fence, rebase check) and was not handed to certification.\n\n{digest}"));
        self.log(&format!("{}: {} REOPENED — fast tier red, handoff refused", self.f(), self.s.bead));
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
        let f = self.fayth.name.clone();
        self.stop_heartbeat();
        self.phase("teardown");
        self.fixture_drop();
        let _ = std::fs::remove_file(self.run_dir().join("aeon").join(format!("{id}.lease")));
        self.restore_world();
        let _ = std::env::set_current_dir(&self.s.repo);

        // The bead's state is its lifecycle row's, never bd's status (design §3.4, sp-mve9i):
        // `st` is the ledger's word. A session hands its bead on only through the work verbs
        // (every session runs restricted since sp-v62vn), which the disposition reads as
        // `submitted` — there is no "the model closed the bead" branch here any more.
        let lc = self.lc_bead(&id);
        let st = decide::ledger_word(lc.as_ref()).to_string();
        // Operator-wait is the lifecycle record's (sp-v62vn follow-up): the model asks
        // through `work ask`/`work blocked`, whose broker places an `ask` hold on this bead's
        // row. The claim released any bead already carrying a non-wait hold
        // (run.rs blocking_holds), so an ask hold here was placed during this session.
        let asked = lc.as_ref().is_some_and(|r| r.held("ask"));
        let disposition = lc.as_ref().and_then(|r| r.disposition.as_deref());
        let disposition_note = lc.as_ref().and_then(|r| r.disposition_note.clone()).unwrap_or_default();
        let logf = self.s.logf.as_ref().map(|p| p.display().to_string()).unwrap_or_default();

        let mut i = DispositionIn { status: st.clone(), session_rc: self.s.session_rc, committed: false, session_started: self.s.session_started, harness_red: self.s.harness_red.is_some(), ..Default::default() };
        let (mut reset, mut thrash_note, mut thrash_tip, mut streak) = (String::new(), String::new(), String::new(), 0i64);
        let (mut lapsed_quiet, mut lapsed_last, mut gw) = (String::new(), String::new(), String::new());
        let mut unlanded_reason = String::new();
        let work = self.s.work.clone();
        let short_tip = |this: &Self| {
            let o = this.d.git.git(work.as_deref().unwrap_or(Path::new("/dev/null")), &["rev-parse", "--short", "HEAD"]);
            if o.success() { o.text() } else { "?".into() }
        };
        // Full hash, matching session_start_tip's own format (sp-1zxru-2) — short_tip's
        // truncated form is for notes/thrash-tip identity, not this equality check.
        let full_tip = |this: &Self| {
            let o = this.d.git.git(work.as_deref().unwrap_or(Path::new("/dev/null")), &["rev-parse", "HEAD"]);
            if o.success() { o.text().trim().to_string() } else { "?".into() }
        };
        let requeue_cause = self.s.requeue_cause.clone();
        // capacity_reset_at, in-process now (wave 4.26). `Some(0)` ("hit, but no
        // resetsAt") still counts as a capacity return — see capacity.rs's own doc.
        let cr = crate::capacity::reset_at(Path::new(&logf), &self.conf.trace_mark());
        if let Some(r) = cr {
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
        } else if decide::open_ask_blocker(&bd::json(self.d.bd, &["show", &id]), &id, &self.conf.ask_label()) {
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
            // The aeon's own last word, gathered here and used only if the disposition
            // below lands on NoProgress (sp-1zxru) — never consulted for the decision
            // itself, which stays session_outcome's and committed's alone.
            unlanded_reason = decide::last_assistant_text(seg.as_deref()).unwrap_or_default();
            i.outcome = Some(decide::session_outcome(seg.as_deref()).to_string());
        }
        // SESSION_RC, committed and REQUEUE_CAUSE are passed whatever was gathered, as
        // aeon.sh passed them; the precedence lives in decide::disposition.
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
            NoteKey::Capacity => {
                let at: i64 = reset.trim().parse().unwrap_or(0);
                if let Some(line) = crate::capacity::pause_set(&self.conf.capacity_pause(), &self.conf.ledger(), self.now(), self.conf.capacity_backoff(), at, &id) {
                    self.log(&line);
                }
                self.release();
                self.bump_requeue(&cause);
                self.note("Returned unchanged by aeon.sh: the account's capacity window was spent mid-session, so this bead was never judged. No attempt was charged and nothing about the work is implied. Summoning is paused until the window reopens.");
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
                self.note(&format!("STICKING POINT: {n}\n\nRequeued (thrash): the deliverable did not move for {thrash_minutes}m while turns advanced, and this is the {streak}th consecutive thrash with branch {} still at {thrash_tip} — nothing has been committed since the last one. An attempt IS charged this time: the sticking point above is the next aeon's first move, not something to rediscover by reading back through this bead's notes.", self.s.branch));
                self.log(&format!("{f}: {id} thrash-requeued — attempt charged (streak {streak}, tip {thrash_tip} unchanged; last: {n})"));
                return self.finish(rc, &status);
            }
            NoteKey::Thrash => {
                self.release();
                self.bump_requeue(&cause);
                let n = if thrash_note.is_empty() { "?" } else { &thrash_note };
                self.note(&format!("Requeued (thrash): the deliverable did not move for {thrash_minutes}m while turns advanced. Last action: {n}. No attempt charged — the next aeon should start from this sticking point."));
                self.log(&format!("{f}: {id} thrash-requeued — no attempt charged (streak {streak}, tip {thrash_tip}; last: {n})"));
                return self.finish(rc, &status);
            }
            NoteKey::Lapsed => {
                let q = if lapsed_quiet.is_empty() { "?" } else { &lapsed_quiet };
                let l = if lapsed_last.is_empty() { "?" } else { &lapsed_last };
                self.sdo("bump_lapsed", &s(&[&id, l]));
                let tip = short_tip(self);
                self.sdo("write_lapse_record", &s(&[&id, q, l, &tip]));
                self.note(&format!("Lease lapsed: the trace was silent for {q}s (limit {}s). Last: {l}. Branch spira/{id} preserved. Attempt 2 should start from where attempt 1 wedged.", self.fayth.lease_seconds()));
                self.log(&format!("{f}: {id} lease lapsed — attempt charged (quiet {q}s)"));
                self.release();
                return self.finish(rc, &status);
            }
            NoteKey::GateUnfinished => {
                self.release();
                self.bump_requeue(&cause);
                self.note(&format!("Released by aeon.sh: the session ended while its landing gate was still running, so it never held a verdict about its own work. No attempt was charged and nothing about the work is implied — {gw}. Run the gate through gate-run.sh, which waits in bounded slices, and do not end the session while it is unfinished."));
                self.log(&format!("{f}: {id} released with its gate still running — no attempt charged ({gw})"));
                return self.finish(rc, &status);
            }
            NoteKey::DecisionBlocked => {
                self.release();
                self.bump_requeue(&cause);
                let ask = self.conf.ask_label();
                self.note(&format!("Released by aeon.sh: blocked on an open decision bead ({ask} label) — waiting for operator input. No attempt charged; the bead becomes ready when the decision is resolved."));
                self.log(&format!("{f}: {id} has open decision blocker — released, no attempt charged"));
                return self.finish(rc, &status);
            }
            NoteKey::Timeout => {
                let t = self.fayth.timeout_seconds.map(|t| t.to_string()).unwrap_or_else(|| "?".into());
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
                self.note("Released by aeon.sh: the session asked the operator a question (work ask) and exited awaiting the answer; the bead's lifecycle row carries the ask hold. No attempt charged; the bead becomes claimable when the operator's reply lifts the hold.");
                self.log(&format!("{f}: {id} operator-wait — asked the operator (ask hold on the row), released, no attempt charged"));
                self.release();
                return self.finish(rc, &status);
            }
            NoteKey::Submitted => {
                if self.has_own_commit() {
                    if let Some(red) = self.fast_tier_red() {
                        self.refuse_handoff(&red);
                        return self.finish(rc, "open");
                    }
                }
                self.note("Submitted: work committed on branch and marked submitted; the landing pass closes this bead when it lands, citing the merge commit. No attempt charged.");
                self.log(&format!("{f}: {id} submitted — no attempt charged"));
                self.release();
                // The aeon exits with its own rc (0: the bead was handed on, so the unit never
                // goes FAILED), but the ledger's `done` line records the MODEL's real exit code
                // (UC-aeon-execution-18) — a session that submitted and then exited 1 is a fact
                // the ledger keeps, not one the aeon's success erases.
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
                self.note(&format!("Harness red (rc={rc}): the aeon could not start its session — {why}. NO attempt was charged and nothing about the work is implied; this repeats until the harness config is fixed."));
                self.log(&format!("{f}: {id} harness red (rc={rc}): {why} — no attempt charged"));
                self.release();
                return self.finish(rc, &status);
            }
            NoteKey::PreSession => {
                if rc == 0 {
                    rc = 1;
                }
                self.note(&format!("Pre-session death (rc={rc}): the aeon died during setup before its Claude session started. Attempt charged — this failure repeats until the box state changes."));
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
            // back in front of the next summon (aeon-ledger.log: resumed every ~75s).
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
                let streak: i64 = self.sv("thrash_streak_bump", &s(&[&id, &tip, &reason])).text().trim().parse().unwrap_or(0);
                let cap = self.conf.i("SPIRA_THRASH_STREAK_CAP");
                self.bump_requeue(&cause);
                self.release();
                if streak >= cap {
                    let subj = format!("aeon cannot progress: {reason}");
                    let body = format!(
                        "## Note\n{id} has made no progress across {streak} consecutive no-progress exits, branch {} stuck at {tip}. Each exit was held for a backoff instead of resumed, and no attempt was charged for any of them. The aeon's own last word: {reason}\n\nChange the approach, split the bead, or drop it. This bead is blocked on this question until it is closed.\n",
                        self.s.branch
                    );
                    // A question with a blocking edge: the bead is unclaimable until the
                    // Concierge answers (law-a-retry-must-change-an-input).
                    let _ = self.d.exec.exec(
                        "env",
                        &s(&["SPIRA_MAIL_ALLOW_BLOCKING=1", "mail", "send", "concierge", "--from", "Aeon <aeon@spira>", "--subject", &subj, "--kind", "question", "--default", "split or drop the bead; it cannot progress with unchanged inputs", "--bead", &id]),
                        Some(body.into_bytes()),
                        None,
                    );
                    self.note(&format!("No progress ({o}): {reason}\n\nAfter {streak} consecutive no-progress exits at {tip}, routed to the Concierge as a blocking question: the bead is not claimable again until it is answered. No attempt charged."));
                    self.log(&format!("{f}: {id} no-progress streak {streak}/{cap} at {tip} — routed to the Concierge, no attempt charged"));
                } else {
                    let backoff_min = decide::no_progress_backoff_minutes(streak);
                    let until_epoch = self.now() + backoff_min * 60;
                    let until = util::iso_utc(until_epoch);
                    // A timed `wait` hold on the lifecycle row: claim reads the expiry from its
                    // reason, so nothing has to lift it and no bd status or defer is written.
                    let held = self.d.exec.exec("spira-lc", &s(&["hold", &id, "wait", &spira_config::lc_state::snooze_reason(until_epoch), "aeon"]), None, None);
                    if held.code != 0 {
                        self.log(&format!("{f}: {id} snooze hold refused (rc={}): {}", held.code, held.stdout.trim()));
                    }
                    self.note(&format!("No progress ({o}): {reason}\n\nHeld for {backoff_min}m (no-progress streak {streak}, branch stuck at {tip}) — not re-claimed until {until}. No attempt charged."));
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
            let _ = std::fs::remove_dir_all(Path::new(&mail).join(format!("aeon-{}", self.s.bead)));
        }
    }

    /// The disposition branches that `exit $rc` inside the case: identity removed LAST, then
    /// the ledger line.
    fn finish(&self, rc: i32, status: &str) -> i32 {
        // aeon.sh exited inside the case without removing the pidfile or the mailbox; the
        // pidfile is removed here after every bead operation (DESIGN.md §2.10.7).
        self.remove_identity();
        self.ledger_done(rc, status);
        rc
    }
}
