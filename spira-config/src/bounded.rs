//! A subprocess that must answer within `CALL_TIMEOUT_SECS`: the program is the caller's direct
//! child and the deadline is enforced in the calling process, by a pidfd watchdog that kills the
//! child's whole process group. Exit codes are `timeout`'s: 124 deadline, 128+N signal, 127
//! missing program, 126 not executable, 125 unparseable deadline.
//! A real batch job is marked `// batch-job: <why>` at its call instead (spira-lint call-deadline).

use std::ffi::OsStr;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const CALL_TIMEOUT_SECS: &str = "5";

const EXIT_DEADLINE: i32 = 124;
const EXIT_BAD_DEADLINE: i32 = 125;
const EXIT_NOT_EXECUTABLE: i32 = 126;
const EXIT_NOT_FOUND: i32 = 127;

pub fn bounded(program: impl AsRef<OsStr>) -> Bounded {
    bounded_for(program, CALL_TIMEOUT_SECS)
}

/// [`bounded`] with a deadline of the caller's own, in `timeout`'s grammar: a decimal number with
/// an optional `s`, `m`, `h` or `d`; `0` is no deadline.
pub fn bounded_for(program: impl AsRef<OsStr>, secs: &str) -> Bounded {
    Bounded {
        cmd: Command::new(program),
        deadline: parse_deadline(secs),
        stdin: None,
        stdout: None,
        stderr: None,
    }
}

fn parse_deadline(s: &str) -> Result<Option<Duration>, ()> {
    let (num, unit) = match s.char_indices().last() {
        Some((i, c @ ('s' | 'm' | 'h' | 'd'))) => (
            &s[..i],
            match c {
                's' => 1.0,
                'm' => 60.0,
                'h' => 3600.0,
                _ => 86400.0,
            },
        ),
        _ => (s, 1.0),
    };
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return Err(());
    }
    let secs: f64 = num.parse().map_err(|_| ())?;
    let d = Duration::try_from_secs_f64(secs * unit).map_err(|_| ())?;
    Ok((!d.is_zero()).then_some(d))
}

/// What a helper needs to run either a [`Bounded`] or a plain `Command` it was handed.
pub trait Runnable {
    fn output(&mut self) -> io::Result<Output>;
    fn status(&mut self) -> io::Result<ExitStatus>;
}

impl Runnable for Command {
    fn output(&mut self) -> io::Result<Output> {
        Command::output(self)
    }
    fn status(&mut self) -> io::Result<ExitStatus> {
        Command::status(self)
    }
}

impl Runnable for Bounded {
    fn output(&mut self) -> io::Result<Output> {
        Bounded::output(self)
    }
    fn status(&mut self) -> io::Result<ExitStatus> {
        Bounded::status(self)
    }
}

pub struct Bounded {
    cmd: Command,
    deadline: Result<Option<Duration>, ()>,
    stdin: Option<Stdio>,
    stdout: Option<Stdio>,
    stderr: Option<Stdio>,
}

impl Bounded {
    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.cmd.arg(arg);
        self
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.cmd.args(args);
        self
    }

    pub fn env(&mut self, key: impl AsRef<OsStr>, val: impl AsRef<OsStr>) -> &mut Self {
        self.cmd.env(key, val);
        self
    }

    pub fn envs<I, K, V>(&mut self, vars: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.cmd.envs(vars);
        self
    }

    pub fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        self.cmd.env_remove(key);
        self
    }

    pub fn env_clear(&mut self) -> &mut Self {
        self.cmd.env_clear();
        self
    }

    pub fn current_dir(&mut self, dir: impl AsRef<Path>) -> &mut Self {
        self.cmd.current_dir(dir);
        self
    }

    pub fn stdin(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
        self.stdin = Some(cfg.into());
        self
    }

    pub fn stdout(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
        self.stdout = Some(cfg.into());
        self
    }

    pub fn stderr(&mut self, cfg: impl Into<Stdio>) -> &mut Self {
        self.stderr = Some(cfg.into());
        self
    }

    /// # Safety
    /// As [`CommandExt::pre_exec`].
    pub unsafe fn pre_exec<F>(&mut self, f: F) -> &mut Self
    where
        F: FnMut() -> io::Result<()> + Send + Sync + 'static,
    {
        unsafe { self.cmd.pre_exec(f) };
        self
    }

    pub fn get_program(&self) -> &OsStr {
        self.cmd.get_program()
    }

    pub fn get_args(&self) -> std::process::CommandArgs<'_> {
        self.cmd.get_args()
    }

    pub fn spawn(&mut self) -> io::Result<BoundedChild> {
        self.start(false)
    }

    pub fn output(&mut self) -> io::Result<Output> {
        match self.start(true) {
            Ok(child) => child.wait_with_output(),
            Err(e) => match refusal(&e, self.deadline.is_err()) {
                Some(status) => Ok(Output { status, stdout: Vec::new(), stderr: Vec::new() }),
                None => Err(e),
            },
        }
    }

    pub fn status(&mut self) -> io::Result<ExitStatus> {
        match self.start(false) {
            Ok(mut child) => child.wait(),
            Err(e) => refusal(&e, self.deadline.is_err()).ok_or(e),
        }
    }

    fn start(&mut self, capture: bool) -> io::Result<BoundedChild> {
        let deadline = self
            .deadline
            .map_err(|()| io::Error::new(io::ErrorKind::InvalidInput, "unparseable deadline"))?;
        let default = |cfg: Option<Stdio>, d: fn() -> Stdio| cfg.or_else(|| capture.then(d));
        if let Some(s) = default(self.stdin.take(), Stdio::null) {
            self.cmd.stdin(s);
        }
        if let Some(s) = default(self.stdout.take(), Stdio::piped) {
            self.cmd.stdout(s);
        }
        if let Some(s) = default(self.stderr.take(), Stdio::piped) {
            self.cmd.stderr(s);
        }
        self.cmd.process_group(0);
        let mut child = self.cmd.spawn()?;
        let expired = Arc::new(AtomicBool::new(false));
        if let Some(limit) = deadline {
            if let Err(e) = watch(&child, limit, expired.clone()) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }
        Ok(BoundedChild {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
            child,
            expired,
        })
    }
}

