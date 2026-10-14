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
use std::time::{Duration, Instant, SystemTime};

use crate::db::Conn;

const SWEEP_EVERY: Duration = Duration::from_secs(5);

static SPANS: std::sync::OnceLock<spira_otel::Sink> = std::sync::OnceLock::new();

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

    let check_every = match spira_config::process::cfg_parse::<u64>("SPIRA_LC_LIVE_CHECK_SECS") {
        Ok(n) if n >= 1 => Duration::from_secs(n),
        Ok(_) => {
            eprintln!("spira-lc serve: SPIRA_LC_LIVE_CHECK_SECS must be at least 1");
            return 2;
        }
        Err(e) => {
            eprintln!("spira-lc serve: {e}");
            return 2;
        }
    };

    let spans = spira_config::process::cfg("SPIRA_RUN")
        .and_then(|run| spira_config::process::cfg_parse::<u32>("SPIRA_OTEL_KEEP_DAYS").map(|keep| (run, keep)));
    match spans {
        Ok((run, keep)) => {
            let _ = SPANS.set(spira_otel::Sink::new(spira_otel::dir_of(std::path::Path::new(&run)), "spira-lc", keep));
        }
        Err(e) => {
            eprintln!("spira-lc serve: {e}");
            return 2;
        }
    }

    let conn = match Conn::from_env() {
        Ok(mut c) => {
            c.enable_live();
            c.enable_group_commit();
            Arc::new(c)
        }
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
    match conn.live_load() {
        Ok(n) => eprintln!("spira-lc serve: {n} live rows in memory"),
        Err(e) => eprintln!("spira-lc serve: live rows not loaded, reads go to Dolt until they are: {e}"),
    }
    std::thread::scope(|scope| {
        let checker = Arc::clone(&conn);
        scope.spawn(move || loop {
            std::thread::sleep(check_every);
            live_tick(&checker);
        });
        let sweeper = Arc::clone(&conn);
        scope.spawn(move || loop {
            std::thread::sleep(SWEEP_EVERY);
            cold_tick(&sweeper);
            if let Err(e) = crate::mending::expire_overdue(&sweeper, "spira-lc.sweep") {
                eprintln!("spira-lc serve: mending sweep: {e:?}");
            }
        });
        for conn_stream in listener.incoming() {
            match conn_stream {
                Ok(stream) => {
                    let conn = Arc::clone(&conn);
                    let accepted = Instant::now();
                    scope.spawn(move || handle(stream, &conn, accepted));
                }
                Err(e) => eprintln!("spira-lc serve: accept error: {e}"),
            }
        }
    });
    0
}

/// Between attempts to fetch the older rows: each is a read of the whole table.
const COLD_RETRY: Duration = Duration::from_secs(30);

fn cold_tick(conn: &Conn) {
    if conn.live.as_ref().is_some_and(|l| l.cold_due(COLD_RETRY)) {
        match conn.live_load_cold() {
            Ok(n) => eprintln!("spira-lc serve: {n} older rows in memory"),
            Err(e) => eprintln!("spira-lc serve: older rows not loaded, reads that need them go to Dolt: {e}"),
        }
    }
}

const LIVE_SETTLE_MARGIN: Duration = Duration::from_secs(2);

