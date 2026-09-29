//! The ports every external effect goes through (DESIGN.md §7): `Runner` for processes,
//! `Clock` for time, `Sink` for this process's own stdout/stderr. Tests fake all three.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Io {
    /// Collect into `Out`.
    Capture,
    /// Straight to this process's own stream (the sentinel log).
    Inherit,
    Null,
    /// Append to a file (`>> f 2>&1` when used for both streams).
    Append(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub prog: String,
    pub args: Vec<String>,
    pub stdin: Option<Vec<u8>>,
    /// Added to (and overriding) the runner's base environment.
    pub env: Vec<(String, String)>,
    pub stdout: Io,
    pub stderr: Io,
    pub timeout: Option<Duration>,
}

impl Spec {
    pub fn new(prog: impl Into<String>, args: &[&str]) -> Spec {
        Spec {
            prog: prog.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            stdin: None,
            env: Vec::new(),
            stdout: Io::Capture,
            stderr: Io::Capture,
            timeout: None,
        }
    }
    pub fn args_owned(prog: impl Into<String>, args: Vec<String>) -> Spec {
        let mut s = Spec::new(prog, &[]);
        s.args = args;
        s
    }
    pub fn stdin(mut self, b: impl Into<Vec<u8>>) -> Spec {
        self.stdin = Some(b.into());
        self
    }
    pub fn env(mut self, k: &str, v: impl Into<String>) -> Spec {
        self.env.push((k.to_string(), v.into()));
        self
    }
    pub fn out(mut self, o: Io) -> Spec {
        self.stdout = o;
        self
    }
    pub fn err(mut self, e: Io) -> Spec {
        self.stderr = e;
        self
    }
    pub fn timeout(mut self, secs: u64) -> Spec {
        self.timeout = Some(Duration::from_secs(secs));
        self
    }
    /// The whole command line, for tests and diagnostics.
    pub fn line(&self) -> String {
        let mut v = vec![self.prog.clone()];
        v.extend(self.args.iter().cloned());
        v.join(" ")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Out {
    /// Exit status; 128+signal when killed; 124 on our own timeout; 127 when it could not
    /// be started at all.
    pub rc: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    pub fn ok(&self) -> bool {
        self.rc == 0
    }
}

pub trait Runner {
    fn run(&self, spec: &Spec) -> Out;
}

pub trait Clock {
    fn now(&self) -> i64;
}

pub trait Sink {
    fn out(&self, line: &str);
    fn err(&self, line: &str);
}

// ---------------------------------------------------------------------------------------
// Real implementations.

pub struct RealClock;
impl Clock for RealClock {
    fn now(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }
}

pub struct RealSink;
impl Sink for RealSink {
    fn out(&self, line: &str) {
        let mut o = std::io::stdout().lock();
        let _ = writeln!(o, "{line}");
        let _ = o.flush();
    }
    fn err(&self, line: &str) {
        let mut e = std::io::stderr().lock();
        let _ = writeln!(e, "{line}");
        let _ = e.flush();
    }
}

/// Runs processes with exactly `base_env` (the environment conf.sh resolved, from the
/// context probe) plus the spec's own additions.
pub struct RealRunner {
    pub base_env: Option<Vec<(String, String)>>,
}

fn stdio_for(io: &Io) -> std::io::Result<Stdio> {
    Ok(match io {
        Io::Capture => Stdio::piped(),
        Io::Inherit => Stdio::inherit(),
        Io::Null => Stdio::null(),
        Io::Append(p) => {
            let f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)?;
            Stdio::from(f)
        }
    })
}

impl Runner for RealRunner {
    fn run(&self, spec: &Spec) -> Out {
        let _ = std::io::stdout().flush();
        let mut cmd = Command::new(&spec.prog);
        cmd.args(&spec.args);
        if let Some(base) = &self.base_env {
            cmd.env_clear();
            cmd.envs(base.iter().map(|(k, v)| (k, v)));
        }
        cmd.envs(spec.env.iter().map(|(k, v)| (k, v)));
        cmd.stdin(if spec.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        let (so, se) = match (stdio_for(&spec.stdout), stdio_for(&spec.stderr)) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(e), _) | (_, Err(e)) => {
                return Out {
                    rc: 127,
                    stdout: String::new(),
                    stderr: e.to_string(),
                }
            }
        };
        cmd.stdout(so).stderr(se);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return Out {
                    rc: 127,
                    stdout: String::new(),
                    stderr: format!("{}: {e}", spec.prog),
                }
            }
        };
        let writer = spec.stdin.clone().and_then(|b| {
            child.stdin.take().map(|mut w| {
                std::thread::spawn(move || {
                    let _ = w.write_all(&b);
                })
            })
        });
        let rd = |p: Option<Box<dyn Read + Send>>| {
            p.map(|mut r| {
                std::thread::spawn(move || {
                    let mut v = Vec::new();
                    let _ = r.read_to_end(&mut v);
                    v
                })
            })
        };
        let ro = rd(child
            .stdout
            .take()
            .map(|r| Box::new(r) as Box<dyn Read + Send>));
        let re = rd(child
            .stderr
            .take()
            .map(|r| Box::new(r) as Box<dyn Read + Send>));
        let start = Instant::now();
        let mut timed_out = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break Some(s),
                Ok(None) => {}
                Err(_) => break None,
            }
            if let Some(t) = spec.timeout {
                if start.elapsed() >= t {
                    timed_out = true;
                    let _ = child.kill();
                    break child.wait().ok();
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        if let Some(w) = writer {
            let _ = w.join();
        }
        let stdout = ro.and_then(|h| h.join().ok()).unwrap_or_default();
        let stderr = re.and_then(|h| h.join().ok()).unwrap_or_default();
        use std::os::unix::process::ExitStatusExt;
        let rc = if timed_out {
            124
        } else {
            match status {
                Some(s) => s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0)),
                None => 127,
            }
        };
        Out {
            rc,
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        }
    }
}

