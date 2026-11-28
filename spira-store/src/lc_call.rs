//! The store's door from Rust: [`LcClient`] speaks spira-lc's socket protocol in-process (one
//! JSON argv line in; a `{"exit_code","len"}` line and `len` bytes out), so a caller pays no
//! process. Running `spira-lc` bounded, without resolving anything through the caller's PATH,
//! survives here only as the declared stand-in route.

use std::collections::BTreeMap;
use std::io::Write;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use ports::{Dial, Stream};

mod message;
pub use message::{Exchange, UnixDial, UnixPort};

pub const LC_TIMEOUT: Duration = Duration::from_secs(15); // batch-job: spira-lc requests queue behind a loaded Dolt in the round VM

/// The file beside a store's socket that makes it a pilgrim world: a store of its own that no
/// dispatcher claims from, where the pilgrim may file, link and close beads.
pub fn pilgrim_world_marker(socket: &str) -> String {
    format!("{socket}.pilgrim-world")
}

/// The file a sim world's directory carries; only a socket inside such a directory may be marked
/// a pilgrim world.
pub const SIM_WORLD_MARKER: &str = ".sim-world";

pub fn socket_in_sim_world(socket: &str) -> bool {
    let path = std::path::Path::new(socket);
    path.is_absolute() && path.parent().is_some_and(|dir| dir.join(SIM_WORLD_MARKER).is_file())
}

/// spira-lc's exit code for a request the machine looked at and refused.
pub const REFUSED: i32 = 3;
/// spira-lc's exit code for "cannot tell": the machine could not answer.
pub const CANNOT_TELL: i32 = 2;
/// law-calls-have-deadlines: the service stops 3 s after receiving; a client waits 5 s.
pub const DEADLINE: Duration = Duration::from_secs(5);

/// What the service says of a call it cut at its own deadline, and the text it is built on.
pub const DEADLINE_MESSAGE: &str = "deadline";
pub const DEADLINE_ANSWER: &str = "cannot tell: deadline";

/// The repair sweeps over every row, which the service bounds at its query deadline.
const SWEEPS: &[&str] = &["reconcile-asks", "reconcile-epics", "reconcile-content", "backfill-repo", "backfill-claim-inputs", "drop-orphans"];
/// How long a client waits for a sweep: the service's query deadline plus the reply.
pub const SWEEP_DEADLINE: Duration = Duration::from_secs(12); // batch-job: a sweep the service bounds at its 10 s query deadline

/// How long a client waits for `argv`: a watch waits its own hold and a second to answer, a sweep
/// the service's query deadline, every other verb [`DEADLINE`].
pub fn call_wait(argv: &[String]) -> Duration {
    match argv.first().map(String::as_str) {
        Some("watch") => {
            let hold = argv.iter().position(|a| a == "--wait-ms").and_then(|i| argv.get(i + 1)).and_then(|v| v.parse::<u64>().ok());
            Duration::from_millis(hold.unwrap_or(WATCH_HOLD_MS)) + WATCH_ANSWER
        }
        Some(v) if SWEEPS.contains(&v) => SWEEP_DEADLINE,
        _ => DEADLINE,
    }
}

/// How long one call waits: [`call_wait`], cut to a shorter deadline the caller set.
fn client_wait(argv: &[String], deadline: Duration) -> Duration {
    let wait = call_wait(argv);
    if deadline == DEADLINE { wait } else { wait.min(deadline) }
}

/// What a watch's client waits beyond the hold for the reply to arrive.
const WATCH_ANSWER: Duration = Duration::from_secs(1);
/// The one hold every watcher sends, and what a watch naming none waits: the service's longest.
pub const WATCH_HOLD_MS: u64 = 30_000;

/// Why a call has no answer to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LcError {
    /// The machine looked at the request and refused it (exit 3).
    Refused(String),
    /// No answer: unreachable, lost, past its deadline, or "cannot tell".
    Fault(String),
    /// "Cannot tell" because the store ended a read at its read limit; the store answered.
    ReadEnded(String),
    /// Nothing answered the socket (refused or absent), and nothing was sent.
    NotRunning(String),
}

impl std::fmt::Display for LcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LcError::Refused(m) | LcError::Fault(m) | LcError::ReadEnded(m) | LcError::NotRunning(m) => f.write_str(m),
        }
    }
}

impl From<LcError> for String {
    fn from(e: LcError) -> String {
        e.to_string()
    }
}

/// A call's answer: its exit code, what it printed, and (a CLI only) what it printed on stderr.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reply {
    pub code: i32,
    pub out: String,
    pub err: String,
    /// The service ended one of the request's store reads at its read limit.
    pub read_ended: bool,
}

impl Reply {
    /// The first non-empty line of the reason a non-zero answer gives.
    pub fn why(&self) -> String {
        let first = |t: &str| t.lines().find(|l| !l.trim().is_empty()).map(str::to_string);
        first(&self.err).or_else(|| first(&self.out)).unwrap_or_else(|| "no message".into())
    }

    /// The stdout when the call applied (exit 0); a refusal (3) or anything else is an error.
    pub fn into_answer(self) -> Result<String, LcError> {
        match self.code {
            0 => Ok(self.out),
            REFUSED => Err(LcError::Refused(self.text())),
            _ if self.read_ended => Err(LcError::ReadEnded(self.text())),
            _ => Err(LcError::Fault(self.text())),
        }
    }

    /// Everything the call printed, trimmed, stdout first.
    pub fn text(&self) -> String {
        format!("{}{}", self.out.trim(), self.err.trim())
    }
}

/// How a request reaches the machine. `Err` is a call that produced no answer at all.
pub trait Transport: Send + Sync {
    fn exchange(&self, argv: &[String], stdin: Option<&str>, deadline: Duration) -> Result<Reply, String>;

    /// [`exchange`](Self::exchange) with a socket nobody answers told apart from a fault; `not_running_wait` is how
    /// long a refused or absent socket is waited for.
    fn exchange_kind(&self, argv: &[String], stdin: Option<&str>, deadline: Duration, _not_running_wait: Duration) -> Result<Reply, LcError> {
        self.exchange(argv, stdin, deadline).map_err(LcError::Fault)
    }
}

/// The service on its Unix socket: no process is launched.
pub struct Socket {
    path: PathBuf,
}

impl Socket {
    fn call(&self, argv: &[String], stdin: Option<&str>, deadline: Duration, not_running_wait: Duration) -> Result<Reply, Fail> {
        let wait = client_wait(argv, deadline);
        retrying_resent(argv, &self.path, wait, not_running_wait, |left| {
            exchange_once(connect_within(&self.path, left)?, argv, stdin)
        })
    }
}

impl Transport for Socket {
    fn exchange(&self, argv: &[String], stdin: Option<&str>, deadline: Duration) -> Result<Reply, String> {
        self.call(argv, stdin, deadline, deadline).map_err(Fail::into_text)
    }

    fn exchange_kind(&self, argv: &[String], stdin: Option<&str>, deadline: Duration, not_running_wait: Duration) -> Result<Reply, LcError> {
        self.call(argv, stdin, deadline, not_running_wait).map_err(|f| match f {
            Fail::NotRunning(m) => LcError::NotRunning(m),
            f => LcError::Fault(f.into_text()),
        })
    }
}

/// Connects to the store's socket. A refused or absent socket is the store not running and
/// nothing was sent; any other failure is final.
pub fn connect(path: &Path) -> Result<Box<dyn Stream>, Fail> {
    connect_within(path, DEADLINE)
}

/// [`connect`] with each read and write on the stream bounded by `limit`.
pub fn connect_within(path: &Path, limit: Duration) -> Result<Box<dyn Stream>, Fail> {
    let dial = UnixDial { path: path.to_path_buf(), limit: limit.max(Duration::from_millis(1)) };
    dial.dial().map_err(|e| connect_failure(path, &e))
}

fn unanswering(path: &Path, e: &dyn std::fmt::Display) -> Fail {
    Fail::Final(format!("cannot tell: the lifecycle service is not answering on {}: {e} — nothing was read or written", path.display()))
}

/// What a failed connect to the store's socket means: a refused or absent socket is the store not
/// running and nothing was sent; any other failure is final.
pub fn connect_failure(path: &Path, e: &std::io::Error) -> Fail {
    match e.kind() {
        ErrorKind::ConnectionRefused | ErrorKind::NotFound => Fail::NotRunning(format!("store not running: nothing was sent to {} ({e})", path.display())),
        _ => unanswering(path, e),
    }
}

/// A recorded `unpublished` measurement read this many seconds ago is answered, and read, as stale.
pub const UNPUBLISHED_STALE_AFTER_SECS: u64 = 60;

/// Whether `argv` only reads: the one kind of call that may be sent again when the service
/// drops it, because asking twice changes nothing. A write is never in this list.
pub fn is_read(argv: &[String]) -> bool {
    let a: Vec<&str> = argv.iter().map(String::as_str).collect();
    matches!(
        a.as_slice(),
        ["show" | "list" | "history" | "stats" | "claimable", ..] | ["content", "list" | "show" | "memories" | "state" | "search" | "events", ..]
            | ["content", "dep" | "label", "list", ..] | ["fact", "list", ..]
            | ["git-fact", "get", ..]
    )
}

/// Why one attempt has no answer: `NotRunning` is a socket refused or absent, where nothing was
/// sent, so any call may repeat; `Dropped` is a connection the service took down or never
/// answered, which a read may repeat; `Final` is anything else.
#[derive(Debug, PartialEq, Eq)]
pub enum Fail {
    NotRunning(String),
    Dropped(String),
    Final(String),
}

