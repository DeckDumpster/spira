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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_program_runs_under_timeout_with_its_args_intact() {
        let out = bounded("echo").arg("hi").output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "hi\n");
    }

    #[test]
    fn a_call_past_the_deadline_is_killed() {
        let st = bounded("sleep").arg("30").status().unwrap();
        assert_eq!(st.code(), Some(124));
    }
}
