//! The store's door from Rust: [`LcClient`] speaks spira-lc's socket protocol in-process (one
//! JSON argv line in; a `{"exit_code","len"}` line and `len` bytes out), so a caller pays no
//! process. Running `spira-lc` bounded, without resolving anything through the caller's PATH,
//! survives here only as the declared stand-in route.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const LC_TIMEOUT: Duration = Duration::from_secs(15); // batch-job: spira-lc requests queue behind a loaded Dolt in the round VM

/// spira-lc's exit code for a request the machine looked at and refused.
pub const REFUSED: i32 = 3;
/// spira-lc's exit code for "cannot tell": the machine could not answer.
pub const CANNOT_TELL: i32 = 2;
/// law-calls-have-deadlines: the service stops 3 s after receiving; a client waits 5 s.
pub const DEADLINE: Duration = Duration::from_secs(5);

/// The repair sweeps over every row, which the service bounds at its query deadline.
const SWEEPS: &[&str] = &["reconcile-asks", "reconcile-epics", "reconcile-content", "drop-orphans", "facts-query"];
/// How long a client waits for a sweep: the service's query deadline plus the reply.
pub const SWEEP_DEADLINE: Duration = Duration::from_secs(12); // batch-job: a sweep the service bounds at its 10 s query deadline

/// Why a call has no answer to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LcError {
    /// The machine looked at the request and refused it (exit 3).
    Refused(String),
    /// No answer: unreachable, lost, past its deadline, or "cannot tell".
    Fault(String),
}

impl std::fmt::Display for LcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LcError::Refused(m) | LcError::Fault(m) => f.write_str(m),
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
}

impl Reply {
    /// The first non-empty line of the reason a non-zero answer gives.
    pub fn why(&self) -> String {
        let first = |t: &str| t.lines().find(|l| !l.trim().is_empty()).map(str::to_string);
        first(&self.err).or_else(|| first(&self.out)).unwrap_or_else(|| "no message".into())
    }

    /// Everything the call printed, trimmed, stdout first.
    pub fn text(&self) -> String {
        format!("{}{}", self.out.trim(), self.err.trim())
    }
}

/// How a request reaches the machine. `Err` is a call that produced no answer at all.
pub trait Transport: Send + Sync {
    fn exchange(&self, argv: &[String], stdin: Option<&str>, deadline: Duration) -> Result<Reply, String>;
}

/// The service on its Unix socket: no process is launched.
pub struct Socket {
    path: PathBuf,
}

impl Transport for Socket {
    fn exchange(&self, argv: &[String], stdin: Option<&str>, _deadline: Duration) -> Result<Reply, String> {
        let down = |e: &dyn std::fmt::Display| format!("cannot tell: the lifecycle service is not answering on {}: {e} — nothing was read or written", self.path.display());
        let stream = UnixStream::connect(&self.path).map_err(|e| down(&e))?;
        let wait = if argv.first().is_some_and(|v| SWEEPS.contains(&v.as_str())) { SWEEP_DEADLINE } else { DEADLINE };
        stream.set_read_timeout(Some(wait)).and_then(|_| stream.set_write_timeout(Some(wait))).map_err(|e| down(&e))?;
        exchange(stream, argv, stdin)
    }
}

