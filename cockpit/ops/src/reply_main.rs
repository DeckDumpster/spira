//! `reply <bead-id> "<text>"` — answer the operator inside a bead's own comment thread.
//! See cockpit-ops's DESIGN.md and src/reply.rs.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{ExitCode, Stdio};

use cockpit_ops::db;
use cockpit_ops::reply::{answer_message, find_open_ask, run, usage_error, BdResult, Commenter, Follow, Outcome, USAGE};

struct RealBd;

impl Commenter for RealBd {
    fn comment(&self, _db: &Path, id: &str, text: &str) -> BdResult {
        let bd = db::lc_bin();
        let out = spira_config::bounded::bounded(&bd)
            .args(["content", "comments", "add", id, text])
            .env("BEADS_ACTOR", "claude")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();
        match out {
            Ok(o) => {
                let mut combined = String::from_utf8_lossy(&o.stdout).into_owned();
                let stderr = String::from_utf8_lossy(&o.stderr);
                if !stderr.is_empty() {
                    if !combined.is_empty() && !combined.ends_with('\n') {
                        combined.push('\n');
                    }
                    combined.push_str(&stderr);
                }
                BdResult {
                    success: o.status.success(),
                    combined,
                }
            }
            Err(e) => BdResult {
                success: false,
                combined: format!("failed to run {bd}: {e}"),
            },
        }
    }
}

struct RealFollow;

impl Follow for RealFollow {
    fn lift_hold(&self, id: &str, message_id: &str) -> Result<(), String> {
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

    fn answer_ask(&self, id: &str, text: &str) -> bool {
        let mail_root = spira_config::process::cfg("SPIRA_MAIL").unwrap_or_default();
        let Some(ask) = find_open_ask(Path::new(&mail_root), id) else { return false };
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let raw = answer_message(&ask, text, &format!("ans-{id}-{nanos}"));
        let Ok(mut child) = spira_config::bounded::bounded("mail")
            .arg("sendmail")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
        else {
            return false;
        };
        if let Some(mut stdin) = child.stdin.take() {
            if stdin.write_all(raw.as_bytes()).is_err() {
                return false;
            }
        }
        match child.wait_with_output() {
            Ok(o) if o.status.success() => true,
            Ok(o) => {
                eprintln!("reply: mail sendmail failed: {}", String::from_utf8_lossy(&o.stderr).trim());
                false
            }
            Err(_) => false,
        }
    }

    fn deliver(&self, id: &str, text: &str) {
        let run = spira_config::process::cfg("SPIRA_RUN").unwrap_or_default();
        let mail = spira_config::process::cfg("SPIRA_MAIL").unwrap_or_default();
        bead::claimdesc::notify_live_aeon(id, text, &run, &mail);
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let id = args.next().unwrap_or_default();
    let mut text = args.next().unwrap_or_default();

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

    match run(&id, &text, &cockpit_db, &RealBd, &RealFollow) {
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
