//! `reply <bead-id> "<text>"` — answer the operator inside a bead's own comment thread.
//! See cockpit-ops's DESIGN.md and src/reply.rs.

use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use cockpit_ops::db;
use cockpit_ops::reply::{run, usage_error, BdResult, Commenter, Outcome, USAGE};

struct RealBd;

impl Commenter for RealBd {
    fn comment(&self, db: &Path, id: &str, text: &str) -> BdResult {
        let bd = db::bd_bin();
        let out = Command::new(&bd)
            .arg("-C")
            .arg(db)
            .args(["comments", "add", id, text])
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

    match run(&id, &text, &cockpit_db, &RealBd) {
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