/// One request on `stream`: the argv as one JSON line, or, with `stdin`, a `{"argv", "len"}`
/// line followed by exactly `len` bytes of it. The answer is a `{"exit_code", "len"[,
/// "err_len"]}` line, then `len` bytes of stdout and `err_len` of stderr.
///
/// Once a request is on the wire the service may apply it whatever happens to the reply, so a
/// lost reply is "cannot tell", never a retry.
pub fn exchange(stream: UnixStream, argv: &[String], stdin: Option<&str>) -> Result<Reply, String> {
    let lost = |why: &str| format!("cannot tell: the lifecycle service took the request but its reply was lost ({why})");
    let request = match stdin {
        None => serde_json::to_string(argv),
        Some(body) => serde_json::to_string(&serde_json::json!({"argv": argv, "len": body.len()})),
    }
    .map_err(|_| lost("unserialisable request"))?;
    let mut bytes = format!("{request}\n").into_bytes();
    bytes.extend_from_slice(stdin.unwrap_or_default().as_bytes());
    (&stream).write_all(&bytes).map_err(|_| lost("write"))?;
    let mut reader = BufReader::with_capacity(1 << 20, stream);
    let mut head = String::new();
    if !matches!(reader.read_line(&mut head), Ok(n) if n > 0) {
        return Err(lost("no reply"));
    }
    let head: serde_json::Value = serde_json::from_str(head.trim()).map_err(|_| lost("malformed reply"))?;
    let (Some(code), Some(len)) = (head["exit_code"].as_i64(), head["len"].as_u64()) else { return Err(lost("malformed reply")) };
    let err_len = head["err_len"].as_u64().unwrap_or(0);
    let mut body = vec![0u8; (len + err_len) as usize];
    reader.read_exact(&mut body).map_err(|_| lost("truncated reply"))?;
    let err = body.split_off(len as usize);
    let text = |b: Vec<u8>| String::from_utf8(b).map_err(|_| lost("non-utf8 reply"));
    Ok(Reply { code: code as i32, out: text(body)?, err: text(err)? })
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
        Ok(Reply { code, out: String::from_utf8_lossy(&out.stdout).into_owned(), err: String::from_utf8_lossy(&out.stderr).into_owned() })
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
}

impl LcClient {
    /// The service on `path`.
    pub fn socket(path: impl Into<PathBuf>) -> Self {
        LcClient { transport: Arc::new(Socket { path: path.into() }), socket: true, deadline: DEADLINE }
    }

    /// Every call through `transport` (a test's scripted double, typically).
    pub fn over(transport: Arc<dyn Transport>, socket: bool) -> Self {
        LcClient { transport, socket, deadline: DEADLINE }
    }

    /// The route the configuration declares for a caller holding `bin`.
    pub fn for_bin(bin: &str) -> Result<Self, String> {
        let stand_in = std::env::var(crate::lifecycle_row::LC_BIN_ENV).ok();
        let route = route_of(stand_in.as_deref(), crate::process::cfg("SPIRA_LC_SOCKET"), bin)?;
        Ok(match route {
            Route::Socket(path) => LcClient::socket(path),
            Route::Program(bin) => LcClient { transport: Arc::new(Program { bin }), socket: false, deadline: DEADLINE },
        })
    }

    /// [`for_bin`](Self::for_bin) for the configured `spira-lc`.
    pub fn configured() -> Result<Self, String> {
        Self::for_bin(&crate::lifecycle_row::lc_bin()?)
    }

    /// Bounds a program call at `d` (a batch caller's wait); the socket keeps [`DEADLINE`].
    pub fn within(&self, d: Duration) -> Self {
        LcClient { deadline: d, ..self.clone() }
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
        self.transport.exchange(&argv, stdin, self.deadline).map_err(LcError::Fault)
    }

    /// The answer's stdout when it applied (exit 0); a refusal (3) or anything else is an error.
    pub fn answer<S: AsRef<str>>(&self, argv: &[S]) -> Result<String, LcError> {
        let r = self.call(argv)?;
        match r.code {
            0 => Ok(r.out),
            REFUSED => Err(LcError::Refused(r.text())),
            _ => Err(LcError::Fault(r.text())),
        }
    }
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

pub fn is_refusal(code: i32, out: &str) -> bool {
    code != 0 && out.starts_with("cannot tell")
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
    use super::*;

    #[test]
    fn a_deadline_refusal_is_never_stdout() {
        assert!(is_refusal(2, "cannot tell: deadline"));
        assert!(!is_refusal(0, "sp-abc12"), "an id is data");
        assert!(!is_refusal(1, "sp-abc12"));
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
                let (code, body) = answer(&argv);
                log.lock().unwrap().push(argv);
                let (out, err) = body.split_once("\x00").unwrap_or((&body, ""));
                let _ = write!(&s, "{}\n{out}{err}", serde_json::json!({"exit_code": code, "len": out.len(), "err_len": err.len()}));
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
            "close" | "claim-verdicts" => (0, format!("{}\x00", argv.join(" "))),
            "hold" => (3, "\x00lc: sp-a: refused\n".into()),
            _ => (2, "cannot tell: not served".into()),
        }
    }