/// The exit status `timeout` gave for a refusal to start, if this error is one.
fn refusal(e: &io::Error, bad_deadline: bool) -> Option<ExitStatus> {
    let code = if bad_deadline {
        EXIT_BAD_DEADLINE
    } else {
        match e.kind() {
            io::ErrorKind::NotFound => EXIT_NOT_FOUND,
            io::ErrorKind::PermissionDenied => EXIT_NOT_EXECUTABLE,
            _ => return None,
        }
    };
    Some(ExitStatus::from_raw(code << 8))
}

/// One watchdog thread per child: sleeps in `poll` on the child's pidfd, so it costs nothing
/// until the deadline, and kills the group if the child is still running then.
fn watch(child: &Child, limit: Duration, expired: Arc<AtomicBool>) -> io::Result<()> {
    let pid = child.id() as libc::pid_t;
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let pidfd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    let end = Instant::now() + limit;
    std::thread::Builder::new().name("bounded-watchdog".into()).spawn(move || loop {
        let left = end.saturating_duration_since(Instant::now());
        let ms = left.as_millis().saturating_add(u128::from(!left.subsec_nanos().is_multiple_of(1_000_000)));
        let mut p = libc::pollfd { fd: pidfd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        let n = unsafe { libc::poll(&mut p, 1, ms.min(i32::MAX as u128) as i32) };
        if n > 0 {
            return;
        }
        if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if Instant::now() >= end || n < 0 {
            expired.store(true, Ordering::SeqCst);
            unsafe { libc::kill(-pid, libc::SIGKILL) };
            return;
        }
    })?;
    Ok(())
}

pub struct BoundedChild {
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
    child: Child,
    expired: Arc<AtomicBool>,
}

impl BoundedChild {
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn kill(&mut self) -> io::Result<()> {
        if unsafe { libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        drop(self.stdin.take());
        self.child.wait().map(|s| self.map(s))
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        Ok(self.child.try_wait()?.map(|s| self.map(s)))
    }

    pub fn wait_with_output(mut self) -> io::Result<Output> {
        self.child.stdin = self.stdin.take();
        self.child.stdout = self.stdout.take();
        self.child.stderr = self.stderr.take();
        let expired = self.expired.clone();
        let mut out = self.child.wait_with_output()?;
        out.status = map_status(out.status, &expired);
        Ok(out)
    }

    fn map(&self, s: ExitStatus) -> ExitStatus {
        map_status(s, &self.expired)
    }
}

fn map_status(s: ExitStatus, expired: &AtomicBool) -> ExitStatus {
    if expired.load(Ordering::SeqCst) {
        ExitStatus::from_raw(EXIT_DEADLINE << 8)
    } else if let Some(n) = s.signal() {
        ExitStatus::from_raw((128 + n) << 8)
    } else {
        s
    }
}

// Exception to law-a-unit-test-calls-no-binary: spawning a process is the code under test, so
// each test spawns only `/bin/sh` or `sleep`.
#[cfg(test)]
mod tests;
