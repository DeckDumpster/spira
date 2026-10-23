//! `reconcile`: brings the bead store in line with the replies already delivered. `sendmail`
//! only delivers; this pass reads each recent reply (a message with `In-Reply-To`) against the
//! mail it answers and takes the corrective steps that reply implies — note the answer, close
//! the tracking ask, lift the work bead's `ask` hold. Every step is recorded per Message-ID
//! under the state dir, so a repeated pass converges instead of double-applying, a client's
//! retry (same Message-ID, a second delivery) applies once, and a store that was down is
//! simply repaired by the next pass.
//!
//! A refusal (the lifecycle machine will not close a work bead from a mail reply) converges
//! to "answer noted on the bead + one concierge-inbox row", then is done. An answer is never
//! dropped.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::bead::{self, Bd};
use crate::lc::{self, Lc, Lift};
use crate::maildir;
use crate::message;
use crate::sendmail::{find_message_by_id, first_paragraph};

pub const WINDOW_S: u64 = 3 * 86_400;
pub const MAX_ATTEMPTS: usize = 30;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub converged: usize,
    pub pending: usize,
    pub refused: usize,
}

struct Done {
    path: PathBuf,
    lines: HashSet<String>,
    failures: Vec<String>,
}

impl Done {
    fn load(dir: &Path, key: &str) -> Done {
        let safe: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || "@._-".contains(c) { c } else { '_' }).collect();
        let path = dir.join(safe);
        let text = fs::read_to_string(&path).unwrap_or_default();
        Done { path, lines: text.lines().map(str::to_string).collect(), failures: text.lines().filter(|l| l.starts_with("fail:")).map(str::to_string).collect() }
    }
    fn has(&self, s: &str) -> bool {
        self.lines.contains(s)
    }
    fn mark(&mut self, s: &str) {
        if let Some(dir) = self.path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = writeln!(f, "{s}");
        }
        if s.starts_with("fail:") {
            self.failures.push(s.to_string());
        }
        self.lines.insert(s.to_string());
    }
    fn fails(&self, step: &str) -> usize {
        let want = format!("fail:{step}");
        self.failures.iter().filter(|l| **l == want).count()
    }
}

enum Step {
    Done,
    Retry,
    Abandoned,
}

fn step(done: &mut Done, name: &str, f: impl FnOnce() -> Result<(), String>) -> Step {
    if done.has(name) {
        return Step::Done;
    }
    match f() {
        Ok(()) => {
            done.mark(name);
            Step::Done
        }
        Err(e) => {
            eprintln!("mail reconcile: {name}: {e}");
            done.mark(&format!("fail:{name}"));
            if done.fails(name) >= MAX_ATTEMPTS {
                done.mark(name);
                Step::Abandoned
            } else {
                Step::Retry
            }
        }
    }
}

fn tally<'a>(s: Step, name: &'a str, converged: &mut bool, abandoned: &mut Vec<&'a str>) {
    match s {
        Step::Done => {}
        Step::Retry => *converged = false,
        Step::Abandoned => abandoned.push(name),
    }
}

fn is_refusal(err: &str) -> bool {
    err.contains("exited 3")
}

fn inbox_row(mail_root: &Path, subject: &str, body: &str) -> Result<(), String> {
    let dir = mail_root.join("concierge");
    maildir::mail_ensure(&dir).map_err(|e| format!("cannot ensure concierge mailbox: {e}"))?;
    let msgid = maildir::mint_msgid();
    let raw = format!("From: mail-reconcile <mail@spira>\nSubject: {subject}\nX-Spira-Kind: alert\nMessage-ID: <{msgid}@spira>\n\n{body}\n");
    fs::write(dir.join("tmp").join(&msgid), raw).map_err(|e| format!("cannot write inbox row: {e}"))?;
    maildir::mail_deliver(&dir, &msgid, false).map_err(|e| format!("cannot deliver inbox row: {e}"))?;
    Ok(())
}

