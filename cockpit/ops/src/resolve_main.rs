//! `resolve <bead-id> "<reason>"` — close a bead the agent established or did itself.
//! See cockpit-ops's DESIGN.md and src/resolve.rs.

use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use cockpit_ops::db;
use cockpit_ops::resolve::{run, usage_error, BdResult, Closer, Outcome, USAGE};

struct RealBd;

impl Closer for RealBd {
    // Through the lifecycle machine (sp-3fue0j): the row's end is recorded, then the store
    // closed — a raw `bd close` here left resolved beads READY on their rows. `db` is the
    // store spira-lc itself resolves from config.
    fn close(&self, _db: &Path, id: &str, reason: &str) -> BdResult {
        match spira_config::lifecycle_row::close(id, reason, "claude", None) {
            Ok(()) => BdResult { success: true, combined: String::new() },
            Err(e) => BdResult { success: false, combined: e },
        }
    }

    fn show_json(&self, db: &Path, id: &str) -> String {
        Command::new("timeout")
            .arg("5")
            .arg(db::bd_bin())
            .arg("-C")
            .arg(db)
            .args(["show", id, "--json"])
            .stdin(Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    }

    fn has_lifecycle_row(&self, id: &str) -> Result<bool, String> {
        match Command::new("timeout").args(["5", "spira-lc", "show", id]).stdin(Stdio::null()).output() {
            Ok(o) => match o.status.code() {
                Some(0) => Ok(true),
                Some(1) => Ok(false),
                code => Err(format!("spira-lc show exit {code:?}: {}", String::from_utf8_lossy(&o.stderr))),
            },
            Err(e) => Err(format!("running spira-lc: {e}")),
        }
    }

    fn withdraw_ask(&self, work_bead: &str) -> (i32, String) {
        let mut c = Command::new(spira_config::lc_call::lc_bin());
        c.args(["withdraw-ask", work_bead, "claude"]);
        spira_config::lc_call::run_bounded(c, spira_config::lc_call::LC_TIMEOUT)
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
