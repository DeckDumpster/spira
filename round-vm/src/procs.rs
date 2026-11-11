//! Process identity, file locking and subprocess construction.

use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use crate::schema::ProcId;

/// Start time of `pid` (field 22 of /proc/<pid>/stat), or None if it does not exist.
pub fn start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm (field 2) may contain spaces and parentheses; everything after the LAST ')' is
    // fields 3.. separated by single spaces.
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(19)?.parse().ok()
}

impl ProcId {
    pub fn current() -> ProcId {
        let pid = std::process::id();
        ProcId { pid, start: start_time(pid).unwrap_or(0) }
    }

    pub fn of(pid: u32) -> Option<ProcId> {
        start_time(pid).map(|start| ProcId { pid, start })
    }

    /// The same process is still running: same pid AND same start time, so a recycled pid
    /// never reads as the owner.
    pub fn alive(&self) -> bool {
        start_time(self.pid) == Some(self.start)
    }
}

/// An exclusive `flock` on a file, released on drop.
pub struct FileLock {
    _file: File,
}

impl FileLock {
    pub fn exclusive(path: &Path) -> Result<FileLock, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        loop {
            // SAFETY: flock on a descriptor this function owns for the lock's lifetime.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if rc == 0 {
                return Ok(FileLock { _file: file });
            }
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::Interrupted {
                return Err(format!("flock {}: {err}", path.display()));
            }
        }
    }
}

impl FileLock {
    /// `None` when another descriptor holds the lock.
    pub fn try_exclusive(path: &Path) -> Result<Option<FileLock>, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let file = OpenOptions::new().create(true).truncate(false).write(true).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        // SAFETY: flock on a descriptor this function owns for the lock's lifetime.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Some(FileLock { _file: file }));
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::WouldBlock { Ok(None) } else { Err(format!("flock {}: {err}", path.display())) }
    }
}

const GUARDED_SIGNALS: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP];

fn guarded_set() -> libc::sigset_t {
    // SAFETY: sigset_t is plain data initialised by sigemptyset before use.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for s in GUARDED_SIGNALS {
            libc::sigaddset(&mut set, s);
        }
        set
    }
}

/// Sends `script` to the child's stdin and closes it. A BrokenPipe is the shell having exited
/// before reading all of it: not a fault here, so the caller goes on to the shell's own status.
pub fn feed_script(child: &mut std::process::Child, script: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let Some(mut si) = child.stdin.take() else { return Ok(()) };
    match si.write_all(script) {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(e),
        _ => Ok(()),
    }
}

/// A `Command` whose child starts with the termination signals unblocked, whatever this
/// process has blocked for its own signal thread (see [`block_termination_signals`]).
pub fn command<S: AsRef<std::ffi::OsStr>>(program: S) -> Command {
    let mut cmd = Command::new(program);
    // SAFETY: pthread_sigmask is async-signal-safe; the closure touches nothing else.
    unsafe {
        cmd.pre_exec(|| {
            let set = guarded_set();
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
            Ok(())
        });
    }
    cmd
}

/// Blocks SIGTERM/SIGINT/SIGHUP in the calling thread (and every thread it spawns
/// afterwards) and starts a thread that waits for one of them and hands it to `on_signal`.
/// Call before spawning any other thread.
pub fn block_termination_signals<F>(on_signal: F)
where
    F: FnOnce(i32) + Send + 'static,
{
    let set = guarded_set();
    // SAFETY: masking signals for this thread; the set was built above.
    unsafe {
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
    std::thread::spawn(move || {
        let set = guarded_set();
        let mut sig: libc::c_int = 0;
        // SAFETY: sigwait on a set blocked in every thread of this process.
        let rc = unsafe { libc::sigwait(&set, &mut sig) };
        if rc == 0 {
            on_signal(sig);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exited_early_shell() -> (std::process::Child, String) {
        use std::io::Read;
        let mut c = command("sh").args(["-c", "echo refused >&2; exit 7"]).stdin(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
        let mut said = String::new();
        c.stderr.take().unwrap().read_to_string(&mut said).unwrap();
        (c, said)
    }

    #[test]
    fn a_script_fed_to_a_shell_that_already_exited_reports_the_shells_status_and_stderr() {
        let (mut c, said) = exited_early_shell();
        feed_script(&mut c, &vec![b'#'; 200_000]).unwrap();
        assert_eq!(c.wait().unwrap().code(), Some(7));
        assert_eq!(said.trim(), "refused");
    }

    #[test]
    fn a_stdin_write_error_other_than_broken_pipe_is_still_returned() {
        let mut c = command("sh").args(["-c", "exit 0"]).stdin(std::process::Stdio::null()).spawn().unwrap();
        c.stdin = Some(std::process::ChildStdin::from(std::os::fd::OwnedFd::from(File::open("/dev/null").unwrap())));
        assert!(feed_script(&mut c, b"x").is_err());
        let _ = c.wait();
    }

    #[test]
    fn current_process_is_alive_and_a_different_start_time_is_not() {
        let me = ProcId::current();
        assert!(me.start > 0);
        assert!(me.alive());
        assert!(!ProcId { pid: me.pid, start: me.start + 1 }.alive());
    }
}
