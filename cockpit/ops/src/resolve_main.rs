//! `resolve <bead-id> "<reason>"` — close a bead the agent established or did itself.
//! See cockpit-ops's DESIGN.md and src/resolve.rs.

use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use cockpit_ops::db;
use cockpit_ops::resolve::{run, usage_error, BdResult, Closer, Outcome, USAGE};

struct RealBd;

impl Closer for RealBd {
    fn close(&self, db: &Path, id: &str, reason: &str) -> BdResult {
        let bd = db::bd_bin();
        let out = Command::new(&bd)
            .arg("-C")
            .arg(db)
            .args(["close", id, "--force", "--reason", reason])
            .env("BEADS_ACTOR", "claude")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();
        match out {
            Ok(o) => {
                // Concatenated, not byte-interleaved — see ../DESIGN.md Decisions.
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
    let mut reason = args.next().unwrap_or_default();

    if usage_error(&id, &reason) {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }

    if reason == "-" {
        reason.clear();
        if let Err(e) = std::io::stdin().read_to_string(&mut reason) {
            eprintln!("resolve: failed to read reason from stdin: {e}");
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

    match run(&id, &reason, &cockpit_db, &RealBd) {
        Outcome::Closed(s) => {
            println!("{s}");
            ExitCode::SUCCESS
        }
        Outcome::Failed(s) => {
            eprintln!("{s}");
            ExitCode::from(1)
        }
    }
}
