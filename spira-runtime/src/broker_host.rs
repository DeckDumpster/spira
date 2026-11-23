//! The `spira-lc work-exec` process broker.exec launches; task modules launch none themselves.

use std::time::Duration;

pub const KILL_GRACE_SECS: u64 = 5;

/// `timeout -k 5 <bound> spira-lc work-exec <id>`, held to a little over the bound here too.
pub fn exec_program(id: &str, bound_ms: u64) -> (i32, String) {
    let bin = match spira_store::lc_call::lc_bin() {
        Ok(b) => b,
        Err(e) => return (2, e),
    };
    let secs = bound_ms.div_ceil(1000).max(1);
    let mut cmd = std::process::Command::new("timeout");
    cmd.args(["-k", &KILL_GRACE_SECS.to_string(), &secs.to_string()]).arg(&bin).args(["work-exec", id]);
    spira_store::lc_call::run_bounded(cmd, Duration::from_secs(secs + KILL_GRACE_SECS + 5))
}
