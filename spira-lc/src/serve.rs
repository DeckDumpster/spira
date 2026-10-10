//! The system service entry point (design §3.6.4): `spira-lc serve` binds the Unix socket
//! that carries the machine's own DB credential (via this process's environment) so that no
//! other process needs to read it, and holds one database connection for its whole
//! lifetime, so a request through it pays no connect or handshake.
//!
//! Every request runs through `crate::dispatch`, the exact function a one-shot CLI
//! invocation calls directly — there is one implementation of show/list/history/event, not
//! a daemon copy and a CLI copy that could drift apart.
//!
//! Deploys inert: nothing connects to this socket yet. The wire format (one JSON line in —
//! an argv array, exactly what `env::args().skip(1)` would have produced — one JSON line
//! out) is deliberately minimal; the aeon-facing semantic layer (design §3.5, a separate
//! bead) is the real client this exists for.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use crate::db::Conn;

pub fn run(args: &[String]) -> i32 {
    // An explicit argv[0] (the launching unit's own ExecStart) wins outright; otherwise the
    // one source of config (per Ryan 2026-10-05): $SPIRA_TOML's declared socket path, never
    // this process's own environment, and no literal fallback.
    let socket_path = match args.first().cloned() {
        Some(p) => p,
        None => match spira_config::process::cfg("SPIRA_LC_SOCKET") {
            Ok(p) => p,
            Err(e) => {
                eprintln!("spira-lc serve: {e}");
                return 2;
            }
        },
    };

    let conn = match Conn::from_env() {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("spira-lc serve: cannot configure a connection: {e:?}");
            return 2;
        }
    };

    let listener = match inherited_listener(
        std::env::var("LISTEN_PID").ok().as_deref(),
        std::env::var("LISTEN_FDS").ok().as_deref(),
        std::process::id(),
    ) {
        Some(l) => l,
        None => match bind_socket(&socket_path) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("spira-lc serve: cannot bind {socket_path}: {e}");
                return 2;
            }
        },
    };

    eprintln!("spira-lc serve: listening on {socket_path}");
    // A request may spawn a child (`work blocked` runs `mail`) that calls back into this
    // socket; serving one connection at a time deadlocks on that. The session mutex still
    // serializes the queries themselves.
    std::thread::scope(|scope| {
        for conn_stream in listener.incoming() {
            match conn_stream {
                Ok(stream) => {
                    let conn = Arc::clone(&conn);
                    scope.spawn(move || handle(stream, &conn));
                }
                Err(e) => eprintln!("spira-lc serve: accept error: {e}"),
            }
        }
    });
    0
}

/// The first descriptor systemd passes (fd 3) when it owns the listening socket, so a restart
/// of this process queues connections in the kernel instead of refusing them. Only honoured
/// when `LISTEN_PID` names this process.
fn inherited_listener(pid: Option<&str>, fds: Option<&str>, me: u32) -> Option<UnixListener> {
    use std::os::unix::io::FromRawFd;
    if pid?.parse::<u32>().ok()? != me || fds?.parse::<u32>().ok()? < 1 {
        return None;
    }
    // SAFETY: LISTEN_PID matched, so systemd opened fd 3 for this process and nothing else owns it.
    Some(unsafe { UnixListener::from_raw_fd(3) })
}

fn bind_socket(socket_path: &str) -> std::io::Result<UnixListener> {
    if std::path::Path::new(socket_path).exists() {
        let _ = std::fs::remove_file(socket_path);
    }
    let listener = UnixListener::bind(socket_path)?;
    // Group-readable/writable only: the operator's group (spira) can reach the socket, the
    // credential this process holds is never written to it, only used to answer requests.
    if let Ok(meta) = std::fs::metadata(socket_path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o660);
        let _ = std::fs::set_permissions(socket_path, perms);
    }
    Ok(listener)
}

