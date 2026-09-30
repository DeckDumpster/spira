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
        Outcome::Closed(format!("resolved {id} ({db_name})"))
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
