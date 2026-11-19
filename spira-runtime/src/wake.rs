//! The one process the service starts: the Concierge's wake verb, which holds until the input
//! line is empty and so may run for minutes.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Runs the wake and kills its whole process group once `deadline` passes, so no wake outlives its effect.
pub fn concierge_wake(concierge: &Path, line: &str, deadline: Duration) -> Result<String, String> {
    // FORK-EDGE: concierge.sh wake (68-line case over the script's 684-line session preamble) types into
    // the pane with tmux send-keys, which nothing built here may do; it stays the operator tool's.
    let mut child = Command::new(concierge) // batch-job: wake holds until the input line is empty, which can take minutes
        .arg("wake")
        .arg(line)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("{}: {e}", concierge.display()))?;
    let mut pipe = child.stderr.take().ok_or("no stderr pipe")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = pipe.read_to_string(&mut buf);
        let _ = tx.send(buf);
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if started.elapsed() >= deadline {
            let pid = i32::try_from(child.id()).unwrap_or(0);
            if pid > 0 {
                unsafe { libc::kill(-pid, libc::SIGKILL) };
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{} wake exceeded its {deadline:?} deadline and was killed", concierge.display()));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    if status.success() {
        return Ok("woke".into());
    }
    Err(rx.recv_timeout(Duration::from_millis(200)).unwrap_or_default().trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wake_that_outlives_its_deadline_is_killed_and_reported_failed() {
        let dir = testkit::TempDir::new("wake-deadline");
        let script = dir.join("concierge.sh");
        let pid_file = dir.join("pid");
        testkit::write_exe(&script, &format!("#!/bin/sh\necho $$ > {}\nexec sleep 30\n", pid_file.display()));
        let r = concierge_wake(&script, "line", Duration::from_millis(500));
        assert!(r.is_err());
        let pid: i32 = std::fs::read_to_string(&pid_file).expect("the script started").trim().parse().unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "the wake outlived its deadline");
    }
}