fn handle(stream: UnixStream, conn: &Arc<Conn>) {
    let received = Instant::now();
    let mut reader = BufReader::new(stream.try_clone().expect("clone unix stream"));
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let argv: Vec<String> = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(e) => {
            let _ = write_response(&stream, 2, &format!("bad request: {e}"));
            return;
        }
    };
    let map = std::fs::read_to_string(personas_path()).map(|t| crate::work::parse_persona_uids(&t)).unwrap_or_default();
    let (code, out) = match crate::work::bind_peer(&argv, peer_uid(&stream), &map) {
        Ok(argv) => {
            let worker = Arc::clone(conn);
            within(crate::db::QUERY_DEADLINE.saturating_sub(received.elapsed()), move || crate::dispatch(&argv, &worker))
                .unwrap_or_else(|| (2, format!("cannot tell: {}", crate::db::DEADLINE_MESSAGE)))
        }
        Err(refusal) => refusal,
    };
    let _ = write_response(&stream, code, &out);
}

/// `f`'s answer, or `None` once `deadline` passes; the abandoned work finishes unobserved.
fn within<T: Send + 'static>(deadline: Duration, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(deadline).ok()
}

/// Beside the socket, so the one configured socket path locates it: `<uid> <persona>` lines.
fn personas_path() -> String {
    format!("{}.personas", spira_config::process::cfg("SPIRA_LC_SOCKET").unwrap_or_default())
}

#[repr(C)]
struct Ucred {
    pid: i32,
    uid: u32,
    gid: u32,
}

extern "C" {
    fn getsockopt(fd: i32, level: i32, name: i32, val: *mut Ucred, len: *mut u32) -> i32;
}

pub(crate) fn peer_uid(stream: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    const SOL_SOCKET: i32 = 1;
    const SO_PEERCRED: i32 = 17;
    let mut cred = Ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<Ucred>() as u32;
    (unsafe { getsockopt(stream.as_raw_fd(), SOL_SOCKET, SO_PEERCRED, &mut cred, &mut len) } == 0).then_some(cred.uid)
}

#[cfg(test)]
mod tests {
    #[test]
    fn inherited_listener_needs_this_process_and_a_descriptor() {
        assert!(super::inherited_listener(None, None, 7).is_none());
        assert!(super::inherited_listener(Some("8"), Some("1"), 7).is_none(), "another process's descriptors are not ours");
        assert!(super::inherited_listener(Some("7"), Some("0"), 7).is_none());
        assert!(super::inherited_listener(Some("x"), Some("1"), 7).is_none());
    }

    #[test]
    fn a_slow_request_is_abandoned_at_the_deadline_not_waited_for() {
        let (release, held) = std::sync::mpsc::channel::<()>();
        let slow = super::within(std::time::Duration::from_millis(300), move || {
            let _ = held.recv();
            1
        });
        assert_eq!(slow, None, "answered at the deadline while the work was still running");
        drop(release);
        assert_eq!(super::within(std::time::Duration::from_secs(5), || 7), Some(7));
    }

    #[test]
    fn a_response_is_a_header_line_then_the_raw_bytes_once() {
        use std::io::Read;
        let (a, mut b) = std::os::unix::net::UnixStream::pair().unwrap();
        let body = "{\"q\": \"x\"}\nsecond line";
        super::write_response(&a, 3, body).unwrap();
        drop(a);
        let mut got = String::new();
        b.read_to_string(&mut got).unwrap();
        assert_eq!(got, format!("{{\"exit_code\":3,\"len\":{}}}\n{body}", body.len()));
    }

    #[test]
    fn peer_uid_reads_the_connecting_process() {
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        let me = std::fs::metadata("/proc/self").unwrap();
        assert_eq!(super::peer_uid(&a), Some(std::os::unix::fs::MetadataExt::uid(&me)));
    }
}

fn write_response(mut stream: &UnixStream, exit_code: i32, stdout: &str) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(stdout.len() + 48);
    writeln!(buf, "{}", serde_json::json!({"exit_code": exit_code, "len": stdout.len()}))?;
    buf.extend_from_slice(stdout.as_bytes());
    stream.write_all(&buf)
}
