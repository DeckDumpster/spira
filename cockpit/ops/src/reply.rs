//! reply — answer the operator inside a bead's own comment thread.
//!
//! Replaces `cockpit/reply.sh` (sp-llbmi). See ../DESIGN.md.
//!
//! Never in the chat transcript, never by filing a new bead (the operator's own words: *"if i
//! add a comment to a bead in the beads pane, i don't want you to respond here then open
//! another bead... you're just creating more work and spreading out the context... the
//! conversation in the beads pane is already threaded"*). Forking a question into a new bead
//! once caused the same question to be answered three times over. The actor is fixed at
//! `claude`, distinct from the pane's `$SPIRA_OPERATOR_ACTOR`, so the answer-watcher can tell
//! the agent's replies from the operator's own.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

pub struct BdResult {
    pub success: bool,
    pub combined: String,
}

pub trait Commenter {
    fn comment(&self, db: &Path, id: &str, text: &str) -> BdResult;
}

/// What an answer does beyond recording itself: lift the ask hold, reach the live holder.
pub trait Follow {
    fn lift_hold(&self, id: &str, message_id: &str) -> Result<(), String>;
    fn open_ask(&self, id: &str) -> Option<AskMail>;
    /// Send `text` as the mail reply to `ask`; true when it was sent, which is what lifts the
    /// hold.
    fn answer_ask(&self, ask: &AskMail, text: &str) -> bool;
    /// Deliver `text` to the operator's mailbox in the ask's own thread, so what the operator
    /// reads is what the store holds.
    fn copy_to_operator(&self, ask: &AskMail, text: &str, default: Option<&str>) -> Result<(), String>;
    fn deliver(&self, id: &str, text: &str);
}

pub enum Outcome {
    Replied(String),
    Failed(String),
}

pub fn usage_error(id: &str, text: &str) -> bool {
    id.is_empty() || text.is_empty()
}

pub const USAGE: &str = "usage: reply <bead-id> \"<text>\" [--default \"<new default>\"]   (text - reads stdin)";

pub fn run(id: &str, text: &str, default: Option<&str>, db: &Path, commenter: &dyn Commenter, follow: &dyn Follow) -> Outcome {
    let ask = follow.open_ask(id);
    if default.is_some() && ask.is_none() {
        return Outcome::Failed(format!("reply: --default revises an open ask, and {id} has none"));
    }
    let r = commenter.comment(db, id, text);
    let db_name = db
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !r.success {
        // Unlike the bash original, bd's own output is included — see ../DESIGN.md
        // Decisions.
        let mut msg = format!("reply: failed to comment on {id} in {}\n", db.display());
        for line in r.combined.lines() {
            msg.push_str("  bd: ");
            msg.push_str(line);
            msg.push('\n');
        }
        msg.pop();
        return Outcome::Failed(msg);
    }
    let mut failures = Vec::new();
    if let (Some(d), Some(ask)) = (default, &ask) {
        if !ask.bead.is_empty() {
            let w = commenter.comment(db, &ask.bead, &format!("Default: {d}"));
            if !w.success {
                failures.push(format!("default not recorded on {}: {}", ask.bead, w.combined.trim()));
            }
        }
    }
    match &ask {
        Some(ask) => {
            if default.is_none() && !follow.answer_ask(ask, text) {
                let message_id = format!("answer-{id}");
                if let Err(e) = follow.lift_hold(id, &message_id) {
                    eprintln!("reply: ask hold on {id} not lifted: {e}");
                }
            }
            if let Err(e) = follow.copy_to_operator(ask, text, default) {
                failures.push(format!("not delivered to the operator's mailbox: {e}"));
            }
        }
        None => {
            if let Err(e) = follow.lift_hold(id, &format!("answer-{id}")) {
                eprintln!("reply: ask hold on {id} not lifted: {e}");
            }
        }
    }
    follow.deliver(id, text);
    if failures.is_empty() {
        Outcome::Replied(format!("replied on {id} ({db_name})"))
    } else {
        Outcome::Failed(format!("reply: recorded on {id} ({db_name}) but {}", failures.join("; ")))
    }
}

pub struct AskMail {
    pub message_id: String,
    pub subject: String,
    pub bead: String,
}

const ASK_MAILBOXES: [&str; 2] = ["operator", "concierge"];

/// The newest question or decision mail in the operator's or Concierge's box about work bead `id`.
pub fn find_open_ask(mail_root: &Path, id: &str) -> Option<AskMail> {
    let mut best: Option<(std::time::SystemTime, AskMail)> = None;
    for mailbox in ASK_MAILBOXES {
        for sub in ["new", "cur"] {
            let Ok(dir) = fs::read_dir(mail_root.join(mailbox).join(sub)) else { continue };
            for entry in dir.flatten() {
                let Some(ask) = read_ask(&entry.path(), id) else { continue };
                let at = entry.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                if best.as_ref().is_none_or(|(t, _)| at > *t) {
                    best = Some((at, ask));
                }
            }
        }
    }
    best.map(|(_, a)| a)
}

