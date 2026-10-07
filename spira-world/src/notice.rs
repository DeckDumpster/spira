//! Deadline notices to live aeons across a `world.sh stop`/`drain`.
//!
//! The notice rides the same channel `bead.sh amend` uses: a message into the aeon's
//! mailbox, which its PostToolUse hook hands to the working session.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;

/// The second notice goes out once this many seconds remain.
pub const SECOND_AT_SECS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    First,
    Second,
}

pub fn message(stage: Stage, what: &str, remaining_secs: u64, deadline_utc: &str) -> String {
    let lead = match stage {
        Stage::First => format!("The world is {what}."),
        Stage::Second => format!("Reminder: the world is {what}."),
    };
    format!(
        "{lead} You have {}m {}s, until {deadline_utc}.\n\n\
         - If your work fits in that time: finish it and `work submit`.\n\
         - Otherwise: commit a WIP checkpoint on your branch, leave a bead note \
         (`work note`) saying where you stopped and what is next, and exit cleanly. \
         The bead is released to READY, no attempt is charged, and your branch is kept.\n",
        remaining_secs / 60,
        remaining_secs % 60
    )
}

/// Which notices are due at `elapsed` of `limit` seconds, tracking who already has them.
#[derive(Default)]
pub struct Notifier {
    first: BTreeSet<String>,
    second: BTreeSet<String>,
}

impl Notifier {
    /// Calls `send(bead, stage, remaining)` for each live bead owed a notice. The second
    /// notice is skipped when the first already went out inside its window.
    pub fn tick(&mut self, elapsed: u64, limit: u64, live: &[String], mut send: impl FnMut(&str, Stage, u64)) {
        let remaining = limit.saturating_sub(elapsed);
        for b in live {
            if self.first.insert(b.clone()) {
                send(b, Stage::First, remaining);
                if remaining <= SECOND_AT_SECS {
                    self.second.insert(b.clone());
                }
            } else if remaining <= SECOND_AT_SECS && self.second.insert(b.clone()) {
                send(b, Stage::Second, remaining);
            }
        }
    }
}

/// Mails `bead`'s live aeon. False when it has no open mailbox or the send failed.
pub fn send(mail_root: &Path, bead: &str, body: &str) -> bool {
    if !mail_root.join(format!("aeon-{bead}")).join("new").is_dir() {
        return false;
    }
    let Ok(mut child) = spira_config::bounded::bounded("mail")
        .args(["send", &format!("aeon-{bead}"), "--from", "world <world@spira>", "--subject", "Deadline: wrap up or checkpoint"])
        .env("SPIRA_MAIL_LINT_CONSIDERED", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(body.as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn beads(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn run(limit: u64, live: &[&str], at: &[u64]) -> Vec<(String, Stage, u64)> {
        let mut n = Notifier::default();
        let mut got = Vec::new();
        for &t in at {
            n.tick(t, limit, &beads(live), |b, s, r| got.push((b.to_string(), s, r)));
        }
        got
    }

    #[test]
    fn both_notices_carry_the_remaining_time() {
        let got = run(1800, &["sp-a"], &[0, 10, 1490, 1500, 1510]);
        assert_eq!(got, vec![("sp-a".into(), Stage::First, 1800), ("sp-a".into(), Stage::Second, 310 - 10)]);
    }

    #[test]
    fn no_live_aeons_sends_nothing() {
        assert!(run(1800, &[], &[0, 1500]).is_empty());
    }

    #[test]
    fn a_short_window_gets_one_notice() {
        assert_eq!(run(240, &["sp-a"], &[0, 10, 200]).len(), 1);
    }

    #[test]
    fn message_names_the_deadline_and_both_choices() {
        let m = message(Stage::First, "draining", 1800, "12:30:00Z");
        assert!(m.contains("30m 0s") && m.contains("12:30:00Z"));
        assert!(m.contains("work submit") && m.contains("WIP checkpoint") && m.contains("work note"));
        assert!(message(Stage::Second, "draining", 300, "x").starts_with("Reminder"));
    }
}
