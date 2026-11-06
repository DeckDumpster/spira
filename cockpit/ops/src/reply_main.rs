//! `reply <bead-id> "<text>"` — answer the operator inside a bead's own comment thread.
//! See cockpit-ops's DESIGN.md and src/reply.rs.

use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

use cockpit_ops::db;
use cockpit_ops::reply::{answer_message, find_open_ask, operator_copy, run, usage_error, AskMail, BdResult, Commenter, Follow, Outcome, USAGE};

fn unique() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{}-{nanos}", std::process::id())
}

/// `mail sendmail [--to]`, delivered in process by the mail library.
fn sendmail(raw: &str, to: Option<&str>) -> Result<(), String> {
    cockpit_ops::lcio::sendmail(raw, to)
}

struct RealBd;

impl Commenter for RealBd {
    /// Through the store's content door, in-process, which applies every protection bdq
    /// applies. The comment's author is the store's: BEADS_ACTOR does not cross the socket, as
    /// it did not when this ran the `spira-lc` CLI, which forwarded there.
    fn comment(&self, _db: &Path, id: &str, text: &str) -> BdResult {
        match cockpit_ops::lcio::client().map(|c| c.call(&["content", "comments", "add", id, text])) {
            Ok(Ok(r)) => {
                let mut combined = r.out;
                if !r.err.is_empty() {
                    if !combined.is_empty() && !combined.ends_with('\n') {
                        combined.push('\n');
                    }
                    combined.push_str(&r.err);
                }
                BdResult { success: r.code == 0, combined }
            }
            Ok(Err(e)) => BdResult { success: false, combined: e.to_string() },
            Err(e) => BdResult { success: false, combined: e },
        }
    }
}

struct RealFollow;

impl Follow for RealFollow {
    fn lift_hold(&self, id: &str, message_id: &str) -> Result<(), String> {
        // FORK-EDGE: unserved verb reply (a callers.rs verb the socket does not serve).
        let out = spira_config::bounded::bounded("spira-lc")
            .args(["reply", id, message_id, "claude"])
            .output()
            .map_err(|e| format!("failed to run spira-lc: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    fn open_ask(&self, id: &str) -> Option<AskMail> {
        let mail_root = spira_config::process::cfg("SPIRA_MAIL").unwrap_or_default();
        find_open_ask(Path::new(&mail_root), id)
    }

    fn answer_ask(&self, ask: &AskMail, text: &str) -> bool {
        let raw = answer_message(ask, text, &format!("ans-{}", unique()));
        match sendmail(&raw, None) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("reply: mail sendmail failed: {e}");
                false
            }
        }
    }

    fn copy_to_operator(&self, ask: &AskMail, text: &str, default: Option<&str>) -> Result<(), String> {
        let raw = operator_copy(ask, text, default, &format!("rev-{}", unique()));
        sendmail(&raw, Some("operator"))
    }

    fn deliver(&self, id: &str, text: &str) {
        let run = spira_config::process::cfg("SPIRA_RUN").unwrap_or_default();
        let mail = spira_config::process::cfg("SPIRA_MAIL").unwrap_or_default();
        bead::claimdesc::notify_live_dwarf(id, text, &run, &mail);
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let id = args.next().unwrap_or_default();
    let mut text = args.next().unwrap_or_default();
    let mut default: Option<String> = None;
    while let Some(a) = args.next() {
        match (a.as_str(), args.next()) {
            ("--default", Some(d)) if !d.trim().is_empty() => default = Some(d),
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        }
    }

    if usage_error(&id, &text) {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }

    if text == "-" {
        text.clear();
        if let Err(e) = std::io::stdin().read_to_string(&mut text) {
            eprintln!("reply: failed to read text from stdin: {e}");
            return ExitCode::from(2);
        }
    }

    let cockpit_db = match db::cockpit_db() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };

    match run(&id, &text, default.as_deref(), &cockpit_db, &RealBd, &RealFollow) {
        Outcome::Replied(s) => {
            println!("{s}");
            ExitCode::SUCCESS
        }
        Outcome::Failed(s) => {
            eprintln!("{s}");
            ExitCode::from(1)
        }
    }
}