/// One pass of the timer: load the rows if they are not in memory, else check them against Dolt.
fn live_tick(conn: &Conn) {
    if let Some(live) = conn.live.as_ref().filter(|l| !l.is_loaded()) {
        if !live.settled(crate::db::QUERY_DEADLINE + LIVE_SETTLE_MARGIN) {
            return;
        }
        if let Err(e) = conn.live_load() {
            eprintln!("spira-lc serve: live rows still not loaded: {e}");
        }
        return;
    }
    if conn.live.as_ref().is_some_and(|l| l.verify_due(crate::db::QUERY_DEADLINE * 2 + LIVE_SETTLE_MARGIN)) {
        match conn.live_check() {
            Ok(c) if c.divergent.is_empty() => {}
            Ok(c) => eprintln!("spira-lc serve: an abandoned write committed after the reload; reconciled: {}\n{}", c.divergent.join(", "), crate::ops::last_writers(conn, &c.divergent)),
            Err(e) => eprintln!("spira-lc serve: post-reload verification failed: {e}"),
        }
        return;
    }
    match crate::ops::run_live_check(conn, &crate::ops::raise_incident) {
        Ok(c) if c.divergent.is_empty() => {}
        Ok(c) => eprintln!("spira-lc serve: live rows diverged from Dolt and were reloaded{}: {}\n{}", if c.explained { " (explained by an ambiguous write)" } else { "" }, c.divergent.join(", "), crate::ops::last_writers(conn, &c.divergent)),
        Err(e) => eprintln!("spira-lc serve: live check failed: {e}"),
    }
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

fn handle(stream: UnixStream, conn: &Arc<Conn>, accepted: Instant) {
    serve_one(stream, conn, SPANS.get(), accepted);
}

fn serve_one(stream: UnixStream, conn: &Arc<Conn>, spans: Option<&spira_otel::Sink>, accepted: Instant) {
    let received = Instant::now();
    let started_work = Arc::new(std::sync::Mutex::new(None::<Instant>));
    let started = SystemTime::now();
    let mut reader = BufReader::new(stream.try_clone().expect("clone unix stream"));
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let argv: Vec<String> = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("bad request: {e}");
            let _ = write_response(&stream, 2, &msg);
            record_span(spans, "-", "-", line.len(), msg.len(), 2, started, SystemTime::now(), Instant::now().saturating_duration_since(accepted));
            return;
        }
    };
    let caller = peer_pid(&stream).map(crate::slow::caller_of_pid).unwrap_or_else(|| "-".to_string());
    let verb = argv.first().cloned().unwrap_or_default();
    let map = std::fs::read_to_string(personas_path()).map(|t| crate::work::parse_persona_uids(&t)).unwrap_or_default();
    let (code, out) = match crate::work::bind_peer(&argv, peer_uid(&stream), &map) {
        Ok(argv) => {
            let worker = Arc::clone(conn);
            let who = caller.clone();
            let began = Arc::clone(&started_work);
            let limit = request_deadline(&argv);
            within(limit.saturating_sub(received.elapsed()), move || {
                *began.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
                crate::slow::set_caller(&who);
                crate::db::set_request_deadline(Some(received + limit));
                crate::dispatch(&argv, &worker)
            })
                .unwrap_or_else(|| (2, format!("cannot tell: {}", crate::db::DEADLINE_MESSAGE)))
        }
        Err(refusal) => refusal,
    };
    crate::slow::request_done(&verb, &caller, received.elapsed());
    let _ = write_response(&stream, code, &out);
    let began = *started_work.lock().unwrap_or_else(|e| e.into_inner());
    let waited = began.unwrap_or_else(Instant::now).saturating_duration_since(accepted);
    record_span(spans, &verb, &caller, line.len(), out.len(), code, started, SystemTime::now(), waited);
}

fn record_span(spans: Option<&spira_otel::Sink>, verb: &str, caller: &str, request_size: usize, response_size: usize, code: i32, start: SystemTime, end: SystemTime, queue_wait: Duration) {
    if let Some(sink) = spans {
        sink.emit(&spira_otel::Call {
            method: verb,
            caller,
            request_size: request_size as u64,
            response_size: response_size as u64,
            exit_code: code,
            start,
            end,
            queue_wait,
        });
    }
}

/// How long after receiving a store request this service stops working on it
/// (law-calls-have-deadlines: the receiver stops 3 s after receiving; the client waits 5 s).
pub const REQUEST_DEADLINE: Duration = Duration::from_secs(3);

