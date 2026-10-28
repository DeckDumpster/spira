//! The shared `Clock`, `Proc` and `Rng` ports (law-new-subsystems-are-simulation-ready).

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Milliseconds on one timeline: wall (since the epoch) or virtual (since zero).
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

pub struct WallClock;

impl Clock for WallClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
    }
}

#[derive(Clone, Default)]
pub struct VirtualClock(Arc<AtomicU64>);

impl VirtualClock {
    pub fn new(start_ms: u64) -> Self {
        VirtualClock(Arc::new(AtomicU64::new(start_ms)))
    }

    pub fn advance_to(&self, ms: u64) {
        self.0.fetch_max(ms, Ordering::SeqCst);
    }

    pub fn advance(&self, by_ms: u64) {
        self.0.fetch_add(by_ms, Ordering::SeqCst);
    }
}

impl Clock for VirtualClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

pub trait Rng: Send {
    fn next_u64(&mut self) -> u64;
}

/// SplitMix64: the generator the simulator already uses, so a seed means the same stream.
#[derive(Clone, Debug)]
pub struct SeededRng(u64);

impl SeededRng {
    pub fn new(seed: u64) -> Self {
        SeededRng(seed)
    }
}

impl Rng for SeededRng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcSpec {
    pub prog: String,
    pub args: Vec<String>,
    pub stdin: Vec<u8>,
    pub deadline: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcOutput {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Running a program. `Err` is a failure to run it (spawn failed, deadline passed); a program
/// that ran and exited non-zero is an `Ok` with its code.
pub trait Proc: Send + Sync {
    fn run(&self, spec: &ProcSpec) -> Result<ProcOutput, String>;
}

pub struct SysProc;

impl Proc for SysProc {
    fn run(&self, spec: &ProcSpec) -> Result<ProcOutput, String> {
        let mut child = Command::new(&spec.prog)
            .args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn {}: {e}", spec.prog))?;
        let mut stdin = child.stdin.take().ok_or("no stdin pipe")?;
        let input = spec.stdin.clone();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
        let started = Instant::now();
        loop {
            match child.try_wait().map_err(|e| e.to_string())? {
                Some(_) => break,
                None if started.elapsed() >= spec.deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{} exceeded its {:?} deadline", spec.prog, spec.deadline));
                }
                None => std::thread::sleep(Duration::from_millis(2)),
            }
        }
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        Ok(ProcOutput { code: out.status.code(), stdout: out.stdout, stderr: out.stderr })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(prog: &str, args: &[&str], deadline_ms: u64) -> ProcSpec {
        ProcSpec {
            prog: prog.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            stdin: b"in".to_vec(),
            deadline: Duration::from_millis(deadline_ms),
        }
    }

    #[test]
    fn virtual_clock_only_moves_forward() {
        let c = VirtualClock::new(10);
        c.advance_to(5);
        assert_eq!(c.now_ms(), 10);
        c.advance(7);
        assert_eq!(c.now_ms(), 17);
    }

    #[test]
    fn same_seed_same_stream_and_seeds_differ() {
        let (mut a, mut b, mut c) = (SeededRng::new(7), SeededRng::new(7), SeededRng::new(8));
        let (x, y) = (a.next_u64(), b.next_u64());
        assert_eq!(x, y);
        assert_ne!(x, c.next_u64());
    }

    #[test]
    fn sys_proc_pipes_stdin_and_enforces_the_deadline() {
        let out = SysProc.run(&spec("cat", &[], 2000)).unwrap();
        assert_eq!(out.stdout, b"in");
        let err = SysProc.run(&spec("sleep", &["5"], 40)).unwrap_err();
        assert!(err.contains("deadline"), "{err}");
    }
}
