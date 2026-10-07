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
        let message_id = format!("answer-{id}");
        if let Err(e) = follow.lift_hold(id, &message_id) {
            eprintln!("reply: ask hold on {id} not lifted: {e}");
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
    }
    impl Follow for Rec {
        fn lift_hold(&self, id: &str, m: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("lift {id} {m}"));
            Ok(())
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
        assert_eq!(*f.calls.borrow(), vec!["lift sp-abc answer-sp-abc", "deliver sp-abc use A"]);
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
