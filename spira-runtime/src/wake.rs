//! The one process the service starts: the Concierge's wake verb, which holds until the input
//! line is empty and so may run for minutes.

use std::path::Path;
use std::process::{Command, Stdio};

pub fn concierge_wake(concierge: &Path, line: &str) -> Result<String, String> {
    // FORK-EDGE: concierge.sh wake (68-line case over the script's 684-line session preamble) types into
    // the pane with tmux send-keys, which nothing built here may do; it stays the operator tool's.
    let out = Command::new(concierge) // batch-job: wake holds until the input line is empty, which can take minutes
        .arg("wake")
        .arg(line)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", concierge.display()))?;
    let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if out.status.success() {
        Ok("woke".into())
    } else {
        Err(said)
    }
}