/// The request's deadline: [`REQUEST_DEADLINE`] for every store call. Two verbs are batch
/// jobs this service brokers, not calls, and keep the bound they had: `work`'s lane verbs run a
/// harness tool (groomer sweep, bead filing, mail) for the aeon, and `facts-query` is census's
/// aggregate over the whole fact history.
fn request_deadline(argv: &[String]) -> Duration {
    match argv.first().map(String::as_str) {
        // batch-job: broker-run tools and census's aggregate, bounded at the query deadline.
        Some("work") | Some("facts-query") => crate::db::QUERY_DEADLINE,
        _ => REQUEST_DEADLINE,
    }
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

fn peer_cred(stream: &UnixStream) -> Option<Ucred> {
    use std::os::fd::AsRawFd;
    const SOL_SOCKET: i32 = 1;
    const SO_PEERCRED: i32 = 17;
    let mut cred = Ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<Ucred>() as u32;
    (unsafe { getsockopt(stream.as_raw_fd(), SOL_SOCKET, SO_PEERCRED, &mut cred, &mut len) } == 0).then_some(cred)
}

pub(crate) fn peer_uid(stream: &UnixStream) -> Option<u32> {
    peer_cred(stream).map(|c| c.uid)
}

fn peer_pid(stream: &UnixStream) -> Option<i32> {
    peer_cred(stream).map(|c| c.pid)
}

#[cfg(test)]
mod tests {
    fn ask(sink: Option<&spira_otel::Sink>, request: &str) -> (String, i32) {
        use std::io::{Read, Write};
        let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        (&client).write_all(request.as_bytes()).unwrap();
        let conn = std::sync::Arc::new(crate::db::Conn::unreachable());
        super::serve_one(server, &conn, sink, std::time::Instant::now());
        let mut got = String::new();
        (&client).read_to_string(&mut got).unwrap();
        let head: serde_json::Value = serde_json::from_str(got.lines().next().unwrap()).unwrap();
        (got, head["exit_code"].as_i64().unwrap() as i32)
    }

    #[test]
    fn a_served_request_writes_one_span_and_a_dead_sink_changes_nothing() {
        let dir = testkit::TempDir::new("lc-serve-otel");
        let sink = spira_otel::Sink::new(dir.path(), "spira-lc", 14);
        let req = "[\"no-such-verb\"]\n";
        let (reply, code) = ask(Some(&sink), req);
        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(files.len(), 1);
        let text = std::fs::read_to_string(files[0].path()).unwrap();
        assert_eq!(text.lines().count(), 1, "exactly one span per request");
        let r: opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest = serde_json::from_str(text.trim()).unwrap();
        let span = &r.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(span.name, "no-such-verb");
        let body_len = reply.split_once('\n').unwrap().1.len() as i64;
        assert!(span.attributes.iter().any(|a| a.key == "spira.queue_wait_us"), "queue wait is recorded on every span");
        let size = span.attributes.iter().find(|a| a.key == "rpc.response.size").unwrap();
        assert_eq!(format!("{:?}", size.value), format!("Some(AnyValue {{ value: Some(IntValue({body_len})) }})"));

        let blocked = dir.join("spira-lc.1970-01-01.jsonl");
        std::fs::write(&blocked, "x").unwrap();
        let dead = spira_otel::Sink::new(blocked.join("sub"), "spira-lc", 14);
        let before = spira_otel::dropped();
        let (reply2, code2) = ask(Some(&dead), req);
        assert_eq!((reply2, code2), (reply, code), "a failing sink leaves the reply and exit code unchanged");
        assert_eq!(spira_otel::dropped(), before + 1);
    }

    /// law-calls-have-deadlines: a store call is abandoned 3 s after it was received; only
    /// the brokered batch verbs keep the longer bound.
    #[test]
    fn a_store_call_is_abandoned_at_three_seconds_and_only_batch_verbs_wait_longer() {
        let d = |v: &str| super::request_deadline(&[v.to_string(), "x".to_string()]);
        for verb in ["show", "list", "event", "fact", "cut", "land", "stats", ""] {
            assert_eq!(d(verb), std::time::Duration::from_secs(3), "{verb}");
        }
        assert_eq!(d("work"), crate::db::QUERY_DEADLINE);
        assert_eq!(d("facts-query"), crate::db::QUERY_DEADLINE);
    }

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
    fn peer_pid_reads_the_connecting_process() {
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        assert_eq!(super::peer_pid(&a), Some(std::process::id() as i32));
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
