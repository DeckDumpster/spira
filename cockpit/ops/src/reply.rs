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
    /// Send `text` as the mail reply to the open ask on `id`; true when it was sent, which is
    /// what lifts the hold. False when the bead has no ask mail to answer.
    fn answer_ask(&self, id: &str, text: &str) -> bool;
    fn deliver(&self, id: &str, text: &str);
}

pub enum Outcome {
    Replied(String),
    Failed(String),
}

pub fn usage_error(id: &str, text: &str) -> bool {
    id.is_empty() || text.is_empty()
}

pub const USAGE: &str = "usage: reply <bead-id> \"<text>\"   (or - to read stdin)";

pub fn run(id: &str, text: &str, db: &Path, commenter: &dyn Commenter, follow: &dyn Follow) -> Outcome {
    let r = commenter.comment(db, id, text);
    if r.success {
        if !follow.answer_ask(id, text) {
            let message_id = format!("answer-{id}");
            if let Err(e) = follow.lift_hold(id, &message_id) {
                eprintln!("reply: ask hold on {id} not lifted: {e}");
            }
        }
        follow.deliver(id, text);
    }
    let db_name = db
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if r.success {
        Outcome::Replied(format!("replied on {id} ({db_name})"))
    } else {
        // Unlike the bash original, bd's own output is included — see ../DESIGN.md
        // Decisions.
        let mut msg = format!("reply: failed to comment on {id} in {}\n", db.display());
        for line in r.combined.lines() {
            msg.push_str("  bd: ");
            msg.push_str(line);
            msg.push('\n');
        }
        msg.pop();
        Outcome::Failed(msg)
    }
}

pub struct AskMail {
    pub message_id: String,
    pub subject: String,
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
    let (mut work, mut kind, mut message_id, mut subject) = (String::new(), String::new(), String::new(), String::new());
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
            "message-id" => message_id = value.trim_matches(|c| c == '<' || c == '>').to_string(),
            "subject" => subject = value,
            _ => {}
        }
    }
    let asks = kind == "question" || kind == "decision";
    (work == id && asks && !message_id.is_empty()).then_some(AskMail { message_id, subject })
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
    }
    impl Follow for Rec {
        fn lift_hold(&self, id: &str, m: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("lift {id} {m}"));
            Ok(())
        }
        fn answer_ask(&self, id: &str, _text: &str) -> bool {
            self.calls.borrow_mut().push(format!("answer {id}"));
            self.has_ask
        }
        fn deliver(&self, id: &str, text: &str) {
            self.calls.borrow_mut().push(format!("deliver {id} {text}"));
        }
    }

    #[test]
    fn an_answer_lifts_the_hold_and_reaches_the_holder_in_one_call() {
        let c = Fake { success: true, combined: "" };
        let f = Rec::default();
        run("sp-abc", "use A", Path::new("/db"), &c, &f);
        assert_eq!(*f.calls.borrow(), vec!["answer sp-abc", "lift sp-abc answer-sp-abc", "deliver sp-abc use A"]);
    }

    #[test]
    fn an_answer_to_an_open_ask_is_the_mail_reply_and_needs_no_second_lift() {
        let c = Fake { success: true, combined: "" };
        let f = Rec { has_ask: true, ..Rec::default() };
        run("sp-abc", "use A", Path::new("/db"), &c, &f);
        assert_eq!(*f.calls.borrow(), vec!["answer sp-abc", "deliver sp-abc use A"]);
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
        run("sp-abc", "use A", Path::new("/db"), &c, &f);
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
        let out = run("sp-abc", "an answer", Path::new("/var/spira/db"), &c, &Rec::default());
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
        let out = run("sp-abc", "an answer", Path::new("/db"), &c, &Rec::default());
        match out {
            Outcome::Failed(s) => {
                assert!(s.starts_with("reply: failed to comment on sp-abc in /db\n"));
                assert!(s.contains("  bd: refused: no such bead"));
            }
            Outcome::Replied(s) => panic!("expected Failed, got Replied({s})"),
        }
    }
}
