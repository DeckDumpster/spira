//! Per-probe deadlines. A probe that waits on bd/dolt under host load must cost the pass
//! one `?` field, never the pass itself: systemd kills a pass that outruns its start timeout
//! and the detector is gone exactly when the box is loaded.

use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct State {
    probe: Duration,
    pass_end: Option<Instant>,
    slow: Vec<String>,
}

static STATE: Mutex<State> = Mutex::new(State { probe: Duration::from_secs(30), pass_end: None, slow: Vec::new() });

/// `pass` is the whole-pass budget: no probe is allowed past it, so the sum of slow probes
/// cannot reach the unit's start timeout.
pub fn init(probe: Duration, pass: Duration) {
    let mut s = STATE.lock().unwrap();
    s.probe = probe;
    s.pass_end = Some(Instant::now() + pass);
    s.slow.clear();
}

/// Labels of the probes that hit their deadline this pass, in order.
pub fn slow_probes() -> Vec<String> {
    STATE.lock().unwrap().slow.clone()
}

fn limit() -> Duration {
    let s = STATE.lock().unwrap();
    let left = s.pass_end.map(|e| e.saturating_duration_since(Instant::now())).unwrap_or(s.probe);
    s.probe.min(left).max(Duration::from_secs(1))
}

/// `Command::output` bounded by the probe deadline. On expiry the child's whole process
/// group is killed, the probe is recorded as slow, and the caller gets `TimedOut` — which
/// every caller already renders as `?`.
/// SIGKILL to the probe's own process group (it was spawned as the group's leader).
fn kill_group(leader: u32) {
    // SAFETY: kill(2) on a negative pid signals that process group; no memory is touched.
    unsafe {
        libc::kill(-(leader as i32), libc::SIGKILL);
    }
}

pub fn output(label: &str, cmd: &mut Command) -> io::Result<Output> {
    output_within(label, cmd, limit())
}

pub fn output_within(label: &str, cmd: &mut Command, within: Duration) -> io::Result<Output> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()?;
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let t_out = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let t_err = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err.read_to_end(&mut b);
        b
    });
    let end = Instant::now() + within;
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break Some(st);
        }
        if Instant::now() >= end {
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    match status {
        Some(status) => Ok(Output { status, stdout: t_out.join().unwrap_or_default(), stderr: t_err.join().unwrap_or_default() }),
        None => {
            kill_group(child.id());
            let _ = child.kill();
            let _ = child.wait();
            drop((t_out, t_err));
            STATE.lock().unwrap().slow.push(format!("{label} (>{}s)", within.as_secs()));
            Err(io::Error::new(io::ErrorKind::TimedOut, format!("{label} exceeded its deadline")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probe_past_its_deadline_takes_its_whole_group_with_it() {
        let d = testkit::TempDir::new("wt-deadline");
        let pidfile = d.join("grandchild.pid");
        let script = format!("sleep 30 & echo $! > {}; wait", pidfile.display());
        let r = output_within("test", Command::new("sh").arg("-c").arg(&script), Duration::from_millis(500));
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::TimedOut);
        let pid = std::fs::read_to_string(&pidfile).unwrap().trim().to_string();
        let gone = (0..50).any(|_| {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            let dead = stat.is_empty() || stat.rsplit(')').next().is_some_and(|r| r.trim_start().starts_with('Z'));
            if !dead {
                // clock-safe: a 1 s cap on the wait for the named signal, the grandchild's exit
                std::thread::sleep(Duration::from_millis(20));
            }
            dead
        });
        assert!(gone, "the grandchild {pid} outlived its probe");
    }
}
