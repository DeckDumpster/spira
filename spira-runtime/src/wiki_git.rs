//! The one process the runtime launches: git on the wiki checkout. The wiki is not store state,
//! so spira-lc has no verb over it. Effects run on the offload pool, never on the event loop.

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

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.args(["-c", script]);
        c
    }

    #[test]
    fn a_process_that_outlives_its_deadline_is_killed_and_is_an_error() {
        let err = bounded(sh("exec sleep 30"), Duration::from_millis(100)).unwrap_err();
        assert!(err.contains("deadline"), "{err}");
    }

    #[test]
    fn output_larger_than_a_pipe_buffer_arrives_whole() {
        let (code, out) = bounded(sh("head -c 1000000 /dev/zero"), Duration::from_secs(10)).unwrap();
        assert_eq!((code, out.len()), (Some(0), 1_000_000));
    }
}