fn read_ask(path: &Path, id: &str) -> Option<AskMail> {
    let f = fs::File::open(path).ok()?;
    let (mut work, mut kind, mut message_id, mut subject, mut bead) = (String::new(), String::new(), String::new(), String::new(), String::new());
    for line in BufReader::new(f).lines() {
        let line = line.ok()?;
        if line.trim().is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else { continue };
        let value = value.trim().to_string();
        match name.to_ascii_lowercase().as_str() {
            "x-spira-work-bead" => work = value,
            "x-spira-kind" => kind = value,
            "x-spira-bead" => bead = value,
            "message-id" => message_id = value.trim_matches(|c| c == '<' || c == '>').to_string(),
            "subject" => subject = value,
            _ => {}
        }
    }
    let asks = kind == "question" || kind == "decision";
    (work == id && asks && !message_id.is_empty()).then_some(AskMail { message_id, subject, bead })
}

/// The RFC 5322 reply `mail sendmail` reads: threaded to the ask by `In-Reply-To`, which is
/// what makes sendmail lift the hold and deliver to the asker.
pub fn answer_message(ask: &AskMail, text: &str, answer_id: &str) -> String {
    let subject = ask.subject.strip_prefix("Re: ").unwrap_or(&ask.subject);
    format!(
        "From: claude <claude@spira>\nSubject: Re: {subject}\nMessage-ID: <{answer_id}@spira>\nIn-Reply-To: <{}>\n\n{text}\n",
        ask.message_id
    )
}

