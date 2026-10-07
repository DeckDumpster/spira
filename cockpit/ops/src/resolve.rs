//! resolve — close a bead the agent established or did itself, without paging the operator.
//!
//! Replaces `cockpit/resolve.sh` (sp-llbmi). See ../DESIGN.md.
//!
//! The operator's own call: *"for beads that are machine checkable... just go machine check
//! them. i don't need to take action if you can. this is perfectly acceptable for
//! non-destructive actions"* (law-check-it-yourself-before-asking). `--force`: rig beads are
//! usually assigned to the mayor, and closing one as `claude` is refused otherwise — the
//! assignee check is right for work, and this is a verdict on an ask, not work reassignment.
//! Reasons carry their evidence: a close that says "done" is a claim with nothing behind it.

use std::path::Path;

pub struct BdResult {
    pub success: bool,
    pub combined: String,
}

pub trait Closer {
    fn close(&self, db: &Path, id: &str, reason: &str) -> BdResult;
    /// `bd show <id> --json`'s text, for the `work-bead:` labels the ask carries.
    fn show_json(&self, db: &Path, id: &str) -> String;
    /// `spira-lc withdraw-ask <work-bead> claude` → its exit code and output.
    fn withdraw_ask(&self, work_bead: &str) -> (i32, String);
}