/// Applies one delivered reply. `true` when every step it implies has converged.
fn apply(bd: &dyn Bd, lc: &dyn Lc, mail_root: &Path, reply: &str, key: &str, done: &mut Done, report: &mut Report) -> bool {
    let in_reply_to: String = message::header_ci_before_blank(reply, "in-reply-to").chars().filter(|c| *c != '<' && *c != '>').collect();
    let Some(orig_path) = find_message_by_id(mail_root, &in_reply_to) else {
        return true;
    };
    let orig = fs::read_to_string(&orig_path).unwrap_or_default();
    let orig_bead = message::header_ci_before_blank(&orig, "x-spira-bead");
    let orig_kind = message::header_ci_before_blank(&orig, "x-spira-kind");
    let work_bead = message::header_ci_before_blank(&orig, "x-spira-work-bead");
    let orig_subject = message::header_ci_before_blank(&orig, "subject");
    let actor = lc::actor_of(&message::header_ci_before_blank(reply, "from"), "operator");
    let mut para = first_paragraph(reply);
    if para.is_empty() {
        para = format!("answered by mail <{in_reply_to}> \"{orig_subject}\" (reply body empty)");
    }
    let tracked = matches!(orig_kind.as_str(), "question" | "decision" | "suit");

    let mut converged = true;
    let mut abandoned: Vec<&str> = Vec::new();

    if !orig_bead.is_empty() && !tracked {
        let s = step(done, "note-orig", || bead::note(bd, &orig_bead, &format!("Reply from {actor} to \"{orig_subject}\": {para}")));
        tally(s, "note-orig", &mut converged, &mut abandoned);
    } else if !orig_bead.is_empty() && !done.has("close") {
        match bead::sendmail_close_bead(bd, true, &orig_bead, &orig_kind, &para) {
            Ok(warnings) => {
                warnings.iter().for_each(|w| eprintln!("{w}"));
                done.mark("close");
            }
            Err(_) if bead::bead_status(bd, true, &orig_bead).as_deref() == Some("closed") => done.mark("close"),
            Err(e) if is_refusal(&e) => {
                let n = step(done, "refused-note", || bead::note(bd, &orig_bead, &format!("Answer from {actor} to \"{orig_subject}\" (not applied as a close: {e}): {para}")));
                let i = step(done, "refused-inbox", || {
                    inbox_row(mail_root, &format!("Mail reply to {orig_bead} refused as a close"), &format!("{actor} replied to \"{orig_subject}\" (thread bead {orig_bead}); the lifecycle refused the close: {e}\nThe answer is noted on {orig_bead}:\n{para}"))
                });
                if matches!((n, i), (Step::Done, Step::Done)) {
                    report.refused += 1;
                    done.mark("close");
                } else {
                    converged = false;
                }
            }
            Err(e) => {
                let s = step(done, "close", || Err(e));
                tally(s, "close", &mut converged, &mut abandoned);
            }
        }
    }

    if !work_bead.is_empty() {
        if orig_bead.is_empty() {
            let s = step(done, "note-work", || bead::note(bd, &work_bead, &format!("Answer from {actor} to \"{orig_subject}\": {para}")));
            tally(s, "note-work", &mut converged, &mut abandoned);
        }
        let s = step(done, "lift", || match lc::lift_ask(lc, &work_bead, Lift::Reply { message_id: key }, &actor) {
            Some(w) => Err(w),
            None => Ok(()),
        });
        tally(s, "lift", &mut converged, &mut abandoned);
    }

    if !orig_bead.is_empty() && matches!(orig_kind.as_str(), "question" | "decision") {
        let s = step(done, "answer", || match lc::answer_ask(lc, &orig_bead, &para, &actor, "mail", key) {
            Some(w) => Err(w),
            None => Ok(()),
        });
        tally(s, "answer", &mut converged, &mut abandoned);
    }

    if !abandoned.is_empty() {
        let names = abandoned.join(", ");
        let _ = step(done, "abandoned-inbox", || inbox_row(mail_root, "Mail reply reconciliation gave up", &format!("Reply {key} to \"{orig_subject}\": steps [{names}] failed {MAX_ATTEMPTS} times and were abandoned; apply by hand.")));
    }
    converged
}