// ---------------------------------------------------------------------------------------
// The host: the three ports plus the environment every child of this pass shares.

pub struct Host<'a> {
    pub runner: &'a dyn Runner,
    pub clock: &'a dyn Clock,
    pub sink: &'a dyn Sink,
    /// Added to every child: the snapshot paths (SPIRA_LIST_SNAPSHOT …).
    pub extra_env: RefCell<Vec<(String, String)>>,
}

impl<'a> Host<'a> {
    pub fn new(runner: &'a dyn Runner, clock: &'a dyn Clock, sink: &'a dyn Sink) -> Host<'a> {
        Host {
            runner,
            clock,
            sink,
            extra_env: RefCell::new(Vec::new()),
        }
    }
    pub fn now(&self) -> i64 {
        self.clock.now()
    }
    /// lib.sh `log`: `<UTC ts> spira: <msg>` on stdout.
    pub fn log(&self, msg: &str) {
        self.sink.out(&format!("{} spira: {msg}", utc(self.now())));
    }
    /// `log … >&2`.
    pub fn log_err(&self, msg: &str) {
        self.sink.err(&format!("{} spira: {msg}", utc(self.now())));
    }
    /// `[ -n "$text" ] && printf '%s\n' "$text"` where `$text` came from `$(…)`: verbatim
    /// output passed through, trailing newlines stripped, nothing at all when empty.
    pub fn print(&self, text: &str) {
        let t = text.trim_end_matches('\n');
        if !t.is_empty() {
            self.sink.out(t);
        }
    }
    pub fn set_env(&self, k: &str, v: &str) {
        let mut e = self.extra_env.borrow_mut();
        e.retain(|(x, _)| x != k);
        e.push((k.to_string(), v.to_string()));
    }
    pub fn unset_env(&self, k: &str) {
        self.extra_env.borrow_mut().retain(|(x, _)| x != k);
    }
    pub fn run(&self, spec: Spec) -> Out {
        let mut s = spec;
        let mut env: Vec<(String, String)> = self.extra_env.borrow().clone();
        env.append(&mut s.env);
        s.env = env;
        self.runner.run(&s)
    }
}

// ---------------------------------------------------------------------------------------
// Time formatting without a date library.

/// Days since 1970-01-01 → (y, m, d). Howard Hinnant's civil_from_days.
pub fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// (y, m, d) → days since 1970-01-01.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `date -u +%Y-%m-%dT%H:%M:%SZ`.
pub fn utc(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let s = epoch.rem_euclid(86_400);
    let (y, m, d) = civil(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        (s % 3600) / 60,
        s % 60
    )
}

/// Python's `datetime.fromisoformat(ts.replace("Z", "+00:00"))` for the shapes bd writes;
/// None for a naive or unparseable timestamp (python then prints "?").
pub fn parse_iso(ts: &str) -> Option<i64> {
    let ts = ts.trim();
    if ts.len() < 19 {
        return None;
    }
    let b = ts.as_bytes();
    let num = |a: usize, z: usize| -> Option<i64> { ts.get(a..z)?.parse().ok() };
    if b[4] != b'-'
        || b[7] != b'-'
        || !(b[10] == b'T' || b[10] == b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let (y, mo, d, h, mi, s) = (
        num(0, 4)?,
        num(5, 7)?,
        num(8, 10)?,
        num(11, 13)?,
        num(14, 16)?,
        num(17, 19)?,
    );
    let mut rest = &ts[19..];
    if let Some(r) = rest.strip_prefix('.') {
        let n = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
        rest = &r[n..];
    }
    let off = if rest == "Z" {
        0
    } else if rest.len() == 6
        && (rest.starts_with('+') || rest.starts_with('-'))
        && &rest[3..4] == ":"
    {
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        let oh: i64 = rest[1..3].parse().ok()?;
        let om: i64 = rest[4..6].parse().ok()?;
        sign * (oh * 3600 + om * 60)
    } else {
        return None;
    };
    Some(days_from_civil(y, mo as u32, d as u32) * 86_400 + h * 3600 + mi * 60 + s - off)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_formats_like_date_u() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(1_790_653_638), "2026-09-29T03:47:18Z");
    }

