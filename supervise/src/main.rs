// spira-supervise: main-PID supervisor for collect.sh (or any configured child).
//
// Spawns the child, sends READY=1 when it is up, then sends WATCHDOG=1 on an
// interval — but only while the child is demonstrably healthy.  "Healthy" for
// the cockpit collector means the snapshot file is being written on schedule,
// measured by the same age threshold watchtower and doctor use (SPIRA_SNAP_STALE_S).
//
// A heartbeat sent while the child is hung would defeat the watchdog.  When
// the snapshot goes stale the supervisor stops pinging; systemd kills the
// unit via WatchdogSec and Restart= brings the pair back.
//
// Uses sd_notify directly — no exec to systemd-notify — because the manager
// credits a WATCHDOG=1 datagram only when its sender credentials match the
// unit's main PID, and exec to a child cannot satisfy that (sp-3az9w).
//
// Required environment:
//   SPIRA_RUN          runtime directory; snapshot is $SPIRA_RUN/cockpit.env
//
// Optional environment (defaults shown):
//   SPIRA_SNAP_STALE_S    60    stale threshold in seconds (same key as watchtower/doctor)
//   WATCHDOG_USEC         0     set by systemd when WatchdogSec= is configured
//   NOTIFY_SOCKET               set by systemd for sd_notify delivery
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

static SIGTERM_RECEIVED: AtomicBool = AtomicBool::new(false);
// Stores the child PID so the signal handler can forward SIGTERM immediately.
static CHILD_PID: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_sigterm(_: libc::c_int) {
    SIGTERM_RECEIVED.store(true, Ordering::Release);
    let pid = CHILD_PID.load(Ordering::Acquire);
    if pid > 0 {
        // kill is async-signal-safe.
        unsafe { libc::kill(pid, libc::SIGTERM); }
    }
}

// Send an sd_notify datagram to NOTIFY_SOCKET.  No-op when NOTIFY_SOCKET is
// absent (e.g. manual invocation or Type=simple without notify support).
fn sd_notify(msg: &str) {
    let sock_path = match env::var("NOTIFY_SOCKET") {
        Ok(p) if !p.is_empty() => p,
        _ => return,
    };
    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return;
        }
        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;

        // Abstract sockets start with '@' in the env var; the kernel sees '\0' instead.
        let path_bytes: Vec<u8> = if sock_path.starts_with('@') {
            let mut v = vec![0u8];
            v.extend_from_slice(&sock_path.as_bytes()[1..]);
            v
        } else {
            let mut v = sock_path.into_bytes();
            v.push(0); // null-terminate filesystem socket path
            v
        };

        let copy_len = path_bytes.len().min(addr.sun_path.len());
        std::ptr::copy_nonoverlapping(
            path_bytes.as_ptr(),
            addr.sun_path.as_mut_ptr() as *mut u8,
            copy_len,
        );

        let addr_len =
            (std::mem::offset_of!(libc::sockaddr_un, sun_path) + copy_len) as libc::socklen_t;

        libc::sendto(
            fd,
            msg.as_ptr() as *const libc::c_void,
            msg.len(),
            libc::MSG_NOSIGNAL,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            addr_len,
        );
        libc::close(fd);
    }
}

// Returns the age of the snapshot file in seconds, or None if it cannot be stat'd.
fn snapshot_age_secs(snap: &PathBuf) -> Option<u64> {
    let mtime = fs::metadata(snap).ok()?.modified().ok()?;
    SystemTime::now().duration_since(mtime).ok().map(|d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn scratch_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "supervise-test-{}-{}-{}",
            name,
            std::process::id(),
            SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ))
    }

    #[test]
    fn missing_snapshot_has_no_age() {
        let p = scratch_path("missing");
        assert_eq!(snapshot_age_secs(&p), None);
    }

    #[test]
    fn freshly_written_snapshot_is_a_few_seconds_old_at_most() {
        let p = scratch_path("fresh");
        fs::write(&p, "x").unwrap();
        let age = snapshot_age_secs(&p).expect("freshly written file must have an age");
        assert!(age < 5, "age was {age}s");
        let _ = fs::remove_file(&p);
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("spira-supervise: usage: spira-supervise <cmd> [args...]");
        return ExitCode::from(2);
    }

    let spira_run = match env::var("SPIRA_RUN") {
        Ok(r) if !r.is_empty() => r,
        _ => {
            eprintln!("spira-supervise: SPIRA_RUN must be set");
            return ExitCode::from(2);
        }
    };

    let stale_secs: u64 = env::var("SPIRA_SNAP_STALE_S")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);

    let watchdog_usec: u64 = env::var("WATCHDOG_USEC")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let ping_interval = if watchdog_usec > 0 {
        Duration::from_micros(watchdog_usec / 2)
    } else {
        Duration::from_secs(stale_secs.max(2) / 2)
    };

    let snap_path = PathBuf::from(&spira_run).join("cockpit.env");

    // Install SIGTERM handler before spawning so no signal is lost between
    // spawn and the CHILD_PID store.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_sigterm as *const () as libc::sighandler_t;
        libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
    }

    let mut child = match Command::new(&args[1]).args(&args[2..]).spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("spira-supervise: spawn {}: {}", &args[1], e);
            return ExitCode::FAILURE;
        }
    };

    CHILD_PID.store(child.id() as i32, Ordering::Release);

    // Child is running; the supervisor is the main PID and ready to heartbeat.
    sd_notify("READY=1\n");

    let mut last_ping: Option<Instant> = None;

    loop {
        thread::sleep(Duration::from_secs(1));

        if SIGTERM_RECEIVED.load(Ordering::Acquire) {
            // SIGTERM already forwarded to child by the handler; wait for it to exit.
            let _ = child.wait();
            return ExitCode::SUCCESS;
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                eprintln!(
                    "spira-supervise: child exited {}",
                    status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into())
                );
                return ExitCode::from(1);
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("spira-supervise: waitpid: {}", e);
                return ExitCode::FAILURE;
            }
        }

        let fresh = snapshot_age_secs(&snap_path)
            .map(|age| age < stale_secs)
            .unwrap_or(false);

        if watchdog_usec > 0 {
            let due = match last_ping {
                None => true,
                Some(t) => t.elapsed() >= ping_interval,
            };
            if fresh && due {
                sd_notify("WATCHDOG=1\n");
                last_ping = Some(Instant::now());
            }
        }
    }
}