/// One pass over every mailbox's recent replies. Safe to run at any cadence and concurrently
/// with delivery; steps already recorded are never repeated.
pub fn pass(bd: &dyn Bd, lc: &dyn Lc, db_configured: bool, mail_root: &Path, state_dir: &Path, now_s: u64) -> Report {
    let mut report = Report::default();
    if !db_configured {
        return report;
    }
    let mut mailboxes: Vec<String> = fs::read_dir(mail_root)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    mailboxes.sort();
    for sub in ["new", "cur"] {
        for mbox in &mailboxes {
            for f in maildir::sorted_entries(&mail_root.join(mbox).join(sub)) {
                if now_s.saturating_sub(maildir::mtime_secs(&f)) > WINDOW_S {
                    continue;
                }
                let Ok(text) = fs::read_to_string(&f) else { continue };
                if message::header_ci_before_blank(&text, "in-reply-to").is_empty() || message::header_ci_before_blank(&text, "x-spira-kind") == "revision" {
                    continue;
                }
                let own: String = message::header_ci_before_blank(&text, "message-id").chars().filter(|c| *c != '<' && *c != '>').collect();
                let key = if own.is_empty() { f.file_name().and_then(|n| n.to_str()).unwrap_or("").split(':').next().unwrap_or("").to_string() } else { own };
                if key.is_empty() {
                    continue;
                }
                let mut done = Done::load(state_dir, &key);
                if done.has("complete") {
                    continue;
                }
                if apply(bd, lc, mail_root, &text, &key, &mut done, &mut report) {
                    done.mark("complete");
                    report.converged += 1;
                } else {
                    report.pending += 1;
                }
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead::fake::FakeBd;
    use crate::bead::BdOut;
    use crate::lc::fake::FakeLc;
    use crate::sendmail::sendmail;

    struct Fx {
        d: testkit::TempDir,
    }

    impl Fx {
        fn new() -> Fx {
            let d = testkit::TempDir::new("mail-reconcile");
            fs::create_dir_all(d.path().join("home/chamber")).unwrap();
            Fx { d }
        }
        fn home(&self) -> PathBuf {
            self.d.path().join("home")
        }
        fn root(&self) -> PathBuf {
            self.d.path().join("mail")
        }
        fn state(&self) -> PathBuf {
            self.d.path().join("state")
        }
        fn question(&self, mailbox: &str, headers: &str) -> String {
            let root = self.root();
            maildir::mail_ensure(&root.join(mailbox)).unwrap();
            let msgid = maildir::mint_msgid();
            let orig = format!("From: Builder <builder@spira>\nSubject: May I?\nX-Spira-Kind: question\n{headers}Message-ID: <{msgid}@spira>\n\n## Question\nq\n");
            fs::write(root.join(mailbox).join("tmp").join(&msgid), &orig).unwrap();
            maildir::mail_deliver(&root.join(mailbox), &msgid, false).unwrap();
            msgid
        }
        fn reply(&self, q: &str, id: &str, body: &str) {
            let raw = format!("From: Operator <operator@spira>\nSubject: Re: May I?\nMessage-ID: <{id}@spira>\nIn-Reply-To: <{q}@spira>\n\n{body}\n");
            sendmail(&self.home(), &self.root(), false, &raw).unwrap();
        }
        fn pass(&self, bd: &FakeBd, lc: &FakeLc) -> Report {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
            pass(bd, lc, true, &self.root(), &self.state(), now)
        }
        fn inbox(&self) -> usize {
            maildir::sorted_entries(&self.root().join("concierge").join("new")).iter().filter(|f| fs::read_to_string(f).unwrap_or_default().contains("X-Spira-Kind: alert")).count()
        }
    }

    #[test]
    fn a_reply_to_an_ask_closes_it_lifts_the_hold_and_records_the_answer() {
        let fx = Fx::new();
        let q = fx.question("operator", "X-Spira-Bead: sp-ask1\nX-Spira-Work-Bead: sp-work1\n");
        fx.reply(&q, "ans-1", "Yes.");
        let bd = FakeBd::new(vec![BdOut::ok(""), BdOut::ok("[]")]);
        let lc = FakeLc::new(0);
        let r = fx.pass(&bd, &lc);
        assert_eq!(r, Report { converged: 1, pending: 0, refused: 0 });
        assert_eq!(bd.calls()[0].0[..2], ["close".to_string(), "sp-ask1".to_string()]);
        let calls = lc.calls();
        assert_eq!(calls[0], vec!["reply", "sp-work1", "ans-1@spira", "operator"]);
        assert_eq!(calls[1][..2], ["close-ask", "sp-ask1"]);
    }

    #[test]
    fn a_second_pass_applies_nothing_again() {
        let fx = Fx::new();
        let q = fx.question("operator", "X-Spira-Bead: sp-ask1\nX-Spira-Work-Bead: sp-work1\n");
        fx.reply(&q, "ans-1", "Yes.");
        let bd = FakeBd::new(vec![BdOut::ok(""), BdOut::ok("[]")]);
        let lc = FakeLc::new(0);
        fx.pass(&bd, &lc);
        let (b1, l1) = (bd.calls().len(), lc.calls().len());
        let r = fx.pass(&bd, &lc);
        assert_eq!(r, Report::default());
        assert_eq!((bd.calls().len(), lc.calls().len()), (b1, l1));
    }

    #[test]
    fn the_same_message_id_delivered_twice_applies_once() {
        let fx = Fx::new();
        let q = fx.question("operator", "X-Spira-Bead: sp-ask1\n");
        fx.reply(&q, "ans-1", "Yes.");
        fx.reply(&q, "ans-1", "Yes.");
        let bd = FakeBd::new(vec![BdOut::ok(""), BdOut::ok("[]")]);
        let lc = FakeLc::new(0);
        fx.pass(&bd, &lc);
        let closes = bd.calls().iter().filter(|c| c.0[0] == "close").count();
        assert_eq!(closes, 1, "{:?}", bd.calls());
        assert_eq!(lc.calls().len(), 1, "{:?}", lc.calls());
    }

    #[test]
    fn a_store_that_is_down_leaves_the_step_for_the_next_pass() {
        let fx = Fx::new();
        let q = fx.question("operator", "X-Spira-Bead: sp-ask1\n");
        fx.reply(&q, "ans-1", "Yes.");
        let down = FakeBd::new(vec![BdOut::fail(1, "store unreachable"), BdOut::fail(1, "store unreachable")]);
        let lc = FakeLc::new(0);
        let r = fx.pass(&down, &lc);
        assert_eq!((r.converged, r.pending, r.refused), (0, 1, 0));
        let up = FakeBd::new(vec![BdOut::ok(""), BdOut::ok("[]")]);
        let r = fx.pass(&up, &lc);
        assert_eq!((r.converged, r.pending), (1, 0));
        assert_eq!(up.calls()[0].0[..2], ["close".to_string(), "sp-ask1".to_string()]);
    }

    #[test]
    fn a_refused_close_leaves_a_note_and_exactly_one_inbox_row() {
        let fx = Fx::new();
        let q = fx.question("archivist", "X-Spira-Bead: sp-work1\n");
        fx.reply(&q, "ans-1", "Prove it on a copy.");
        let refuse = || BdOut::fail(1, "spira-lc close sp-work1 exited 3: a work bead in READY is not closed by a mail reply");
        let bd = FakeBd::new(vec![refuse(), BdOut::ok("{}"), BdOut::ok("")]);
        let lc = FakeLc::new(0);
        let r = fx.pass(&bd, &lc);
        assert_eq!((r.converged, r.pending, r.refused), (1, 0, 1));
        let calls = bd.calls();
        let note = calls.iter().find(|c| c.0[0] == "note").expect("the answer is noted");
        assert_eq!(note.0[1], "sp-work1");
        assert!(note.0[2].contains("Prove it on a copy."), "{note:?}");
        assert_eq!(fx.inbox(), 1);
        fx.pass(&bd, &lc);
        assert_eq!(fx.inbox(), 1, "the refusal is reported once");
    }

    #[test]
    fn a_reply_to_a_routed_question_notes_the_work_bead_and_closes_nothing() {
        let fx = Fx::new();
        let q = fx.question("concierge", "X-Spira-Work-Bead: sp-work1\n");
        let raw = format!("From: Concierge <concierge@spira>\nSubject: Re: May I?\nMessage-ID: <c-1@spira>\nIn-Reply-To: <{q}@spira>\n\nGo ahead.\n");
        sendmail(&fx.home(), &fx.root(), false, &raw).unwrap();
        let bd = FakeBd::new(vec![BdOut::ok("")]);
        let lc = FakeLc::new(0);
        fx.pass(&bd, &lc);
        let calls = bd.calls();
        assert!(!calls.iter().any(|c| c.0[0] == "close"), "{calls:?}");
        assert_eq!(calls[0].0[..2], ["note".to_string(), "sp-work1".to_string()]);
        assert_eq!(lc.calls()[0][..2], ["reply".to_string(), "sp-work1".to_string()]);
    }

    #[test]
    fn a_reply_to_a_notice_notes_its_bead_and_closes_nothing() {
        let fx = Fx::new();
        let root = fx.root();
        maildir::mail_ensure(&root.join("concierge")).unwrap();
        let msgid = maildir::mint_msgid();
        let orig = format!("From: Sentinel <sentinel@spira>\nSubject: poisoned\nX-Spira-Kind: alert\nX-Spira-Bead: sp-work1\nMessage-ID: <{msgid}@spira>\n\nchange the approach?\n");
        fs::write(root.join("concierge/tmp").join(&msgid), &orig).unwrap();
        maildir::mail_deliver(&root.join("concierge"), &msgid, false).unwrap();
        let raw = format!("From: Concierge <concierge@spira>\nSubject: Re: poisoned\nMessage-ID: <c-2@spira>\nIn-Reply-To: <{msgid}@spira>\n\nKeep going.\n");
        sendmail(&fx.home(), &fx.root(), false, &raw).unwrap();
        let bd = FakeBd::new(vec![BdOut::ok("")]);
        let lc = FakeLc::new(0);
        fx.pass(&bd, &lc);
        assert!(!bd.calls().iter().any(|c| c.0[0] == "close"));
        assert_eq!(bd.calls()[0].0[..2], ["note".to_string(), "sp-work1".to_string()]);
        assert!(lc.calls().is_empty());
    }

    #[test]
    fn an_empty_reply_body_closes_with_a_traceable_reason() {
        let fx = Fx::new();
        let q = fx.question("operator", "X-Spira-Bead: sp-dec1\n");
        fx.reply(&q, "ans-1", "");
        let bd = FakeBd::new(vec![BdOut::ok(""), BdOut::ok("[]")]);
        fx.pass(&bd, &FakeLc::new(0));
        let reason = bd.calls()[0].1.clone().unwrap();
        assert!(reason.contains(&q) && reason.contains("May I?"), "{reason}");
    }
}