/// The answer addressed to the operator, which never closes the ask (a revision is not his decision): threaded to the ask, subject marked `Revised:`,
/// and carrying the new default as `X-Spira-Default` so accepting it sends the revised value.
pub fn operator_copy(ask: &AskMail, text: &str, default: Option<&str>, copy_id: &str) -> String {
    let subject = ask.subject.trim_start_matches("Re: ").trim_start_matches("Revised: ");
    let (header, line) = match default {
        Some(d) => (format!("X-Spira-Default: {}\n", d.split_whitespace().collect::<Vec<_>>().join(" ")), format!("\nDefault is now: {d}\n")),
        None => (String::new(), String::new()),
    };
    format!(
        "From: claude <claude@spira>\nSubject: Revised: {subject}\nX-Spira-Kind: revision\n{header}Message-ID: <{copy_id}@spira>\nIn-Reply-To: <{}>\n\n{text}\n{line}",
        ask.message_id
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        success: bool,
        combined: &'static str,
    }
    impl Commenter for Fake {
        fn comment(&self, _db: &Path, _id: &str, _text: &str) -> BdResult {
            BdResult {
                success: self.success,
                combined: self.combined.to_string(),
            }
        }
    }

    #[derive(Default)]
    struct Rec {
        calls: std::cell::RefCell<Vec<String>>,
        has_ask: bool,
        copy_fails: bool,
    }
    impl Follow for Rec {
        fn lift_hold(&self, id: &str, m: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("lift {id} {m}"));
            Ok(())
        }
        fn open_ask(&self, _id: &str) -> Option<AskMail> {
            self.has_ask.then(|| AskMail { message_id: "m@spira".into(), subject: "q".into(), bead: "sp-ask".into() })
        }
        fn answer_ask(&self, _ask: &AskMail, _text: &str) -> bool {
            self.calls.borrow_mut().push("answer".into());
            true
        }
        fn copy_to_operator(&self, _ask: &AskMail, _text: &str, default: Option<&str>) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("copy {default:?}"));
            if self.copy_fails { Err("boom".into()) } else { Ok(()) }
        }
        fn deliver(&self, id: &str, text: &str) {
            self.calls.borrow_mut().push(format!("deliver {id} {text}"));
        }
    }

    #[test]
    fn an_answer_lifts_the_hold_and_reaches_the_holder_in_one_call() {
        let c = Fake { success: true, combined: "" };
        let f = Rec::default();
        run("sp-abc", "use A", None, Path::new("/db"), &c, &f);
        assert_eq!(*f.calls.borrow(), vec!["lift sp-abc answer-sp-abc", "deliver sp-abc use A"]);
    }

    #[test]
    fn an_answer_to_an_open_ask_is_the_mail_reply_and_is_copied_to_the_operator() {
        let c = Fake { success: true, combined: "" };
        let f = Rec { has_ask: true, ..Rec::default() };
        run("sp-abc", "use A", None, Path::new("/db"), &c, &f);
        assert_eq!(*f.calls.borrow(), vec!["answer", "copy None", "deliver sp-abc use A"]);
    }

    #[test]
    fn a_revised_default_is_mailed_to_the_operator_without_answering_the_ask() {
        let c = Fake { success: true, combined: "" };
        let f = Rec { has_ask: true, ..Rec::default() };
        let out = run("sp-abc", "use A", Some("B"), Path::new("/db"), &c, &f);
        assert!(matches!(out, Outcome::Replied(_)));
        assert_eq!(*f.calls.borrow(), vec!["copy Some(\"B\")", "deliver sp-abc use A"]);
    }

    #[test]
    fn a_failed_mail_delivery_fails_the_reply_after_the_store_write() {
        let c = Fake { success: true, combined: "" };
        let f = Rec { has_ask: true, copy_fails: true, ..Rec::default() };
        match run("sp-abc", "use A", None, Path::new("/db"), &c, &f) {
            Outcome::Failed(s) => assert!(s.contains("not delivered to the operator's mailbox: boom"), "{s}"),
            Outcome::Replied(s) => panic!("swallowed: {s}"),
        }
    }

    #[test]
    fn a_default_with_no_open_ask_is_refused_before_anything_is_written() {
        let c = Fake { success: true, combined: "" };
        let f = Rec::default();
        assert!(matches!(run("sp-abc", "x", Some("B"), Path::new("/db"), &c, &f), Outcome::Failed(_)));
        assert!(f.calls.borrow().is_empty());
    }

    #[test]
    fn the_operator_copy_is_threaded_revised_and_states_the_default() {
        let ask = AskMail { message_id: "m-ask@spira".into(), subject: "Re: which way?".into(), bead: "sp-ask".into() };
        let msg = operator_copy(&ask, "ok", Some("B"), "rev-1");
        assert!(msg.contains("In-Reply-To: <m-ask@spira>\n"), "{msg}");
        assert!(msg.contains("Subject: Revised: which way?\n"), "{msg}");
        assert!(msg.contains("X-Spira-Default: B\n") && msg.contains("Default is now: B"), "{msg}");
    }

    fn put(root: &Path, mailbox: &str, name: &str, headers: &str) {
        let dir = root.join(mailbox).join("new");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(name), format!("{headers}\nbody\n")).unwrap();
    }

    #[test]
    fn the_ask_found_is_the_question_about_that_bead_and_the_reply_threads_to_it() {
        let d = testkit::TempDir::new("reply-ask");
        put(d.path(), "operator", "a", "Subject: other\nX-Spira-Kind: question\nX-Spira-Work-Bead: sp-zzz\nMessage-ID: <m-other@spira>\n");
        put(d.path(), "operator", "b", "Subject: note\nX-Spira-Kind: note\nX-Spira-Work-Bead: sp-abc\nMessage-ID: <m-note@spira>\n");
        assert!(find_open_ask(d.path(), "sp-abc").is_none(), "a note is not an ask");
        put(d.path(), "concierge", "c", "Subject: which way?\nX-Spira-Kind: decision\nX-Spira-Work-Bead: sp-abc\nMessage-ID: <m-ask@spira>\n");
        let ask = find_open_ask(d.path(), "sp-abc").unwrap();
        assert_eq!(ask.message_id, "m-ask@spira");
        let msg = answer_message(&ask, "use A", "ans-1");
        assert!(msg.contains("In-Reply-To: <m-ask@spira>\n"), "{msg}");
        assert!(msg.contains("Subject: Re: which way?\n"), "{msg}");
        assert!(msg.ends_with("\n\nuse A\n"), "{msg}");
    }

    #[test]
    fn a_failed_comment_neither_lifts_nor_delivers() {
        let c = Fake { success: false, combined: "no" };
        let f = Rec::default();
        run("sp-abc", "use A", None, Path::new("/db"), &c, &f);
        assert!(f.calls.borrow().is_empty());
    }

    #[test]
    fn usage_error_when_id_or_text_missing() {
        assert!(usage_error("", "hi"));
        assert!(usage_error("sp-1", ""));
        assert!(!usage_error("sp-1", "hi"));
    }

    #[test]
    fn success_message_names_id_and_db_basename() {
        let c = Fake { success: true, combined: "" };
        let out = run("sp-abc", "an answer", None, Path::new("/var/spira/db"), &c, &Rec::default());
        match out {
            Outcome::Replied(s) => assert_eq!(s, "replied on sp-abc (db)"),
            Outcome::Failed(s) => panic!("expected Replied, got Failed({s})"),
        }
    }

    #[test]
    fn failure_message_includes_bds_combined_output() {
        let c = Fake {
            success: false,
            combined: "refused: no such bead",
        };
        let out = run("sp-abc", "an answer", None, Path::new("/db"), &c, &Rec::default());
        match out {
            Outcome::Failed(s) => {
                assert!(s.starts_with("reply: failed to comment on sp-abc in /db\n"));
                assert!(s.contains("  bd: refused: no such bead"));
            }
            Outcome::Replied(s) => panic!("expected Failed, got Replied({s})"),
        }
    }
}
