//! The teardown (aeon.sh's `cleanup`, DESIGN.md §4.3-4.5): runs once, on every exit path
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

    fn defer_self_cert(&self, why: &str) {
        let br = &self.s.branch;
        self.note(&format!("{why} Certification is handed to the landing pass rather than run here: self-certifying blocks this session on host-wide gate admission while it holds its fleet slot. The landing pass gates and certifies {br} on its next pass."));
        self.log(&format!("{}: {} closed — certification of {br} handed to the landing pass", self.f(), self.s.bead));
    }

    /// Commits naming this bead between the base and the branch.
    fn has_own_commit(&self) -> bool {
        let o = self.d.git.git(&self.s.repo, &["log", "--format=%s%n%b", &format!("{}..{}", self.s.base_fq, self.s.branch)]);
        o.success() && o.stdout.contains(&self.s.bead)
    }

    fn already_certified(&self) -> Option<String> {
        let tip = self.d.git.git(&self.s.repo, &["rev-parse", &self.s.branch]);
        let tip = if tip.success() { tip.text() } else { String::new() };
        let ls = self.conf.land_state(&self.s.bead);
        let mut it = ls.split_whitespace();
        let (state, ltip) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
        (state == "CERTIFIED" && !tip.is_empty() && ltip == tip).then_some(tip)
    }

    pub fn teardown(&mut self, rc: i32) -> i32 {
        let mut rc = rc;
        let id = self.s.bead.clone();
        let f = self.fayth.name.clone();
        self.stop_heartbeat();
        self.fixture_drop();
        let _ = std::fs::remove_file(self.run_dir().join("aeon").join(format!("{id}.lease")));
        self.restore_world();
        let _ = std::env::set_current_dir(&self.s.repo);

        let st = bd::show(self.d.bd, &id).and_then(|r| r.status).unwrap_or_default();
        let mut st = st;

        let ow_marker = self.run_dir().join(format!("{id}.operator-wait"));
        let mut ow_mine = false;
        if ow_marker.is_file() {
            let stamp = std::fs::read_to_string(&ow_marker).unwrap_or_default();
            if stamp.trim_end_matches('\n') == self.s.session_epoch.to_string() && self.s.session_epoch != 0 {
                ow_mine = true;
            } else {
                let _ = std::fs::remove_file(&ow_marker);
                self.log(&format!("{f}: {id} operator-wait marker predates this session — cleared, not treated as a wait"));
            }
        }
        let logf = self.s.logf.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        let mut gate_why = String::new();

        if st != "closed" {
            let mut i = DispositionIn { status: if st.is_empty() { "?".into() } else { st.clone() }, session_rc: self.s.session_rc, committed: false, session_started: self.s.session_started, ..Default::default() };
            let (mut reset, mut thrash_note, mut thrash_tip, mut streak) = (String::new(), String::new(), String::new(), 0i64);
            let (mut lapsed_quiet, mut lapsed_last, mut gw) = (String::new(), String::new(), String::new());
            let mut unlanded_reason = String::new();
            let run = self.run_dir().to_path_buf();
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
            } else if run.join(format!("{id}.slain")).exists() {
                i.slain = true;
            } else if run.join(format!("{id}.thrash")).exists() {
                i.thrash = true;
                thrash_note = std::fs::read_to_string(run.join(format!("{id}.thrash"))).unwrap_or_default().trim_end_matches('\n').to_string();
                let _ = std::fs::remove_file(run.join(format!("{id}.thrash")));
                thrash_tip = short_tip(self);
                let n = if thrash_note.is_empty() { "?" } else { &thrash_note };
                streak = self.sv("thrash_streak_bump", &s(&[&id, &thrash_tip, n])).text().trim().parse().unwrap_or(0);
                i.thrash_charged = streak >= self.conf.n("SPIRA_THRASH_STREAK_CAP", 2);
            } else if run.join(format!("{id}.lapsed")).exists() {
                i.lapsed = true;
                let body = std::fs::read_to_string(run.join(format!("{id}.lapsed"))).unwrap_or_default().trim_end_matches('\n').to_string();
                match body.split_once('\t') {
                    Some((q, l)) => {
                        lapsed_quiet = q.into();
                        lapsed_last = l.into();
                    }
                    None => {
                        lapsed_quiet = body.clone();
                        lapsed_last = body;
                    }
                }
                let _ = std::fs::remove_file(run.join(format!("{id}.lapsed")));
            } else if let Some((2, why)) = self.gate_status() {
                i.gate_unfinished = true;
                gw = why;
            // literal-ok: Rust fallback mirroring conf.sh's default when SPIRA_ASK_LABEL is unset (same as below)
            } else if decide::open_ask_blocker(&bd::json(self.d.bd, &["show", &id]), &id, &self.conf.or("SPIRA_ASK_LABEL", "needs-operator")) {
                i.decision_blocked = true;
            } else if self.s.session_rc == 124 && !self.s.committed {
                // timeout — decided from session_rc / committed
            } else if requeue_cause.is_some() {
                // a harness requeue — the cause is passed below
            } else if ow_mine {
                i.operator_wait = true;
            } else if self.sv("lc_bead_verified", &s(&[&id])).success() {
                i.submitted = true;
            } else if bd::show(self.d.bd, &id).is_some_and(|r| r.has_label(&self.conf.submitted_label())) {
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
            let thrash_minutes = self.conf.n("SPIRA_THRASH_MINUTES", 20);
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
                    let ask = self.conf.or("SPIRA_ASK_LABEL", "needs-operator"); // literal-ok: Rust fallback mirroring conf.sh's default when SPIRA_ASK_LABEL is unset
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
                    let _ = std::fs::remove_file(&ow_marker);
                    self.bump_requeue(&cause);
                    self.note("Released by aeon.sh: the session sent a kind-question mail to the operator and exited awaiting a reply. No attempt charged; the bead becomes ready when the question is answered.");
                    self.log(&format!("{f}: {id} operator-wait — sent kind-question mail, released, no attempt charged"));
                    self.release();
                    return self.finish(rc, &status);
                }
                NoteKey::Submitted => {
                    self.note("Submitted: work committed on branch and marked submitted; the landing pass closes this bead when it lands, citing the merge commit. No attempt charged.");
                    self.log(&format!("{f}: {id} submitted — no attempt charged"));
                    self.release();
                    return self.finish(rc, &status);
                }
                NoteKey::YieldHeadless => {
                    self.note("Yield-headless: the session ended its turn waiting for a background task notification. This session runs headless — there is no notification channel, so the session terminated and its background tasks were killed. Attempt charged; commit before any long step rather than backgrounding and yielding.");
                    self.log(&format!("{f}: {id} yield-headless — ended turn waiting for background task, attempt charged"));
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
                    let cap = self.conf.n("SPIRA_THRASH_STREAK_CAP", 2);
                    self.bump_requeue(&cause);
                    self.release();
                    if streak >= cap {
                        let subj = format!("aeon cannot progress: {reason}");
                        let body = format!(
                            "## Note\n{id} has made no progress across {streak} consecutive no-progress exits, branch {} stuck at {tip}. Each exit was held for a backoff instead of resumed, and no attempt was charged for any of them. The aeon's own last word: {reason}\n\nChange the approach, split the bead, or drop it.\n",
                            self.s.branch
                        );
                        let _ = self.d.exec.exec(
                            "mail",
                            &s(&["send", "concierge", "--from", "Aeon <aeon@spira>", "--subject", &subj, "--kind", "note", "--bead", &id]),
                            Some(body.into_bytes()),
                            None,
                        );
                        self.note(&format!("No progress ({o}): {reason}\n\nAfter {streak} consecutive no-progress exits at {tip}, routed to the Concierge instead of held again. No attempt charged."));
                        self.log(&format!("{f}: {id} no-progress streak {streak}/{cap} at {tip} — routed to the Concierge, no attempt charged"));
                    } else {
                        let backoff_min = decide::no_progress_backoff_minutes(streak);
                        let until = util::iso_utc(self.now() + backoff_min * 60);
                        let _ = self.d.bd.bd(&s(&["update", &id, "--defer", &until]));
                        // --defer also sets status=deferred, which bd ready excludes even after
                        // defer_until passes; open + a future defer_until is what auto-releases.
                        let _ = self.d.bd.bd(&s(&["update", &id, "--status", "open"]));
                        self.note(&format!("No progress ({o}): {reason}\n\nHeld for {backoff_min}m (no-progress streak {streak}, branch stuck at {tip}) — not re-claimed until {until}. No attempt charged."));
                        self.log(&format!("{f}: {id} no-progress streak {streak} at {tip} — held {backoff_min}m until {until}, no attempt charged"));
                    }
                }
                NoteKey::NotJudged => {
                    let o = i.outcome.clone().unwrap_or_default();
                    self.bump_requeue(&cause);
                    self.note(&format!("Not judged ({o}): the worker did not survive to judge this bead, so NO attempt was charged and nothing about the work is implied. See {logf}."));
                    self.log(&format!("{f}: {id} never judged ({o}) — no attempt charged"));
                    self.release();
                }
            }
        } else if let Some((gate_st, why)) = self.gate_status() {
            gate_why = why.clone();
            let superseded = bd::show(self.d.bd, &id).is_some_and(|r| r.superseded());
            let queued = self.conf.repos.land_queued(&self.s.repo_name);
            let hasown = queued && self.has_own_commit();
            let br = self.s.branch.clone();
            let rn = self.s.repo_name.clone();
            let base = self.s.base.clone();
            match gate_st {
                0 => {
                    self.note(&format!("Closed against a recorded PASS verdict for this exact tree — {why}"));
                    self.log(&format!("{f}: {id} closed with a recorded PASS gate verdict ({why})"));
                }
                2 => {
                    self.note(&format!("Closed by the session while its landing gate was still running — {why}. The close carries no gate verdict; the landing pass gates this branch again and reopens the bead if it fails."));
                    self.log(&format!("{f}: {id} closed with its gate still running ({why})"));
                }
                1 => {
                    if !queued {
                        let w = if why.is_empty() { "gate returned fail".to_string() } else { why.clone() };
                        self.note(&format!("Closed against a recorded FAIL verdict for this exact tree — {w}. The landing pass will reopen this bead."));
                        self.log(&format!("{f}: {id} closed against a recorded FAIL gate verdict"));
                    } else if !hasown {
                        self.log(&format!("{f}: {id} closed against a recorded FAIL gate verdict, but {br} carries no commit of {id}'s own ahead of base — not this bead's fault, not reopening"));
                    } else {
                        self.bead_reopen("cert-gate-red", &format!("Reopened by aeon.sh: closed against a recorded FAIL gate verdict for {br} in {rn}.\n\n{why}"));
                        st = "open".into();
                        self.log(&format!("{f}: {id} REOPENED — closed against a recorded FAIL gate verdict"));
                    }
                }
                3..=5 => {
                    let (plain_note, plain_log, none_note, none_log, cert_log, defer_why) = match gate_st {
                        3 => (
                            "Closed without ever obtaining a gate verdict — no gate ran or finished for this branch.".to_string(),
                            format!("{f}: {id} closed with no gate verdict (none ran)"),
                            format!("Closed without ever obtaining a gate verdict — no gate ran or finished for this branch. {br} carries no commit of {id}'s own ahead of {rn}'s base, so there is nothing to certify."),
                            format!("{f}: {id} closed with no gate verdict (none ran) — {br} has no commit naming {id} ahead of base, nothing to certify"),
                            "closed with no gate verdict".to_string(),
                            "Closed without ever obtaining a gate verdict — no gate ran or finished for this branch.".to_string(),
                        ),
                        4 => (
                            format!("Closed holding a gate verdict that no longer applies — {why}. Something (most likely this teardown's own rebase onto {base} after the close) changed the tree the verdict was for. This is not a missing gate run; the landing pass gates {br} as it now stands."),
                            format!("{f}: {id} closed with a stale gate verdict ({why})"),
                            format!("Closed holding a gate verdict that no longer applies — {why}. {br} carries no commit of {id}'s own ahead of {rn}'s base, so there is nothing to certify. This is not a missing gate run."),
                            format!("{f}: {id} closed with a stale gate verdict ({why}) — nothing ahead of base to certify"),
                            "closed with a stale gate verdict".to_string(),
                            format!("Closed holding a gate verdict that no longer applies — {why}. This is not a missing gate run."),
                        ),
                        _ => (
                            format!("Closed without ever obtaining a gate verdict — the gate run died before recording one. {why}"),
                            format!("{f}: {id} closed with no gate verdict (run died) — {why}"),
                            format!("Closed without ever obtaining a gate verdict — the gate run died before recording one. {br} carries no commit of {id}'s own ahead of {rn}'s base, so there is nothing to certify."),
                            format!("{f}: {id} closed with no gate verdict (run died) — nothing ahead of base to certify"),
                            "closed with no gate verdict (run died)".to_string(),
                            format!("Closed without ever obtaining a gate verdict — the gate run died before recording one. {why}"),
                        ),
                    };
                    if !queued {
                        self.note(&plain_note);
                        self.log(&plain_log);
                    } else if !hasown {
                        self.note(&none_note);
                        self.log(&none_log);
                    } else if let Some(tip) = self.already_certified() {
                        self.log(&format!("{f}: {id} {cert_log}, but {tip} is already CERTIFIED — the session submitted it itself"));
                    } else if superseded {
                        self.log(&format!("{f}: {id} {cert_log}, but the bead is superseded — not handing {br} to certification, leaving it for the Sending to reap"));
                    } else {
                        self.defer_self_cert(&defer_why);
                    }
                }
                _ => {}
            }
        }

        // A closed bead still owes its own operator-wait marker.
        if ow_mine && ow_marker.is_file() {
            let _ = std::fs::remove_file(&ow_marker);
            self.note("Closed carrying an operator-wait marker: this session sent a kind-question mail to the operator before closing. Recorded here so the wait is not invisible to anyone asking what is blocked on a reply.");
            self.log(&format!("{f}: {id} closed with its own operator-wait marker — consumed, recorded as operator-wait"));
        }

        // ---- the submitted conversion (sp-qsona) ----
        if st == "closed" && self.fayth.graph_only {
            self.log(&format!("{f}: {id} closed a work bead — graph-only persona, no commit expected, not converted"));
        } else if !self.s.lc_model_restricted && st == "closed" {
            if let Some(row) = bd::show(self.d.bd, &id) {
                let ty = row.issue_type.clone().unwrap_or_default();
                let deliv = row.labels().iter().any(|l| l.starts_with("delivers:"));
                if self.sv("bead_is_work_type", &s(&[&ty])).success() && !deliv {
                    if row.superseded() {
                        self.log(&format!("{f}: {id} closed a superseded work bead — not converted, close stands"));
                    } else if !self.has_own_commit() {
                        self.log(&format!("{f}: {id} closed a work bead but {} carries no commit of its own ahead of {} — not converted, close stands", self.s.branch, self.s.base_fq));
                    } else {
                        let g = if gate_why.is_empty() { String::new() } else { format!(" Gate at close: {gate_why}.") };
                        self.bead_reopen("work-close-converted", &format!("Submitted: work committed on branch; marked submitted instead of closed. The landing pass closes this bead when it lands, citing the merge commit.{g}"));
                        let _ = self.d.bd.bd(&s(&["label", "add", &id, &self.conf.submitted_label()]));
                        self.log(&format!("{f}: {id} closed a work bead directly — converted to submitted"));
                        st = "submitted".into();
                    }
                }
            }
        }

        let final_rc = self.s.session_rc;
        self.remove_identity();
        self.ledger_done(final_rc, if st.is_empty() { "?" } else { &st });
        if st == "closed" || st == "submitted" {
            return 0;
        }
        final_rc
    }

    fn remove_identity(&self) {
        if let Some(p) = &self.s.pidfile {
            let _ = std::fs::remove_file(p);
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
