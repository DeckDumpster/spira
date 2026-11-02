//! A subprocess that must answer within `CALL_TIMEOUT_SECS`: the program runs under `timeout`.
//! A real batch job is marked `// batch-job: <why>` at its call instead (spira-lint call-deadline).

use std::ffi::OsStr;
use std::process::Command;

pub const CALL_TIMEOUT_SECS: &str = "5";

pub fn bounded(program: impl AsRef<OsStr>) -> Command {
    let mut c = Command::new("timeout");
    c.arg(CALL_TIMEOUT_SECS).arg(program);
    c
}

/// [`bounded`] with a deadline of the caller's own, in seconds as `timeout` takes them.
pub fn bounded_for(program: impl AsRef<OsStr>, secs: &str) -> Command {
    let mut c = Command::new("timeout");
    c.arg(secs).arg(program);
    c
}
