//! The processes the runtime launches, each under a deadline: git on the wiki checkout (not store
//! state, so spira-lc has no verb over it), tmux on a view flip, `overrides.sh apply` on the
//! overrides tick, and the bead-stage exporter on a reconciler-flow pass. Effects run on the offload
//! pool, never on the event loop.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const GIT_DEADLINE: Duration = Duration::from_secs(4);

/// Runs git in `dir`; the exit code and stdout, or an error when git could not start or outlived
/// `GIT_DEADLINE`. Stdout is drained while git runs so a large listing cannot fill the pipe and
/// stall it into the deadline.
pub fn git(dir: &Path, args: &[&str]) -> Result<(Option<i32>, Vec<u8>), String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    bounded(cmd, GIT_DEADLINE)
}

/// Runs tmux under `deadline` and returns its stdout. `TMUX` is cleared: it names whatever server
/// the service was started from, not the cockpit's own.
pub fn tmux(program: &Path, args: &[&str], deadline: Duration) -> Result<String, String> {
    let mut cmd = Command::new(program);
    cmd.args(args).env_remove("TMUX");
    match bounded(cmd, deadline)? {
        (Some(0), out) => Ok(String::from_utf8_lossy(&out).into_owned()),
        (code, _) => Err(format!("tmux {} exited {code:?}", args.first().unwrap_or(&""))),
    }
}

/// `gh auth token` under `config_dir` (when set); the exit code and stdout.
pub fn gh_auth_token(gh: &str, config_dir: &str, deadline: Duration) -> Result<(Option<i32>, Vec<u8>), String> {
    let mut cmd = Command::new(gh);
    cmd.args(["auth", "token"]);
    if !config_dir.is_empty() {
        cmd.env("GH_CONFIG_DIR", config_dir);
    }
    bounded(cmd, deadline)
}

/// One `overrides.sh apply` pass under `timeout`; a failed override fails the pass, and its
/// stdout and stderr together say which.
pub fn overrides_apply() -> Result<String, String> {
    // FORK-EDGE: overrides.sh is 223 lines of shell (over the ~100-line port bound).
    let out = spira_config::bounded::bounded("overrides.sh").arg("apply").stdin(Stdio::null()).output().map_err(|e| format!("overrides.sh: {e}"))?;
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if out.status.success() { Ok(said.trim().to_string()) } else { Err(said.trim().to_string()) }
}

/// One `<exporter> lifecycle` run under `timeout`, which brings the `bead-stage` family up to the
/// lifecycle log; a failed run names its exit code and what it said on stderr.
pub fn lifecycle_export(bin: &str) -> Result<(), String> {
    // FORK-EDGE: sibling binary tsd-lifecycle-export (the bead-stage exporter).
    let out = spira_config::bounded::bounded(bin).arg("lifecycle").stdin(Stdio::null()).output().map_err(|e| format!("{bin}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!("{bin} lifecycle: exit {}: {}", out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).trim()))
}

fn bounded(mut cmd: Command, deadline: Duration) -> Result<(Option<i32>, Vec<u8>), String> {
    use std::io::Read;
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|e| format!("spawn: {e}"))?;
    let mut out = child.stdout.take().ok_or("no stdout pipe")?;
    let reader = std::thread::spawn(move || {
        let mut buf = vec![];
        let _ = out.read_to_end(&mut buf);
        buf
    });
    let started = Instant::now();
    let code = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => break status.code(),
            None if started.elapsed() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("process exceeded its {deadline:?} deadline"));
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    };
    Ok((code, reader.join().map_err(|_| "git reader panicked")?))
}
