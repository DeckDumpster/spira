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
        // Drained while the child runs: a child writing past the pipe buffer blocks until read,
        // and a wait that reads only after exit reports that stall as a missed deadline.
        let drain = |mut pipe: Box<dyn std::io::Read + Send>| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = pipe.read_to_end(&mut buf);
                buf
            })
        };
        let out = drain(Box::new(child.stdout.take().ok_or("no stdout pipe")?));
        let err = drain(Box::new(child.stderr.take().ok_or("no stderr pipe")?));
        let started = Instant::now();
        let status = loop {
            match child.try_wait().map_err(|e| e.to_string())? {
                Some(st) => break st,
                None if started.elapsed() >= spec.deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{} exceeded its {:?} deadline", spec.prog, spec.deadline));
                }
                None => std::thread::sleep(Duration::from_millis(2)),
            }
        };
        Ok(ProcOutput { code: status.code(), stdout: out.join().unwrap_or_default(), stderr: err.join().unwrap_or_default() })
    }
}

/// A `Proc` that counts every program it is asked to run, then refuses it or hands it on.
/// A simulation that must spawn nothing reads `spawned()` as zero.
pub struct CountingProc {
    inner: Option<Arc<dyn Proc>>,
    spawned: AtomicU64,
    seen: std::sync::Mutex<Vec<String>>,
}

impl CountingProc {
    pub fn refusing() -> Arc<Self> {
        Arc::new(CountingProc { inner: None, spawned: AtomicU64::new(0), seen: Default::default() })
    }

    pub fn over(inner: Arc<dyn Proc>) -> Arc<Self> {
        Arc::new(CountingProc { inner: Some(inner), spawned: AtomicU64::new(0), seen: Default::default() })
    }

    pub fn spawned(&self) -> u64 {
        self.spawned.load(Ordering::SeqCst)
    }

    /// The programs asked for, in order.
    pub fn programs(&self) -> Vec<String> {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Proc for CountingProc {
    fn run(&self, spec: &ProcSpec) -> Result<ProcOutput, String> {
        self.spawned.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).push(spec.prog.clone());
        match &self.inner {
            Some(p) => p.run(spec),
            None => Err(format!("{}: no process may be spawned here", spec.prog)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counting_proc_counts_what_it_refuses_and_what_it_passes_on() {
        struct Echo;
        impl Proc for Echo {
            fn run(&self, _: &ProcSpec) -> Result<ProcOutput, String> {
                Ok(ProcOutput { code: Some(0), stdout: b"hi".to_vec(), stderr: vec![] })
            }
        }
        let spec = ProcSpec { prog: "git".into(), args: vec![], stdin: vec![], deadline: Duration::from_secs(1) };
        let refusing = CountingProc::refusing();
        assert!(refusing.run(&spec).is_err());
        assert_eq!((refusing.spawned(), refusing.programs()), (1, vec!["git".to_string()]));
        let over = CountingProc::over(Arc::new(Echo));
        assert_eq!(over.run(&spec).unwrap().stdout, b"hi");
        assert_eq!(over.spawned(), 1);
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
}
