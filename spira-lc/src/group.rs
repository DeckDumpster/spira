//! Group commit (sp-yvyvzd): concurrent writes that reach `spira-lc serve` together commit in
//! ONE transaction instead of one each. Dolt fsyncs its chunk journal once per transaction
//! commit and has no server-side group commit, so on a slow disk every lifecycle write paid a
//! whole fsync and every read queued behind it; this is that group commit, on our side.
//!
//! The contract each caller keeps: its own result sets, its own success or failure.
//!
//!   - Members run in the order they arrived, one statement batch each, inside one
//!     `START TRANSACTION ... COMMIT`. A member sees the writes of the members before it —
//!     exactly what it would have seen had they committed one after another — so a
//!     compare-and-swap that would have lost to an earlier member still loses
//!     (`ROW_COUNT() = 0`, logged as refused), and `LAST_INSERT_ID()` is still its own.
//!   - Any member's failure, or a serialization failure at COMMIT, means NOTHING committed:
//!     the group is rolled back and every member is run again alone, the way it always ran,
//!     so each gets the answer it would have had without the group ([`Group::Retry`]).
//!   - A COMMIT whose reply is lost may or may not have committed: every member is told
//!     "cannot tell", never retried ([`Group::Unknown`]).
//!
//! Only a script that is one whole transaction (`START TRANSACTION; ... COMMIT;`, nothing
//! else transactional inside) can join a group; anything else runs alone ([`txn_body`]).
//! This module is pure: it is handed the statement executor, so it is tested without a server.

use serde_json::Value;

use crate::db::ScriptFailure;

pub type Sets = Vec<Vec<Value>>;

/// At most this many writes share one transaction: a group is bounded so one slow member
/// cannot hold many callers past their deadline.
pub const MAX_GROUP: usize = 32;

/// How long a write that finds the store idle waits for others to join it. Writes that arrive
/// while a commit is in flight queue anyway and form the next group without waiting.
pub const WINDOW: std::time::Duration = std::time::Duration::from_millis(3);