    #[test]
    fn the_helpers_answer_over_the_socket_and_launch_nothing() {
        let dir = testkit::TempDir::new("lc-client-socket");
        let (path, seen) = serve(&dir, store);
        let c = LcClient::socket(&path);
        let before = launched();
        let rows = crate::lc_state::list_on(&c).unwrap();
        assert_eq!(rows.iter().map(|r| r.bead_id.as_str()).collect::<Vec<_>>(), ["sp-a", "sp-b"]);
        assert_eq!(crate::lc_state::row_on(&c, "sp-a").unwrap().map(|r| r.state), Some("READY".into()));
        assert_eq!(crate::lc_state::row_on(&c, "sp-zz").unwrap(), None, "exit 1 is no row");
        crate::lifecycle_row::set_express_on(&c, "sp-a", true).unwrap();
        assert!(crate::lifecycle_row::set_express_on(&c, "sp-zz", true).is_err(), "no row is nothing to mark");
        assert_eq!(launched(), before, "no program ran");
        let seen = seen.lock().unwrap();
        let event = seen.iter().find(|a| a[0] == "event").expect("express sent its event");
        assert_eq!(event[1..], ["bead", "sp-a", "--expect", "READY", "--version", "4", "--actor", "bead", "--kind", "\"Express\""].map(String::from));
    }

    #[test]
    fn a_refusal_and_a_fault_stay_apart_and_a_dead_socket_is_a_fault() {
        let dir = testkit::TempDir::new("lc-client-codes");
        let (path, _) = serve(&dir, store);
        let c = LcClient::socket(&path);
        assert!(matches!(c.answer(&["plan", "stamp"]), Err(LcError::Refused(m)) if m.contains("no planning plane")));
        assert!(matches!(c.answer(&["nope"]), Err(LcError::Fault(m)) if m.starts_with("cannot tell")));
        assert!(matches!(c.answer(&["hold", "sp-a", "wait"]), Err(LcError::Refused(m)) if m == "lc: sp-a: refused"), "a refusal's stderr is its text");
        let dead = LcClient::socket(dir.join("no-such.sock"));
        assert!(matches!(dead.call(&["list"]), Err(LcError::Fault(m)) if m.contains("not answering")));
    }

    /// The verbs the CLI once answered itself go to the service like any other, and a body
    /// rides the socket to exactly its length.
    #[test]
    fn close_and_a_stdin_body_go_over_the_socket_and_launch_nothing() {
        let dir = testkit::TempDir::new("lc-client-stdin");
        let (path, seen) = serve(&dir, store);
        let c = LcClient::socket(&path);
        let before = launched();
        assert_eq!(c.call(&["close", "sp-a", "--reason", "landed"]).unwrap().out, "close sp-a --reason landed");
        let r = c.call_stdin(&["claim-verdicts"], "[{\"bead_id\":\"sp-a\"}]\nsecond line").unwrap();
        assert_eq!((r.code, r.out.as_str()), (0, "claim-verdicts stdin=[{\"bead_id\":\"sp-a\"}]\nsecond line"));
        assert_eq!(launched(), before, "no program ran");
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[test]
    fn only_a_declared_stand_in_or_direct_mode_runs_a_program_and_an_unresolved_socket_is_refused() {
        let sock = || Ok::<_, String>("/run/x/sock".to_string());
        assert_eq!(route_of(None, sock(), "/r/spira-lc"), Ok(Route::Socket("/run/x/sock".into())));
        assert_eq!(route_of(Some(""), sock(), "/r/spira-lc"), Ok(Route::Socket("/run/x/sock".into())), "an empty override declares nothing");
        assert_eq!(route_of(Some("/fix/spira-lc"), sock(), "/fix/spira-lc"), Ok(Route::Program("/fix/spira-lc".into())));
        assert_eq!(route_of(None, Ok(" ".into()), "/r/spira-lc"), Ok(Route::Program("/r/spira-lc".into())));
        let refused = route_of(None, Err("SPIRA_TOML is not set".into()), "/r/spira-lc").unwrap_err();
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
