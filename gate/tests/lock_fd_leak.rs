//! Its own test binary, one test per process (sp-j6ozkz): the lock fd below is opened with no
//! O_CLOEXEC on purpose, so inside the shared lib test binary any sibling test that spawned a
//! child while it was open inherited it, and the probe found the lock held: a red that
//! depended on scheduling, not on run_gate (it flaked in rounds r-auto-47 and r-auto-50).
//! Alone in its process, the only children are the ones run_gate starts.

/// sp-ohwg7: a podman conmon started by a gate trial held an flock the gate's CALLER
/// had open (a lander's `exec 9>…` lock, opened by bash with no O_CLOEXEC) for 53+
/// minutes after the caller exited. Reproduced here without podman: `cmd` backgrounds
/// a `sleep`, the same shape (a long-lived child, started from inside a locked
/// section, that outlives the trial) — before `close_inherited_fds`, it inherits the
/// open-but-not-CLOEXEC lock fd by plain fork, and the lock stays held after this test
/// drops its own reference.
#[test]
fn run_gate_never_lets_a_daemon_it_starts_inherit_the_callers_lock_fd() {
    use gate::ports::World;
    use std::ffi::CString;

    let dir = testkit::TempDir::new("gate-run-gate-fd-leak");
    let tree = dir.path();
    let lockfile = dir.join("caller.lock");
    let lock_c = CString::new(lockfile.as_os_str().as_encoded_bytes()).unwrap();

    // Simulate the lander's own `exec 9>lockfile; flock 9`: opened directly via
    // libc::open with no O_CLOEXEC — exactly what bash's redirection does, and
    // exactly what std::fs::File never does (SAFETY: a plain open/flock on a path we
    // own, cleaned up below).
    let lock_fd = unsafe { libc::open(lock_c.as_ptr(), libc::O_WRONLY | libc::O_CREAT, 0o644) };
    assert!(lock_fd >= 0, "open {}: {}", lockfile.display(), std::io::Error::last_os_error());
    assert_eq!(unsafe { libc::flock(lock_fd, libc::LOCK_EX) }, 0, "acquire the caller's lock");

    let real = gate::real::Real::new(std::path::PathBuf::new());
    let env: Vec<(String, String)> = vec![("PATH".into(), std::env::var("PATH").unwrap_or_default())];
    let (rc, out) = real.run_gate(
        tree,
        &env,
        "10",
        "sleep 30 >/dev/null 2>&1 & echo $! > child.pid",
    );
    assert_eq!(rc, 0, "trial command: {out}");

    // The caller exits: drop our own reference to the lock, exactly as the lander's
    // own fd 9 closes when its process exits.
    unsafe { libc::close(lock_fd) };

    // A fresh probe, from a fresh fd: free unless some other open file description —
    // the backgrounded "daemon", if it inherited one — still holds it.
    let probe_fd = unsafe { libc::open(lock_c.as_ptr(), libc::O_WRONLY, 0) };
    assert!(probe_fd >= 0);
    let free = unsafe { libc::flock(probe_fd, libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if free {
        unsafe { libc::flock(probe_fd, libc::LOCK_UN) };
    }
    unsafe { libc::close(probe_fd) };

    // Clean up the daemon regardless of the assertion below.
    if let Ok(s) = std::fs::read_to_string(tree.join("child.pid")) {
        if let Ok(pid) = s.trim().parse::<i32>() {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }

    assert!(free, "the backgrounded child inherited the caller's lock fd and is still holding it");
}