/// The statements between a whole-transaction script's `START TRANSACTION;` and its final
/// `COMMIT;`, or `None` when the script is not exactly one transaction and must run alone.
pub fn txn_body(script: &str) -> Option<&str> {
    let s = script.trim();
    let body = s.strip_prefix("START TRANSACTION;")?.strip_suffix("COMMIT;")?;
    let upper = body.to_ascii_uppercase();
    let transactional = ["START TRANSACTION", "BEGIN", "COMMIT", "ROLLBACK", "SAVEPOINT", "LOCK TABLES", "UNLOCK TABLES"];
    // A statement boundary followed by a transaction-control keyword, or one at the very start.
    let controls = upper
        .split(';')
        .map(|stmt| stmt.trim_start())
        .any(|stmt| transactional.iter().any(|kw| stmt.starts_with(kw) && stmt[kw.len()..].chars().next().is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')));
    if controls || body.trim().is_empty() {
        return None;
    }
    Some(body)
}

/// What became of a group.
#[derive(Debug)]
pub enum Group {
    /// One COMMIT applied every member: each member's own result sets, in member order.
    Committed(Vec<Sets>),
    /// Nothing committed; run each member alone. `drop_session` when the connection may be
    /// left mid-transaction (a ROLLBACK that itself failed).
    Retry { drop_session: bool },
    /// The COMMIT was sent and its answer lost: every member cannot tell.
    Unknown(String),
}

/// Runs `bodies` (each from [`txn_body`]) as one transaction through `exec`.
pub fn run(bodies: &[&str], mut exec: impl FnMut(&str) -> Result<Sets, ScriptFailure>) -> Group {
    if exec("START TRANSACTION").is_err() {
        return Group::Retry { drop_session: true };
    }
    let mut outs = Vec::with_capacity(bodies.len());
    for body in bodies {
        match exec(body) {
            Ok(sets) => outs.push(sets),
            Err(_) => {
                let rolled_back = exec("ROLLBACK").is_ok();
                return Group::Retry { drop_session: !rolled_back };
            }
        }
    }
    match exec("COMMIT") {
        Ok(_) => Group::Committed(outs),
        // 40001 at COMMIT: the server discarded the whole transaction (db.rs's module doc).
        Err(ScriptFailure::LostRace) => Group::Retry { drop_session: false },
        Err(ScriptFailure::CannotTell(e)) => Group::Unknown(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W1: &str = "START TRANSACTION;\nUPDATE bead SET v=1 WHERE id='a';\nSELECT 1 AS applied;\nCOMMIT;\n";
    const W2: &str = "START TRANSACTION;\nINSERT INTO fact VALUES (2);\nCOMMIT;\n";

    fn row(v: &str) -> Sets {
        vec![vec![serde_json::json!({ "applied": v })]]
    }

    #[test]
    fn a_whole_transaction_script_yields_its_body_and_nothing_else_does() {
        assert_eq!(txn_body(W1), Some("\nUPDATE bead SET v=1 WHERE id='a';\nSELECT 1 AS applied;\n"));
        assert!(txn_body(W2).is_some());
        assert_eq!(txn_body("INSERT INTO fact VALUES (1);"), None, "autocommit statements run alone");
        assert_eq!(txn_body("START TRANSACTION;\nCOMMIT;\n"), None, "an empty transaction runs alone");
        assert_eq!(txn_body("START TRANSACTION;\nINSERT INTO a VALUES (1);\nCOMMIT;\nINSERT INTO b VALUES (2);\n"), None, "work after the COMMIT");
        assert_eq!(txn_body("START TRANSACTION;\nINSERT INTO a VALUES (1);\nCOMMIT;\nSTART TRANSACTION;\nINSERT INTO b VALUES (2);\nCOMMIT;\n"), None, "two transactions");
        assert_eq!(txn_body("START TRANSACTION;\nSAVEPOINT x;\nINSERT INTO a VALUES (1);\nCOMMIT;\n"), None);
        assert_eq!(txn_body("START TRANSACTION;\n rollback;\nCOMMIT;\n"), None, "case-insensitive");
        assert!(txn_body("START TRANSACTION;\nUPDATE bead SET reason='commit message' WHERE id='a';\nCOMMIT;\n").is_some(), "a keyword inside a value is not control");
        assert!(txn_body("START TRANSACTION;\nINSERT INTO committee VALUES (1);\nCOMMIT;\n").is_some(), "a word that merely starts with a keyword");
    }

    #[test]
    fn n_members_commit_in_one_transaction_each_with_its_own_answer() {
        let mut sent = Vec::new();
        let bodies = [txn_body(W1).unwrap(), txn_body(W2).unwrap(), txn_body(W1).unwrap()];
        let mut n = 0;
        let g = run(&bodies, |sql| {
            sent.push(sql.to_string());
            if sql.contains("UPDATE") || sql.contains("INSERT") {
                n += 1;
                Ok(row(&n.to_string()))
            } else {
                Ok(vec![])
            }
        });
        let Group::Committed(outs) = g else { panic!("{g:?}") };
        assert_eq!(outs, vec![row("1"), row("2"), row("3")], "each member's own result sets, in member order");
        assert_eq!(sent.iter().filter(|s| *s == "COMMIT").count(), 1, "three writes, one commit: {sent:?}");
        assert_eq!(sent.iter().filter(|s| *s == "START TRANSACTION").count(), 1);
        assert_eq!((sent.first().map(String::as_str), sent.last().map(String::as_str)), (Some("START TRANSACTION"), Some("COMMIT")));
    }

    #[test]
    fn a_failing_member_rolls_back_the_group_and_every_member_reruns_alone() {
        let mut sent = Vec::new();
        let bodies = [txn_body(W1).unwrap(), txn_body(W2).unwrap()];
        let g = run(&bodies, |sql| {
            sent.push(sql.to_string());
            if sql.contains("INSERT") { Err(ScriptFailure::CannotTell("Error 1062: duplicate".into())) } else { Ok(vec![]) }
        });
        assert!(matches!(g, Group::Retry { drop_session: false }), "{g:?}");
        assert_eq!(sent.last().map(String::as_str), Some("ROLLBACK"));
        assert!(!sent.iter().any(|s| s == "COMMIT"), "nothing committed: {sent:?}");
    }

    #[test]
    fn a_rollback_that_fails_drops_the_session() {
        let g = run(&[txn_body(W2).unwrap()], |sql| if sql == "START TRANSACTION" { Ok(vec![]) } else { Err(ScriptFailure::CannotTell("broken pipe".into())) });
        assert!(matches!(g, Group::Retry { drop_session: true }), "{g:?}");
    }

    #[test]
    fn a_serialization_failure_at_commit_reruns_alone_and_a_lost_commit_reply_cannot_tell() {
        let raced = run(&[txn_body(W1).unwrap(), txn_body(W2).unwrap()], |sql| if sql == "COMMIT" { Err(ScriptFailure::LostRace) } else { Ok(vec![]) });
        assert!(matches!(raced, Group::Retry { drop_session: false }), "{raced:?}");
        let lost = run(&[txn_body(W1).unwrap(), txn_body(W2).unwrap()], |sql| if sql == "COMMIT" { Err(ScriptFailure::CannotTell("deadline".into())) } else { Ok(vec![]) });
        assert!(matches!(lost, Group::Unknown(ref e) if e == "deadline"), "a COMMIT whose reply is lost is never retried: {lost:?}");
    }
}
