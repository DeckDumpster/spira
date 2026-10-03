//! The system service entry point (design §3.6.4): `spira-lc serve` binds the Unix socket
//! that carries the machine's own DB credential (via this process's environment) so that no
//! other process needs to read it, and holds one persistent `dolt` session for its whole
//! lifetime — the same `Conn`, reused, is what lets a transition through this daemon meet
//! the design's p99 < 50ms bench; a fresh CLI invocation cannot, because starting a new
//! `dolt` process and connection costs 150-230ms on its own, before any SQL runs.
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

use crate::db::Conn;

pub fn run(args: &[String]) -> i32 {
    let socket_path = args
        .first()
        .cloned()
        .or_else(|| std::env::var("SPIRA_LC_SOCKET").ok())
        .unwrap_or_else(|| "/run/spira-lc/sock".to_string());

    let conn = match Conn::persistent_session() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("spira-lc serve: cannot configure a connection: {e:?}");
            return 2;
        }
    };

    if std::path::Path::new(&socket_path).exists() {
        let _ = std::fs::remove_file(&socket_path);
    }
    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("spira-lc serve: cannot bind {socket_path}: {e}");
            return 2;
        }
    };
    // Group-readable/writable only: the operator's group (spira) can reach the socket, the
    // credential this process holds is never written to it, only used to answer requests.
    if let Ok(meta) = std::fs::metadata(&socket_path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o660);
        let _ = std::fs::set_permissions(&socket_path, perms);
    }

    eprintln!("spira-lc serve: listening on {socket_path}");
    // A request may spawn a child (`work blocked` runs `mail`) that calls back into this
    // socket; serving one connection at a time deadlocks on that. The session mutex still
    // serializes the queries themselves.
    std::thread::scope(|scope| {
        for conn_stream in listener.incoming() {
            match conn_stream {
                Ok(stream) => {
                    let conn = &conn;
                    scope.spawn(move || handle(stream, conn));
                }
                Err(e) => eprintln!("spira-lc serve: accept error: {e}"),
            }
        }
    });
    0
}

fn handle(stream: UnixStream, conn: &Conn) {
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
    let (code, out) = crate::dispatch(&argv, conn);
    let _ = write_response(&stream, code, &out);
}

fn write_response(mut stream: &UnixStream, exit_code: i32, stdout: &str) -> std::io::Result<()> {
    let resp = serde_json::json!({"exit_code": exit_code, "stdout": stdout});
    writeln!(stream, "{}", resp)
}