/// The work beads an ask names on its `work-bead:<id>` labels (written by `mail send` on
/// the tracking bead of a question about a work bead), read out of `bd show --json`.
pub fn work_beads(show_json: &str) -> Vec<String> {
    let re = regex::Regex::new(r#""work-bead:([A-Za-z0-9][A-Za-z0-9.\-]*)""#).expect("static regex");
    let mut out: Vec<String> = re.captures_iter(show_json).map(|c| c[1].to_string()).collect();
    out.dedup();
    out
}

pub enum Outcome {
    Closed(String),
    Failed(String),
}

pub fn usage_error(id: &str, reason: &str) -> bool {
    id.is_empty() || reason.is_empty()
}

pub const USAGE: &str = "usage: resolve <bead-id> \"<reason with evidence>\"   (or - to read stdin)";

pub fn run(id: &str, reason: &str, db: &Path, closer: &dyn Closer) -> Outcome {
    let r = closer.close(db, id, reason);
    let db_name = db
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if r.success {
        // A RESOLVED ASK IS A QUESTION CLOSED WITHOUT THE OPERATOR'S ANSWER (sp-v62vn
        // follow-up): the agent established it, or moot-sweep found its condition cleared.
        // `work ask` held the asking bead and only a Reply or an AskWithdrawn lifts it, so a
        // resolve emits withdraw-ask for every work bead the ask names. Exit 1 (no row) and 3
        // (no ask hold) are not failures; a cannot-tell fails the resolve: the ask is closed but the bead is still held.
        let mut msg = format!("resolved {id} ({db_name})");
        let mut unlifted = false;
        for w in work_beads(&closer.show_json(db, id)) {
            match closer.withdraw_ask(&w) {
                (0, _) => msg.push_str(&format!("\nwithdrew the ask hold on {w}")),
                (1, _) | (3, _) => {}
                (code, out) => {
                    unlifted = true;
                    msg.push_str(&format!("\nFAILED: the ask hold on {w} was not lifted (spira-lc withdraw-ask exit {code}): {}", out.trim()));
                }
            }
        }
        if unlifted {
            return Outcome::Failed(msg);
        }
        Outcome::Closed(msg)
    } else {
        // Unlike the bash original, bd's own output is included in the returned message
        // rather than printed separately — see ../DESIGN.md Decisions.
        let mut msg = format!("resolve: failed to close {id} in {}\n", db.display());
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
    impl Closer for Fake {
        fn close(&self, _db: &Path, _id: &str, _reason: &str) -> BdResult {
            BdResult { success: self.success, combined: self.combined.to_string() }
        }
        fn show_json(&self, _db: &Path, _id: &str) -> String {
            String::new()
        }
        fn withdraw_ask(&self, _w: &str) -> (i32, String) {
            panic!("no work bead, no withdraw")
        }
    }

    struct AskFake {
        labels: &'static str,
        code: i32,
        withdrawn: std::cell::RefCell<Vec<String>>,
    }
    impl Closer for AskFake {
        fn close(&self, _db: &Path, _id: &str, _reason: &str) -> BdResult {
            BdResult { success: true, combined: String::new() }
        }
        fn show_json(&self, _db: &Path, _id: &str) -> String {
            format!(r#"[{{"id":"sp-ask1","labels":[{}]}}]"#, self.labels)
        }
        fn withdraw_ask(&self, w: &str) -> (i32, String) {
            self.withdrawn.borrow_mut().push(w.to_string());
            (self.code, "cannot tell: socket".into())
        }
    }

    /// THE GAP THIS CLOSES (sp-v62vn follow-up): resolving an ask about a work bead —
    /// the question closed without the operator's answer — withdraws that bead's ask hold.
    #[test]
    fn resolving_an_ask_about_a_work_bead_withdraws_its_ask_hold() {
        let c = AskFake { labels: r#""overseer","work-bead:sp-w1""#, code: 0, withdrawn: Default::default() };
        let Outcome::Closed(s) = run("sp-ask1", "moot", Path::new("/db"), &c) else { panic!() };
        assert_eq!(*c.withdrawn.borrow(), vec!["sp-w1".to_string()]);
        assert!(s.contains("withdrew the ask hold on sp-w1"), "{s}");
    }

    #[test]
    fn a_refused_withdraw_is_quiet_and_a_cannot_tell_is_reported() {
        let c = AskFake { labels: r#""work-bead:sp-w1""#, code: 3, withdrawn: Default::default() };
        let Outcome::Closed(s) = run("sp-ask1", "moot", Path::new("/db"), &c) else { panic!() };
        assert_eq!(s, "resolved sp-ask1 (db)");
        let c = AskFake { labels: r#""work-bead:sp-w1""#, code: 2, withdrawn: Default::default() };
        let Outcome::Failed(s) = run("sp-ask1", "moot", Path::new("/db"), &c) else { panic!("a hold left in place is a failed resolve") };
        assert!(s.contains("FAILED: the ask hold on sp-w1 was not lifted"), "{s}");
        let c = AskFake { labels: r#""overseer""#, code: 0, withdrawn: Default::default() };
        run("sp-ask1", "moot", Path::new("/db"), &c);
        assert!(c.withdrawn.borrow().is_empty());
    }

    #[test]
    fn usage_error_when_id_or_reason_missing() {
        assert!(usage_error("", "why"));
        assert!(usage_error("sp-1", ""));
        assert!(!usage_error("sp-1", "why"));
    }

    #[test]
    fn success_message_names_id_and_db_basename() {
        let c = Fake { success: true, combined: "" };
        let out = run("sp-abc", "did the thing, see diff", Path::new("/var/spira/db"), &c);
        match out {
            Outcome::Closed(s) => assert_eq!(s, "resolved sp-abc (db)"),
            Outcome::Failed(s) => panic!("expected Closed, got Failed({s})"),
        }
    }

    #[test]
    fn failure_message_includes_bds_combined_output() {
        // The schema-mismatch scar this guards: bd's refusal text must survive, not just
        // a generic "failed to close" line.
        let c = Fake { success: false, combined: "refused: schema mismatch on write" };
        let out = run("sp-abc", "why", Path::new("/db"), &c);
        match out {
            Outcome::Failed(s) => {
                assert!(s.starts_with("resolve: failed to close sp-abc in /db\n"));
                assert!(s.contains("  bd: refused: schema mismatch on write"));
            }
            Outcome::Closed(s) => panic!("expected Failed, got Closed({s})"),
        }
    }
}
