//! Test scaffolding shared across the workspace. Contract: DESIGN.md.

use std::ffi::OsStr;
use std::io::Write;
use std::ops::Deref;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A fresh scratch directory under the system temp dir, removed with everything under it
/// when dropped — including when a failing test unwinds. The only way test code in this
/// workspace gets scratch space (spira-lint `tmp-leak`, sp-qgfdi).
#[derive(Debug)]
pub struct TempDir(PathBuf);

impl TempDir {
    /// `<temp_dir>/<tag>-<pid>-<n>`, created empty, its path canonical. `<n>` is
    /// process-wide, so two calls never share a directory.
    pub fn new(tag: &str) -> TempDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&d).unwrap_or_else(|e| panic!("mkdir {}: {e}", d.display()));
        TempDir(d.canonicalize().unwrap_or(d))
    }

    /// The directory. It lives exactly as long as this value.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<OsStr> for TempDir {
    fn as_ref(&self) -> &OsStr {
        self.0.as_os_str()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A spawned test fixture process, killed when this value is dropped — including when a
/// failing test unwinds out from under it (sp-r70dc).
///
/// A test fixture that stands in for a long-lived process (a fake aeon, a fake gate —
/// typically `bash -c "exec -a NAME sleep 9999"`, held alive only long enough for one
/// assertion) used to be killed by an explicit `child.kill(); child.wait();` written after
/// the assertion. A failed assertion, a `panic!`, or the test binary itself being killed
/// (gate timeout) skips that line, and the fixture outlives the test as an orphan — found
/// in production as a `gate.sh 9999` process surviving up to 2.8 hours, once even holding
/// a pipe open that failed an unrelated gate run.
///
/// `ChildGuard::spawn` makes the child the leader of its own process group
/// (`process_group(0)`), so `Drop` can kill the WHOLE group, not just the one pid — a
/// fixture that backgrounds a grandchild (`sh -c "sleep 9999 &"`) leaves nothing behind
/// either. Killing is idempotent: calling it after the child has already exited (on its
/// own, or via an explicit `guard.kill()` the test called itself to observe the kill's
/// effect) is a no-op, never a signal to a reused pid.
#[derive(Debug)]
pub struct ChildGuard(std::process::Child);

impl ChildGuard {
    /// Spawn `cmd` as the leader of a fresh process group and hold it in a guard.
    /// Panics if the spawn itself fails — this is test scaffolding, and a fixture that
    /// cannot start must fail the test that needed it, the same contract `write_exe` keeps.
    pub fn spawn(cmd: &mut Command) -> ChildGuard {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        let child = cmd.spawn().unwrap_or_else(|e| panic!("spawn {cmd:?}: {e}"));
        ChildGuard(child)
    }

    /// The child's pid (also its process group id, since it leads its own group).
    pub fn id(&self) -> u32 {
        self.0.id()
    }

    /// Kill the process group and wait for the leader to exit. Safe to call more than
    /// once — `Drop` calls it again as a backstop — and safe to call after the child has
    /// already exited on its own.
    pub fn kill(&mut self) {
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return;
        }
        // SAFETY: a plain signal(2) by pid. The negative pid form targets every process in
        // the group this child leads (it was spawned with process_group(0)), not just it.
        unsafe { libc::kill(-(self.0.id() as i32), libc::SIGKILL) };
        let _ = self.0.wait();
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Write `body` to `path` as an executable (mode 0755) that a test will then exec,
/// WITHOUT THIS PROCESS EVER HOLDING A WRITE DESCRIPTOR ON IT.
///
/// `fs::write` followed by an exec fails with ETXTBSY whenever another test thread forks
/// while the write descriptor is open. The child carries a copy of it until it execs, and
/// the kernel refuses to exec a file anyone has open for writing. A child process writes
/// the file here instead, so no fork in this process can inherit a descriptor on it.
pub fn write_exe(path: impl AsRef<Path>, body: &str) {
    let path = path.as_ref();
    // /bin/sh by absolute path, and /bin/cat INSIDE the script also by absolute path: another
    // test in the same binary may have set the process PATH to a directory with neither in it
    // (release/src/tests.rs does, under its own ENV_LOCK) — sp-e7fe2 fixed the outer `sh` but
    // left the inner `cat` a bare word still resolved through that same mutated PATH, so the
    // race it was meant to remove kept firing, just one level deeper ("sh: 1: cat: not found",
    // sp-tuupa).
    let mut c = Command::new("/bin/sh")
        .arg("-c")
        .arg("/bin/cat > \"$1\"")
        .arg("sh")
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .expect("sh to write an executable");
    c.stdin
        .take()
        .expect("its stdin")
        .write_all(body.as_bytes())
        .expect("the executable's body");
    assert!(c.wait().expect("the writer").success(), "could not write {}", path.display());
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|e| panic!("chmod 755 {}: {e}", path.display()));
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

thread_local! {
    static ENV_HELD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Process environment edits for one test, serialized across every test thread in the
/// binary and undone on drop — including when the test panics. The only way test code in
/// this workspace mutates the environment (spira-lint `env-set-var-leak`).
#[must_use]
pub struct EnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    saved: Vec<(String, Option<std::ffi::OsString>)>,
}

/// Hold the environment lock, then set (`Some`) or unset (`None`) each key.
pub fn env(edits: &[(&str, Option<&str>)]) -> EnvGuard {
    let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    ENV_HELD.with(|h| h.set(true));
    let mut saved = Vec::new();
    for (k, v) in edits {
        saved.push((k.to_string(), std::env::var_os(k)));
        match v {
            Some(v) => std::env::set_var(k, v),
            None => std::env::remove_var(k),
        }
    }
    EnvGuard { _lock: lock, saved }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in self.saved.drain(..).rev() {
            match v {
                Some(v) => std::env::set_var(&k, v),
                None => std::env::remove_var(&k),
            }
        }
        ENV_HELD.with(|h| h.set(false));
    }
}

/// Held by a test that READS the environment without editing it, so no [`env`] edit lands
/// under it. Reentrant: a no-op on a thread already holding an [`EnvGuard`].
pub fn env_read() -> Option<std::sync::MutexGuard<'static, ()>> {
    if ENV_HELD.with(|h| h.get()) {
        None
    } else {
        Some(ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> TempDir {
        TempDir::new(&format!("testkit-{tag}"))
    }

    /// How many processes on this box currently belong to process group `pgid` — read
    /// straight from `/proc/<pid>/stat` (field 5, past the LAST `)` so a `comm` containing
    /// its own parens, spaces or digits never misleads the split).
    fn group_member_count(pgid: u32) -> usize {
        let Ok(entries) = std::fs::read_dir("/proc") else { return 0 };
        entries
            .flatten()
            .filter(|e| e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()).is_some())
            .filter(|e| {
                let Ok(stat) = std::fs::read_to_string(e.path().join("stat")) else { return false };
                let Some((_, rest)) = stat.rsplit_once(')') else { return false };
                rest.split_whitespace().nth(2).and_then(|s| s.parse::<u32>().ok()) == Some(pgid)
            })
            .count()
    }

    #[test]
    fn a_panicking_test_leaves_no_child_behind() {
        let (tx, rx) = std::sync::mpsc::channel();
        let r = std::thread::spawn(move || {
            let mut c = Command::new("sleep");
            c.arg("9999");
            let g = ChildGuard::spawn(&mut c);
            tx.send(g.id()).unwrap();
            panic!("the test fails while the guard is still held");
        })
        .join();
        assert!(r.is_err(), "the spawned thread was expected to panic");
        let pid = rx.recv().unwrap();
        // Drop runs during the panicking thread's unwind, before join() returns — the kill
        // (and the wait() inside it) is already complete by the time we get here.
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists(), "pid {pid} survived the panic");
    }

    #[test]
    fn kill_takes_the_whole_group_a_backgrounded_grandchild_included() {
        let mut c = Command::new("sh");
        c.args(["-c", "sleep 9999 & exec sleep 9999"]);
        let mut g = ChildGuard::spawn(&mut c);
        let pgid = g.id();
        // Positive control: wait for the shell to actually have forked and backgrounded
        // its grandchild before asserting anything about the group.
        let mut tries = 0;
        while group_member_count(pgid) < 2 && tries < 200 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            tries += 1;
        }
        assert_eq!(group_member_count(pgid), 2, "fixture: leader + backgrounded grandchild both up");
        g.kill();
        assert_eq!(group_member_count(pgid), 0, "the whole group is gone, not just the leader");
    }

    #[test]
    fn kill_is_idempotent_and_safe_after_the_child_exited_on_its_own() {
        let mut c = Command::new("true");
        let mut g = ChildGuard::spawn(&mut c);
        let _ = g.0.wait();
        g.kill();
        g.kill();
    }


    #[test]
    fn a_temp_dir_is_fresh_distinct_and_gone_after_drop() {
        let a = TempDir::new("testkit-td");
        let b = TempDir::new("testkit-td");
        assert_ne!(a.path(), b.path());
        assert!(a.is_dir() && std::fs::read_dir(&a).unwrap().next().is_none());
        std::fs::create_dir_all(a.join("x/y")).unwrap();
        std::fs::write(a.join("x/y/f"), "z").unwrap();
        let p = a.to_path_buf();
        drop(a);
        assert!(!p.exists(), "{} survived its drop", p.display());
        assert!(b.is_dir());
    }

    #[test]
    fn a_panicking_test_still_removes_its_dir() {
        let (tx, rx) = std::sync::mpsc::channel();
        let r = std::thread::spawn(move || {
            let d = TempDir::new("testkit-panic");
            tx.send(d.to_path_buf()).unwrap();
            panic!("the test fails");
        })
        .join();
        assert!(r.is_err());
        let p = rx.recv().unwrap();
        assert!(!p.exists(), "{} survived an unwinding test", p.display());
    }

    #[test]
    fn the_file_holds_exactly_the_body_and_is_executable() {
        let d = dir("body");
        let p = d.join("x");
        let body = "#!/bin/sh\nprintf '%s' \"a b\"\n# no trailing newline after this";
        write_exe(&p, body);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), body);
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o755);
        let out = Command::new(&p).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "a b");
    }

    #[test]
    fn a_rewrite_replaces_the_old_body() {
        let d = dir("rewrite");
        let p = d.join("x");
        write_exe(&p, "#!/bin/sh\necho one\n");
        write_exe(&p, "#!/bin/sh\necho two\n");
        assert_eq!(String::from_utf8_lossy(&Command::new(&p).output().unwrap().stdout), "two\n");
    }

    /// THE RACE ITSELF. Eight threads each write a program and exec it at once, a hundred
    /// times over, while every one of them is forking. With `fs::write` in place of
    /// write_exe this fails within a few rounds; here every exec must succeed.
    #[test]
    fn concurrent_writers_and_forkers_never_see_text_file_busy() {
        let d = dir("race");
        let dp = d.to_path_buf();
        let hs: Vec<_> = (0..8)
            .map(|t| {
                let d = dp.clone();
                std::thread::spawn(move || {
                    for i in 0..100 {
                        let p = d.join(format!("p{t}-{i}"));
                        write_exe(&p, "#!/bin/sh\nexit 0\n");
                        let st = Command::new(&p).status().unwrap_or_else(|e| panic!("exec {}: {e}", p.display()));
                        assert!(st.success());
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
    }
}