impl Fail {
    pub fn into_text(self) -> String {
        let (Fail::NotRunning(m) | Fail::Dropped(m) | Fail::Final(m)) = self;
        m
    }
}

const RETRY_EVERY: Duration = Duration::from_millis(50);

static MINTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn mint_request_id() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("call:{}-{nanos}-{}", std::process::id(), MINTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

/// [`retrying`] for a call whose answer is a [`Reply`]. A write goes out under one request id
/// (the one in scope, else a minted one) and, when the service answers that it cut it at its
/// deadline, is sent again under that id while `budget` can cover another attempt: the service's
/// ledger answers the re-send with the first send's result. Reads, refusals and dropped
/// connections are as in [`retrying`].
pub fn retrying_resent(argv: &[String], target: &Path, budget: Duration, not_running_wait: Duration, mut once: impl FnMut(Duration) -> Result<Reply, Fail>) -> Result<Reply, Fail> {
    if is_read(argv) {
        return retrying(argv, target, budget, not_running_wait, once);
    }
    let id = current_request_id().unwrap_or_else(mint_request_id);
    let began = Instant::now();
    as_request(&id, || {
        let mut resent = false;
        loop {
            let left = budget.saturating_sub(began.elapsed());
            let answer = retrying(argv, target, left, not_running_wait, &mut once);
            match &answer {
                Ok(r) if r.code == CANNOT_TELL && r.text() == DEADLINE_ANSWER && began.elapsed() + RETRY_EVERY * 2 < budget => {
                    resent = true;
                    std::thread::sleep(RETRY_EVERY);
                }
                Ok(r) if resent && r.code == 0 => {
                    eprintln!("spira-lc: the store cut the write at its deadline; re-sent under request {id}");
                    return answer;
                }
                _ => return answer,
            }
        }
    })
}

/// Runs `once` (given the time left) until it answers. A `NotRunning` attempt is tried again
/// for any argv, within `not_running_wait` of the start; a read whose attempt was `Dropped` is tried
/// again within `budget`, measured from the first attempt that reached the service. A write
/// that reached the service has exactly one attempt.
pub fn retrying<T>(argv: &[String], target: &Path, budget: Duration, not_running_wait: Duration, mut once: impl FnMut(Duration) -> Result<T, Fail>) -> Result<T, Fail> {
    let began = Instant::now();
    let mut reached: Option<Instant> = None;
    loop {
        let attempt = Instant::now();
        match once(reached.map_or(budget, |r| budget.saturating_sub(r.elapsed()))) {
            Ok(v) => return Ok(v),
            Err(Fail::Final(m)) => return Err(Fail::Final(m)),
            Err(Fail::NotRunning(_)) if began.elapsed() >= not_running_wait => {
                return Err(Fail::NotRunning(format!("store not running: nothing was sent; waited {} s for {}", began.elapsed().as_secs(), target.display())));
            }
            Err(Fail::NotRunning(_)) => std::thread::sleep(RETRY_EVERY.min(not_running_wait.saturating_sub(began.elapsed()))),
            Err(Fail::Dropped(m)) => {
                let since = *reached.get_or_insert(attempt);
                if !is_read(argv) || since.elapsed() + RETRY_EVERY >= budget {
                    return Err(Fail::Dropped(m));
                }
                std::thread::sleep(RETRY_EVERY);
            }
        }
    }
}

const BOUNDING: &[&str] = &["--id", "--external-ref", "--parent", "--updated-after", "--closed-after", "--created-after"];

/// Whether `argv` is a `content list` of every bead there is: `--all --limit 0` with nothing
/// that bounds it to ids, one external reference, a parent or a time window (a label can match every bead). The store
/// refuses it from a scheduler task or the cockpit collector.
pub fn reads_whole_history<S: AsRef<str>>(argv: &[S]) -> bool {
    let a: Vec<&str> = argv.iter().map(AsRef::as_ref).collect();
    if a.get(..2) != Some(&["content", "list"][..]) {
        return false;
    }
    let unlimited = a.windows(2).any(|w| matches!(w[0], "--limit" | "-n") && w[1] == "0") || a.contains(&"--limit=0");
    let bounded = a.iter().any(|x| BOUNDING.contains(&x.split('=').next().unwrap_or("")));
    a.contains(&"--all") && unlimited && !bounded
}

/// The caller's environment a `content` write carries to the service: the variables bdq's write
/// guards read from their own environment (`bead::guard`) — the rune, the czar class and
/// trigger bead, and every czar stage. The service judges the write on these, as bdq would have
/// in this process.
pub const GUARD_VARS: &[&str] = &["SPIRA_RUNE", "SPIRA_CZAR_CLASS", "SPIRA_CZAR_TRIGGER_BEAD", "BEAD_CROSS_REPO_CONSIDERED", "BEAD_STATUTE_CONSIDERED", "BEAD_CONTRACT_CONSIDERED"];
/// Every `SPIRA_CZAR_STAGE_<CLASS>` is a guard variable.
pub const GUARD_PREFIX: &str = "SPIRA_CZAR_STAGE_";

/// The guard variables among `env`.
pub fn guard_vars(env: impl IntoIterator<Item = (String, String)>) -> BTreeMap<String, String> {
    env.into_iter().filter(|(k, _)| GUARD_VARS.contains(&k.as_str()) || k.starts_with(GUARD_PREFIX)).collect()
}

/// What a request for `argv` carries of this process's environment: its guard variables for a
/// `content` call, nothing for any other.
pub fn caller_of(argv: &[String]) -> BTreeMap<String, String> {
    if argv.first().map(String::as_str) == Some("content") {
        guard_vars(std::env::vars())
    } else {
        BTreeMap::new()
    }
}

thread_local! {
    static TASK: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// Runs `f` with this thread's store calls named for `task`; the outer name returns afterwards,
/// on unwind too. A thread `f` spawns sends none.
pub fn as_task<T>(task: &str, f: impl FnOnce() -> T) -> T {
    struct Restore(String);
    impl Drop for Restore {
        fn drop(&mut self) {
            TASK.with(|t| *t.borrow_mut() = std::mem::take(&mut self.0));
        }
    }
    let _restore = Restore(TASK.with(|t| std::mem::replace(&mut *t.borrow_mut(), task.to_string())));
    f()
}

thread_local! {
    static REQUEST: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// Runs `f` with this thread's store calls sent under request `id`, so the store answers a
/// resend with the first send's result; the outer id returns afterwards, on unwind too.
pub fn as_request<T>(id: &str, f: impl FnOnce() -> T) -> T {
    struct Restore(String);
    impl Drop for Restore {
        fn drop(&mut self) {
            REQUEST.with(|t| *t.borrow_mut() = std::mem::take(&mut self.0));
        }
    }
    let _restore = Restore(REQUEST.with(|t| std::mem::replace(&mut *t.borrow_mut(), id.to_string())));
    f()
}

/// The request id this thread's calls are sent under; an empty id is none.
pub fn current_request_id() -> Option<String> {
    REQUEST.with(|t| Some(t.borrow().clone()).filter(|t| !t.is_empty()))
}

/// `f` under `task` when one is given, else as it stands: how a thread spawned inside a task carries its name.
pub fn as_task_of<T>(task: Option<&str>, f: impl FnOnce() -> T) -> T {
    match task {
        Some(task) => as_task(task, f),
        None => f(),
    }
}

/// The task this thread's calls are made for; an empty name is none.
pub fn current_task() -> Option<String> {
    TASK.with(|t| Some(t.borrow().clone()).filter(|t| !t.is_empty()))
}

/// A request's first line: the argv as a JSON array, or, with a body, caller variables or a
/// task or request id, an `{"argv"[, "len"][, "caller"][, "task"][, "request_id"]}` object (`len` only when a body follows).
pub fn request_head(argv: &[String], body_len: Option<usize>, caller: &BTreeMap<String, String>, task: Option<&str>, request_id: Option<&str>) -> Result<String, serde_json::Error> {
    if body_len.is_none() && caller.is_empty() && task.is_none() && request_id.is_none() {
        return serde_json::to_string(argv);
    }
    let mut o = serde_json::Map::new();
    o.insert("argv".into(), serde_json::json!(argv));
    if let Some(n) = body_len {
        o.insert("len".into(), n.into());
    }
    if !caller.is_empty() {
        o.insert("caller".into(), serde_json::json!(caller));
    }
    if let Some(t) = task {
        o.insert("task".into(), t.into());
    }
    if let Some(r) = request_id {
        o.insert("request_id".into(), r.into());
    }
    serde_json::to_string(&o)
}

/// One request on `stream`: the argv as one JSON line, or, with `stdin`, a `{"argv", "len"}`
/// line followed by exactly `len` bytes of it. The answer is a `{"exit_code", "len"[,
/// "err_len"][, "read_ended"]}` line, then `len` bytes of stdout and `err_len` of stderr.
///
/// Once a request is on the wire the service may apply it whatever happens to the reply, so a
/// lost reply is "cannot tell", never a retry.
pub fn exchange_once(mut stream: Box<dyn Stream>, argv: &[String], stdin: Option<&str>) -> Result<Reply, Fail> {
    let bytes = encode_request(argv, stdin, current_task().as_deref(), current_request_id().as_deref())?;
    match Exchange::new(bytes).poll_exchange(stream.as_mut(), &mut Context::from_waker(Waker::noop())) {
        Poll::Ready(answer) => answer,
        Poll::Pending => Err(lost("the stream waited on a blocking call")),
    }
}

fn dropped(why: &str) -> Fail {
    Fail::Dropped(format!("cannot tell: the lifecycle service took the request but its reply was lost ({why})"))
}

fn lost(why: &str) -> Fail {
    Fail::Final(format!("cannot tell: the lifecycle service took the request but its reply was lost ({why})"))
}

/// The bytes of one request: its head line, then `stdin` when there is one, under `task` and
/// `request_id` as the caller names them.
pub fn encode_request(argv: &[String], stdin: Option<&str>, task: Option<&str>, request_id: Option<&str>) -> Result<Vec<u8>, Fail> {
    let request = request_head(argv, stdin.map(str::len), &caller_of(argv), task, request_id).map_err(|_| lost("unserialisable request"))?;
    let mut bytes = format!("{request}\n").into_bytes();
    bytes.extend_from_slice(stdin.unwrap_or_default().as_bytes());
    Ok(bytes)
}

/// A reply's first line: the exit code and the lengths of the stdout and stderr that follow.
pub struct ReplyHead {
    code: i64,
    len: u64,
    err_len: u64,
    read_ended: bool,
}

impl ReplyHead {
    pub fn parse(line: &str) -> Result<Self, Fail> {
        let head: serde_json::Value = serde_json::from_str(line.trim()).map_err(|_| lost("malformed reply"))?;
        let (Some(code), Some(len)) = (head["exit_code"].as_i64(), head["len"].as_u64()) else { return Err(lost("malformed reply")) };
        Ok(ReplyHead { code, len, err_len: head["err_len"].as_u64().unwrap_or(0), read_ended: head["read_ended"].as_bool().unwrap_or(false) })
    }

    /// How many bytes follow the head line.
    pub fn body_len(&self) -> usize {
        (self.len + self.err_len) as usize
    }

    /// The reply, from the [`body_len`](Self::body_len) bytes that followed the head line.
    pub fn reply(self, mut body: Vec<u8>) -> Result<Reply, Fail> {
        let err = body.split_off(self.len as usize);
        let text = |b: Vec<u8>| String::from_utf8(b).map_err(|_| lost("non-utf8 reply"));
        Ok(Reply { code: self.code as i32, out: text(body)?, err: text(err)?, read_ended: self.read_ended })
    }
}

/// [`exchange_once`] with the failure as its text: one attempt, never repeated.
pub fn exchange(stream: Box<dyn Stream>, argv: &[String], stdin: Option<&str>) -> Result<Reply, String> {
    exchange_once(stream, argv, stdin).map_err(Fail::into_text)
}

/// A `spira-lc` program, run once per call: the stand-in a fixture declares, or the CLI in the
/// declared direct mode.
pub struct Program {
    bin: String,
}

#[cfg(test)]
thread_local! {
    static LAUNCHED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// How many programs this thread's calls have launched.
#[cfg(test)]
pub fn launched() -> u32 {
    LAUNCHED.with(|l| l.get())
}

impl Transport for Program {
    fn exchange(&self, argv: &[String], stdin: Option<&str>, deadline: Duration) -> Result<Reply, String> {
        #[cfg(test)]
        LAUNCHED.with(|l| l.set(l.get() + 1));
        let secs = deadline.as_secs().max(1);
        let mut cmd = Command::new("timeout");
        cmd.arg(secs.to_string()).arg(&self.bin).args(argv);
        cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("cannot run {}: {e}", self.bin))?;
        if let (Some(text), Some(mut si)) = (stdin, child.stdin.take()) {
            si.write_all(text.as_bytes()).map_err(|e| format!("{}: writing stdin: {e}", self.bin))?;
        }
        let out = child.wait_with_output().map_err(|e| format!("{}: {e}", self.bin))?;
        let code = out.status.code().unwrap_or(-1);
        if code == 124 {
            return Err(format!("{} {} timed out after {secs}s", self.bin, argv.join(" ")));
        }
        Ok(Reply { code, out: String::from_utf8_lossy(&out.stdout).into_owned(), err: String::from_utf8_lossy(&out.stderr).into_owned(), read_ended: false })
    }
}

/// Where a call goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// The service on this socket.
    Socket(PathBuf),
    /// This program, per call.
    Program(String),
}

/// The route for a caller holding `bin`: a stand-in declared through `SPIRA_LC_BIN` (fixtures;
/// nothing sets it in production) or a socket declared empty (direct mode) runs `bin`; any
/// other declared socket is the service. A socket that does not resolve is refused, never read
/// as either.
pub fn route_of(stand_in: Option<&str>, socket: Result<String, String>, bin: &str) -> Result<Route, String> {
    if stand_in.is_some_and(|s| !s.is_empty()) {
        return Ok(Route::Program(bin.to_string()));
    }
    match socket {
        Ok(p) if p.trim().is_empty() => Ok(Route::Program(bin.to_string())),
        Ok(p) => Ok(Route::Socket(PathBuf::from(p))),
        Err(e) => Err(format!("cannot tell: the lifecycle service socket (SPIRA_LC_SOCKET) does not resolve: {e}")),
    }
}

/// The one store client: a route and a deadline. Every verb goes the one route; a verb the
/// service does not serve is its refusal, never a fall-through to a program (sp-6n7nuv).
#[derive(Clone)]
pub struct LcClient {
    transport: Arc<dyn Transport>,
    socket: bool,
    deadline: Duration,
    not_running_wait: Duration,
}

impl LcClient {
    /// The service on `path`.
    pub fn socket(path: impl Into<PathBuf>) -> Self {
        LcClient { transport: Arc::new(Socket { path: path.into() }), socket: true, deadline: DEADLINE, not_running_wait: DEADLINE }
    }

    /// Every call through `transport` (a test's scripted double, typically).
    pub fn over(transport: Arc<dyn Transport>, socket: bool) -> Self {
        LcClient { transport, socket, deadline: DEADLINE, not_running_wait: DEADLINE }
    }

    /// The route the configuration declares for a caller holding `bin`.
    pub fn for_bin(bin: &str) -> Result<Self, String> {
        let stand_in = std::env::var(crate::lifecycle_row::LC_BIN_ENV).ok();
        let route = route_of(stand_in.as_deref(), spira_config::process::cfg("SPIRA_LC_SOCKET"), bin)?;
        Ok(match route {
            Route::Socket(path) => LcClient::socket(path),
            Route::Program(bin) => LcClient { transport: Arc::new(Program { bin }), socket: false, deadline: DEADLINE, not_running_wait: DEADLINE },
        })
    }

    /// [`for_bin`](Self::for_bin) for the configured `spira-lc`.
    pub fn configured() -> Result<Self, String> {
        Self::for_bin(&crate::lifecycle_row::lc_bin()?)
    }

    /// Bounds a call at `d`: a program run, the wait for an absent store, and a socket exchange
    /// that reached the store. A client never bounded keeps [`DEADLINE`] (a sweep, [`SWEEP_DEADLINE`]).
    pub fn within(&self, d: Duration) -> Self {
        LcClient { deadline: d, not_running_wait: d, ..self.clone() }
    }

    /// Sets how long a refused or absent socket is waited for, apart from [`within`](Self::within).
    pub fn not_running_wait(&self, d: Duration) -> Self {
        LcClient { not_running_wait: d, ..self.clone() }
    }

    /// Whether calls reach the service in-process rather than through a program.
    pub fn is_socket(&self) -> bool {
        self.socket
    }

    /// One request; any exit code is an answer. `Err` only when there is none.
    pub fn call<S: AsRef<str>>(&self, argv: &[S]) -> Result<Reply, LcError> {
        self.send(argv, None)
    }

    /// [`call`](Self::call) with `stdin` as the request body: the program's stdin, or the
    /// length-prefixed body of a socket request, which the service reads to exactly its length.
    pub fn call_stdin<S: AsRef<str>>(&self, argv: &[S], stdin: &str) -> Result<Reply, LcError> {
        self.send(argv, Some(stdin))
    }

    fn send<S: AsRef<str>>(&self, argv: &[S], stdin: Option<&str>) -> Result<Reply, LcError> {
        let argv: Vec<String> = argv.iter().map(|a| a.as_ref().to_string()).collect();
        if let Some(held) = ThreadReads::ask(&argv, stdin) {
            return held;
        }
        let reply = self.transport.exchange_kind(&argv, stdin, self.deadline, self.not_running_wait)?;
        match queued_id(&reply.out) {
            Some(id) if reply.code == 0 => self.await_record(id),
            _ => Ok(reply),
        }
    }

    /// The answer of the recorded request `id`, each poll a plain read of its record.
    pub fn await_record(&self, id: &str) -> Result<Reply, LcError> {
        let show = ["work-request".to_string(), "show".to_string(), id.to_string()];
        let began = std::time::Instant::now();
        loop {
            let r = self.transport.exchange_kind(&show, None, self.deadline, self.not_running_wait)?;
            if r.code != 0 {
                return Ok(r);
            }
            match record_answer(&r.out) {
                Ok(Some((code, out))) => return Ok(Reply { code, out, err: String::new(), read_ended: false }),
                Ok(None) => {}
                Err(e) => return Err(LcError::Fault(format!("request {id}: {e}"))),
            }
            if began.elapsed() >= RECORD_WAIT {
                return Ok(Reply { code: CANNOT_TELL, out: record_timeout(id), err: String::new(), read_ended: false });
            }
            std::thread::sleep(RECORD_POLL); // waits-on: the scheduler finishing the request; cap RECORD_WAIT
        }
    }

    /// The answer's stdout when it applied (exit 0); a refusal (3) or anything else is an error.
    pub fn answer<S: AsRef<str>>(&self, argv: &[S]) -> Result<String, LcError> {
        self.call(argv)?.into_answer()
    }
}

/// Answers a request from memory: `None` sends it to the transport as before.
pub type ReadAnswerer = fn(&[String]) -> Option<Result<Reply, LcError>>;

thread_local! {
    static THREAD_READS: std::cell::Cell<Option<ReadAnswerer>> = const { std::cell::Cell::new(None) };
}

/// While held, every bodiless call this thread makes through any [`LcClient`] is offered to the
/// answerer first, so reads made inside libraries are served as the caller's own are. The
/// answerer's own calls go to the transport. Not nestable.
pub struct ThreadReads(std::marker::PhantomData<*const ()>);

impl ThreadReads {
    pub fn begin(answerer: ReadAnswerer) -> ThreadReads {
        THREAD_READS.set(Some(answerer));
        ThreadReads(std::marker::PhantomData)
    }

    fn ask(argv: &[String], stdin: Option<&str>) -> Option<Result<Reply, LcError>> {
        let answerer = THREAD_READS.take()?;
        let held = stdin.is_none().then(|| answerer(argv)).flatten();
        THREAD_READS.set(Some(answerer));
        held
    }
}

impl Drop for ThreadReads {
    fn drop(&mut self) {
        THREAD_READS.set(None);
    }
}

/// The request id in `out` when it is the service's answer to a verb it recorded for the
/// scheduler (`queued wr-<n>`), which the caller then polls with `work-request show`.
pub fn queued_id(out: &str) -> Option<&str> {
    let id = out.trim().strip_prefix("queued ")?;
    let n = id.strip_prefix("wr-")?;
    (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).then_some(id)
}

/// How often a client asks whether a recorded request has finished.
pub const RECORD_POLL: Duration = Duration::from_millis(500);
/// How long a client waits for a recorded request: the broker's 300 s bound and a margin for the scheduler to pick it up.
// batch-job: a recorded verb runs in the scheduler under the broker's own bound; this adds the pick-up margin.
pub const RECORD_WAIT: Duration = Duration::from_secs(330);

/// A recorded request's answer, from its `work-request show` record: `None` while it is queued or
/// running, else the exit code and output the scheduler's runner left.
pub fn record_answer(record: &str) -> Result<Option<(i32, String)>, String> {
    let r: serde_json::Value = serde_json::from_str(record).map_err(|e| format!("malformed record: {e}"))?;
    if r["state"] != "done" {
        return Ok(None);
    }
    Ok(Some((r["code"].as_i64().map_or(CANNOT_TELL, |c| c as i32), r["output"].as_str().unwrap_or("").to_string())))
}

/// The text a client prints when a recorded request has not finished within [`RECORD_WAIT`].
pub fn record_timeout(id: &str) -> String {
    format!("cannot tell: request {id} had not finished after {}s; `spira-lc work-request show {id}` has its state", RECORD_WAIT.as_secs())
}

/// Print a service answer: a "cannot tell" refusal goes to stderr, so a caller capturing stdout
/// as an id or a value gets nothing, never the refusal text (law-never-derive-an-id-from-output).
pub fn print_answer(code: i32, out: &str) {
    if out.is_empty() {
        return;
    }
    if is_refusal(code, out) {
        eprintln!("{out}");
    } else {
        println!("{out}");
    }
}

/// Whether an answer is "cannot tell": its exit code, never its text (sp-cxfgj5,
/// law-lifecycle-state-is-not-text). `out` is kept for the callers that pass it; it decides
/// nothing.
pub fn is_refusal(code: i32, _out: &str) -> bool {
    cannot_tell(code)
}

/// Exit 2: the machine could not answer, whatever it printed.
pub fn cannot_tell(code: i32) -> bool {
    code == CANNOT_TELL
}

/// `SPIRA_LC_BIN`, else `spira-lc` in `$SPIRA_RELEASE`; a refusal naming the key otherwise.
pub fn lc_bin() -> Result<PathBuf, String> {
    crate::lifecycle_row::lc_bin().map(PathBuf::from)
}

/// Run `bin args` with a deadline enforced here, not by a `timeout` program. A deadline
/// hit is exit 124. Output is combined stdout+stderr.
pub fn run_bounded(mut cmd: Command, limit: Duration) -> (i32, String) {
    let dir = std::env::temp_dir();
    let tag = format!("spira-lc-call-{}-{:?}", std::process::id(), std::thread::current().id());
    let (op, ep) = (dir.join(format!("{tag}.out")), dir.join(format!("{tag}.err")));
    let files = std::fs::File::create(&op).and_then(|o| std::fs::File::create(&ep).map(|e| (o, e)));
    let (o, e) = match files {
        Ok(f) => f,
        Err(err) => return (2, format!("running spira-lc: {err}")),
    };
    cmd.stdin(Stdio::null()).stdout(o).stderr(e);
    let result = match cmd.spawn() {
        Err(err) => (2, format!("running spira-lc: {err}")),
        Ok(mut child) => {
            let start = Instant::now();
            let code = loop {
                match child.try_wait() {
                    Ok(Some(st)) => break st.code().unwrap_or(2),
                    Ok(None) if start.elapsed() >= limit => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break 124;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                    Err(_) => break 2,
                }
            };
            let read = |p: &PathBuf| std::fs::read_to_string(p).unwrap_or_default();
            (code, format!("{}{}", read(&op), read(&ep)))
        }
    };
    let _ = std::fs::remove_file(&op);
    let _ = std::fs::remove_file(&ep);
    result
}

#[cfg(test)]
mod tests {
    #[test_id(t6e45p3)]
    #[test]
    fn a_thread_read_answers_what_it_holds_sends_the_rest_and_lets_its_own_calls_through() {
        use super::{LcClient, LcError, Reply, ThreadReads};
        fn answerer(argv: &[String]) -> Option<Result<Reply, LcError>> {
            (argv[0] == "list").then(|| {
                // A `list` of its own: offered back to the answerer, this would never return.
                let own = LcClient::over(std::sync::Arc::new(super::fake::Content::new(vec![])), true).call(&["list", "--ids", "sp-a"])?;
                Ok(Reply { code: 0, out: format!("held {}", own.code), ..Reply::default() })
            })
        }
        let sent = std::sync::Arc::new(super::fake::Content::new(vec![]));
        let lc = LcClient::over(sent.clone(), true);
        {
            let _held = ThreadReads::begin(answerer);
            assert!(lc.call(&["list", "--live"]).unwrap().out.starts_with("held "), "answered in place of the store");
            assert!(lc.call(&["list", "--live"]).unwrap().out.starts_with("held "), "and again after its own call");
            let _ = lc.call(&["content", "list"]);
            let _ = lc.call_stdin(&["list"], "body");
        }
        let _ = lc.call(&["list", "--live"]);
        let verbs: Vec<String> = sent.requests().iter().map(|a| a.join(" ")).collect();
        assert_eq!(verbs, ["content list", "list", "list --live"], "unheld, bodied and after the guard go to the store");
    }
    use std::io::{BufRead, BufReader, Read};
    use std::os::unix::net::UnixStream;
    #[test_id(t862q11)]
    #[test]
    fn a_watch_waits_its_hold_and_a_second_and_every_other_verb_keeps_the_deadline() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(call_wait(&argv(&["watch", "lifecycle/", "--from-rev", "7", "--wait-ms", "30000"])) >= Duration::from_secs(31));
        assert_eq!(call_wait(&argv(&["watch", "lifecycle/", "--from-rev", "7", "--wait-ms", "2000"])), Duration::from_secs(3));
        assert!(call_wait(&argv(&["watch", "lifecycle/", "--from-rev", "7"])) >= Duration::from_secs(31), "a watch that names no hold gets the longest");
        assert_eq!(call_wait(&argv(&["list", "--live"])), DEADLINE);
        assert_eq!(call_wait(&argv(&["reconcile-asks"])), SWEEP_DEADLINE);
        let watch = argv(&["watch", "lifecycle/", "--from-rev", "7", "--wait-ms", "30000"]);
        assert!(client_wait(&watch, DEADLINE) >= Duration::from_secs(31), "the call a default client makes waits out the hold");
        assert_eq!(client_wait(&watch, Duration::from_secs(2)), Duration::from_secs(2), "a caller's shorter deadline still binds");
    }

    #[test_id(tsmi0of)]
    #[test]
    fn a_record_answers_only_once_it_is_done() {
        assert_eq!(record_answer("{\"state\":\"queued\"}"), Ok(None));
        assert_eq!(record_answer("{\"state\":\"running\",\"code\":null}"), Ok(None));
        assert_eq!(record_answer("{\"state\":\"done\",\"code\":124,\"output\":\"killed\"}"), Ok(Some((124, "killed".to_string()))));
        assert!(record_answer("not json").is_err());
    }

    #[test_id(tegw1u1)]
    #[test]
    fn a_client_call_answers_with_the_finished_request_not_the_queue_receipt() {
        struct Recorded(std::sync::Mutex<u32>);
        impl Transport for Recorded {
            fn exchange(&self, argv: &[String], _: Option<&str>, _: Duration) -> Result<Reply, String> {
                let ok = |out: &str| Ok(Reply { code: 0, out: out.into(), err: String::new(), read_ended: false });
                if argv[0] != "work-request" {
                    return ok("queued wr-7\n");
                }
                let mut polls = self.0.lock().unwrap();
                *polls += 1;
                ok(if *polls < 2 { "{\"state\":\"running\"}" } else { "{\"state\":\"done\",\"code\":3,\"output\":\"sp-new\"}" })
            }
        }
        let c = LcClient::over(Arc::new(Recorded(Default::default())), true);
        let r = c.call(&["work", "sp-x", "file"]).unwrap();
        assert_eq!((r.code, r.out.as_str()), (3, "sp-new"));
    }

    #[test_id(tnue6lm)]
    #[test]
    fn only_the_services_queued_line_names_a_request() {
        assert_eq!(queued_id("queued wr-12\n"), Some("wr-12"));
        for not in ["queued wr-", "queued wr-1x", "queued 12", "queued wr-1 and more", "SUBMITTED", ""] {
            assert_eq!(queued_id(not), None, "{not:?}");
        }
    }

    use super::*;

    #[test_id(tnme0ro)]
    #[test]
    fn a_relative_socket_is_never_inside_a_sim_world_whatever_the_cwd() {
        let dir = testkit::TempDir::new("lc-call-relative");
        std::fs::write(dir.join(SIM_WORLD_MARKER), "").unwrap();
        assert!(socket_in_sim_world(dir.join("sock").to_str().unwrap()), "the absolute socket beside the marker is inside");
        assert!(!socket_in_sim_world("sock"), "a bare name resolves against the cwd, so it is refused");
    }

    /// A service that drops its first `drops` connections after reading the request (a restart
    /// killing it mid-flight), then answers every later one. Returns the socket and the count
    /// of connections it saw.
    fn flaky(drops: usize) -> (testkit::TempDir, PathBuf, Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::Ordering;
        let dir = testkit::TempDir::new("lc-call-flaky");
        let path = dir.join("sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let n = count.fetch_add(1, Ordering::SeqCst);
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                if n >= drops {
                    (&stream).write_all(b"{\"exit_code\":0,\"len\":2}\nok").unwrap();
                }
            }
        });
        (dir, path, seen)
    }

    fn call(path: &std::path::Path, argv: &[&str]) -> Result<Reply, LcError> {
        LcClient::socket(path).call(argv)
    }

    #[test_id(tzprugn)]
    #[test]
    fn a_read_whose_connection_the_service_dropped_is_asked_again_and_answered() {
        let (_dir, path, seen) = flaky(2);
        let r = call(&path, &["show", "sp-a"]).expect("a read survives a restart");
        assert_eq!(r.out, "ok");
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test_id(t1i5wnl)]
    #[test]
    fn a_write_whose_reply_was_lost_is_never_sent_twice() {
        let (_dir, path, seen) = flaky(1);
        let e = call(&path, &["hold", "sp-a"]).unwrap_err();
        assert!(e.to_string().starts_with("cannot tell"), "{e}");
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test_id(tloz7he)]
    #[test]
    fn a_read_the_service_never_answers_gives_up_inside_its_budget() {
        let argv = vec!["list".to_string()];
        let mut tries = 0;
        let r: Result<(), Fail> = retrying(&argv, Path::new("s"), Duration::from_millis(200), Duration::ZERO, |_| {
            tries += 1;
            Err(Fail::Dropped("gone".into()))
        });
        assert_eq!(r, Err(Fail::Dropped("gone".to_string())));
        assert!(tries > 1, "{tries}");
    }

    #[test_id(t9y9cgb)]
    #[test]
    fn only_reads_are_retried() {
        let s = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        for r in [&["show", "x"][..], &["claimable"], &["list", "--state", "WORKING"], &["content", "list"], &["content", "show", "x"], &["content", "search", "x"], &["content", "events", "--id", "x"], &["content", "dep", "list", "x"], &["content", "label", "list", "x"], &["fact", "list", "--ids", "x"]] {
            assert!(is_read(&s(r)), "{r:?}");
        }
        for w in [&["hold", "x"][..], &["event", "x"], &["content", "close", "x"], &["content", "note", "x"], &["content", "comments", "add", "x", "text"], &["content", "label", "add", "sp-a", "x"], &["work", "x", "show"], &["create-bead"], &["fact", "add", "x", "--kind", "claimed", "--actor", "a"], &["facts"], &[]] {
            assert!(!is_read(&s(w)), "{w:?}");
        }
    }

    fn answering(listener: std::os::unix::net::UnixListener, accepted: Arc<std::sync::atomic::AtomicUsize>) {
        for stream in listener.incoming().flatten() {
            accepted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            (&stream).write_all(b"{\"exit_code\":0,\"len\":0}\n").unwrap();
        }
    }

    fn bound_after_300ms(path: PathBuf) -> Arc<std::sync::atomic::AtomicUsize> {
        let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = Arc::clone(&accepted);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300)); // clock-safe: the outage the caller must wait through
            let _ = std::fs::remove_file(&path);
            answering(std::os::unix::net::UnixListener::bind(&path).unwrap(), count);
        });
        accepted
    }

    #[test_id(t9qp4jw)]
    #[test]
    fn a_write_cut_at_the_deadline_is_resent_through_the_socket_client_under_one_id() {
        let dir = testkit::TempDir::new("lc-call-resend");
        let path = dir.join("sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let heads = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = Arc::clone(&heads);
        std::thread::spawn(move || {
            for (n, stream) in listener.incoming().flatten().enumerate() {
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                seen.lock().unwrap().push(line);
                let (code, body) = if n == 0 { (CANNOT_TELL, DEADLINE_ANSWER) } else { (0, "") };
                (&stream).write_all(format!("{{\"exit_code\":{code},\"len\":{}}}\n{body}", body.len()).as_bytes()).unwrap();
            }
        });
        let got = LcClient::socket(&path).call(&["hold", "sp-a", "x"]).unwrap();
        assert_eq!(got.code, 0);
        let ids: Vec<String> = heads.lock().unwrap().iter().map(|h| serde_json::from_str::<serde_json::Value>(h.trim()).unwrap()["request_id"].as_str().unwrap().to_string()).collect();
        assert!(ids.len() == 2 && ids[0] == ids[1], "{ids:?}");
    }

    #[test_id(trk6qt9)]
    #[test]
    fn a_write_made_while_the_store_is_absent_is_applied_once_when_it_returns() {
        let dir = testkit::TempDir::new("lc-call-absent");
        let path = dir.join("sock");
        let accepted = bound_after_300ms(path.clone());
        let r = LcClient::socket(&path).within(Duration::from_secs(2)).call(&["hold", "sp-a", "wait"]).expect("the write waits for the store");
        assert_eq!(r.code, 0);
        assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test_id(t7yb0zu)]
    #[test]
    fn a_write_made_while_the_socket_refuses_is_applied_once_when_it_returns() {
        let dir = testkit::TempDir::new("lc-call-stale");
        let path = dir.join("sock");
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists(), "the stale file is left behind");
        let accepted = bound_after_300ms(path.clone());
        let r = LcClient::socket(&path).within(Duration::from_secs(2)).call(&["hold", "sp-a", "wait"]).expect("the write waits for the store");
        assert_eq!(r.code, 0);
        assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test_id(t0qdzzr)]
    #[test]
    fn an_absent_store_past_the_wait_is_not_running_with_nothing_sent() {
        let dir = testkit::TempDir::new("lc-call-wait");
        let e = LcClient::socket(dir.join("sock")).within(Duration::from_millis(300)).call(&["hold", "sp-a", "wait"]).unwrap_err();
        assert!(matches!(&e, LcError::NotRunning(m) if m.contains("store not running") && m.contains("nothing was sent")), "{e:?}");
    }

    #[test_id(tcakhbj)]
    #[test]
    fn a_store_that_accepts_and_never_replies_is_cut_at_the_bound() {
        let dir = testkit::TempDir::new("lc-call-silent");
        let listener = std::os::unix::net::UnixListener::bind(dir.join("silent")).unwrap();
        std::thread::spawn(move || {
            let mut open = Vec::new();
            for s in listener.incoming().flatten() {
                open.push(s);
            }
        });
        let client = LcClient::socket(dir.join("silent")).within(Duration::from_millis(300));
        let (done, answered) = std::sync::mpsc::channel();
        std::thread::spawn(move || done.send(client.call(&["list"])).unwrap());
        let r = answered.recv_timeout(Duration::from_secs(1)).expect("the call returns at the bound, not at DEADLINE"); // clock-safe: caps the wait for the call to return
        assert!(r.is_err(), "{r:?}");
    }

    #[test_id(te8wt3y)]
    #[test]
    fn a_not_running_wait_set_apart_from_within_bounds_only_the_wait() {
        let dir = testkit::TempDir::new("lc-call-apart");
        let e = LcClient::socket(dir.join("sock")).within(Duration::from_secs(30)).not_running_wait(Duration::ZERO).call(&["list"]).unwrap_err();
        assert!(matches!(e, LcError::NotRunning(_)));
    }

    fn whole(a: &[&str]) -> bool {
        reads_whole_history(&[&["content", "list"][..], a].concat())
    }

    #[test_id(tpfwsbd)]
    #[test]
    fn the_whole_history_is_all_and_unlimited_and_unbounded_in_every_spelling() {
        assert!(whole(&["--all", "--limit", "0", "--json"]));
        assert!(whole(&["--all", "-n", "0"]));
        assert!(whole(&["--all", "--limit=0", "--label-any", "x"]), "a label can match every bead");
        for bounded in [&["--all", "--limit", "0", "--id=sp-a"][..], &["--all", "--limit", "0", "--id", "sp-a"], &["--all", "--limit", "0", "--external-ref", "incident:x"], &["--all", "--limit", "0", "--parent", "e"], &["--all", "--limit=0", "--updated-after=2026-01-01"], &["--limit", "0"], &["--all"], &["--all", "--limit", "50"]] {
            assert!(!whole(bounded), "{bounded:?}");
        }
        assert!(!reads_whole_history(&["list", "--all", "--limit", "0"]) && !reads_whole_history(&["content", "show", "--all", "--limit", "0"]));
    }

    #[test_id(t3z91va)]
    #[test]
    fn a_deadline_refusal_is_never_stdout() {
        assert!(is_refusal(2, "cannot tell: deadline"));
        assert!(!is_refusal(0, "sp-abc12"), "an id is data");
        assert!(!is_refusal(1, "sp-abc12"));
    }

    /// sp-cxfgj5: exit 2 is "cannot tell" whatever it printed, and text never makes an
    /// answer a refusal.
    #[test_id(tt4i438)]
    #[test]
    fn cannot_tell_is_the_exit_code_not_the_text() {
        assert!(is_refusal(2, "deadline exceeded"), "exit 2 never reaches stdout as data");
        assert!(is_refusal(2, ""));
        assert!(!is_refusal(0, "cannot tell: this is a bead title"), "an applied answer's text is data");
        assert!(cannot_tell(CANNOT_TELL) && !cannot_tell(REFUSED) && !cannot_tell(0));
    }

    use std::sync::Mutex;

    /// A stand-in service on a real socket: answers each request by its verb, records the argv.
    fn serve(dir: &testkit::TempDir, answer: fn(&[String]) -> (i32, String)) -> (PathBuf, Arc<Mutex<Vec<Vec<String>>>>) {
        let path = dir.join("lc.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for s in listener.incoming().flatten() {
                let mut reader = BufReader::new(&s);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let head: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
                let mut argv: Vec<String> = serde_json::from_value(if head.is_array() { head.clone() } else { head["argv"].clone() }).unwrap();
                if let Some(n) = head["len"].as_u64() {
                    let mut body = vec![0u8; n as usize];
                    reader.read_exact(&mut body).unwrap();
                    argv.push(format!("stdin={}", String::from_utf8(body).unwrap()));
                }
                let log_verb = argv[0].clone();
                let (code, body) = answer(&argv);
                log.lock().unwrap().push(argv);
                let (out, err) = body.split_once("\x00").unwrap_or((&body, ""));
                let mut reply = serde_json::json!({"exit_code": code, "len": out.len(), "err_len": err.len()});
                if log_verb == "read-ended" {
                    reply["read_ended"] = true.into();
                }
                let _ = write!(&s, "{reply}\n{out}{err}");
            }
        });
        (path, seen)
    }

    fn store(argv: &[String]) -> (i32, String) {
        match argv[0].as_str() {
            "list" => (0, r#"[{"bead_id":"sp-a","state":"READY","holds":"[]"},{"bead_id":"sp-b","state":"WORKING","holder":"d1"}]"#.into()),
            "show" if argv[1] == "sp-a" => (0, r#"{"bead":{"bead_id":"sp-a","state":"READY","version":"4"},"delivery":null}"#.into()),
            "show" => (1, String::new()),
            "event" | "create-bead" | "promote-filed" | "set-repo" => (0, String::new()),
            "plan" => (3, "refused: no planning plane".into()),
            "close" | "batch-progress" => (0, format!("{}\x00", argv.join(" "))),
            "hold" => (3, "\x00lc: sp-a: refused\n".into()),
            _ => (2, "cannot tell: not served".into()),
        }
    }

    #[test_id(t1ibc4t)]
    #[test]
    fn the_helpers_answer_over_the_socket_and_launch_nothing() {
        let dir = testkit::TempDir::new("lc-client-socket");
        let (path, seen) = serve(&dir, store);
        let c = LcClient::socket(&path);
        let before = launched();
        let rows = crate::lc_state::list_live_on(&c).unwrap();
        assert_eq!(rows.iter().map(|r| r.bead_id.as_str()).collect::<Vec<_>>(), ["sp-a", "sp-b"]);
        assert_eq!(crate::lc_state::row_on(&c, "sp-a").unwrap().map(|r| r.state), Some("READY".into()));
        assert_eq!(crate::lc_state::row_on(&c, "sp-zz").unwrap(), None, "exit 1 is no row");
        crate::lifecycle_row::set_express_on(&c, "sp-a", true).unwrap();
        assert!(crate::lifecycle_row::set_express_on(&c, "sp-zz", true).is_err(), "no row is nothing to mark");
        crate::lifecycle_row::content_on_base_on(&c, "sp-a", "p", "x").unwrap();
        assert_eq!(launched(), before, "no program ran");
        crate::lc_state::list_ids_on(&c, &["sp-a".to_string(), "sp-b".to_string()]).unwrap();
        let seen = seen.lock().unwrap();
        assert!(seen.iter().any(|a| a == &["list", "--ids", "sp-a,sp-b"].map(String::from)), "named beads are read by id");
        assert_eq!(seen[0], ["list", "--live"].map(String::from), "the live set, never the whole table");
        let event = seen.iter().find(|a| a[0] == "event").expect("express sent its event");
        assert_eq!(event[1..], ["bead", "sp-a", "--expect", "READY", "--version", "4", "--actor", "bead", "--kind", "\"Express\""].map(String::from));
        let cob = seen.iter().rev().find(|a| a[0] == "event").unwrap();
        assert_eq!(cob[1..], ["bead", "sp-a", "--expect", "READY", "--version", "4", "--actor", "x", "--kind", "{\"ContentOnBase\":{\"proof\":\"p\"}}"].map(String::from));
    }

    #[test_id(tosw076)]
    #[test]
    fn a_refusal_and_a_fault_stay_apart_and_a_dead_socket_is_a_fault() {
        let dir = testkit::TempDir::new("lc-client-codes");
        let (path, _) = serve(&dir, store);
        let c = LcClient::socket(&path);
        assert!(matches!(c.answer(&["plan", "stamp"]), Err(LcError::Refused(m)) if m.contains("no planning plane")));
        assert!(matches!(c.answer(&["nope"]), Err(LcError::Fault(m)) if m.starts_with("cannot tell")));
        assert!(matches!(c.answer(&["hold", "sp-a", "wait"]), Err(LcError::Refused(m)) if m == "lc: sp-a: refused"), "a refusal's stderr is its text");
        let dead = LcClient::socket(dir.join("no-such.sock"));
        assert!(matches!(dead.within(Duration::ZERO).call(&["list"]), Err(LcError::NotRunning(m)) if m.contains("store not running")));
    }

    /// sp-iqsanu: an answer the service marks `read_ended` is a typed variant, never text to match;
    /// the same exit without the mark stays a fault.
    #[test_id(ta2lpwr)]
    #[test]
    fn a_cannot_tell_marked_read_ended_is_its_own_error_and_unmarked_stays_a_fault() {
        let dir = testkit::TempDir::new("lc-client-read-ended");
        let (path, _) = serve(&dir, |_| (2, "cannot tell: dwell: read ended".into()));
        let c = LcClient::socket(&path);
        assert!(matches!(c.answer(&["read-ended"]), Err(LcError::ReadEnded(m)) if m == "cannot tell: dwell: read ended"));
        assert!(matches!(c.answer(&["dwell"]), Err(LcError::Fault(m)) if m == "cannot tell: dwell: read ended"));
    }

    fn closer(argv: &[String]) -> (i32, String) {
        match (argv[0].as_str(), argv.get(1).map(String::as_str)) {
            ("close", Some("sp-ok")) => (0, String::new()),
            ("close", Some("sp-refused")) => (3, "\x00a terminal row\n".into()),
            _ => (2, "cannot tell: not served".into()),
        }
    }

    /// sp-cxfgj5: a close's refusal and its fault are variants a caller matches, never
    /// "exited 3" in a string.
    #[test_id(tt52b8e)]
    #[test]
    fn a_close_answers_a_typed_refusal_or_fault_never_text_to_match() {
        let dir = testkit::TempDir::new("lc-client-close");
        let (path, _) = serve(&dir, closer);
        let c = LcClient::socket(&path);
        let close = |id: &str| crate::lifecycle_row::close_on(&c, id, "why", "t", &[]);
        assert_eq!(close("sp-ok"), Ok(()));
        assert!(matches!(close("sp-refused"), Err(LcError::Refused(_))));
        assert!(matches!(close("sp-other"), Err(LcError::Fault(_))));
    }

    /// The verbs the CLI once answered itself go to the service like any other, and a body
    /// rides the socket to exactly its length.
    #[test_id(t4b1tyx)]
    #[test]
    fn close_and_a_stdin_body_go_over_the_socket_and_launch_nothing() {
        let dir = testkit::TempDir::new("lc-client-stdin");
        let (path, seen) = serve(&dir, store);
        let c = LcClient::socket(&path);
        let before = launched();
        assert_eq!(c.call(&["close", "sp-a", "--reason", "landed"]).unwrap().out, "close sp-a --reason landed");
        let r = c.call_stdin(&["batch-progress"], "[{\"bead_id\":\"sp-a\"}]\nsecond line").unwrap();
        assert_eq!((r.code, r.out.as_str()), (0, "batch-progress stdin=[{\"bead_id\":\"sp-a\"}]\nsecond line"));
        assert_eq!(launched(), before, "no program ran");
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[test_id(t0ljmq3)]
    #[test]
    fn exchange_once_sends_the_scoped_request_id_and_none_outside_it() {
        let head_sent = |scoped: bool| {
            let (client, server) = UnixStream::pair().unwrap();
            let serve = std::thread::spawn(move || {
                let mut r = BufReader::new(&server);
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                (&server).write_all(b"{\"exit_code\":0,\"len\":0}\n").unwrap();
                serde_json::from_str::<serde_json::Value>(line.trim()).unwrap()
            });
            let argv = vec!["content".to_string(), "create".to_string()];
            let _ = if scoped { as_request("r-1", || exchange_once(Box::new(UnixPort::from(client)), &argv, None)) } else { exchange_once(Box::new(UnixPort::from(client)), &argv, None) };
            serve.join().unwrap()
        };
        assert_eq!(head_sent(true)["request_id"], "r-1");
        assert!(head_sent(false).get("request_id").is_none());
        assert_eq!(current_request_id(), None);
    }

    fn scripted(answers: &[(i32, &str)], argv: &[&str], scope: Option<&str>) -> (Result<Reply, Fail>, Vec<Option<String>>) {
        let argv: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
        let mut answers = answers.iter().map(|(c, o)| Reply { code: *c, out: o.to_string(), err: String::new(), read_ended: false });
        let mut sent = Vec::new();
        let mut run = || {
            retrying_resent(&argv, std::path::Path::new("sock"), Duration::from_secs(5), Duration::ZERO, |_| {
                sent.push(current_request_id());
                Ok(answers.next().expect("no more sends than answers"))
            })
        };
        let got = match scope {
            Some(id) => as_request(id, run),
            None => run(),
        };
        (got, sent)
    }

    #[test_id(t6jlg4a)]
    #[test]
    fn a_write_cut_at_the_deadline_is_sent_again_under_the_same_minted_id() {
        let (got, sent) = scripted(&[(2, DEADLINE_ANSWER), (0, "")], &["hold", "sp-a", "x"], None);
        assert_eq!(got.unwrap().code, 0);
        assert_eq!(sent.len(), 2);
        assert!(sent[0].is_some() && sent[0] == sent[1], "{sent:?}");
        assert_eq!(current_request_id(), None);
    }

    #[test_id(tiiqz6l)]
    #[test]
    fn a_write_inside_a_request_scope_is_resent_under_that_id() {
        let (got, sent) = scripted(&[(2, DEADLINE_ANSWER), (0, "")], &["hold", "sp-a", "x"], Some("r-1"));
        assert_eq!(got.unwrap().code, 0);
        assert_eq!(sent, [Some("r-1".to_string()), Some("r-1".to_string())]);
    }

    #[test_id(ty4wicx)]
    #[test]
    fn a_refused_write_is_sent_once() {
        let (got, sent) = scripted(&[(REFUSED, "no")], &["hold", "sp-a", "x"], None);
        assert_eq!(got.unwrap().code, REFUSED);
        assert_eq!(sent.len(), 1);
    }

    #[test_id(tqxj51i)]
    #[test]
    fn a_write_cut_every_time_gives_the_last_cut_answer_when_the_budget_runs_out() {
        let argv = vec!["hold".to_string(), "sp-a".to_string()];
        let mut sends = 0;
        let got = retrying_resent(&argv, std::path::Path::new("sock"), Duration::from_millis(300), Duration::ZERO, |_| {
            sends += 1;
            Ok(Reply { code: CANNOT_TELL, out: DEADLINE_ANSWER.to_string(), err: String::new(), read_ended: false })
        });
        assert!(sends >= 2, "re-sent at least once: {sends}");
        assert_eq!(got.unwrap().out, DEADLINE_ANSWER);
    }

    #[test_id(tu6flc1)]
    #[test]
    fn a_write_whose_connection_dropped_gets_one_attempt_from_the_resending_step() {
        let argv = vec!["hold".to_string(), "sp-a".to_string()];
        let mut sends = 0;
        let got = retrying_resent(&argv, std::path::Path::new("sock"), DEADLINE, Duration::ZERO, |_| {
            sends += 1;
            Err(Fail::Dropped("gone".into()))
        });
        assert_eq!((sends, got), (1, Err(Fail::Dropped("gone".into()))));
    }

    #[test_id(tmkobyd)]
    #[test]
    fn a_read_is_never_sent_under_a_minted_id() {
        let (got, sent) = scripted(&[(0, "[]")], &["show", "sp-a"], None);
        assert_eq!(got.unwrap().code, 0);
        assert_eq!(sent, [None]);
    }

    #[test_id(to264vq)]
    #[test]
    fn the_deadline_answer_is_built_on_the_deadline_message() {
        assert!(DEADLINE_ANSWER.ends_with(DEADLINE_MESSAGE));
    }

    #[test_id(togk1zq)]
    #[test]
    fn a_request_id_rides_the_object_head_and_a_plain_request_stays_an_array() {
        let argv = vec!["content".to_string(), "note".to_string(), "sp-a".to_string()];
        let head: serde_json::Value = serde_json::from_str(&request_head(&argv, None, &BTreeMap::new(), None, Some("r-1")).unwrap()).unwrap();
        assert_eq!((head["request_id"].as_str(), head["argv"].clone()), (Some("r-1"), serde_json::json!(argv)));
        assert!(request_head(&argv, None, &BTreeMap::new(), None, None).unwrap().starts_with('['));
    }

    /// A content write carries the caller's guard variables — the rune, the czar context and
    /// every stage — and nothing else of its environment; any other request is unchanged.
    #[test_id(tauaokh)]
    #[test]
    fn a_content_request_carries_the_guard_variables_and_nothing_else() {
        let env = [("SPIRA_RUNE", "czar"), ("SPIRA_CZAR_CLASS", "deadlock"), ("SPIRA_CZAR_TRIGGER_BEAD", "sp-t"), ("SPIRA_CZAR_STAGE_DEADLOCK", "act"), ("SPIRA_DB", "db"), ("HOME", "h")];
        let vars = guard_vars(env.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        assert_eq!(vars.keys().map(String::as_str).collect::<Vec<_>>(), ["SPIRA_CZAR_CLASS", "SPIRA_CZAR_STAGE_DEADLOCK", "SPIRA_CZAR_TRIGGER_BEAD", "SPIRA_RUNE"]);
        let argv: Vec<String> = ["content", "close", "sp-a"].iter().map(|s| s.to_string()).collect();
        let head: serde_json::Value = serde_json::from_str(&request_head(&argv, None, &vars, None, None).unwrap()).unwrap();
        assert_eq!(head["argv"], serde_json::json!(argv));
        assert_eq!(head["caller"]["SPIRA_CZAR_STAGE_DEADLOCK"], "act");
        assert!(head.get("len").is_none(), "no body, no len");
        let head: serde_json::Value = serde_json::from_str(&request_head(&argv, Some(4), &vars, None, None).unwrap()).unwrap();
        assert_eq!(head["len"], 4);
        assert_eq!(request_head(&argv, None, &BTreeMap::new(), None, None).unwrap(), r#"["content","close","sp-a"]"#, "no caller variables: the plain array");
        assert!(caller_of(&["list".to_string()]).is_empty(), "only a content call carries them");
    }

    #[test_id(t35fxxc)]
    #[test]
    fn a_call_inside_a_task_carries_its_name_on_the_wire() {
        let head_of = |task: Option<&str>| {
            let (a, b) = UnixStream::pair().unwrap();
            let argv = vec!["list".to_string()];
            let t = std::thread::spawn(move || {
                let mut line = String::new();
                BufReader::new(&b).read_line(&mut line).unwrap();
                (&b).write_all(b"{\"exit_code\":0,\"len\":0}\n").unwrap();
                line
            });
            match task {
                Some(n) => as_task(n, || exchange_once(Box::new(UnixPort::from(a)), &argv, None)).unwrap(),
                None => exchange_once(Box::new(UnixPort::from(a)), &argv, None).unwrap(),
            };
            t.join().unwrap()
        };
        let tasked: serde_json::Value = serde_json::from_str(head_of(Some("asks")).trim()).unwrap();
        assert_eq!((tasked["task"].as_str(), tasked["argv"][0].as_str()), (Some("asks"), Some("list")));
        assert_eq!(head_of(None).trim(), r#"["list"]"#, "outside a task the wire is unchanged");
    }

    #[test_id(tmc9bta)]
    #[test]
    fn a_thread_spawned_inside_a_task_sends_the_name_it_is_handed_and_none_otherwise() {
        as_task("outer", || {
            let task = current_task();
            assert_eq!(std::thread::spawn(current_task).join().unwrap(), None, "a spawned thread starts unnamed");
            let carried = std::thread::spawn(move || as_task_of(task.as_deref(), current_task)).join().unwrap();
            assert_eq!(carried.as_deref(), Some("outer"));
        });
        assert_eq!(as_task_of(None, current_task), None);
    }

    #[test_id(t113s7p)]
    #[test]
    fn as_task_restores_the_outer_name() {
        assert_eq!(current_task(), None);
        as_task("outer", || {
            as_task("inner", || assert_eq!(current_task().as_deref(), Some("inner")));
            assert_eq!(current_task().as_deref(), Some("outer"));
            let r = std::panic::catch_unwind(|| as_task("doomed", || panic!("boom")));
            assert!(r.is_err());
            assert_eq!(current_task().as_deref(), Some("outer"), "restored after a panic");
            as_task("", || assert_eq!(current_task(), None, "an empty name is none"));
        });
        assert_eq!(current_task(), None);
    }

    #[test_id(tswivb6)]
    #[test]
    fn only_a_declared_stand_in_or_direct_mode_runs_a_program_and_an_unresolved_socket_is_refused() {
        let sock = || Ok::<_, String>("run/x/sock".to_string());
        assert_eq!(route_of(None, sock(), "r/spira-lc"), Ok(Route::Socket("run/x/sock".into())));
        assert_eq!(route_of(Some(""), sock(), "r/spira-lc"), Ok(Route::Socket("run/x/sock".into())), "an empty override declares nothing");
        assert_eq!(route_of(Some("fix/spira-lc"), sock(), "fix/spira-lc"), Ok(Route::Program("fix/spira-lc".into())));
        assert_eq!(route_of(None, Ok(" ".into()), "r/spira-lc"), Ok(Route::Program("r/spira-lc".into())));
        let refused = route_of(None, Err("SPIRA_TOML is not set".into()), "r/spira-lc").unwrap_err();
        assert!(refused.starts_with("cannot tell") && refused.contains("SPIRA_LC_SOCKET"), "{refused}");
    }

    /// The production code of a module: everything above its test module.
    fn code_of(src: &str) -> &str {
        src.split("#[cfg(test)]").next().unwrap_or(src)
    }

    fn launches(src: &str) -> Vec<&'static str> {
        const LAUNCHERS: [&str; 3] = [concat!("Command", "::new"), concat!("bounded", "::"), concat!("bounded", "_for(")];
        LAUNCHERS.into_iter().filter(|l| code_of(src).contains(l)).collect()
    }

    /// law-a-loops-hot-read-is-in-process: every lifecycle helper speaks to the store through
    /// `LcClient`; the one place a stand-in program may be run is this module's transport.
    #[test_id(tpevfic)]
    #[test]
    fn no_lifecycle_helper_launches_a_process_of_its_own() {
        let planted = concat!("fn f() { std::process::", "Command", "::new(\"spira-lc\"); }\n#[cfg(test)]\nmod t {}");
        assert_eq!(launches(planted).len(), 1, "the matcher finds a planted launch");
        assert!(launches(concat!("#[cfg(test)]\nfn f() { ", "Command", "::new(\"x\"); }")).is_empty(), "a test's own launch is not the module's");
        for (name, src) in [
            ("lc_state.rs", include_str!("lc_state.rs")),
            ("lifecycle_row.rs", include_str!("lifecycle_row.rs")),
            ("planning.rs", include_str!("planning.rs")),
        ] {
            assert_eq!(launches(src), Vec::<&str>::new(), "{name} launches a process instead of calling LcClient");
        }
    }
}

/// A scripted `content list` over rows held in memory, answering as bd does: closed rows only
/// with `--all`, 50 rows unless `--limit`, and the whole-history read refused as the store
/// refuses it. Every request it sees is recorded.
pub mod fake {
    use super::{reads_whole_history, Reply, Transport, REFUSED};
    use serde_json::Value;
    use std::sync::Mutex;
    use std::time::Duration;

    /// The ids `open_asks_json` holds.
    pub const OPEN_ASK_IDS: [&str; 3] = ["sp-ask-a", "sp-ask-b", "sp-ask-c"];

    /// `list-asks` over three open asks.
    pub fn open_asks_json() -> String {
        serde_json::json!([
            {"ask_id": "sp-ask-a", "opened_at": 300, "title": "which disk", "class": "design", "work_bead": ""},
            {"ask_id": "sp-ask-b", "opened_at": 200, "title": "", "class": "policy", "work_bead": "sp-w"},
            {"ask_id": "sp-ask-c", "opened_at": 100}
        ])
        .to_string()
    }

    /// An open decision bead the ask record does not hold: bd status alone counts it, the record does not.
    pub fn decision_without_record() -> Value {
        serde_json::json!({"id": "sp-no-record", "status": "open", "labels": ["fixture-ask-label"]})
    }

    pub struct Content {
        pub rows: Vec<Value>,
        /// Lifecycle rows: what `list --live` and `list --ids` answer from.
        pub live: Vec<Value>,
        pub seen: Mutex<Vec<Vec<String>>>,
    }

    impl Content {
        pub fn new(rows: Vec<Value>) -> Self {
            Content { rows, live: vec![], seen: Mutex::new(vec![]) }
        }

        pub fn with_live(mut self, live: Vec<Value>) -> Self {
            self.live = live;
            self
        }

        pub fn requests(&self) -> Vec<Vec<String>> {
            self.seen.lock().unwrap().clone()
        }
    }

    fn flag<'a>(a: &'a [String], name: &str) -> Option<&'a str> {
        a.iter().position(|x| x == name).and_then(|i| a.get(i + 1)).map(String::as_str)
    }

    fn labels(r: &Value) -> Vec<&str> {
        r["labels"].as_array().map(|l| l.iter().filter_map(Value::as_str).collect()).unwrap_or_default()
    }

    impl Transport for Content {
        fn exchange(&self, argv: &[String], _: Option<&str>, _: Duration) -> Result<Reply, String> {
            self.seen.lock().unwrap().push(argv.to_vec());
            if reads_whole_history(argv) {
                return Ok(Reply { code: REFUSED, out: String::new(), err: "refused: whole bead history\n".into(), read_ended: false });
            }
            if argv == ["list", "--live"] {
                return Ok(Reply { code: 0, out: Value::Array(self.live.clone()).to_string(), err: String::new(), read_ended: false });
            }
            if argv.get(..2) == Some(&["list".to_string(), "--ids".to_string()][..]) {
                let ids: Vec<&str> = argv.get(2).map(|v| v.split(',').collect()).unwrap_or_default();
                let rows: Vec<&Value> = self.live.iter().filter(|r| r["bead_id"].as_str().is_some_and(|id| ids.contains(&id))).collect();
                return Ok(Reply { code: 0, out: serde_json::to_string(&rows).unwrap_or_default(), err: String::new(), read_ended: false });
            }
            if argv.first().map(String::as_str) == Some("show") {
                let row = self.live.iter().find(|r| argv.get(1).map(String::as_str) == r["bead_id"].as_str());
                return Ok(match row {
                    Some(r) => Reply { code: 0, out: serde_json::json!({"bead": r}).to_string(), err: String::new(), read_ended: false },
                    None => Reply { code: 1, out: String::new(), err: "no such row\n".into(), read_ended: false },
                });
            }
            if argv.get(..2) == Some(&["content".to_string(), "show".to_string()][..]) {
                let row = self.rows.iter().find(|r| argv.get(2).map(String::as_str) == r["id"].as_str());
                return Ok(match row {
                    Some(r) => Reply { code: 0, out: r.to_string(), err: String::new(), read_ended: false },
                    None => Reply { code: 1, out: String::new(), err: "not found\n".into(), read_ended: false },
                });
            }
            if argv.get(..2) != Some(&["content".to_string(), "list".to_string()][..]) {
                return Ok(Reply { code: 0, out: "[]".into(), err: String::new(), read_ended: false });
            }
            let a = &argv[2..];
            let all = a.iter().any(|x| x == "--all");
            let ids: Option<Vec<&str>> = flag(a, "--id").map(|v| v.split(',').collect());
            let parent = flag(a, "--parent");
            let any: Option<Vec<&str>> = flag(a, "--label-any").map(|v| v.split(',').collect());
            let since = flag(a, "--updated-after");
            let label = flag(a, "--label");
            let closed_after = flag(a, "--closed-after");
            let external_ref = flag(a, "--external-ref");
            let kind = flag(a, "--type").or_else(|| flag(a, "-t"));
            let limit = flag(a, "--limit").and_then(|v| v.parse::<usize>().ok()).unwrap_or(50);
            let rows: Vec<&Value> = self
                .rows
                .iter()
                .filter(|r| all || r["status"] != "closed")
                .filter(|r| ids.as_ref().is_none_or(|ids| ids.contains(&r["id"].as_str().unwrap_or(""))))
                .filter(|r| parent.is_none_or(|p| r["parent"].as_str() == Some(p)))
                .filter(|r| any.as_ref().is_none_or(|any| labels(r).iter().any(|l| any.contains(l))))
                .filter(|r| label.is_none_or(|l| labels(r).contains(&l)))
                .filter(|r| external_ref.is_none_or(|x| r["external_ref"].as_str() == Some(x)))
                .filter(|r| closed_after.is_none_or(|d| r["closed_at"].as_str().is_some_and(|c| c >= d)))
                .filter(|r| since.is_none_or(|d| r["updated_at"].as_str().is_some_and(|u| u >= d)))
                .filter(|r| kind.is_none_or(|k| r["issue_type"].as_str() == Some(k)))
                .take(if limit == 0 { usize::MAX } else { limit })
                .collect();
            Ok(Reply { code: 0, out: serde_json::to_string(&rows).unwrap_or_default(), err: String::new(), read_ended: false })
        }
    }
}
