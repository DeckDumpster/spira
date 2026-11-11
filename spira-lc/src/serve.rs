//! The system service entry point (design §3.6.4): `spira-lc serve` binds the Unix socket
//! that carries the machine's own DB credential (via this process's environment) so that no
//! other process needs to read it, and holds one database connection for its whole
//! lifetime, so a request through it pays no connect or handshake.
//!
//! Every request runs through `crate::dispatch`, the exact function a one-shot CLI
//! invocation calls directly — there is one implementation of show/list/history/event, not
//! a daemon copy and a CLI copy that could drift apart.
//!
//! The wire format: one JSON line in — an argv array, exactly what `env::args().skip(1)`
//! would have produced, or `{"argv": [...], "len": N}` followed by exactly `N` bytes of the
//! stdin the CLI would have read — then one `{"exit_code", "len"[, "err_len"]}` line out,
//! followed by the stdout bytes and any stderr bytes. The composed verbs (composed.rs) are
//! answered here too, their bd half included; a verb neither serves is refused, never handed on.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime};

use crate::callers::Answer;
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

    // With SPIRA_LC_STORE_PATH set the control store is opened here, once, so a store that
    // cannot be opened stops the service instead of failing every request it would serve.
    let control = match crate::control::served::configured() {
        None => None,
        Some(Ok(store)) => Some(store),
        Some(Err(e)) => {
            eprintln!("spira-lc serve: the control store: {e}");
            return 2;
        }
    };

    if let Some(store) = &control {
        let model = store.contract().backend();
        let (keys, bytes) = model.size();
        eprintln!("spira-lc serve: read model: {keys} keys, {} KiB, at revision {}", bytes / 1024, model.revision());
    }

    // lc-serve is the store's only opener, so it is the one process that can ship it off-box.
    let shipping = match &control {
        None => None,
        Some(store) => {
            let shipper = crate::lifecycle_run_dir().and_then(|run| match crate::durability::configured(store.contract().backend().inner().clone(), &run) {
                None => Ok(None),
                Some(s) => s.map(Some),
            });
            match shipper {
                Ok(None) => {
                    eprintln!("spira-lc serve: SPIRA_LC_BACKUP_URL is empty: the control store is not shipped off-box");
                    // A floor left by an earlier shipper would pin compaction for good.
                    if let Err(e) = store.contract().backend().inner().clear_compaction_floor() {
                        eprintln!("spira-lc serve: the control store's compaction floor: {e}");
                        return 2;
                    }
                    None
                }
                Ok(Some(s)) => Some(s),
                Err(e) => {
                    eprintln!("spira-lc serve: the control store's shipper: {e}");
                    return 2;
                }
            }
        }
    };

    let conn = match Conn::from_env() {
        Ok(mut c) => {
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

    crate::plan::serve_on(Arc::clone(&conn));
    match crate::plan::read_watermark(&conn) {
        Ok(w) => crate::plan::install_mirror(w),
        Err(e) => {
            eprintln!("spira-lc serve: planning watermark unread, bd kv is mirrored from the next stamp: {e}");
            crate::plan::install_mirror(0);
        }
    }
    eprintln!("spira-lc serve: listening on {socket_path}");
    // A request may spawn a child (`work blocked` runs `mail`) that calls back into this
    // socket; serving one connection at a time deadlocks on that. The session mutex still
    // serializes the queries themselves.
    if crate::control::served::configured().is_none() {
        eprintln!("spira-lc serve: SPIRA_LC_STORE_PATH is unset: every lifecycle verb answers cannot-tell");
    }
    std::thread::scope(|scope| {
        match (spira_config::process::cfg_parse::<u64>("SPIRA_LC_SNAPSHOT_SECS"), spira_config::process::cfg("SPIRA_LC_SNAPSHOT")) {
            (Ok(secs), Ok(path)) if secs > 0 && !path.is_empty() => {
                let refresher = Arc::clone(&conn);
                scope.spawn(move || loop {
                    if let Err(e) = crate::snapshot::write(&refresher, &path) {
                        eprintln!("spira-lc serve: snapshot {path}: {e}");
                    }
                    std::thread::sleep(Duration::from_secs(secs));
                });
            }
            (Ok(_), Ok(_)) => {}
            (Err(e), _) | (_, Err(e)) => eprintln!("spira-lc serve: snapshot not written: {e}"),
        }
        if let Some(mut shipping) = shipping {
            scope.spawn(move || {
                let endpoint = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok();
                let metrics = spira_otel::metrics::MetricsSink::new(endpoint.as_deref(), "spira-lc", Duration::from_millis(250));
                let mut red = false;
                loop {
                    std::thread::sleep(shipping.every);
                    red = crate::durability::tick(&mut shipping, red, &metrics, &runtime::ports::WallClock);
                }
            });
        }
        if let Some(store) = control {
            scope.spawn(move || loop {
                std::thread::sleep(SWEEP_EVERY);
                gc_tick(&store);
            });
        }
        scope.spawn(crate::plan::run_mirror);
        scope.spawn(move || loop {
            std::thread::sleep(SWEEP_EVERY);
            alarm_tick();
            match crate::control::served::configured() {
                Some(Ok(store)) => match store_sweep(&store, crate::db::now_epoch()) {
                    Ok(n) if n > 0 => eprintln!("spira-lc serve: {n} overdue mending rows expired"),
                    Ok(_) => {}
                    Err(e) => eprintln!("spira-lc serve: mending sweep: {e}"),
                },
                Some(Err(e)) => eprintln!("spira-lc serve: mending sweep: the control store: {e}"),
                None => {}
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

/// One pass of the control store's GC controller: finalized keys, orphans and lapsed TTLs.
fn gc_tick<B: state_store::Backend>(store: &crate::control::store::Store<B>) {
    let contract = store.contract();
    if let Err(e) = state_store::gc::Controller::new(contract).tick(contract.now_ms()) {
        eprintln!("spira-lc serve: control-store gc: {e}");
    }
}

/// On the store the sweep is `Expire` on overdue MENDING rows; the dolt ticks read tables
/// the store does not have, and epics are reconciled by the `reconcile-content` verb.
fn store_sweep<B: state_store::Backend>(store: &crate::control::store::Store<B>, now: i64) -> Result<usize, String> {
    let request = crate::control::served::request_id();
    let call = crate::control::state::Call { now, request: &request };
    let argv: Vec<String> = ["mending", "sweep", "--actor", "spira-lc.sweep"].iter().map(|s| s.to_string()).collect();
    match crate::control::state::run(store, &argv, &call) {
        (0, out) => out.trim().parse().map_err(|_| format!("sweep answered {out:?}")),
        (code, out) => Err(format!("exit {code}: {out}")),
    }
}

fn alarm_tick() {
    let lines = crate::slow::latency_breaches(Instant::now());
    if lines.is_empty() {
        return;
    }
    let Some(path) = spira_config::process::cfg("SPIRA_RUN").ok().filter(|r| !r.is_empty()).map(|r| std::path::Path::new(&r).join("lc-latency.log")) else {
        return;
    };
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    if let Err(e) = std::fs::OpenOptions::new().create(true).append(true).open(&path).and_then(|mut f| f.write_all(text.as_bytes())) {
        eprintln!("spira-lc serve: {}: {e}", path.display());
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
    let (req, size) = match read_request(&mut reader) {
        Ok(None) => return,
        Ok(Some(r)) => r,
        Err((size, msg)) => {
            let _ = write_response(&stream, 2, &msg, "");
            record_span(spans, "-", "-", size, msg.len(), 2, started, SystemTime::now(), Instant::now().saturating_duration_since(accepted));
            return;
        }
    };
    let caller = peer_pid(&stream).map(crate::slow::caller_of_pid).unwrap_or_else(|| "-".to_string());
    let verb = req.argv.first().cloned().unwrap_or_default();
    let map = std::fs::read_to_string(personas_path()).map(|t| crate::work::parse_persona_uids(&t)).unwrap_or_default();
    let argv_seen = req.argv.clone();
    let ans = match crate::work::bind_peer(&req.argv, peer_uid(&stream), &map) {
        Ok(argv) => {
            let worker = Arc::clone(conn);
            let who = caller.clone();
            let began = Arc::clone(&started_work);
            let limit = request_deadline(&argv);
            let min_rev = req.min_rev;
            let req = Request { argv, stdin: req.stdin, caller: req.caller, min_rev };
            within(limit.saturating_sub(received.elapsed()), move || {
                *began.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
                crate::slow::set_caller(&who);
                crate::db::set_request_deadline(Some(received + limit));
                state_store::model::track(!min_rev.is_none_or(model_reached));
                let mut local = Local(&worker);
                let ans = answer(req, &mut local, &mut crate::bd::LiveBd, crate::content_cache::served(), &crate::lifecycle_run_dir);
                (ans, state_store::model::stamp())
            })
            .unwrap_or_else(|| (Answer::out(2, format!("cannot tell: {}", crate::db::DEADLINE_MESSAGE)), None))
        }
        Err((code, out)) => (Answer::out(code, out), None),
    };
    let (ans, rev) = ans;
    let method = crate::slow::method_of(&argv_seen);
    crate::slow::request_done(&verb, &method, &caller, received.elapsed());
    crate::slow::sample(&method, received.elapsed());
    let _ = write_response_at(&stream, ans.code, &ans.stdout, &ans.stderr, rev);
    let began = *started_work.lock().unwrap_or_else(|e| e.into_inner());
    let waited = began.unwrap_or_else(Instant::now).saturating_duration_since(accepted);
    record_span(spans, &verb, &caller, size, ans.stdout.len() + ans.stderr.len(), ans.code, started, SystemTime::now(), waited);
}

/// One request: the argv `env::args().skip(1)` would have produced, and the bytes the CLI's
/// stdin would have held, when the client sent any.
#[derive(Debug)]
pub(crate) struct Request {
    pub argv: Vec<String>,
    pub stdin: Option<String>,
    /// The caller's guard variables (`spira_config::lc_call::GUARD_VARS`), which a content
    /// write is judged on as bdq judged it in the caller's own process.
    pub caller: std::collections::BTreeMap<String, String>,
    /// The store revision the caller's answer must reflect (`{"min_rev": N}`): a caller that
    /// read its own write's revision off that write's reply. Its reads wait for the read model
    /// to reach it, briefly, and otherwise go to the database.
    pub min_rev: Option<u64>,
}

/// How long a request naming a `min_rev` waits for the read model before reading the database.
/// The model applies every commit before the commit returns, so a revision this service
/// answered is reached at once; the wait covers a model catching up after a failed commit.
const MIN_REV_WAIT: Duration = Duration::from_millis(250);

/// Whether the read model holds `rev`, waiting [`MIN_REV_WAIT`] for it; true with no control
/// store, whose dolt reads have no model to lag.
fn model_reached(rev: u64) -> bool {
    match crate::control::served::configured() {
        Some(Ok(store)) => store.contract().backend().wait_for(rev, MIN_REV_WAIT),
        _ => true,
    }
}

/// Longest request body read: a claim-verdicts batch or a section spec, never a file transfer.
const MAX_BODY: u64 = 16 << 20;

/// The request on `reader`: a JSON argv array on one line, or `{"argv": [...], "len": N}` on
/// one line followed by exactly `N` bytes of stdin; the object may also carry `caller` and
/// `min_rev`. `Ok(None)` when the client sent nothing;
/// `Err((bytes read, refusal))` for a request that cannot be read.
fn read_request(reader: &mut impl BufRead) -> Result<Option<(Request, usize)>, (usize, String)> {
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return Ok(None);
    }
    let bad = |e: &dyn std::fmt::Display| (line.len(), format!("bad request: {e}"));
    let head: serde_json::Value = serde_json::from_str(line.trim()).map_err(|e| bad(&e))?;
    let (argv, len, caller, min_rev) = match head {
        serde_json::Value::Array(_) => (head, None, Default::default(), None),
        serde_json::Value::Object(mut o) => {
            let len = match o.get("len") {
                None if o.contains_key("caller") || o.contains_key("min_rev") => None,
                l => Some(l.and_then(serde_json::Value::as_u64).ok_or_else(|| bad(&"a request object needs a numeric len"))?),
            };
            if let Some(len) = len.filter(|&l| l > MAX_BODY) {
                return Err(bad(&format!("a {len}-byte body is over the {MAX_BODY}-byte bound")));
            }
            let caller = match o.remove("caller") {
                None => Default::default(),
                Some(c) => serde_json::from_value(c).map_err(|e| bad(&format!("caller: {e}")))?,
            };
            let min_rev = match o.remove("min_rev") {
                None => None,
                Some(r) => Some(r.as_u64().ok_or_else(|| bad(&"min_rev is a revision number"))?),
            };
            (o.remove("argv").unwrap_or_default(), len, caller, min_rev)
        }
        _ => return Err(bad(&"a request is an argv array or an {argv, len} object")),
    };
    let argv: Vec<String> = serde_json::from_value(argv).map_err(|e| bad(&e))?;
    let stdin = match len {
        None => None,
        Some(n) => {
            let mut body = vec![0u8; n as usize];
            std::io::Read::read_exact(reader, &mut body).map_err(|e| bad(&format!("the {n}-byte body: {e}")))?;
            Some(String::from_utf8(body).map_err(|e| bad(&e))?)
        }
    };
    let size = line.len() + stdin.as_ref().map_or(0, String::len);
    Ok(Some((Request { argv, stdin, caller, min_rev }, size)))
}

/// The primitives, in this process against its own connection: a composed verb's reads and
/// events never go back out through the socket.
struct Local<'a>(&'a Conn);

impl crate::callers::Machine for Local<'_> {
    fn call(&mut self, args: &[String]) -> (i32, String) {
        crate::dispatch(args, self.0)
    }
}

/// The answer to one request. A composed verb (composed.rs) runs here with its bd half; any
/// other verb is a primitive, its stdin body carried inline exactly as the CLI carries it. A
/// verb neither serves is the primitives' own refusal — nothing falls through to a CLI.
pub(crate) fn answer(
    req: Request,
    m: &mut dyn crate::callers::Machine,
    bd: &mut dyn crate::callers::Bd,
    content: &dyn crate::composed::Content,
    run_dir: &dyn Fn() -> Result<std::path::PathBuf, String>,
) -> Answer {
    let Request { argv, stdin, caller, .. } = req;
    if !argv.first().is_some_and(|v| crate::composed::is_served(v)) {
        let argv = match stdin {
            Some(body) => crate::cutover::inline_payload_from(argv, &mut body.as_bytes()),
            None => argv,
        };
        let (code, out) = m.call(&argv);
        return Answer::out(code, out);
    }
    let mut body = stdin;
    let mut read_stdin = move || body.take().ok_or_else(|| "nothing was sent".to_string());
    let now = crate::db::now_epoch();
    let ports = crate::composed::Ports { m, bd, content, stdin: &mut read_stdin, run_dir, now, caller: &caller };
    let mut ans = crate::composed::run(&argv, ports);
    let id = argv.get(1).map(String::as_str).unwrap_or("");
    if let Some(e) = crate::composed::write_cert_log(id, &ans, run_dir(), now) {
        ans.stderr.push_str(&format!("{e}\n"));
    }
    ans
}

#[allow(clippy::too_many_arguments)]
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

// batch-job: just under the 310 s the `work` client waits for a tool-running verb.
pub const TOOL_DEADLINE: Duration = Duration::from_secs(305);

/// The request's deadline: [`REQUEST_DEADLINE`] for every store call. Two verbs are batch
/// jobs this service brokers, not calls, and keep the bound they had: `work`'s lane verbs run a
/// harness tool (groomer sweep, bead filing, mail) for the dwarf, and `facts-query` is census's
/// aggregate over the whole fact history.
fn request_deadline(argv: &[String]) -> Duration {
    match argv.first().map(String::as_str) {
        // batch-job: a work verb that runs a brokered tool waits the client's tool bound less a margin.
        Some("work") if argv.get(2).is_some_and(|v| crate::work::brokers_tool(v)) => TOOL_DEADLINE,
        // batch-job: census's aggregate, bounded at the query deadline.
        Some("work") | Some("facts-query") => crate::db::QUERY_DEADLINE,
        // batch-job: bounded under the client's 5 s wait; the verb owns its own shorter budget.
        Some("unpublished") => crate::unpublished::DEADLINE,
        // batch-job: a repair sweep over every row, bounded at the query deadline like census's.
        Some(v) if crate::composed::SWEEPS.contains(&v) => crate::db::QUERY_DEADLINE,
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

/// A header line, then the stdout bytes; a non-empty stderr follows them, its length in the
/// header's `err_len` (a client that predates it reads `len` bytes and stops).
fn write_response(stream: &UnixStream, exit_code: i32, stdout: &str, stderr: &str) -> std::io::Result<()> {
    write_response_at(stream, exit_code, stdout, stderr, None)
}

/// [`write_response`] with the store revision the answer reflects as the header's `rev`: every
/// read it made was at that revision or later, and every write it made is at or below it, so a
/// caller that needs its own write passes it back as `min_rev`. A client that predates it
/// ignores the key.
fn write_response_at(mut stream: &UnixStream, exit_code: i32, stdout: &str, stderr: &str, rev: Option<u64>) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(stdout.len() + stderr.len() + 64);
    let mut head = serde_json::json!({"exit_code": exit_code, "len": stdout.len()});
    if !stderr.is_empty() {
        head["err_len"] = stderr.len().into();
    }
    if let Some(rev) = rev {
        head["rev"] = rev.into();
    }
    writeln!(buf, "{head}")?;
    buf.extend_from_slice(stdout.as_bytes());
    buf.extend_from_slice(stderr.as_bytes());
    stream.write_all(&buf)
}

#[cfg(test)]
mod tests {
    fn lc(store: &crate::control::store::Store<state_store::MemBackend>, argv: &[&str], now: i64) -> String {
        let args: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let request = crate::control::served::request_id();
        let (code, out) = crate::control::state::run(store, &args, &crate::control::state::Call { now, request: &request });
        assert_eq!(code, 0, "{argv:?}: {out}");
        out
    }

    #[test]
    fn the_store_sweep_expires_an_overdue_mending_row_and_leaves_a_live_one() {
        use state_store::{Config, Contract, MemBackend, Ports};
        const AT: i64 = 1_767_225_600;
        let clock = runtime::ports::VirtualClock::new(AT as u64 * 1_000);
        let contract = Contract::open(MemBackend::new(), Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        let store = crate::control::store::Store::new(contract, 1_000);
        let tip = "1".repeat(40);
        let run = |argv: &[&str]| lc(&store, argv, AT);
        run(&["create-bead", "sp-a", "--repo", "spira", "--ready"]);
        run(&["event", "bead", "sp-a", "--expect", "READY", "--version", "0", "--actor", "t", "--kind", r#"{"Claim":{"holder":"dwarf-1","lease_until":1767229200}}"#]);
        run(&["event", "bead", "sp-a", "--expect", "WORKING", "--version", "1", "--actor", "t", "--kind", &format!(r#"{{"Submit":{{"tip":"{tip}"}}}}"#)]);
        run(&["cut", "b-1", "--repo", "spira", "--head", "H", "--base", "B", "--members", &format!("sp-a:{tip}"), "--actor", "t"]);
        let round = |expect: &str, v: u64, kind: &str| run(&["event", "batch", "b-1", "--expect", expect, "--version", &v.to_string(), "--actor", "t", "--kind", kind]);
        round("OPEN", 1, r#"{"PassStarted":{"n":1,"head":"H"}}"#);
        round("CI_RUNNING", 2, r#"{"SuitesStarted":{"n":1}}"#);
        round("CI_RUNNING", 3, r#"{"Failed":{"suite":"test-a.sh","n":1}}"#);
        round("CI_RUNNING", 4, r#"{"Failed":{"suite":"test-b.sh","n":1}}"#);
        for (suite, secs) in [("test-a.sh", "60"), ("test-b.sh", "3600")] {
            run(&["mending", "pickup", "b-1", "--pass", "1", "--suite", suite, "--deadline-secs", secs, "--actor", "mender"]);
        }
        let state = |suite: &str| store.mending("b-1", 1, suite).unwrap().unwrap().row.state.as_str().to_string();
        assert_eq!(super::store_sweep(&store, AT), Ok(0), "nothing is overdue yet");
        assert_eq!(super::store_sweep(&store, AT + 61), Ok(1));
        assert_eq!(state("test-a.sh"), "RETURNED", "the overdue row expired");
        assert_eq!(state("test-b.sh"), "MENDING", "the row within its deadline is left");
        assert_eq!(super::store_sweep(&store, AT + 61), Ok(0), "a second sweep finds nothing overdue");
    }

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
    fn unpublished_is_served_without_the_store_and_waits_longer_than_a_store_call() {
        let (reply, code) = ask(None, "[\"unpublished\"]\n");
        assert_eq!(code, 2, "{reply}");
        assert!(reply.contains("unpublished: usage"), "the verb answered, not the store: {reply}");
        let argv = |v: &str| vec![v.to_string()];
        assert!(super::request_deadline(&argv("unpublished")) > super::request_deadline(&argv("show")));
        assert!(super::request_deadline(&argv("unpublished")) < std::time::Duration::from_secs(5), "the server answers before the 5 s client gives up");
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
        let w = |v: &str| super::request_deadline(&["work".to_string(), "sp-x".to_string(), v.to_string()]);
        for verb in ["show", "note", "done", "blocked", "superseded-by"] {
            assert_eq!(w(verb), crate::db::QUERY_DEADLINE, "{verb}");
        }
        for verb in ["submit", "file-followup", "split"] {
            assert_eq!(w(verb), super::TOOL_DEADLINE, "{verb}");
        }
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
    fn a_response_is_a_header_line_then_the_raw_bytes_once() {
        use std::io::Read;
        let (a, mut b) = std::os::unix::net::UnixStream::pair().unwrap();
        let body = "{\"q\": \"x\"}\nsecond line";
        super::write_response(&a, 3, body, "").unwrap();
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

    // ---- the composed verbs, served (sp-ipwlic) -------------------------------------------

    use crate::callers::world::World;
    use crate::callers::Machine;

    /// bd's content half as a double: answers every read with its argv, records every write.
    #[derive(Default)]
    struct Content(std::cell::RefCell<Vec<String>>);

    impl crate::composed::Content for Content {
        fn read(&self, args: &[String]) -> Result<String, String> {
            Ok(format!("read {}", args.join(" ")))
        }
        fn write(&self, args: &[&str]) -> Result<String, String> {
            self.0.borrow_mut().push(args.join(" "));
            Ok(String::new())
        }
        fn write_stdin(&self, args: &[String], stdin: Option<&str>) -> Result<String, String> {
            let body = stdin.map_or(String::new(), |b| format!(" <<{b}"));
            self.0.borrow_mut().push(format!("{}{body}", args.join(" ")));
            Ok(format!("wrote {}", args.join(" ")))
        }
        fn delete(&self, _: &[String]) -> Result<(), String> {
            Ok(())
        }
        fn ask_label(&self) -> Result<String, String> {
            // literal-ok: test fixture label
            Ok("needs-ryan".into())
        }
        fn registry(&self) -> spira_config::repos::Registry {
            spira_config::repos::Registry::new(Some("spira|/r/spira|main|gate.sh\n"), &Default::default(), std::path::Path::new("/r/spira"))
        }
    }

    /// One request through the server's own reader, answer and writer, with `w` as its store and
    /// bd; the client half is spira-lc's own socket client.
    fn over(w: &World, content: &Content, argv: &[&str], stdin: Option<&str>) -> (i32, String, String) {
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let body = stdin.map(str::to_string);
        let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        let asked = std::thread::spawn(move || crate::client::exchange(client, &argv, body.as_deref()));
        let mut reader = std::io::BufReader::new(server.try_clone().unwrap());
        let (req, _) = super::read_request(&mut reader).unwrap().unwrap();
        let (mut m, mut b) = (w, w);
        let ans = super::answer(req, &mut m, &mut b, content, &|| Err("no run dir here".to_string()));
        super::write_response(&server, ans.code, &ans.stdout, &ans.stderr).unwrap();
        drop(server);
        asked.join().unwrap()
    }

    fn filed(w: &World, id: &str) {
        w.file(id, "task", None, &[]);
        w.ok(&["create-bead", id, "--ready"]);
    }

    #[test]
    fn hold_held_and_unhold_are_answered_by_the_socket_with_the_clis_exit_codes() {
        let (w, c) = (World::new(), Content::default());
        filed(&w, "sp-a");
        assert_eq!(over(&w, &c, &["held", "sp-a", "wait"], None).0, 1, "not held yet");
        let (code, _, err) = over(&w, &c, &["hold", "sp-a", "wait", "snooze-until:9999999999", "groomer"], None);
        assert_eq!(code, 0, "{err}");
        assert_eq!(over(&w, &c, &["held", "sp-a", "wait"], None).0, 0);
        assert_eq!(over(&w, &c, &["holds", "sp-a"], None).1.trim(), "wait");
        assert_eq!(over(&w, &c, &["unhold", "sp-a", "wait", "groomer"], None).0, 0);
        assert_eq!(over(&w, &c, &["held", "sp-a", "wait"], None).0, 1);
        let (code, _, err) = over(&w, &c, &["hold", "sp-a", "nonsense"], None);
        assert_ne!(code, 0);
        assert!(!err.is_empty(), "a refusal keeps its text, on stderr: {err}");
    }

    #[test]
    fn close_reads_its_reason_from_the_request_body_and_writes_bd_on_the_server() {
        let (w, c) = (World::new(), Content::default());
        filed(&w, "sp-a");
        let (code, _, err) = over(&w, &c, &["close", "sp-a", "--reason-file", "-", "--dropped", "--actor", "queue"], None);
        assert_eq!(code, 2, "no body sent: {err}");
        assert!(err.contains("--reason-file: stdin: nothing was sent"), "{err}");
        assert!(w.writes.borrow().is_empty());
        let (code, _, err) = over(&w, &c, &["close", "sp-a", "--reason-file", "-", "--dropped", "--actor", "queue"], Some("not needed after all\n"));
        assert_eq!(code, 0, "{err}");
        assert_eq!(*w.writes.borrow(), ["close sp-a"], "the server wrote bd");
        assert!(w.lc(&["show", "sp-a"]).1.contains("DROPPED"));
    }

    #[test]
    fn close_ask_closes_the_ask_and_settles_its_decision_bead() {
        let (w, c) = (World::new(), Content::default());
        w.file("sp-k", "task", None, &[]);
        w.ok(&["create-ask", "sp-k"]);
        let (code, out, err) = over(&w, &c, &["close-ask", "sp-k", "--exit", "answered", "--quote", "yes", "--actor", "operator", "--channel", "mail"], None);
        assert_eq!(code, 0, "{out}{err}");
        assert_eq!(*w.writes.borrow(), ["close sp-k"], "the decision bead closed with it");
    }

    #[test]
    fn content_is_served_and_keeps_its_refusal_text() {
        let (w, c) = (World::new(), Content::default());
        assert_eq!(over(&w, &c, &["content", "show", "sp-a", "--json"], None), (0, "read show sp-a --json".into(), String::new()));
        assert_eq!(over(&w, &c, &["content", "config", "get", "issue_prefix"], None).1, "read config get issue_prefix");
        let (code, _, err) = over(&w, &c, &["content", "delete", "sp-a"], None);
        assert_eq!(code, 3);
        assert_eq!(err, "spira-lc content: `delete sp-a` is not a content verb\n");
        assert_eq!(over(&w, &c, &["bd-note", "sp-a", "hello"], None).0, 0);
        assert_eq!(*c.0.borrow(), ["note sp-a hello"]);
    }

    #[test]
    fn a_scheduler_task_is_refused_the_whole_history_over_the_served_door() {
        let (w, c) = (World::new(), Content::default());
        let whole = ["content", "list", "--all", "--limit", "0", "--json"];
        crate::slow::set_caller("spira-runtime");
        let (code, out, err) = over(&w, &c, &whole, None);
        assert_eq!((code, out.as_str()), (3, ""), "{err}");
        assert!(err.contains("exit 3"), "{err}");
        assert!(c.0.borrow().is_empty(), "bd was never asked");
        crate::slow::set_caller("spira-lc");
        assert_eq!(over(&w, &c, &whole, None).0, 0, "any other caller is served");
    }

    /// A content write's guard variables ride the request to the server, which refuses the
    /// czar's shadow-stage mutation with bdq's exit code and text, and writes nothing.
    #[test]
    fn a_czar_content_write_is_judged_on_the_callers_variables_on_the_server() {
        let (w, c) = (World::new(), Content::default());
        let caller: std::collections::BTreeMap<String, String> =
            [("SPIRA_RUNE", "czar"), ("SPIRA_CZAR_CLASS", "deadlock"), ("SPIRA_CZAR_TRIGGER_BEAD", "sp-t")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let argv: Vec<String> = ["content", "update", "sp-a", "--add-label", "x"].iter().map(|s| s.to_string()).collect();
        let line = spira_config::lc_call::request_head(&argv, None, &caller).unwrap();
        let mut wire = std::io::Cursor::new(format!("{line}\n").into_bytes());
        let (req, _) = super::read_request(&mut wire).unwrap().unwrap();
        assert_eq!(req.caller, caller, "the variables survive the wire");
        assert_eq!(req.stdin, None, "no len, no body");
        let (mut m, mut b) = (&w, &w);
        let ans = super::answer(req, &mut m, &mut b, &c, &|| Err("no run dir here".to_string()));
        assert_eq!((ans.code, ans.stderr.as_str()), (1, "czar-fence: deadlock is shadow — mutation refused (SPIRA_CZAR_STAGE_DEADLOCK=act to enable)\n"));
        assert!(c.0.borrow().is_empty(), "nothing was written");

        let mut acting = caller.clone();
        acting.insert("SPIRA_CZAR_STAGE_DEADLOCK".into(), "act".into());
        let req = super::Request { argv: argv.clone(), stdin: None, caller: acting, min_rev: None };
        let ans = super::answer(req, &mut m, &mut b, &c, &|| Err("no run dir here".to_string()));
        assert_eq!(ans.code, 0, "{ans:?}");
        assert_eq!(*c.0.borrow(), ["update sp-a --add-label x"]);

        let line = spira_config::lc_call::request_head(&argv, Some(2), &caller).unwrap();
        let mut wire = std::io::Cursor::new(format!("{line}\nhi").into_bytes());
        assert_eq!(super::read_request(&mut wire).unwrap().unwrap().0.stdin.as_deref(), Some("hi"), "a body still rides with its len");
        let mut wire = std::io::Cursor::new(b"{\"argv\":[\"list\"]}\n".to_vec());
        assert!(super::read_request(&mut wire).is_err(), "an object with neither len nor caller is still refused");
    }

    #[test]
    fn a_request_may_name_the_revision_its_answer_must_reflect_and_the_answer_names_the_one_it_does() {
        use std::io::Read;
        let mut wire = std::io::Cursor::new(b"{\"argv\":[\"show\",\"sp-a\"],\"min_rev\":42}\n".to_vec());
        let (req, _) = super::read_request(&mut wire).unwrap().unwrap();
        assert_eq!((req.argv.as_slice(), req.min_rev, req.stdin), (&["show".to_string(), "sp-a".to_string()][..], Some(42), None));
        let mut wire = std::io::Cursor::new(b"{\"argv\":[\"show\"],\"min_rev\":\"x\"}\n".to_vec());
        assert!(super::read_request(&mut wire).is_err(), "a min_rev that is not a revision is refused");
        assert_eq!(super::read_request(&mut std::io::Cursor::new(b"[\"show\"]\n".to_vec())).unwrap().unwrap().0.min_rev, None);

        let (a, mut b) = std::os::unix::net::UnixStream::pair().unwrap();
        super::write_response_at(&a, 0, "{}\n", "", Some(43)).unwrap();
        super::write_response(&a, 0, "{}\n", "").unwrap();
        drop(a);
        let mut got = String::new();
        b.read_to_string(&mut got).unwrap();
        let heads: Vec<serde_json::Value> = got.lines().filter(|l| l.starts_with("{\"")).map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(heads[0]["rev"], 43, "{got}");
        assert!(heads[1].get("rev").is_none(), "an answer that touched no store names no revision: {got}");
    }

    // ---- content writes over the socket: one test per sub-verb ------------------------------

    /// `argv` (after `content`) over the socket: the answer, and what the server wrote to bd.
    fn content_write(argv: &[&str], stdin: Option<&str>) -> ((i32, String, String), Vec<String>) {
        let (w, c) = (World::new(), Content::default());
        let full: Vec<&str> = std::iter::once("content").chain(argv.iter().copied()).collect();
        let ans = over(&w, &c, &full, stdin);
        let wrote = c.0.borrow().clone();
        (ans, wrote)
    }

    fn written(argv: &[&str], stdin: Option<&str>, want: &str) {
        let ((code, out, err), wrote) = content_write(argv, stdin);
        assert_eq!(code, 0, "{argv:?}: {err}");
        assert_eq!(out, format!("wrote {}", argv.join(" ")), "bd's stdout is the answer");
        assert_eq!(wrote, [want], "the server wrote bd once, with bd's own argv");
    }

    #[test]
    fn content_label_add_and_remove_are_written_by_the_server() {
        written(&["label", "add", "sp-a", "spira:x"], None, "label add sp-a spira:x");
        written(&["label", "remove", "sp-a", "spira:x"], None, "label remove sp-a spira:x");
        written(&["update", "sp-a", "--add-label", "x", "--remove-label", "y"], None, "update sp-a --add-label x --remove-label y");
    }

    /// bd as the content door writes it, over `World`'s planning plane: a create files the
    /// bead and prints its id, a close closes it.
    struct Filing<'w>(&'w World, &'w str);

    impl crate::composed::Content for Filing<'_> {
        fn read(&self, _: &[String]) -> Result<String, String> {
            Ok(String::new())
        }
        fn write(&self, _: &[&str]) -> Result<String, String> {
            Ok(String::new())
        }
        fn write_stdin(&self, args: &[String], _: Option<&str>) -> Result<String, String> {
            match args.first().map(String::as_str) {
                Some("create") => {
                    self.0.file(self.1, "task", None, &[]);
                    Ok(format!("{}\n", self.1))
                }
                Some("close") => {
                    let mut b = self.0;
                    crate::callers::Bd::close(&mut b, &args[1], "")?;
                    Ok(String::new())
                }
                _ => Ok(String::new()),
            }
        }
        fn delete(&self, _: &[String]) -> Result<(), String> {
            Ok(())
        }
        fn ask_label(&self) -> Result<String, String> {
            // literal-ok: test fixture label
            Ok("needs-ryan".into())
        }
        fn registry(&self) -> spira_config::repos::Registry {
            spira_config::repos::Registry::new(Some("spira|/r/spira|main|gate.sh\n"), &Default::default(), std::path::Path::new("/r/spira"))
        }
    }

    fn served(w: &World, c: &dyn crate::composed::Content, argv: &[&str], caller: &[(&str, &str)]) -> super::Answer {
        let req = super::Request {
            argv: std::iter::once("content").chain(argv.iter().copied()).map(str::to_string).collect(),
            stdin: None,
            caller: caller.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            min_rev: None,
        };
        let (mut m, mut b) = (w, w);
        super::answer(req, &mut m, &mut b, c, &|| Err("no run dir here".to_string()))
    }

    fn state(w: &World, id: &str) -> Option<spira_config::lc_state::Row> {
        match w.lc(&["show", id]) {
            (0, out) => spira_config::lc_state::parse_show(&out).unwrap(),
            _ => None,
        }
    }

    /// A create through the door leaves the lifecycle state a create through bdq leaves: the
    /// row, READY, written by the server in the same request.
    #[test]
    fn a_content_create_gets_its_lifecycle_row_in_the_same_request() {
        let w = World::new();
        let ans = served(&w, &Filing(&w, "sp-new"), &["create", "T", "--type", "task", "--silent"], &[]);
        assert_eq!((ans.code, ans.stdout.as_str(), ans.stderr.as_str()), (0, "sp-new\n", ""), "{ans:?}");
        assert_eq!(state(&w, "sp-new").map(|r| r.state), Some("READY".to_string()), "the row bdq's after_create_ids writes");
        let ans = served(&w, &Filing(&w, "sp-ev"), &["create", "Probe", "--type", "event", "--silent"], &[]);
        assert_eq!(ans.code, 0, "{ans:?}");
        assert!(state(&w, "sp-ev").is_none(), "a rowless type gets no row, as through bdq");
    }

    #[test]
    fn a_lifecycle_row_the_server_cannot_write_is_reported_and_bds_create_stands() {
        let w = World::new();
        let ans = served(&w, &Filing(&w, "not an id"), &["create", "T", "--silent"], &[]);
        assert_eq!((ans.code, ans.stdout.as_str()), (0, "not an id\n"), "bd's create stands: no retry of it");
        assert!(ans.stderr.starts_with("spira-lc content: LIFECYCLE: cannot read any created bead id"), "reported, never swallowed: {ans:?}");
    }

    /// A close is a state verb: refused as bdq refuses it unless the caller overrides, and an
    /// overridden close leaves the row terminal, as bdq's after_close does.
    #[test]
    fn a_content_close_is_refused_unless_overridden_and_then_its_row_is_terminal() {
        let w = World::new();
        filed(&w, "sp-a");
        let f = Filing(&w, "sp-a");
        let ans = served(&w, &f, &["close", "sp-a", "--reason", "landed"], &[]);
        assert_eq!(ans.code, 1);
        assert!(ans.stderr.starts_with("spira: bdq refuses `close`"), "{ans:?}");
        assert_eq!(w.status("sp-a"), "open", "nothing written");
        let ans = served(&w, &f, &["close", "sp-a", "--reason", "landed"], &[("SPIRA_BDQ_STATE_WRITE", "test")]);
        assert_eq!(ans.code, 0, "{ans:?}");
        assert_eq!(ans.stderr, "spira-lc content: STATE WRITE OVERRIDE (close sp-a --reason landed) — bd's status moved outside the lifecycle machine: test\n", "the override is logged");
        assert_eq!(w.status("sp-a"), "closed");
        assert!(state(&w, "sp-a").is_some_and(|r| r.terminal()), "the row follows the close: {:?}", state(&w, "sp-a"));
    }

    #[test]
    fn content_note_is_written_by_the_server_with_its_text_or_the_request_body() {
        written(&["note", "sp-a", "a note"], None, "note sp-a a note");
        written(&["note", "sp-a", "--stdin"], Some("line one\nline two\n"), "note sp-a --stdin <<line one\nline two\n");
        let ((code, _, err), wrote) = content_write(&["note", "sp-a", "--stdin"], None);
        assert_eq!((code, err.as_str()), (2, "spira-lc content: stdin: nothing was sent\n"), "no body, no write");
        assert!(wrote.is_empty());
    }

    #[test]
    fn content_update_set_metadata_is_written_by_the_server() {
        written(&["update", "sp-a", "--set-metadata", "claimdesc_hash=abc", "--set-metadata", "n=1"], None, "update sp-a --set-metadata claimdesc_hash=abc --set-metadata n=1");
    }

    #[test]
    fn content_create_is_written_by_the_server_with_the_request_body() {
        written(&["create", "T", "--type", "task", "--label", "spira", "--silent", "--priority", "3", "--body-file", "-"], Some("## Outcome\n"), "create T --type task --label spira --silent --priority 3 --body-file - <<## Outcome\n");
        written(&["create", "--parent", "sp-e", "--silent", "--title", "child"], None, "create --parent sp-e --silent --title child");
    }

    #[test]
    fn content_sub_verbs_not_served_still_refuse_with_their_text_and_write_nothing() {
        for (argv, why) in [
            (&["delete", "sp-a"][..], "`delete sp-a` is not a content verb"),
            (&["reopen", "sp-a"], "`reopen sp-a` is not a content verb"),
            (&["set-state", "sp-a", "rune=x"], "`set-state sp-a rune=x` is not a content verb"),
            (&["dep", "add", "sp-a", "sp-b"], "`dep add sp-a sp-b` is not a content verb"),
            (&["close", "sp-a", "--reason", "r", "--force"], "`close sp-a --reason r --force` is not a content verb"),
            (&["update", "sp-a", "--status", "open"], "update takes only --add-label/--remove-label <label> and --set-metadata <key=value>, not `--status`"),
            (&["create", "T", "--body-file", "/etc/passwd"], "create takes its body only as --body-file - (the request body)"),
        ] {
            let ((code, out, err), wrote) = content_write(argv, None);
            assert_eq!((code, out.as_str(), err), (3, "", format!("spira-lc content: {why}\n")), "{argv:?}");
            assert!(wrote.is_empty(), "{argv:?} wrote {wrote:?}");
        }
    }

    #[test]
    fn content_writes_keep_the_three_second_bound_under_the_clients_five() {
        for argv in [&["label", "add", "sp-a", "x"][..], &["close", "sp-a"], &["note", "sp-a", "--stdin"], &["update", "sp-a", "--set-metadata", "k=v"], &["create", "T"]] {
            let full: Vec<String> = std::iter::once("content").chain(argv.iter().copied()).map(str::to_string).collect();
            assert_eq!(super::request_deadline(&full), std::time::Duration::from_secs(3), "{argv:?}");
        }
        assert_eq!(crate::client::TIMEOUT, std::time::Duration::from_secs(5));
    }

    /// A primitive's stdin body is carried inline, exactly as the CLI carries its own stdin.
    #[test]
    fn a_primitives_body_is_inlined_as_its_json() {
        struct Rec(Vec<Vec<String>>);
        impl Machine for Rec {
            fn call(&mut self, args: &[String]) -> (i32, String) {
                self.0.push(args.to_vec());
                (0, String::new())
            }
        }
        let w = World::new();
        let (mut rec, mut bd) = (Rec(Vec::new()), &w);
        let req = super::Request { argv: vec!["claim-verdicts".into()], stdin: Some("[{\"bead_id\":\"sp-a\"}]".into()), caller: Default::default(), min_rev: None };
        super::answer(req, &mut rec, &mut bd, &Content::default(), &|| Err(String::new()));
        assert_eq!(rec.0, vec![vec!["claim-verdicts".to_string(), "--json".into(), "[{\"bead_id\":\"sp-a\"}]".into()]]);
    }

    /// sp-6n7nuv: a verb the socket does not serve is refused by the server itself — the
    /// git-proved caller verbs included — and never falls through to anything.
    #[test]
    fn the_real_server_answers_a_composed_verb_and_refuses_an_unserved_one() {
        let (reply, code) = ask(None, "[\"hold\"]\n");
        assert_eq!(code, 2, "{reply}");
        assert!(reply.contains("<poison|ask|wait|manual>"), "the composed verb answered its own usage: {reply}");
        for verb in ["deliver", "content-on-base", "no-such-verb"] {
            let (reply, code) = ask(None, &format!("[\"{verb}\", \"sp-a\"]\n"));
            assert_eq!(code, 2, "{verb}: {reply}");
            assert!(reply.contains("usage: spira-lc show"), "{verb} is refused, not run: {reply}");
        }
    }

    #[test]
    fn a_request_body_is_read_to_exactly_its_length() {
        let mut r = std::io::Cursor::new(b"{\"argv\":[\"close\",\"sp-a\"],\"len\":5}\nhelloEXTRA".to_vec());
        let (req, _) = super::read_request(&mut r).unwrap().unwrap();
        assert_eq!((req.argv, req.stdin.as_deref()), (vec!["close".to_string(), "sp-a".into()], Some("hello")));
        let mut short = std::io::Cursor::new(b"{\"argv\":[\"close\"],\"len\":9}\nhi".to_vec());
        assert!(super::read_request(&mut short).unwrap_err().1.starts_with("bad request"));
        let mut huge = std::io::Cursor::new(format!("{{\"argv\":[\"x\"],\"len\":{}}}\n", super::MAX_BODY + 1).into_bytes());
        assert!(super::read_request(&mut huge).is_err(), "an oversized body is refused before it is read");
    }

    #[test]
    fn a_composed_verb_keeps_the_three_second_bound_and_only_its_sweeps_are_batch_jobs() {
        let d = |v: &str| super::request_deadline(&[v.to_string(), "x".to_string()]);
        for verb in ["close", "reopen", "reconcile-closed", "close-ask", "hold", "unhold", "drop", "disposition", "held", "reply", "content"] {
            assert_eq!(d(verb), std::time::Duration::from_secs(3), "{verb}");
        }
        for verb in crate::composed::SWEEPS {
            assert_eq!(d(verb), crate::db::QUERY_DEADLINE, "{verb}");
        }
    }

    #[test]
    fn a_stderr_answer_rides_after_stdout_with_its_own_length() {
        use std::io::Read;
        let (a, mut b) = std::os::unix::net::UnixStream::pair().unwrap();
        super::write_response(&a, 3, "out", "why\n").unwrap();
        drop(a);
        let mut got = String::new();
        b.read_to_string(&mut got).unwrap();
        assert_eq!(got, "{\"exit_code\":3,\"len\":3,\"err_len\":4}\noutwhy\n");
    }
}
