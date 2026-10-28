//! The one process the service starts: the Concierge's wake verb, which holds until the input
//! line is empty and so may run for minutes.

use std::path::Path;
use std::process::{Command, Stdio};

pub fn concierge_wake(concierge: &Path, line: &str) -> Result<String, String> {
    let out = Command::new(concierge).arg("wake").arg(line).stdin(Stdio::null()).output().map_err(|e| format!("{}: {e}", concierge.display()))?;
    let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if out.status.success() {
        Ok("woke".into())
    } else {
        Err(said)
    }
}