    #[test]
    fn iso_round_trips_and_refuses_naive() {
        assert_eq!(parse_iso("2026-09-29T03:47:18Z"), Some(1_790_653_638));
        assert_eq!(parse_iso("2026-09-29T03:47:18.123Z"), Some(1_790_653_638));
        assert_eq!(parse_iso("2026-09-29T05:47:18+02:00"), Some(1_790_653_638));
        assert_eq!(parse_iso("2026-09-29T03:47:18"), None);
        assert_eq!(parse_iso("garbage"), None);
    }

    #[test]
    fn real_runner_captures_feeds_stdin_and_times_out() {
        let r = RealRunner { base_env: None };
        let o = r.run(&Spec::new("cat", &[]).stdin("hi"));
        assert_eq!((o.rc, o.stdout.as_str()), (0, "hi"));
        let o = r.run(&Spec::new("sh", &["-c", "exit 3"]));
        assert_eq!(o.rc, 3);
        let o = r.run(&Spec::new("sleep", &["5"]).timeout(0));
        assert_eq!(o.rc, 124);
        let o = r.run(&Spec::new("/nonexistent/x", &[]));
        assert_eq!(o.rc, 127);
        let r = RealRunner {
            base_env: Some(vec![("A".into(), "1".into())]),
        };
        let o = r.run(&Spec::new("/usr/bin/env", &[]).env("B", "2"));
        assert_eq!(o.stdout, "A=1\nB=2\n");
    }
}
