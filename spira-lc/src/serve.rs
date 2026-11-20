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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime};

use crate::callers::Answer;

/// Revisions of `events/` one sweep tick folds into the stay tally, so a tick stays well under a second.
const STAYS_REVS_PER_TICK: usize = 1_000;
const EDGES_REVS_PER_TICK: usize = 1_000;
const SWEEP_EVERY: Duration = Duration::from_secs(5);

static SPANS: std::sync::OnceLock<spira_otel::Sink> = std::sync::OnceLock::new();
static LEDGER: std::sync::OnceLock<Ledger> = std::sync::OnceLock::new();
static DEADLINES: std::sync::OnceLock<Deadlines> = std::sync::OnceLock::new();

const LEDGER_KEY: &str = "SPIRA_LC_REQUEST_LEDGER_SECS";

fn ledger_ttl() -> Result<Duration, String> {
    ledger_ttl_from(spira_config::process::cfg)
}

fn ledger_ttl_from(read: impl FnOnce(&str) -> Result<String, String>) -> Result<Duration, String> {
    let value = read(LEDGER_KEY).map_err(|e| format!("spira.lc_request_ledger_secs: {e}"))?;
    spira_config::process::parse_declared::<u64>(LEDGER_KEY, &value)
        .map(Duration::from_secs)
        .map_err(|e| format!("spira.lc_request_ledger_secs: {e}"))
}

const DEADLINE_KEY: &str = "SPIRA_LC_REQUEST_DEADLINE_MS";
const SLOW_KEY: &str = "SPIRA_LC_SLOW_VERB_DEADLINES";
/// The receiver stops 3 s after receiving (law-calls-have-deadlines); no configured deadline may exceed it.
const MAX_CONFIGURED_MS: u64 = 3000;

/// The deadlines a store call is served under, pinned at serve start.
#[derive(Clone, Debug)]
pub(crate) struct Deadlines {
    pub(crate) default: Duration,
    /// verb -> (its deadline, the bead that fixes it)
    pub(crate) slow: std::collections::BTreeMap<String, (Duration, String)>,
}

/// Parses `verb=ms@bead,...`; the verb, its milliseconds and its bead.
pub(crate) fn parse_slow_entries(value: &str) -> Result<Vec<(String, u64, String)>, String> {
    let mut out = Vec::new();
    for entry in value.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let bad = |why: &str| format!("spira.lc_slow_verb_deadlines: entry {entry:?} {why}; the form is verb=ms@bead");
        let (verb, rest) = entry.split_once('=').ok_or_else(|| bad("has no ="))?;
        let (ms, bead) = rest.split_once('@').ok_or_else(|| bad("names no @bead"))?;
        if verb.is_empty() || bead.is_empty() {
            return Err(bad("has an empty verb or bead"));
        }
        let ms: u64 = ms.parse().map_err(|_| bad("has a deadline that is not a whole number of ms"))?;
        if ms == 0 || ms > MAX_CONFIGURED_MS {
            return Err(bad("has a deadline outside 1..=3000 ms"));
        }
        out.push((verb.to_string(), ms, bead.to_string()));
    }
    Ok(out)
}

fn deadlines_from(read: impl Fn(&str) -> Result<String, String>) -> Result<Deadlines, String> {
    let default = read(DEADLINE_KEY).map_err(|e| format!("spira.lc_request_deadline_ms: {e}"))?;
    let default: u64 = spira_config::process::parse_declared(DEADLINE_KEY, &default).map_err(|e| format!("spira.lc_request_deadline_ms: {e}"))?;
    if default == 0 || default > MAX_CONFIGURED_MS {
        return Err(format!("spira.lc_request_deadline_ms: {default} is outside 1..=3000 ms"));
    }
    let slow = read(SLOW_KEY).map_err(|e| format!("spira.lc_slow_verb_deadlines: {e}"))?;
    let slow = parse_slow_entries(&slow)?.into_iter().map(|(v, ms, bead)| (v, (Duration::from_millis(ms), bead))).collect();
    Ok(Deadlines { default: Duration::from_millis(default), slow })
}

enum Entry {
    InFlight,
    Done(Answer, Instant),
}

struct LedgerState {
    ttl: Duration,
    slots: std::sync::Mutex<std::collections::HashMap<String, Entry>>,
    changed: std::sync::Condvar,
}

/// The answers to writes that carried a request id, so a second send under one id is answered
/// by the first rather than applied again.
#[derive(Clone)]
pub(crate) struct Ledger(Arc<LedgerState>);

/// Settles the in-flight slot when its worker ends: records the answer, or, if the worker died
/// without one, frees the id so a retry runs.
struct Settle {
    ledger: Ledger,
    id: String,
    recorded: bool,
}

impl Settle {
    fn record(&mut self, ans: &Answer) {
        let mut slots = self.ledger.0.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots.insert(self.id.clone(), Entry::Done(ans.clone(), Instant::now()));
        self.recorded = true;
        self.ledger.0.changed.notify_all();
    }
}

impl Drop for Settle {
    fn drop(&mut self) {
        if !self.recorded {
            self.ledger.0.slots.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.id);
            self.ledger.0.changed.notify_all();
        }
    }
}

fn deadline_answer() -> Answer {
    Answer::out(2, format!("cannot tell: {}", crate::db::DEADLINE_MESSAGE))
}

impl Ledger {
    pub(crate) fn new(ttl: Duration) -> Ledger {
        Ledger(Arc::new(LedgerState { ttl, slots: Default::default(), changed: Default::default() }))
    }

    /// The answer to the request `id`: the recorded one, the in-flight one's once it ends, or
    /// `work`'s own, run at most once however many sends arrive. No answer within `wait` is a
    /// deadline answer, and a worker still running keeps its slot until it has a real one.
    fn answer(&self, id: &str, wait: Duration, work: impl FnOnce() -> Answer + Send + 'static) -> Answer {
        let end = Instant::now() + wait;
        let mut slots = self.0.slots.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let ttl = self.0.ttl;
            slots.retain(|_, s| !matches!(s, Entry::Done(_, at) if at.elapsed() >= ttl));
            match slots.get(id) {
                Some(Entry::Done(ans, _)) => return Answer { owed: Box::new([]), cert_log: None, ..ans.clone() },
                Some(Entry::InFlight) => {
                    let left = end.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return deadline_answer();
                    }
                    slots = self.0.changed.wait_timeout(slots, left).unwrap_or_else(|e| e.into_inner()).0;
                }
                None => break,
            }
        }
        slots.insert(id.to_string(), Entry::InFlight);
        drop(slots);
        let mut settle = Settle { ledger: self.clone(), id: id.to_string(), recorded: false };
        within(wait, move || {
            let ans = work();
            settle.record(&ans);
            ans
        })
        .unwrap_or_else(deadline_answer)
    }
}

type Work = Arc<dyn Fn(Request) -> Answer + Send + Sync>;

/// What a request is served with: the ledger, the work that answers it and the deadline it keeps.
struct Door {
    ledger: Ledger,
    work: Work,
    deadlines: Deadlines,
}

fn live_work() -> Work {
    Arc::new(|req| {
        let mut local = Local;
        answer(req, &mut local, &mut crate::bd::LiveBd, &crate::composed::LiveContent, &crate::lifecycle_run_dir)
    })
}

/// Arenas glibc may give the connection threads between them: a thread per connection otherwise
/// grows an arena of its own, and what it frees stays there.
const MALLOC_ARENAS: libc::c_int = 2;

const TRIM_EVERY_MS: u64 = 1_000;

/// A thread answering a request: a connection's or a worker that outlives its deadline. The
/// memory they freed goes back to the system when the last one ends.
struct Busy;

static BUSY: AtomicUsize = AtomicUsize::new(0);

impl Busy {
    fn enter() -> Busy {
        BUSY.fetch_add(1, Ordering::SeqCst);
        Busy
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        return_freed_memory(BUSY.fetch_sub(1, Ordering::SeqCst) == 1);
    }
}

/// Gives the system the memory the request threads freed: glibc keeps it in its arenas
/// otherwise. Always when `idle`, and otherwise at most once a second.
fn return_freed_memory(idle: bool) {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = wall_ms();
    let last = LAST.load(Ordering::Relaxed);
    if (idle || now.saturating_sub(last) >= TRIM_EVERY_MS) && LAST.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
        // SAFETY: malloc_trim only returns free pages to the system.
        unsafe {
            libc::malloc_trim(0);
        }
    }
}

pub fn run(args: &[String]) -> i32 {
    // SAFETY: mallopt only sets a glibc tunable, before any thread exists.
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, MALLOC_ARENAS);
    }
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

    let mut startup = Startup::begin();
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

    match ledger_ttl() {
        Ok(ttl) => {
            let _ = LEDGER.set(Ledger::new(ttl));
        }
        Err(e) => {
            eprintln!("spira-lc serve: {e}");
            return 2;
        }
    }

    match crate::slow::answer_limits_from(spira_config::process::cfg) {
        Ok(limits) => crate::slow::pin_limits(limits),
        Err(e) => {
            eprintln!("spira-lc serve: {e}");
            return 2;
        }
    }

    match deadlines_from(spira_config::process::cfg) {
        Ok(d) => {
            let _ = DEADLINES.set(d);
        }
        Err(e) => {
            eprintln!("spira-lc serve: {e}");
            return 2;
        }
    }

    startup.mark("config");

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

    startup.mark("control-store");

    // lc-serve is the store's only opener, so it is the one process that can ship it off-box.
    let shipping = match &control {
        None => None,
        Some(store) => {
            let shipper = crate::lifecycle_run_dir().and_then(|run| match crate::durability::configured(store.contract().backend().clone(), &run) {
                None => Ok(None),
                Some(s) => s.map(Some),
            });
            match shipper {
                Ok(None) => {
                    eprintln!("spira-lc serve: SPIRA_LC_BACKUP_URL is empty: the control store is not shipped off-box");
                    if let Err(e) = store.contract().backend().drop_outbox() {
                        eprintln!("spira-lc serve: the control store's outbox: {e}");
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

    startup.mark("shipper");

    stop_on_sigterm();
    crate::plan::serve_here();
    crate::settle::install();
    startup.mark("plan");
    match crate::plan::read_watermark() {
        Ok(w) => crate::plan::install_mirror(w),
        Err(e) => {
            eprintln!("spira-lc serve: planning watermark unread, bd kv is mirrored from the next stamp: {e}");
            crate::plan::install_mirror(0);
        }
    }
    startup.mark("watermark");
    if let Err(e) = crate::content_sql::check_at_startup() {
        eprintln!("spira-lc serve: content reads are refused until this is fixed: {e}");
    }
    startup.mark("content-check");
    // A request may spawn a child (`work blocked` runs `mail`) that calls back into this
    // socket; serving one connection at a time deadlocks on that.
    if crate::control::served::configured().is_none() {
        eprintln!("spira-lc serve: SPIRA_LC_STORE_PATH is unset: every lifecycle verb answers cannot-tell");
    }
    {
        if let Some(mut shipping) = shipping {
            std::thread::spawn(move || {
                let endpoint = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok();
                let metrics = spira_otel::metrics::MetricsSink::new(endpoint.as_deref(), "spira-lc", Duration::from_millis(250));
                let mut red = false;
                loop {
                    std::thread::sleep(shipping.every);
                    red = crate::durability::tick(&mut shipping, red, &metrics, &runtime::ports::WallClock);
                }
            });
        }
        std::thread::spawn(|| {
            let endpoint = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok();
            let metrics = spira_otel::metrics::MetricsSink::new(endpoint.as_deref(), "spira-lc", Duration::from_millis(250));
            let mut pushed = Default::default();
            loop {
                std::thread::sleep(crate::slow::METRICS_EVERY);
                crate::slow::push_counts(&metrics, &mut pushed);
                crate::slow::push_rss(&metrics);
            }
        });
        if let Some(store) = control {
            std::thread::spawn(move || {
                let _holder = state_store::phases::holder_scoped("gc", "");
                let gc = state_store::gc::Controller::new(store.contract());
                loop {
                    std::thread::sleep(SWEEP_EVERY);
                    gc_tick(&gc, store.contract());
                }
            });
        }
        std::thread::spawn(crate::plan::run_mirror);
        std::thread::spawn(crate::settle::run);
        std::thread::spawn(move || {
            loop {
            std::thread::sleep(SWEEP_EVERY);
            alarm_tick();
            match crate::control::served::configured() {
                Some(Ok(store)) => {
                    {
                        let _holder = state_store::phases::holder_scoped("mending-sweep", "");
                        match store_sweep(&store, crate::db::now_epoch()) {
                            Ok(n) if n > 0 => eprintln!("spira-lc serve: {n} overdue mending rows expired"),
                            Ok(_) => {}
                            Err(e) => eprintln!("spira-lc serve: mending sweep: {e}"),
                        }
                    }
                    {
                        let _holder = state_store::phases::holder_scoped("dwell-warm", "");
                        if let Err(e) = warm_tick(&store, crate::db::now_epoch()) {
                            eprintln!("spira-lc serve: dwell warm: {e}");
                        }
                    }
                    let _holder = state_store::phases::holder_scoped("stay-tally", "");
                    if let Err(e) = store.fold_stays(crate::db::now_epoch(), STAYS_REVS_PER_TICK) {
                        eprintln!("spira-lc serve: stay tally: {e}");
                    }
                    let _holder = state_store::phases::holder_scoped("edge-tally", "");
                    if let Err(e) = store.fold_edges(crate::db::now_epoch(), EDGES_REVS_PER_TICK) {
                        eprintln!("spira-lc serve: edge tally: {e}");
                    }
                }
                Some(Err(e)) => eprintln!("spira-lc serve: mending sweep: the control store: {e}"),
                None => {}
            }
            }
        });
    }
    let listener = match bind_socket(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("spira-lc serve: cannot bind {socket_path}: {e}");
            return 2;
        }
    };
    startup.mark("bind");
    eprintln!("spira-lc serve: listening on {socket_path}");
    startup.ready(SPANS.get());
    let flights = Arc::new(Flights::default());
    let (done, finished) = std::sync::mpsc::channel::<()>();
    {
        let flights = flights.clone();
        std::thread::spawn(move || {
            std::thread::scope(|scope| {
                accept_until_stopped(&listener, &socket_path, scope, &STOPPING, |stream| flights.serve(stream, |stream| handle(stream, Instant::now())))
            });
            let _ = done.send(());
        });
    }
    let (drained, answered) = drain(&flights, &STOPPING, &finished, DRAIN_BOUND);
    eprintln!("spira-lc serve: stopped after draining {drained}, answered {answered} at the bound");
    0
}

/// Wall-clock milliseconds since the epoch, for a log line's timestamp.
pub(crate) fn wall_ms() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

pub(crate) fn startup_line(phase: &str, took: Duration, wall_ms: u64) -> String {
    format!("spira-lc serve: {} start-up {phase} {} ms", spira_lc::sections::rfc3339(wall_ms), took.as_millis())
}

fn log_rss(at: &str) {
    if let Some(line) = crate::slow::rss_anon_line(at) {
        eprintln!("{line}");
    }
}

fn ready_line(phases: &[(&str, Duration)], wall_ms: u64) -> String {
    let total: Duration = phases.iter().map(|(_, took)| *took).sum();
    format!("spira-lc serve: {} ready after {} ms", spira_lc::sections::rfc3339(wall_ms), total.as_millis())
}

/// One line per phase in order, then `ready after` the sum of them.
#[cfg(test)]
fn startup_lines(phases: &[(&str, Duration)], wall_ms: u64) -> Vec<String> {
    let mut lines: Vec<String> = phases.iter().map(|(phase, took)| startup_line(phase, *took, wall_ms)).collect();
    lines.push(ready_line(phases, wall_ms));
    lines
}

/// Times each start-up phase as it ends, so the log says where a restart's seconds went.
struct Startup {
    began: SystemTime,
    mark: Instant,
    phases: Vec<(&'static str, Duration)>,
}

impl Startup {
    fn begin() -> Startup {
        Startup { began: SystemTime::now(), mark: Instant::now(), phases: Vec::new() }
    }

    fn mark(&mut self, phase: &'static str) {
        let took = self.mark.elapsed();
        eprintln!("{}", startup_line(phase, took, wall_ms()));
        log_rss(phase);
        self.phases.push((phase, took));
        self.mark = Instant::now();
    }

    fn ready(&self, spans: Option<&spira_otel::Sink>) {
        eprintln!("{}", ready_line(&self.phases, wall_ms()));
        let endpoint = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok();
        let metrics = spira_otel::metrics::MetricsSink::new(endpoint.as_deref(), "spira-lc", Duration::from_millis(250));
        crate::slow::push_ready(&metrics, self.phases.iter().map(|(_, d)| *d).sum::<Duration>().as_millis() as u64);
        let Some(spans) = spans else { return };
        let us: Vec<(&str, u64)> = self.phases.iter().map(|(p, d)| (*p, d.as_micros() as u64)).collect();
        spans.emit(&spira_otel::Call {
            method: "start-up",
            caller: "-",
            request_size: 0,
            response_size: 0,
            exit_code: 0,
            start: self.began,
            end: SystemTime::now(),
            queue_wait: Duration::ZERO,
            answered: None,
            phases: &us,
            commits_us: &[],
            reads: 0,
            open_phase: None,
        });
    }
}

/// Leaves 30 s of the unit's 90 s `TimeoutStopSec` to write the refusals and exit.
pub const DRAIN_BOUND: Duration = Duration::from_secs(60);

fn stopped_for_restart() -> String {
    format!("cannot tell: spira-lc stopped for a restart before this request finished (drain bound {} s); it may have applied", DRAIN_BOUND.as_secs())
}

/// One accepted connection: a clone of its stream for the drain's refusal, and whether any reply
/// has gone out. The lock makes the worker's reply and the drain's mutually exclusive.
struct Slot {
    stream: UnixStream,
    answered: std::sync::Mutex<bool>,
}

#[derive(Default)]
struct Flights {
    open: std::sync::Mutex<Vec<(u64, Arc<Slot>)>>,
    next: std::sync::atomic::AtomicU64,
}

thread_local! {
    static CURRENT: std::cell::RefCell<Option<Arc<Slot>>> = const { std::cell::RefCell::new(None) };
}

impl Flights {
    fn serve(&self, stream: UnixStream, f: impl FnOnce(UnixStream)) {
        struct Leave<'a>(&'a Flights, u64);
        impl Drop for Leave<'_> {
            fn drop(&mut self) {
                CURRENT.with(|c| *c.borrow_mut() = None);
                self.0.open.lock().unwrap().retain(|(i, _)| *i != self.1);
            }
        }
        let Ok(clone) = stream.try_clone() else {
            eprintln!("spira-lc serve: cannot register a connection for the drain; dropped unanswered");
            return;
        };
        let slot = Arc::new(Slot { stream: clone, answered: std::sync::Mutex::new(false) });
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        self.open.lock().unwrap().push((id, slot.clone()));
        CURRENT.with(|c| *c.borrow_mut() = Some(slot));
        let _leave = Leave(self, id);
        let _busy = Busy::enter();
        f(stream);
    }

    fn len(&self) -> usize {
        self.open.lock().unwrap().len()
    }

    fn refuse_unanswered(&self) -> usize {
        let open = self.open.lock().unwrap().clone();
        let mut refused = 0;
        for (_, slot) in open {
            let mut answered = slot.answered.lock().unwrap();
            if !*answered {
                let _ = slot.stream.set_write_timeout(Some(Duration::from_secs(5)));
                let _ = write_raw(&slot.stream, 2, &stopped_for_restart(), "");
                *answered = true;
                refused += 1;
            }
        }
        refused
    }
}

/// Wait for the accept loop and its connections to finish (`finished`), but for no more than
/// `bound` after `stopping` was seen; then answer every connection still unanswered.
/// Returns (in flight when the stop was seen, answered at the bound).
fn drain(flights: &Flights, stopping: &AtomicBool, finished: &std::sync::mpsc::Receiver<()>, bound: Duration) -> (usize, usize) {
    use std::sync::mpsc::RecvTimeoutError::{Disconnected, Timeout};
    while !stopping.load(Ordering::SeqCst) {
        match finished.recv_timeout(Duration::from_millis(50)) {
            Ok(()) | Err(Disconnected) => return (0, 0),
            Err(Timeout) => {}
        }
    }
    let drained = flights.len();
    match finished.recv_timeout(bound) {
        Ok(()) | Err(Disconnected) => (drained, 0),
        Err(Timeout) => (drained, flights.refuse_unanswered()),
    }
}

static STOPPING: AtomicBool = AtomicBool::new(false);
const SIGTERM: i32 = 15;
const POLLIN: i16 = 1;

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

extern "C" {
    fn signal(signum: i32, handler: extern "C" fn(i32)) -> usize;
    fn poll(fds: *mut PollFd, nfds: u64, timeout_ms: i32) -> i32;
}

extern "C" fn on_sigterm(_: i32) {
    STOPPING.store(true, Ordering::SeqCst);
}

/// SIGTERM stops accepting and drains instead of killing the requests this process has taken.
fn stop_on_sigterm() {
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe { signal(SIGTERM, on_sigterm) };
}

/// Accept on `listener` until `stopping`, serving each connection on its own thread. On the
/// stop the path is removed first, so a new connect is refused at once; connections already
/// queued are then accepted and served, and only afterwards does the listener drop.
fn accept_until_stopped<'scope, 'env, F>(
    listener: &UnixListener,
    socket_path: &str,
    scope: &'scope std::thread::Scope<'scope, 'env>,
    stopping: &AtomicBool,
    serve: F,
) where
    F: Fn(UnixStream) + Send + Sync + Clone + 'scope,
{
    use std::os::unix::io::AsRawFd;
    let mut ready = PollFd { fd: listener.as_raw_fd(), events: POLLIN, revents: 0 };
    while !stopping.load(Ordering::SeqCst) {
        ready.revents = 0;
        // SAFETY: one valid pollfd for a descriptor this function borrows.
        if unsafe { poll(&mut ready, 1, 100) } <= 0 || ready.revents & POLLIN == 0 {
            continue;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let serve = serve.clone();
                scope.spawn(move || serve(stream));
            }
            Err(e) => eprintln!("spira-lc serve: accept error: {e}"),
        }
    }
    if let Err(e) = std::fs::remove_file(socket_path) {
        eprintln!("spira-lc serve: cannot remove {socket_path}: {e}");
    }
    if let Err(e) = listener.set_nonblocking(true) {
        eprintln!("spira-lc serve: cannot drain the queue: {e}");
        return;
    }
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let serve = serve.clone();
                scope.spawn(move || {
                    let _ = stream.set_nonblocking(false);
                    serve(stream)
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => {
                if e.kind() != std::io::ErrorKind::WouldBlock {
                    eprintln!("spira-lc serve: accept error: {e}");
                }
                break;
            }
        }
    }
}

/// One pass of the control store's GC controller; each failed step is logged under its name,
/// and a pass that finished a scan or collected anything says what it did.
fn gc_tick<B: state_store::Backend>(gc: &state_store::gc::Controller<B>, contract: &state_store::Contract<B>) {
    let wall_ms = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    for line in gc_lines(&gc.pass(contract.now_ms()), wall_ms) {
        eprintln!("{line}");
    }
}

fn gc_lines(tick: &state_store::gc::Tick, wall_ms: u64) -> Vec<String> {
    let at = spira_lc::sections::rfc3339(wall_ms);
    let mut lines: Vec<String> = tick.failed.iter().map(|f| format!("spira-lc serve: {at} control-store gc {f}")).collect();
    if tick.orphans.len() + tick.expired.len() > 0 {
        lines.push(format!(
            "spira-lc serve: {at} control-store gc: {} orphans, {} expired",
            tick.orphans.len(),
            tick.expired.len()
        ));
    }
    lines
}

/// On the store the sweep is `Expire` on overdue MENDING rows; the dolt ticks read tables
/// the store does not have, and epics are reconciled by the `reconcile-content` verb.
fn store_sweep<B: state_store::Backend>(store: &crate::control::store::Store<B>, now: i64) -> Result<usize, String> {
    let request = crate::control::served::request_id();
    let call = crate::control::state::Call { now, request: &request, inputs: &crate::control::claimable::Live };
    let argv: Vec<String> = ["mending", "sweep", "--actor", "spira-lc.sweep"].iter().map(|s| s.to_string()).collect();
    match crate::control::state::run(store, &argv, &call) {
        (0, out) => out.trim().parse().map_err(|_| format!("sweep answered {out:?}")),
        (code, out) => Err(format!("exit {code}: {out}")),
    }
}

/// The widest window any caller of the audit index asks: the transitions task's, from the same
/// bounds it reads, and the lifecycle section's two-day lookback.
fn dwell_window_s() -> Result<u64, String> {
    let keys = ["SPIRA_TRANSITIONS_SUBMIT_BOUND", "SPIRA_TRANSITIONS_CUT_BOUND", "SPIRA_TRANSITIONS_DELIVER_BOUND", "SPIRA_TRANSITIONS_ACTIVATE_BOUND"];
    let bounds = keys.iter().map(|k| spira_config::process::cfg_parse::<u64>(k)).collect::<Result<Vec<_>, _>>()?;
    let transitions = bounds.into_iter().max().unwrap_or(0) + 600;
    Ok(transitions.max(2 * spira_lc::goodput::DAY_SECS))
}

/// One bounded step of the backward read of the audit index; each sweep takes the next.
fn warm_tick<B: state_store::Backend>(store: &crate::control::store::Store<B>, now: i64) -> Result<(), String> {
    let window = dwell_window_s()?;
    store.warm_dwell(now, window, crate::control::dwell::WARM_SCANNED).map(|_| ()).map_err(|e| e.to_string())
}

fn alarm_tick() {
    let lines = crate::slow::latency_transitions(Instant::now());
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

fn handle(stream: UnixStream, accepted: Instant) {
    let door = Door { ledger: LEDGER.get().expect("the ledger is set before the socket is bound").clone(), work: live_work(), deadlines: DEADLINES.get().expect("the deadlines are set before the socket is bound").clone() };
    serve_one(stream, SPANS.get(), accepted, &door);
}

fn serve_one(stream: UnixStream, spans: Option<&spira_otel::Sink>, accepted: Instant, door: &Door) {
    let received = Instant::now();
    let started_work = Arc::new(std::sync::Mutex::new(None::<Instant>));
    let started = SystemTime::now();
    let mut reader = BufReader::new(stream.try_clone().expect("clone unix stream"));
    let read_at;
    let (req, size) = match read_request(&mut reader) {
        Ok(None) => return,
        Ok(Some(r)) => {
            read_at = Instant::now();
            r
        }
        Err((size, msg)) => {
            let _ = write_response(&stream, 2, &msg, "");
            let none = state_store::phases::Snapshot::default();
            let call = spira_otel::Call {
                method: "-",
                caller: "-",
                request_size: size as u64,
                response_size: msg.len() as u64,
                exit_code: 2,
                start: started,
                end: SystemTime::now(),
                queue_wait: Instant::now().saturating_duration_since(accepted),
                answered: None,
                phases: &none.phases,
                commits_us: &none.commits_us,
                reads: none.reads,
                open_phase: none.open,
            };
            record_span(spans, &call);
            return;
        }
    };
    let caller = peer_pid(&stream).map(crate::slow::caller_of_pid).unwrap_or_else(|| "-".to_string());
    let verb = req.argv.first().cloned().unwrap_or_default();
    let map = std::fs::read_to_string(personas_path()).map(|t| crate::work::parse_persona_uids(&t)).unwrap_or_default();
    let argv_seen = req.argv.clone();
    let task = req.task.clone();
    let recorder = Arc::new(state_store::phases::Recorder::default());
    let ans = match crate::work::bind_peer(&req.argv, peer_uid(&stream), &map) {
        Ok(argv) => {
            let who = caller.clone();
            let holder_verb = verb.clone();
            let began = Arc::clone(&started_work);
            let limit = request_deadline(&argv, &door.deadlines);
            let ledgered = req.request_id.clone().filter(|_| !spira_store::lc_call::is_read(&argv));
            let request_id = req.request_id;
            let req = Request { argv, stdin: req.stdin, caller: req.caller, task: None, request_id: request_id.clone() };
            let worker_rec = Arc::clone(&recorder);
            let work = Arc::clone(&door.work);
            let run = move || {
                let _tracking = state_store::phases::track_scoped(worker_rec);
                let _holder = state_store::phases::holder_scoped(&holder_verb, &who);
                *began.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
                crate::slow::set_caller(&who);
                let _deadline = bind_deadline(received, limit);
                let _request = crate::control::served::client_request_scoped(request_id);
                work(req)
            };
            let wait = limit.saturating_sub(received.elapsed());
            match ledgered {
                Some(id) => door.ledger.answer(&id, wait, run),
                None => within(wait, run).unwrap_or_else(deadline_answer),
            }
        }
        Err((code, out)) => Answer::out(code, out),
    };
    let method = crate::slow::method_of(&argv_seen);
    if crate::slow::first_of_verb(&verb) {
        log_rss(&format!("after-first-{verb}"));
    }
    let measured = if verb == "ops-view" { method.as_str() } else { verb.as_str() };
    let mut answered = Duration::ZERO;
    reply_then_settle(
        &stream,
        &ans,
        || {
            answered = read_at.elapsed();
            crate::slow::request_done(measured, &method, &caller, task.as_deref(), answered);
            crate::slow::sample(&method, &caller, answered);
        },
        &mut |id, reason| crate::reconcile_content::settle_owed(&mut crate::bd::LiveBd, id, reason),
    );
    let began = *started_work.lock().unwrap_or_else(|e| e.into_inner());
    let waited = began.unwrap_or_else(Instant::now).saturating_duration_since(accepted);
    let store = recorder.snapshot();
    let call = spira_otel::Call {
        method: measured,
        caller: &caller,
        request_size: size as u64,
        response_size: (ans.stdout.len() + ans.stderr.len()) as u64,
        exit_code: ans.code,
        start: started,
        end: SystemTime::now(),
        queue_wait: waited,
        answered: Some(answered),
        phases: &store.phases,
        commits_us: &store.commits_us,
        reads: store.reads,
        open_phase: store.open,
    };
    record_span(spans, &call);
}

/// One request: the argv `env::args().skip(1)` would have produced, and the bytes the CLI's
/// stdin would have held, when the client sent any.
#[derive(Debug)]
pub(crate) struct Request {
    pub argv: Vec<String>,
    pub stdin: Option<String>,
    /// The caller's guard variables (`spira_store::lc_call::GUARD_VARS`), which a content
    /// write is judged on as bdq judged it in the caller's own process.
    pub caller: std::collections::BTreeMap<String, String>,
    /// The scheduler task whose effect made the call (`{"task": ...}`); what the store counts it under.
    pub task: Option<String>,
    /// The id the client sent (`{"request_id": ...}`): a write sent twice under it is applied once.
    pub request_id: Option<String>,
}

/// Longest request body read: a readiness publication or a section spec, never a file transfer.
const MAX_BODY: u64 = 16 << 20;

/// The request on `reader`: a JSON argv array on one line, or `{"argv": [...], "len": N}` on
/// one line followed by exactly `N` bytes of stdin; the object may also carry `caller`,
/// `task` and `request_id`. `Ok(None)` when the client sent nothing;
/// `Err((bytes read, refusal))` for a request that cannot be read.
fn read_request(reader: &mut impl BufRead) -> Result<Option<(Request, usize)>, (usize, String)> {
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return Ok(None);
    }
    let bad = |e: &dyn std::fmt::Display| (line.len(), format!("bad request: {e}"));
    let head: serde_json::Value = serde_json::from_str(line.trim()).map_err(|e| bad(&e))?;
    let (argv, len, caller, task, request_id) = match head {
        serde_json::Value::Array(_) => (head, None, Default::default(), None, None),
        serde_json::Value::Object(mut o) => {
            let len = match o.get("len") {
                None if o.contains_key("caller") || o.contains_key("task") || o.contains_key("request_id") => None,
                l => Some(l.and_then(serde_json::Value::as_u64).ok_or_else(|| bad(&"a request object needs a numeric len"))?),
            };
            if let Some(len) = len.filter(|&l| l > MAX_BODY) {
                return Err(bad(&format!("a {len}-byte body is over the {MAX_BODY}-byte bound")));
            }
            let caller = match o.remove("caller") {
                None => Default::default(),
                Some(c) => serde_json::from_value(c).map_err(|e| bad(&format!("caller: {e}")))?,
            };
            let task = match o.remove("task") {
                None => None,
                Some(t) => Some(t.as_str().ok_or_else(|| bad(&"task is a name"))?.to_string()),
            };
            let request_id = match o.remove("request_id") {
                None => None,
                Some(r) => Some(r.as_str().filter(|r| !r.is_empty()).ok_or_else(|| bad(&"request_id is a non-empty string"))?.to_string()),
            };
            (o.remove("argv").unwrap_or_default(), len, caller, task, request_id)
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
    Ok(Some((Request { argv, stdin, caller, task, request_id }, size)))
}

/// The primitives, in this process: a composed verb's reads and events never go back out
/// through the socket.
struct Local;

impl crate::callers::Machine for Local {
    fn call(&mut self, args: &[String]) -> (i32, String) {
        crate::dispatch(args)
    }
}

/// `work` reads its text from a trailing `--stdin <body>` pair that sits before `--actor`
/// (`work::split_reserved`); a request already carrying one is left alone.
fn with_stdin_trailer(mut argv: Vec<String>, body: String) -> Vec<String> {
    let at = if argv.len() >= 2 && argv[argv.len() - 2] == "--actor" { argv.len() - 2 } else { argv.len() };
    if at >= 2 && argv[at - 2] == "--stdin" {
        return argv;
    }
    argv.splice(at..at, ["--stdin".to_string(), body]);
    argv
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
            Some(body) if argv.first().is_some_and(|v| v == "work") => with_stdin_trailer(argv, body),
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

/// The reply goes out first, then `answered` runs, then the owed store write, which is bounded by bd's own cap, not the request's.
fn reply_then_settle(stream: &UnixStream, ans: &Answer, answered: impl FnOnce(), settle: &mut dyn FnMut(&str, &str) -> Result<(), String>) {
    let _ = write_response(stream, ans.code, &ans.stdout, &ans.stderr);
    answered();
    crate::callers::settle_owed_with(&ans.owed, settle);
}

fn record_span(spans: Option<&spira_otel::Sink>, call: &spira_otel::Call) {
    if let Some(sink) = spans {
        sink.emit(call);
    }
}

// batch-job: just under the 310 s the `work` client waits for a tool-running verb.
pub const TOOL_DEADLINE: Duration = Duration::from_secs(305);

/// The request's deadline: the configured default, or the verb's declared slow entry, for every store call. `work`'s lane verbs are
/// batch jobs this service brokers, not calls: they run a harness tool (groomer sweep, bead
/// filing, mail) for the dwarf, and keep the bound they had.
pub(crate) fn request_deadline(argv: &[String], deadlines: &Deadlines) -> Duration {
    match argv.first().map(String::as_str) {
        // batch-job: a work verb that runs a brokered tool waits the client's tool bound less a margin.
        Some("work") if argv.get(2).is_some_and(|v| crate::work::brokers_tool(v)) => TOOL_DEADLINE,
        Some("work") => crate::db::QUERY_DEADLINE,
        // batch-job: bounded under the client's 5 s wait; the verb owns its own shorter budget.
        Some("unpublished") => crate::unpublished::DEADLINE,
        // batch-job: a repair sweep over every row, bounded at the query deadline like census's.
        Some(v) if crate::composed::SWEEPS.contains(&v) => crate::db::QUERY_DEADLINE,
        verb => verb.and_then(|v| deadlines.slow.get(v)).map_or(deadlines.default, |(d, _)| *d),
    }
}

/// Binds the worker thread to the request deadline: the control store refuses a write begun past it.
fn bind_deadline(received: Instant, limit: Duration) -> state_store::phases::DeadlineScope {
    state_store::phases::deadline_scoped(received + limit)
}

/// `f`'s answer, or `None` once `deadline` passes; the abandoned work finishes unobserved.
fn within<T: Send + 'static>(deadline: Duration, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _busy = Busy::enter();
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
    match CURRENT.with(|c| c.borrow().clone()) {
        None => write_raw(stream, exit_code, stdout, stderr),
        Some(slot) => {
            let mut answered = slot.answered.lock().unwrap();
            if *answered {
                return Ok(());
            }
            *answered = true;
            write_raw(stream, exit_code, stdout, stderr)
        }
    }
}

fn write_raw(mut stream: &UnixStream, exit_code: i32, stdout: &str, stderr: &str) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(stdout.len() + stderr.len() + 64);
    let mut head = serde_json::json!({"exit_code": exit_code, "len": stdout.len()});
    if !stderr.is_empty() {
        head["err_len"] = stderr.len().into();
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
        let (code, out) = crate::control::state::run(store, &args, &crate::control::state::Call { now, request: &request, inputs: &crate::control::claimable::FIXED });
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

    #[test]
    fn start_up_logs_one_line_per_phase_in_order_and_ready_after_their_sum() {
        use std::time::Duration;
        let phases = [("config", Duration::from_millis(3)), ("open-sqlite", Duration::from_millis(52)), ("bind", Duration::from_millis(5))];
        let lines = super::startup_lines(&phases, 0);
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(lines[0].ends_with("start-up config 3 ms"), "{}", lines[0]);
        assert!(lines[1].ends_with("start-up open-sqlite 52 ms"), "{}", lines[1]);
        assert!(lines[2].ends_with("start-up bind 5 ms"), "{}", lines[2]);
        assert!(lines[3].ends_with("ready after 60 ms"), "{}", lines[3]);
        assert!(lines[3].starts_with("spira-lc serve: 1970-01-01T00:00:00"), "{}", lines[3]);
    }

    #[test]
    fn a_gc_pass_is_logged_only_for_what_it_collected_or_failed() {
        let idle = state_store::gc::Tick::default();
        assert!(super::gc_lines(&idle, 0).is_empty(), "a pass that did nothing says nothing");
        let busy = state_store::gc::Tick { orphans: vec!["a".into(), "b".into()], ..Default::default() };
        let lines = super::gc_lines(&busy, 0);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].ends_with("control-store gc: 2 orphans, 0 expired"), "{}", lines[0]);
        assert!(!lines[0].contains("scan"), "{}", lines[0]);
        let failed = state_store::gc::Tick {
            failed: vec![state_store::gc::StepFailure { step: "orphan", error: state_store::Error::Backend("x".into()) }],
            ..Default::default()
        };
        let lines = super::gc_lines(&failed, 0);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("control-store gc orphan: "), "{}", lines[0]);
    }

    fn ask(sink: Option<&spira_otel::Sink>, request: &str) -> (String, i32) {
        use std::io::{Read, Write};
        let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        (&client).write_all(request.as_bytes()).unwrap();
        let door = super::Door { ledger: super::Ledger::new(std::time::Duration::from_secs(60)), work: super::live_work(), deadlines: test_deadlines() };
        super::serve_one(server, sink, std::time::Instant::now(), &door);
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
        assert!(dl(&argv("unpublished")) > dl(&argv("show")));
        assert!(dl(&argv("unpublished")) < std::time::Duration::from_secs(5), "the server answers before the 5 s client gives up");
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
        let answered = span.attributes.iter().find(|a| a.key == "spira.answer_us").expect("the answer time is recorded on a served request");
        let us = |a: &opentelemetry_proto::tonic::common::v1::KeyValue| match a.value.as_ref().and_then(|v| v.value.as_ref()) {
            Some(opentelemetry_proto::tonic::common::v1::any_value::Value::IntValue(n)) => *n as u64,
            other => panic!("{other:?}"),
        };
        assert!(us(answered) <= (span.end_time_unix_nano - span.start_time_unix_nano) / 1000, "the answer is written before the span ends");
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

    fn span_of(snapshot: &state_store::phases::Snapshot) -> opentelemetry_proto::tonic::trace::v1::Span {
        let dir = testkit::TempDir::new("lc-serve-phases");
        let sink = spira_otel::Sink::new(dir.path(), "spira-lc", 14);
        let now = std::time::SystemTime::now();
        let call = spira_otel::Call {
            method: "renew",
            caller: "-",
            request_size: 1,
            response_size: 1,
            exit_code: 0,
            start: now,
            end: now,
            queue_wait: std::time::Duration::ZERO,
            answered: None,
            phases: &snapshot.phases,
            commits_us: &snapshot.commits_us,
            reads: snapshot.reads,
            open_phase: snapshot.open,
        };
        super::record_span(Some(&sink), &call);
        let file = std::fs::read_dir(&dir).unwrap().flatten().next().unwrap();
        let text = std::fs::read_to_string(file.path()).unwrap();
        let r: opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest = serde_json::from_str(text.trim()).unwrap();
        r.resource_spans[0].scope_spans[0].spans[0].clone()
    }

    #[test]
    fn a_span_from_a_recorder_with_two_commits_names_both() {
        use state_store::phases::{self, Phase};
        let rec = std::sync::Arc::new(phases::Recorder::default());
        phases::track(rec.clone());
        phases::time(Phase::Commit, || ());
        phases::time(Phase::Commit, || ());
        phases::time(Phase::Read, || ());
        std::mem::forget(phases::begin(Phase::WriterWait));
        phases::untrack();
        let span = span_of(&rec.snapshot());
        let attr = |k: &str| span.attributes.iter().find(|a| a.key == k).and_then(|a| a.value.clone()).and_then(|v| v.value);
        use opentelemetry_proto::tonic::common::v1::any_value::Value;
        assert!(matches!(attr("spira.store.commits"), Some(Value::IntValue(2))));
        assert!(matches!(attr("spira.store.reads"), Some(Value::IntValue(1))));
        assert!(matches!(attr("spira.phase.commits_us"), Some(Value::ArrayValue(a)) if a.values.len() == 2));
        assert!(attr("spira.phase.commit_us").is_some() && attr("spira.phase.read_us").is_some());
        assert!(matches!(attr("spira.phase.open"), Some(Value::StringValue(s)) if s == "writer_wait"));
        assert!(attr("spira.queue_wait_us").is_some());
    }

    #[test]
    fn a_request_cut_at_its_deadline_leaves_the_phase_it_was_in_on_the_recorder() {
        use state_store::phases::{begin, track_scoped, Phase, Recorder};
        use std::{sync::Arc, time::Duration};
        let rec = Arc::new(Recorder::default());
        let worker = Arc::clone(&rec);
        let (release, release_rx) = std::sync::mpsc::channel::<()>();
        let out = super::within(Duration::from_millis(30), move || {
            let _tracking = track_scoped(worker);
            let _open = begin(Phase::WriterWait);
            let _ = release_rx.recv();
        });
        assert!(out.is_none());
        let patience = std::time::Instant::now() + Duration::from_secs(5);
        while rec.snapshot().open.is_none() && std::time::Instant::now() < patience { // clock-safe: caps the wait for the worker to open its phase
            std::thread::yield_now();
        }
        assert_eq!(rec.snapshot().open, Some("writer_wait"));
        drop(release);
    }

    #[test]
    fn a_write_held_past_its_request_deadline_answers_cannot_tell_and_never_commits() {
        use state_store::{contract::{Config, Contract, Ports}, MemBackend, Object, Request, Txn};
        use std::{sync::Arc, time::{Duration, Instant}};
        let c = Arc::new(Contract::open(MemBackend::new(), Ports::virtual_clock(&runtime::ports::VirtualClock::new(0)), Config { min_lock_ttl_ms: 1_000 }).unwrap());
        let worker = Arc::clone(&c);
        let (release, release_rx) = std::sync::mpsc::channel::<()>();
        let (done, done_rx) = std::sync::mpsc::channel();
        let limit = Duration::from_millis(30);
        let received = Instant::now();
        let out = super::within(limit, move || {
            let _deadline = super::bind_deadline(received, limit);
            let _ = release_rx.recv();
            let r = worker.txn(&Txn::new().put("late", Object::new(serde_json::json!(1))), &Request::new("1", "t", "w"));
            let _ = done.send(state_store::wire::answer(&r));
        });
        assert!(out.is_none());
        drop(release);
        let (code, text) = done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!((code, text.as_str()), (2, r#"{"cannot_tell":"deadline"}"#));
        assert!(c.get("late").unwrap().is_none(), "the abandoned write never commits");
    }

    #[test]
    fn the_answer_time_is_taken_before_the_owed_close_settles() {
        let (server, _client) = std::os::unix::net::UnixStream::pair().unwrap();
        let ans = super::Answer { owed: vec![("sp-d".into(), "why".into())].into(), ..Default::default() };
        let (read_at, order) = (std::time::Instant::now(), std::cell::RefCell::new(Vec::new()));
        let mut answered = std::time::Duration::ZERO;
        super::reply_then_settle(
            &server,
            &ans,
            || {
                answered = read_at.elapsed();
                order.borrow_mut().push("answered");
            },
            &mut |_, _| {
                order.borrow_mut().push("settled");
                Ok(())
            },
        );
        assert_eq!(*order.borrow(), ["answered", "settled"]);
        assert!(answered > std::time::Duration::ZERO, "the answer time was taken");
    }

    #[test]
    fn the_reply_is_on_the_stream_before_an_owed_store_close_finishes() {
        use std::io::Read;
        let (server, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
        let (release, release_rx) = std::sync::mpsc::channel::<()>();
        let (started, started_rx) = std::sync::mpsc::channel::<()>();
        let settler = std::thread::spawn(move || {
            let ans = super::Answer { owed: vec![("sp-d".into(), "why".into())].into(), ..Default::default() };
            super::reply_then_settle(&server, &ans, || {}, &mut |id, reason| {
                assert_eq!((id, reason), ("sp-d", "why"));
                let _ = started.send(());
                let _ = release_rx.recv();
                Ok(())
            });
        });
        started_rx.recv().unwrap();
        let mut head = [0u8; 1];
        client.read_exact(&mut head).unwrap();
        assert_eq!(head[0], b'{', "the reply was written while the settle still blocks");
        drop(release);
        settler.join().unwrap();
    }

    #[test]
    fn a_request_that_touched_no_store_carries_no_phase_attribute() {
        let span = span_of(&state_store::phases::Snapshot::default());
        let named: Vec<_> = span.attributes.iter().map(|a| a.key.as_str()).filter(|k| k.starts_with("spira.phase.") || k.starts_with("spira.store.")).collect();
        assert!(named.is_empty(), "{named:?}");
        assert!(span.attributes.iter().any(|a| a.key == "spira.queue_wait_us"));
    }

    /// law-calls-have-deadlines: a store call is abandoned at the configured default or its declared
    /// slow deadline; only the brokered batch verbs keep the longer bound.
    #[test]
    fn a_store_call_is_abandoned_at_the_configured_default_and_only_declared_and_batch_verbs_wait_longer() {
        let d = |v: &str| dl(&[v.to_string(), "x".to_string()]);
        for verb in ["show", "list", "event", "fact", "cut", "land", "stats", ""] {
            assert_eq!(d(verb), std::time::Duration::from_millis(1000), "{verb}");
        }
        assert_eq!(d("ops-view"), std::time::Duration::from_secs(3));
        assert_eq!(d("work"), crate::db::QUERY_DEADLINE);
        let w = |v: &str| dl(&["work".to_string(), "sp-x".to_string(), v.to_string()]);
        for verb in ["show", "note", "done", "blocked", "superseded-by"] {
            assert_eq!(w(verb), crate::db::QUERY_DEADLINE, "{verb}");
        }
        for verb in ["submit", "file-followup", "split"] {
            assert_eq!(w(verb), super::TOOL_DEADLINE, "{verb}");
        }
    }

    #[test]
    fn a_stopping_server_answers_what_is_queued_and_removes_its_path() {
        use super::accept_until_stopped;
        use std::io::BufReader;
        use std::os::unix::net::{UnixListener, UnixStream};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc;
        let dir = testkit::TempDir::new("lc-drain");
        let path = dir.join("sock");
        let path_str = path.to_str().unwrap().to_string();
        let listener = UnixListener::bind(&path).unwrap();
        let stopping = AtomicBool::new(false);
        let mut first = UnixStream::connect(&path).unwrap();
        std::io::Write::write_all(&mut first, b"x\n").unwrap();
        let (began_tx, began) = mpsc::channel::<()>();
        let (release, released) = mpsc::channel::<()>();
        let released = std::sync::Mutex::new(released);
        let began_tx = std::sync::Mutex::new(began_tx);
        let mut queued = None;
        std::thread::scope(|scope| {
            let stopping = &stopping;
            let queued = &mut queued;
            let path = &path;
            scope.spawn(move || {
                began.recv().unwrap();
                stopping.store(true, Ordering::SeqCst);
                let mut late = UnixStream::connect(path).unwrap();
                std::io::Write::write_all(&mut late, b"y\n").unwrap();
                *queued = Some(late);
                release.send(()).unwrap();
            });
            let (released, began_tx) = (&released, &began_tx);
            accept_until_stopped(&listener, &path_str, scope, stopping, move |stream: UnixStream| {
                let _ = began_tx.lock().unwrap().send(());
                let _ = released.lock().unwrap().recv();
                let mut line = String::new();
                std::io::BufRead::read_line(&mut BufReader::new(&stream), &mut line).unwrap();
                std::io::Write::write_all(&mut &stream, b"served\n").unwrap();
            });
        });
        let mut line = String::new();
        std::io::BufRead::read_line(&mut BufReader::new(&first), &mut line).unwrap();
        assert_eq!(line, "served\n", "the request taken before the stop is answered");
        let late = queued.expect("a connection was made after the flag was set");
        let mut line = String::new();
        std::io::BufRead::read_line(&mut BufReader::new(&late), &mut line).unwrap();
        assert_eq!(line, "served\n", "a connection queued at the stop is answered");
        assert_eq!(UnixStream::connect(&path).unwrap_err().kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn the_drain_bound_leaves_the_unit_its_stop_timeout() {
        assert_eq!(super::DRAIN_BOUND, std::time::Duration::from_secs(60));
        for unit in ["lc-serve.service", "spira-lc.service"] {
            let text = std::fs::read_to_string(spira_config::resolve::checkout_root_for_process().unwrap().join("systemd").join(unit)).unwrap();
            let stop: u64 = text.lines().find_map(|l| l.strip_prefix("TimeoutStopSec=")).unwrap().parse().unwrap();
            assert!(std::time::Duration::from_secs(stop) > super::DRAIN_BOUND + std::time::Duration::from_secs(15), "{unit}: TimeoutStopSec={stop}");
        }
    }

    /// The one handler thread of a test is the only user of the world while the scope runs.
    struct Lent<'a>(&'a World);
    unsafe impl Sync for Lent<'_> {}
    unsafe impl Send for Lent<'_> {}

    /// `argv` through the server's real reader, answer and reply on a connection that
    /// `accept_until_stopped` took, with the stop flag set while the handler has the request.
    fn stopped_during(w: &World, argv: &[&str], stop: &std::sync::atomic::AtomicBool) -> (i32, String, String) {
        use std::os::unix::net::UnixListener;
        let dir = testkit::TempDir::new("lc-drain-hold");
        let path = dir.join("sock");
        let listener = UnixListener::bind(&path).unwrap();
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let client = std::os::unix::net::UnixStream::connect(&path).unwrap();
        let asked = std::thread::spawn(move || crate::client::exchange(client, &argv, None));
        let flights = super::Flights::default();
        let lent = Lent(w);
        std::thread::scope(|scope| {
            let (lent, flights) = (&lent, &flights);
            super::accept_until_stopped(&listener, path.to_str().unwrap(), scope, stop, move |stream| {
                let (w, content) = (lent.0, Content::default());
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let (req, _) = super::read_request(&mut reader).unwrap().unwrap();
                stop.store(true, std::sync::atomic::Ordering::SeqCst);
                let (mut m, mut b) = (w, w);
                let ans = super::answer(req, &mut m, &mut b, &content, &|| Err("no run dir here".to_string()));
                flights.serve(stream.try_clone().unwrap(), |_| {
                    super::reply_then_settle(&stream, &ans, || {}, &mut |id, reason| crate::reconcile_content::settle_owed(&mut b, id, reason));
                });
            });
        });
        asked.join().unwrap()
    }

    #[test]
    fn a_hold_taken_before_the_stop_is_answered_and_durable() {
        let w = World::new();
        filed(&w, "sp-a");
        let stop = std::sync::atomic::AtomicBool::new(false);
        let got = stopped_during(&w, &["hold", "sp-a", "wait", "--reason", "x"], &stop);
        assert_eq!(got.0, 0, "{got:?}");
        assert_eq!(over(&w, &Content::default(), &["held", "sp-a", "wait"], None).0, 0);
    }

    #[test]
    fn a_handler_that_never_finishes_is_answered_by_name_at_the_drain_bound() {
        use std::os::unix::net::{UnixListener, UnixStream};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc;
        let dir = testkit::TempDir::new("lc-drain-bound");
        let path = dir.join("sock");
        let listener = UnixListener::bind(&path).unwrap();
        let client = UnixStream::connect(&path).unwrap();
        let asked = std::thread::spawn(move || crate::client::exchange(client, &["held".to_string()], None));
        let stopping: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
        let flights = std::sync::Arc::new(super::Flights::default());
        let (hold, held) = mpsc::channel::<()>();
        let held = std::sync::Arc::new(std::sync::Mutex::new(held));
        let (began_tx, began) = mpsc::channel::<()>();
        let began_tx = std::sync::Mutex::new(began_tx);
        let (done, finished) = mpsc::channel::<()>();
        {
            let (flights, held) = (flights.clone(), held.clone());
            std::thread::spawn(move || {
                std::thread::scope(|scope| {
                    super::accept_until_stopped(&listener, path.to_str().unwrap(), scope, stopping, |stream| {
                        flights.serve(stream, |stream| {
                            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                            super::read_request(&mut reader).unwrap();
                            began_tx.lock().unwrap().send(()).unwrap();
                            let _ = held.lock().unwrap().recv();
                        })
                    })
                });
                let _ = done.send(());
            });
        }
        began.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        stopping.store(true, Ordering::SeqCst);
        let (verdict_tx, verdict) = mpsc::channel();
        let drained = std::thread::spawn(move || {
            let r = super::drain(&flights, stopping, &finished, std::time::Duration::from_millis(200));
            verdict_tx.send(r).unwrap();
        });
        let (n, m) = verdict.recv_timeout(std::time::Duration::from_secs(5)).expect("the drain returns instead of joining a handler that never finishes");
        assert_eq!((n, m), (1, 1));
        let (code, out, _) = asked.join().unwrap();
        assert_eq!(code, 2);
        assert!(out.contains("stopped for a restart"), "{out}");
        hold.send(()).unwrap();
        drained.join().unwrap();
    }

    #[test]
    fn sigterm_sets_the_stop_flag_instead_of_ending_the_process() {
        extern "C" {
            fn raise(sig: i32) -> i32;
        }
        super::stop_on_sigterm();
        assert!(!super::STOPPING.load(std::sync::atomic::Ordering::SeqCst));
        // SAFETY: the handler installed above only stores to an atomic.
        unsafe { raise(15) };
        assert!(super::STOPPING.load(std::sync::atomic::Ordering::SeqCst));
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
        let me = std::fs::metadata(system_paths::PROC_SELF).unwrap();
        assert_eq!(super::peer_uid(&a), Some(std::os::unix::fs::MetadataExt::uid(&me)));
    }

    // ---- the composed verbs, served (sp-ipwlic) -------------------------------------------

    use crate::callers::world::World;
    use crate::callers::Machine;

    /// bd's content half as a double: answers every read with its argv, records every write.
    #[derive(Default)]
    struct Content(std::cell::RefCell<Vec<String>>);

    impl crate::composed::Content for Content {
        fn read(&self, args: &[String]) -> Result<String, crate::composed::ReadError> {
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
            spira_config::repos::Registry::new(Some("spira|r/spira|main|gate.sh\n"), &Default::default(), std::path::Path::new("r/spira"))
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
        super::reply_then_settle(&server, &ans, || {}, &mut |id, reason| crate::reconcile_content::settle_owed(&mut b, id, reason));
        drop(server);
        asked.join().unwrap()
    }

    fn filed(w: &World, id: &str) {
        w.file(id, "task", None, &[]);
        w.ok(&["create-bead", id, "--ready"]);
    }

    /// The store's socket is absent when the client calls, and comes up 300 ms later: the write
    /// waits for it and is applied once.
    #[test]
    fn a_hold_issued_before_the_socket_exists_is_applied_once_when_it_is_bound() {
        let (w, c) = (World::new(), Content::default());
        filed(&w, "sp-a");
        let dir = testkit::TempDir::new("lc-serve-late");
        let path = dir.join("sock");
        let at = path.to_str().unwrap().to_string();
        let asked = std::thread::spawn(move || crate::client::forward_with(&at, &["hold", "sp-a", "wait", "snooze-until:9999999999", "groomer"].map(String::from), None));
        std::thread::sleep(std::time::Duration::from_millis(300)); // clock-safe: the outage the client must wait through
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let (server, _) = listener.accept().unwrap();
        let mut reader = std::io::BufReader::new(server.try_clone().unwrap());
        let (req, _) = super::read_request(&mut reader).unwrap().unwrap();
        let (mut m, mut b) = (&w, &w);
        let ans = super::answer(req, &mut m, &mut b, &c, &|| Err("no run dir here".to_string()));
        super::reply_then_settle(&server, &ans, || {}, &mut |id, reason| crate::reconcile_content::settle_owed(&mut b, id, reason));
        drop(server);
        let (code, _, err) = asked.join().unwrap();
        assert_eq!(code, 0, "{err}");
        assert_eq!(over(&w, &c, &["held", "sp-a", "wait"], None).0, 0);
        let history = over(&w, &c, &["history", "sp-a"], None).1;
        assert_eq!(history.matches("\"event\": \"Hold\"").count(), 1, "{history}");
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
        let line = spira_store::lc_call::request_head(&argv, None, &caller, None, None).unwrap();
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
        let req = super::Request { argv: argv.clone(), stdin: None, caller: acting, task: None, request_id: None };
        let ans = super::answer(req, &mut m, &mut b, &c, &|| Err("no run dir here".to_string()));
        assert_eq!(ans.code, 0, "{ans:?}");
        assert_eq!(*c.0.borrow(), ["update sp-a --add-label x"]);

        let line = spira_store::lc_call::request_head(&argv, Some(2), &caller, None, None).unwrap();
        let mut wire = std::io::Cursor::new(format!("{line}\nhi").into_bytes());
        assert_eq!(super::read_request(&mut wire).unwrap().unwrap().0.stdin.as_deref(), Some("hi"), "a body still rides with its len");
        let mut wire = std::io::Cursor::new(b"{\"argv\":[\"list\"]}\n".to_vec());
        assert!(super::read_request(&mut wire).is_err(), "an object with neither len nor caller is still refused");
    }

    #[test]
    fn a_tasked_request_is_counted_under_its_task() {
        let w = World::new();
        let c = Content::default();
        for (head, task) in [(r#"{"argv":["tasked-probe","sp-a"],"task":"asks"}"#, "asks"), (r#"["tasked-probe","sp-a"]"#, "-")] {
            let mut wire = std::io::Cursor::new(format!("{head}\n").into_bytes());
            let (req, _) = super::read_request(&mut wire).unwrap().unwrap();
            assert_eq!(req.stdin, None, "a task needs no len");
            let task_seen = req.task.clone();
            let (mut m, mut b) = (&w, &w);
            let _ = super::answer(req, &mut m, &mut b, &c, &|| Err("no run dir here".to_string()));
            crate::slow::request_done("tasked-probe", "tasked-probe", "probe", task_seen.as_deref(), std::time::Duration::from_millis(5));
            let s: serde_json::Value = serde_json::from_str(&crate::slow::stats_json()).unwrap();
            let row = s["callers"].as_array().unwrap().iter().find(|r| r["verb"] == "tasked-probe" && r["task"] == task).unwrap_or_else(|| panic!("no row for {task}: {s}"));
            assert_eq!((row["count"].as_u64(), row["total_ms"].as_u64()), (Some(1), Some(5)));
        }
    }

    #[test]
    fn a_min_rev_key_is_ignored_and_the_reply_header_names_no_revision() {
        use std::io::Read;
        let read = |wire: &[u8]| super::read_request(&mut std::io::Cursor::new(wire.to_vec()));
        let shown = |wire: &[u8]| read(wire).map(|r| r.map(|r| r.0.argv)).map_err(|e| e.1);
        let (with, _) = read(b"{\"argv\":[\"show\",\"sp-a\"],\"len\":0,\"min_rev\":42}\n").unwrap().unwrap();
        let (without, _) = read(b"{\"argv\":[\"show\",\"sp-a\"],\"len\":0}\n").unwrap().unwrap();
        assert_eq!(format!("{with:?}"), format!("{without:?}"), "a min_rev is a key read_request does not know");
        assert_eq!(shown(b"{\"argv\":[\"show\"],\"min_rev\":42}\n"), shown(b"{\"argv\":[\"show\"]}\n"));
        assert_eq!(shown(b"{\"argv\":[\"show\"],\"min_rev\":\"x\"}\n"), shown(b"{\"argv\":[\"show\"]}\n"));

        let (a, mut b) = std::os::unix::net::UnixStream::pair().unwrap();
        super::write_response(&a, 0, "{}\n", "").unwrap();
        drop(a);
        let mut got = String::new();
        b.read_to_string(&mut got).unwrap();
        let head: serde_json::Value = serde_json::from_str(got.lines().next().unwrap()).unwrap();
        assert!(head.get("rev").is_none(), "{got}");
    }

    // ---- the request ledger: a write sent twice under one request id is applied once ----------

    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    /// bd's content half as a double that is safe to share with the worker threads: records
    /// each write, and parks a write until released while `held`.
    struct Notes {
        wrote: Mutex<Vec<String>>,
        held: Mutex<bool>,
        released: Condvar,
    }

    impl Notes {
        fn new(held: bool) -> Arc<Notes> {
            Arc::new(Notes { wrote: Mutex::new(Vec::new()), held: Mutex::new(held), released: Condvar::new() })
        }
        fn release(&self) {
            *self.held.lock().unwrap() = false;
            self.released.notify_all();
        }
        fn writes(&self) -> Vec<String> {
            self.wrote.lock().unwrap().clone()
        }
    }

    impl crate::composed::Content for Notes {
        fn read(&self, args: &[String]) -> Result<String, crate::composed::ReadError> {
            Ok(format!("read {}", args.join(" ")))
        }
        fn write(&self, args: &[&str]) -> Result<String, String> {
            self.wrote.lock().unwrap().push(args.join(" "));
            Ok(String::new())
        }
        fn write_stdin(&self, args: &[String], stdin: Option<&str>) -> Result<String, String> {
            let mut held = self.held.lock().unwrap();
            while *held {
                held = self.released.wait(held).unwrap();
            }
            drop(held);
            let body = stdin.map_or(String::new(), |b| format!(" <<{b}"));
            self.wrote.lock().unwrap().push(format!("{}{body}", args.join(" ")));
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
            spira_config::repos::Registry::new(Some("spira|r/spira|main|gate.sh\n"), &Default::default(), std::path::Path::new("r/spira"))
        }
    }

    struct Rig {
        world: Arc<Mutex<World>>,
        notes: Arc<Notes>,
        door: Arc<super::Door>,
    }

    fn rig(held: bool, deadlines: super::Deadlines) -> Rig {
        let world = Arc::new(Mutex::new(World::new()));
        let notes = Notes::new(held);
        let (w, n) = (Arc::clone(&world), Arc::clone(&notes));
        let work: super::Work = Arc::new(move |req| {
            let world = w.lock().unwrap_or_else(|e| e.into_inner());
            let (mut m, mut b) = (&*world, &*world);
            super::answer(req, &mut m, &mut b, &*n, &|| Err("no run dir here".to_string()))
        });
        let door = Arc::new(super::Door { ledger: super::Ledger::new(Duration::from_secs(60)), work, deadlines });
        Rig { world, notes, door }
    }

    fn three_seconds() -> super::Deadlines {
        super::Deadlines { default: Duration::from_secs(3), slow: Default::default() }
    }

    fn test_deadlines() -> super::Deadlines {
        let slow = [("ops-view".to_string(), (Duration::from_secs(3), "sp-x".to_string()))].into();
        super::Deadlines { default: Duration::from_millis(1000), slow }
    }

    fn dl(argv: &[String]) -> Duration {
        super::request_deadline(argv, &test_deadlines())
    }

    fn config(deadline: Option<&str>, slow: &str) -> impl Fn(&str) -> Result<String, String> {
        let (deadline, slow) = (deadline.map(str::to_string), slow.to_string());
        move |k| match k {
            "SPIRA_LC_REQUEST_DEADLINE_MS" => deadline.clone().ok_or_else(|| "missing".to_string()),
            _ => Ok(slow.clone()),
        }
    }

    #[test]
    fn deadlines_come_from_config_and_bad_config_refuses_naming_its_key() {
        let ok = super::deadlines_from(config(Some("1000"), "")).unwrap();
        assert_eq!((ok.default, ok.slow.len()), (Duration::from_millis(1000), 0), "an empty slow list is accepted");
        let ok = super::deadlines_from(config(Some("800"), "list=3000@sp-a, rc=2000@sp-b")).unwrap();
        assert_eq!(ok.slow["rc"], (Duration::from_millis(2000), "sp-b".to_string()));
        for (deadline, slow, names) in [
            (None, "", "spira.lc_request_deadline_ms"),
            (Some("4000"), "", "spira.lc_request_deadline_ms"),
            (Some("1000"), "list=3000", "spira.lc_slow_verb_deadlines"),
            (Some("1000"), "list=fast@sp-a", "spira.lc_slow_verb_deadlines"),
            (Some("1000"), "list=4000@sp-a", "spira.lc_slow_verb_deadlines"),
        ] {
            let err = super::deadlines_from(config(deadline, slow)).unwrap_err();
            assert!(err.contains(names), "{err}");
        }
        let err = super::deadlines_from(config(Some("1000"), "list=fast@sp-a")).unwrap_err();
        assert!(err.contains("list=fast@sp-a"), "the entry is named: {err}");
    }

    /// One request, `request_id` in its head, through `serve_one` over a socket pair.
    fn send(door: &Arc<super::Door>, argv: &[&str], request_id: Option<&str>) -> (i32, String) {
        use std::io::{Read, Write};
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let head = spira_store::lc_call::request_head(&argv, None, &Default::default(), None, request_id).unwrap();
        let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        (&client).write_all(format!("{head}\n").as_bytes()).unwrap();
        let door = Arc::clone(door);
        let served = std::thread::spawn(move || super::serve_one(server, None, std::time::Instant::now(), &door));
        let mut got = String::new();
        (&client).read_to_string(&mut got).unwrap();
        served.join().unwrap();
        let (line, rest) = got.split_once('\n').unwrap();
        let head: serde_json::Value = serde_json::from_str(line).unwrap();
        let len = head["len"].as_u64().unwrap() as usize;
        (head["exit_code"].as_i64().unwrap() as i32, rest[..len].to_string())
    }

    fn history_rows(r: &Rig, id: &str) -> usize {
        let shown: serde_json::Value = serde_json::from_str(&r.world.lock().unwrap().ok(&["history", id, "--machine", "bead"])).unwrap();
        shown.as_array().map(Vec::len).or_else(|| shown["history"].as_array().map(Vec::len)).unwrap_or_else(|| panic!("{shown}"))
    }

    fn drop_argv(r: &Rig, id: &str) -> Vec<String> {
        let w = r.world.lock().unwrap();
        let shown: serde_json::Value = serde_json::from_str(&w.ok(&["show", id])).unwrap();
        let (state, version) = (shown["bead"]["state"].as_str().unwrap().to_string(), shown["bead"]["version"].as_str().unwrap().to_string());
        let kind = serde_json::to_string(&lifecycle::bead::BeadEventKind::Drop { reason: lifecycle::reason::DropReason::Unwanted }).unwrap();
        ["event", "bead", id, "--expect", &state, "--version", &version, "--actor", "test", "--kind", &kind].map(String::from).to_vec()
    }

    #[test]
    fn the_journal_sweep_finishes_on_a_production_sized_store_as_bounded_steps_through_the_door() {
        use crate::control::journal::SWEEP_STEP;
        use crate::journal::counting::Counting;
        use serde_json::{json, Value};
        use state_store::{Config, Contract, MemBackend, Object, Ports, Request, Txn};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let counter = Arc::new(AtomicUsize::new(0));
        let backend = Counting(MemBackend::new(), counter.clone());
        let ports = || Ports::virtual_clock(&runtime::ports::VirtualClock::new(1_000));
        let raw = Contract::open(backend.clone(), ports(), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        let store = Arc::new(crate::control::store::Store::new(Contract::open(backend, ports(), Config { min_lock_ttl_ms: 1_000 }).unwrap(), 1_000));
        let key = |table: &str, id: &str| crate::control::keys::runtime(table, &[id]);
        let mut writes: Vec<(String, Value)> = Vec::new();
        for n in 0..12_000 {
            writes.push((key("schedule", &format!("carried{n:05}")), json!({"at": 0, "stream": "s"})));
        }
        for n in 0..10_000 {
            writes.push((key("resolved", &format!("ghost{n:05}")), json!({})));
        }
        for n in 0..300 {
            writes.push((key("schedule", &format!("old{n}")), json!({"id": format!("old{n}"), "at": 1, "stream": "s", "kind": {"tag": "t", "body": "b"}, "cause": null, "correlation": "c"})));
            writes.push((key("fired", &format!("old{n}")), json!({})));
            let effect = |id: String| json!({"task": "t", "ekey": format!("e{id}"), "request_id": id, "stream": "s", "spec": {}, "requested_at": 1, "deadline_at": 2});
            writes.push((crate::control::keys::runtime("effect", &["t", &format!("edone{n}")]), effect(format!("done{n}"))));
            writes.push((key("resolved", &format!("done{n}")), json!({})));
        }
        writes.push((key("schedule", "live"), json!({"id": "live", "at": 1, "stream": "s", "kind": {"tag": "t", "body": "b"}, "cause": null, "correlation": "c"})));
        writes.push((crate::control::keys::runtime("effect", &["t", "eopen"]), json!({"task": "t", "ekey": "eopen", "request_id": "open", "stream": "s", "spec": {}, "requested_at": 1, "deadline_at": 2})));
        for (n, chunk) in writes.chunks(100).enumerate() {
            let txn = chunk.iter().fold(Txn::new(), |t, (k, v)| t.put(k, Object::new(v.clone())));
            assert!(raw.txn(&txn, &Request::new(&format!("seed-{n}"), "test", "seed")).unwrap().succeeded);
        }
        let work: super::Work = {
            let store = Arc::clone(&store);
            Arc::new(move |req| {
                let (code, out) = crate::journal::answer(crate::journal::run(&req.argv[1..], &*store));
                super::Answer::out(code, out)
            })
        };
        let door = Arc::new(super::Door { ledger: super::Ledger::new(Duration::from_secs(60)), work, deadlines: test_deadlines() });
        let bound = 2 * SWEEP_STEP + 100;
        let sweep = |dry: bool| -> (u64, u64, u64) {
            let mut cursor: Option<String> = None;
            let mut totals = (0, 0, 0);
            loop {
                let mut argv = vec!["journal", "sweep"];
                if dry {
                    argv.push("--dry-run");
                }
                if let Some(c) = &cursor {
                    argv.extend(["--from", c]);
                }
                counter.store(0, Ordering::SeqCst);
                let (code, out) = send(&door, &argv, None);
                assert_eq!(code, 0, "{out}");
                let asked = counter.load(Ordering::SeqCst);
                assert!(asked <= bound, "one step asked the backend for {asked} keys (bound {bound})");
                let v: Value = serde_json::from_str(&out).unwrap();
                totals = (totals.0 + v["idless_schedules"].as_u64().unwrap(), totals.1 + v["indexed_schedules"].as_u64().unwrap(), totals.2 + v["indexed_effects"].as_u64().unwrap());
                match v["next"].as_str() {
                    Some(c) => cursor = Some(c.to_string()),
                    None => return totals,
                }
            }
        };
        let exists = |k: &str| raw.get(k).unwrap().is_some();
        let indexed = crate::control::keys::runtime("journal-meta", &["indexed"]);
        let count = |table: &str| raw.list(&[crate::control::keys::runtime_table(table).as_str()], &state_store::Selector::Everything).unwrap().0.len();
        let tables = || ["schedule", "pending", "open-effect", "resolved", "journal-meta"].map(count);
        let before = tables();
        assert_eq!(sweep(true), (12_000, 1, 1));
        assert!(tables() == before && !exists(&indexed), "a dry run writes nothing");
        assert_eq!(sweep(false), (12_000, 1, 1));
        assert!(exists(&indexed) && !exists(&key("schedule", "carried00000")) && exists(&key("resolved", "ghost00000")));
        assert!(exists(&key("pending", "live")) && exists(&key("open-effect", "open")), "the open rows are indexed");
        for (argv, wanted) in [(["journal", "open-effects"], "open"), (["journal", "due"], "live")] {
            counter.store(0, Ordering::SeqCst);
            let (code, out) = send(&door, &argv, None);
            let rows: Value = serde_json::from_str(&out).unwrap();
            assert_eq!(code, 0, "{out}");
            assert!(rows.as_array().unwrap().len() == 1 && out.contains(wanted), "{argv:?}: {out}");
            let asked = counter.load(Ordering::SeqCst);
            assert!(asked <= 8, "{argv:?} asked the backend for {asked} keys");
        }
    }

    #[test]
    fn an_event_sent_twice_under_one_request_id_advances_the_bead_once() {
        let r = rig(false, three_seconds());
        filed(&r.world.lock().unwrap(), "sp-a");
        let argv = drop_argv(&r, "sp-a");
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let before = history_rows(&r, "sp-a");
        let first = send(&r.door, &argv, Some("req-1"));
        let second = send(&r.door, &argv, Some("req-1"));
        assert_eq!(first.0, 0, "{first:?}");
        assert_eq!(second, first, "the retry gets the first answer");
        assert_eq!(history_rows(&r, "sp-a"), before + 1, "one transition");
    }

    #[test]
    fn a_store_txn_retried_under_the_clients_id_after_a_restart_applies_once() {
        let r = rig(false, three_seconds());
        filed(&r.world.lock().unwrap(), "sp-a");
        let argv = drop_argv(&r, "sp-a");
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let before = history_rows(&r, "sp-a");
        let first = send(&r.door, &argv, Some("req-1"));
        let restarted = Arc::new(super::Door { ledger: super::Ledger::new(Duration::from_secs(60)), work: Arc::clone(&r.door.work), deadlines: three_seconds() });
        let again = send(&restarted, &argv, Some("req-1"));
        assert_eq!(first.0, 0);
        assert_eq!(again.0, 0, "{again:?}");
        assert_eq!(history_rows(&r, "sp-a"), before + 1);
    }

    #[test]
    fn a_content_note_sent_twice_under_one_request_id_writes_one_note() {
        let r = rig(false, three_seconds());
        let argv = ["content", "note", "sp-a", "x"];
        let first = send(&r.door, &argv, Some("req-1"));
        let second = send(&r.door, &argv, Some("req-1"));
        assert_eq!(first.0, 0, "{first:?}");
        assert_eq!(second, first);
        assert_eq!(r.notes.writes(), ["note sp-a x"]);
    }

    #[test]
    fn without_a_request_id_two_identical_notes_are_two_writes() {
        let r = rig(false, three_seconds());
        let argv = ["content", "note", "sp-a", "x"];
        send(&r.door, &argv, None);
        send(&r.door, &argv, None);
        assert_eq!(r.notes.writes().len(), 2);
    }

    #[test]
    fn a_read_sent_twice_under_one_request_id_is_answered_fresh_each_time() {
        let r = rig(false, three_seconds());
        filed(&r.world.lock().unwrap(), "sp-a");
        let argv = drop_argv(&r, "sp-a");
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let before = send(&r.door, &["show", "sp-a"], Some("req-1"));
        assert_eq!(send(&r.door, &argv, None).0, 0);
        let after = send(&r.door, &["show", "sp-a"], Some("req-1"));
        assert_ne!(before.1, after.1, "the second read ran, not the first one's answer");
    }

    #[test]
    fn a_second_arrival_waits_only_to_its_own_deadline_and_the_worker_still_records_its_answer() {
        let ledger = super::Ledger::new(Duration::from_secs(60));
        let (started, started_rx) = std::sync::mpsc::channel::<()>();
        let (release, release_rx) = std::sync::mpsc::channel::<()>();
        let first = {
            let ledger = ledger.clone();
            std::thread::spawn(move || {
                ledger.answer("r", Duration::from_secs(10), move || {
                    started.send(()).unwrap();
                    release_rx.recv().unwrap();
                    crate::callers::Answer::out(0, "real".to_string())
                })
            })
        };
        started_rx.recv().unwrap();
        let waited = ledger.answer("r", Duration::from_millis(30), || panic!("a second arrival never runs the work"));
        assert_eq!((waited.code, waited.stdout.as_str()), (2, "cannot tell: deadline"));
        release.send(()).unwrap();
        assert_eq!(first.join().unwrap().stdout, "real");
        let replay = ledger.answer("r", Duration::from_secs(1), || panic!("a recorded answer is replayed, not run again"));
        assert_eq!(replay.stdout, "real");
    }

    #[test]
    fn an_empty_or_non_string_request_id_is_a_bad_request() {
        for head in [r#"{"argv":["list"],"request_id":""}"#, r#"{"argv":["list"],"request_id":7}"#] {
            let mut reader = std::io::BufReader::new(format!("{head}\n").into_bytes().leak() as &[u8]);
            let (_, msg) = super::read_request(&mut reader).unwrap_err();
            assert!(msg.starts_with("bad request:") && msg.contains("request_id"), "{msg}");
        }
    }

    fn fifty_ms() -> super::Deadlines {
        super::Deadlines { default: Duration::from_millis(50), slow: Default::default() }
    }

    #[test]
    fn a_retry_after_a_deadline_gets_the_workers_real_answer_and_the_note_exists_once() {
        let r = rig(true, fifty_ms());
        let argv = ["content", "note", "sp-a", "x"];
        let first = send(&r.door, &argv, Some("req-1"));
        assert_eq!(first, (2, "cannot tell: deadline".to_string()), "the worker is parked past the deadline");
        let door = Arc::clone(&r.door);
        let retry = std::thread::spawn(move || send(&door, &argv, Some("req-1")));
        r.notes.release();
        let second = retry.join().unwrap();
        assert_eq!(second.0, 0, "{second:?}");
        let third = send(&r.door, &argv, Some("req-1"));
        assert_eq!(third.0, 0, "{third:?}");
        assert_eq!(r.notes.writes(), ["note sp-a x"]);
    }

    #[test]
    fn serve_refuses_to_start_naming_the_ledger_key_when_it_is_absent() {
        let dir = testkit::TempDir::new("lc-ledger-key");
        let toml = spira_config::process::fixture_toml(&dir, &[]);
        let text = std::fs::read_to_string(&toml).unwrap();
        assert!(text.contains("lc_request_ledger_secs"), "the fixture declares the key");
        let kept: String = text.lines().filter(|l| !l.contains("lc_request_ledger_secs")).map(|l| format!("{l}\n")).collect();
        std::fs::write(&toml, kept).unwrap();
        let _env = testkit::env(&[("SPIRA_TOML", Some(toml.to_str().unwrap()))]);
        let fresh = |k: &str| spira_config::process::fresh(&[k.to_string()]).map(|mut m| m.remove(k).unwrap());
        let err = super::ledger_ttl_from(fresh).unwrap_err();
        assert!(err.contains("lc_request_ledger_secs"), "{err}");
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
        fn read(&self, _: &[String]) -> Result<String, crate::composed::ReadError> {
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
            spira_config::repos::Registry::new(Some("spira|r/spira|main|gate.sh\n"), &Default::default(), std::path::Path::new("r/spira"))
        }
        fn policy(&self) -> Result<bead::filing::Policy, String> {
            let builder = bead::filing::Persona { name: "builder".into(), labels: vec!["builder-lane".into()], requires_build_spec: true, unresolved: None };
            Ok(bead::filing::Policy { personas: vec![builder], ..bead::filing::Policy::none() })
        }
    }

    fn served_with(w: &World, c: &dyn crate::composed::Content, argv: &[&str], stdin: &str) -> super::Answer {
        let req = super::Request { argv: argv.iter().map(|a| a.to_string()).collect(), stdin: Some(stdin.to_string()), caller: Default::default(), task: None, request_id: None };
        let (mut m, mut b) = (w, w);
        super::answer(req, &mut m, &mut b, c, &|| Err("no run dir here".to_string()))
    }

    /// A filing as a crate sends it: refused by the server for a missing build spec, written and
    /// given its row with one; a description replacement from the request body is a served verb.
    #[test]
    fn a_work_filing_over_the_door_meets_the_contract_and_is_written_with_its_row() {
        let w = World::new();
        let f = Filing(&w, "sp-new");
        let create = ["content", "create", "T", "--labels", "builder-lane,repo:spira", "--body-file", "-", "--silent"];
        let ans = served_with(&w, &f, &create, "just an outcome");
        assert_eq!(ans.code, 2, "{ans:?}");
        assert!(ans.stderr.starts_with("bead: refusing: a bead for builder must name what to build"), "{ans:?}");
        assert!(state(&w, "sp-new").is_none(), "nothing was filed");
        let ans = served_with(&w, &f, &create, "## Build\nthe artifact, and where\n");
        assert_eq!((ans.code, ans.stdout.as_str()), (0, "sp-new\n"), "{ans:?}");
        assert_eq!(state(&w, "sp-new").map(|r| r.state), Some("READY".to_string()));
        let ans = served_with(&w, &f, &["content", "update", "sp-new", "--body-file", "-"], "a new description");
        assert_eq!(ans.code, 0, "{ans:?}");
    }

    fn served(w: &World, c: &dyn crate::composed::Content, argv: &[&str], caller: &[(&str, &str)]) -> super::Answer {
        let req = super::Request {
            argv: std::iter::once("content").chain(argv.iter().copied()).map(str::to_string).collect(),
            stdin: None,
            caller: caller.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            task: None,
            request_id: None,
        };
        let (mut m, mut b) = (w, w);
        super::answer(req, &mut m, &mut b, c, &|| Err("no run dir here".to_string()))
    }

    fn state(w: &World, id: &str) -> Option<spira_store::lc_state::Row> {
        match w.lc(&["show", id]) {
            (0, out) => spira_store::lc_state::parse_show(&out).unwrap(),
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

    /// `World` behind the store client's transport, so bdq's own after-create step runs in-process.
    struct InProcess(std::sync::Mutex<World>);

    impl spira_store::lc_call::Transport for InProcess {
        fn exchange(&self, argv: &[String], _: Option<&str>, _: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
            let world = self.0.lock().unwrap();
            let mut m = &*world;
            let (code, out) = m.call(argv);
            Ok(spira_store::lc_call::Reply { code, out, err: String::new() })
        }
    }

    #[test]
    fn bdqs_after_create_step_gives_the_row_the_repo_the_create_was_labelled_with() {
        let world = std::sync::Arc::new(InProcess(std::sync::Mutex::new(World::new())));
        let client = spira_store::lc_call::LcClient::over(world.clone(), true);
        let args: Vec<String> = ["create", "T", "--silent", "--labels", "repo:spira"].iter().map(|a| a.to_string()).collect();
        let repo = bead::bdq::created_repo(&args);
        spira_store::lifecycle_row::after_create_ids_on(&client, "bdq", "sp-new\n", repo.as_deref(), &spira_store::lifecycle_row::Retry::once()).unwrap();
        let live = world.0.lock().unwrap().ok(&["list", "--live"]);
        let rows: Vec<serde_json::Value> = serde_json::from_str(&live).unwrap();
        let row = rows.iter().find(|r| r["bead_id"] == "sp-new").expect("the row exists");
        assert_eq!(row["repo"], "spira", "{row}");
    }

    fn listed_row(world: &InProcess, id: &str) -> serde_json::Value {
        let live = world.0.lock().unwrap().ok(&["list", "--live"]);
        let rows: Vec<serde_json::Value> = serde_json::from_str(&live).unwrap();
        rows.into_iter().find(|r| r["bead_id"] == id).unwrap_or_else(|| panic!("no row for {id}: {live}"))
    }

    /// bdq's hooks after a write, as `bdq` runs them: the ids the write changed, then the sync.
    fn bdq_hooks(client: &spira_store::lc_call::LcClient, args: &[&str], stdout: &str) -> Result<(), String> {
        let ids = spira_store::planning::claim_inputs_changed(args, stdout);
        spira_store::lifecycle_row::sync_claim_inputs_on(client, "bdq", &ids, &spira_store::lifecycle_row::Retry::once())
    }

    fn filed_world(labels: &[&str], parent: Option<&str>) -> (std::sync::Arc<InProcess>, spira_store::lc_call::LcClient) {
        let world = std::sync::Arc::new(InProcess(std::sync::Mutex::new(World::new())));
        world.0.lock().unwrap().file("sp-new", "task", parent, labels);
        let client = spira_store::lc_call::LcClient::over(world.clone(), true);
        spira_store::lifecycle_row::after_create_ids_on(&client, "bdq", "sp-new\n", None, &spira_store::lifecycle_row::Retry::once()).unwrap();
        (world, client)
    }

    #[test]
    fn a_filed_beads_parent_and_labels_are_on_its_record_once_bdqs_hooks_have_run() {
        // literal-ok: test fixture label
        let (world, client) = filed_world(&["repo:spira", "no-loop"], Some("sp-e"));
        assert_eq!(listed_row(&world, "sp-new")["claim"], serde_json::Value::Null, "nothing has synced it yet");
        // literal-ok: test fixture label
        bdq_hooks(&client, &["create", "T", "--silent", "--parent", "sp-e", "--labels", "repo:spira,no-loop"], "sp-new\n").unwrap();
        // literal-ok: test fixture label
        assert_eq!(listed_row(&world, "sp-new")["claim"], serde_json::json!({"blocks": [], "parent": "sp-e", "labels": ["repo:spira", "no-loop"]}));
    }

    #[test]
    fn a_dep_add_then_a_dep_remove_move_the_records_blocks_and_a_related_edge_does_not() {
        let (world, client) = filed_world(&[], None);
        let blocks = || listed_row(&world, "sp-new")["claim"]["blocks"].clone();
        let edge = |to: &str, kind: &str| world.0.lock().unwrap().plan.borrow_mut().get_mut("sp-new").unwrap().dependencies.push((to.into(), kind.into()));
        edge("sp-r", "related");
        bdq_hooks(&client, &["dep", "relate", "sp-new", "sp-r"], "").unwrap();
        assert_eq!(blocks(), serde_json::json!([]));
        edge("sp-b", "blocks");
        bdq_hooks(&client, &["dep", "add", "sp-new", "sp-b"], "").unwrap();
        assert_eq!(blocks(), serde_json::json!(["sp-b"]));
        world.0.lock().unwrap().plan.borrow_mut().get_mut("sp-new").unwrap().dependencies.retain(|(to, _)| to != "sp-b");
        bdq_hooks(&client, &["dep", "remove", "sp-new", "sp-b"], "").unwrap();
        assert_eq!(blocks(), serde_json::json!([]));
    }

    #[test]
    fn a_label_added_through_the_content_door_is_on_the_record_in_the_same_request() {
        struct Labelling<'w>(&'w World);
        impl crate::composed::Content for Labelling<'_> {
            fn read(&self, _: &[String]) -> Result<String, crate::composed::ReadError> {
                Ok(String::new())
            }
            fn write(&self, _: &[&str]) -> Result<String, String> {
                Ok(String::new())
            }
            fn write_stdin(&self, args: &[String], _: Option<&str>) -> Result<String, String> {
                if let ["label", "add", id, label] = args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
                    self.0.plan.borrow_mut().get_mut(*id).unwrap().labels.push(label.to_string());
                }
                Ok(String::new())
            }
            fn delete(&self, _: &[String]) -> Result<(), String> {
                Ok(())
            }
            fn ask_label(&self) -> Result<String, String> {
                Ok(String::new())
            }
            fn registry(&self) -> spira_config::repos::Registry {
                spira_config::repos::Registry::new(Some("spira|r/spira|main|gate.sh\n"), &Default::default(), std::path::Path::new("r/spira"))
            }
        }
        let w = World::new();
        filed(&w, "sp-a");
        let ans = served(&w, &Labelling(&w), &["label", "add", "sp-a", "incident"], &[]);
        assert_eq!((ans.code, ans.stderr.as_str()), (0, ""), "{ans:?}");
        let row: serde_json::Value = serde_json::from_str::<Vec<serde_json::Value>>(&w.ok(&["list", "--live"])).unwrap().into_iter().find(|r| r["bead_id"] == "sp-a").unwrap();
        assert_eq!(row["claim"]["labels"], serde_json::json!(["incident"]), "{row}");
    }

    #[test]
    fn set_carrier_names_the_integration_on_each_member_and_clear_takes_it_off_and_a_member_without_a_record_is_named() {
        let w = World::new();
        filed(&w, "sp-a");
        filed(&w, "sp-b");
        let carried = |id: &str| -> serde_json::Value {
            serde_json::from_str::<Vec<serde_json::Value>>(&w.ok(&["list", "--live"])).unwrap().into_iter().find(|r| r["bead_id"] == id).unwrap()["carried_by"].clone()
        };
        assert_eq!(carried("sp-a"), serde_json::Value::Null);
        w.ok(&["set-carrier", "sp-a", "sp-b", "--by", "sp-int"]);
        assert_eq!((carried("sp-a"), carried("sp-b")), ("sp-int".into(), "sp-int".into()));
        let (code, out) = w.lc(&["set-carrier", "sp-a", "sp-ghost", "sp-b", "--clear"]);
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("sp-ghost") && !out.contains("sp-a"), "{out}");
        assert_eq!((carried("sp-a"), carried("sp-b")), (serde_json::Value::Null, serde_json::Value::Null), "the rest are written");
    }

    #[test]
    fn set_carrier_only_if_unset_keeps_a_member_already_carried_and_sets_one_that_is_not() {
        let world = std::sync::Arc::new(InProcess(std::sync::Mutex::new(World::new())));
        {
            let w = world.0.lock().unwrap();
            filed(&w, "sp-a");
            filed(&w, "sp-b");
            w.ok(&["set-carrier", "sp-a", "--by", "sp-i1"]);
        }
        let client = spira_store::lc_call::LcClient::over(world.clone(), true);
        let reply = client.call(&["set-carrier", "sp-a", "sp-b", "--only-if-unset", "--by", "sp-i2"]).unwrap();
        assert_eq!(reply.code, 0, "{reply:?}");
        assert!(reply.out.contains("kept sp-a") && !reply.out.contains("kept sp-b"), "{reply:?}");
        assert_eq!(listed_row(&world, "sp-a")["carried_by"], "sp-i1");
        assert_eq!(listed_row(&world, "sp-b")["carried_by"], "sp-i2");
    }

    #[test]
    fn a_claim_input_sync_that_fails_after_the_content_write_is_reported_with_its_repair_and_the_write_stands() {
        let w = World::new();
        filed(&w, "sp-a");
        w.plan.borrow_mut().remove("sp-a");
        let ans = served(&w, &Filing(&w, "sp-a"), &["label", "add", "sp-a", "x"], &[]);
        assert_eq!(ans.code, 0, "{ans:?}");
        assert!(ans.stderr.contains("claim inputs not written; repair: spira-lc sync-claim-inputs sp-a"), "{ans:?}");
    }

    fn create_bead_served(w: &World) -> super::Answer {
        let req = super::Request { argv: ["create-bead", "sp-new", "--repo", "spira", "--ready"].map(String::from).to_vec(), stdin: None, caller: Default::default(), task: None, request_id: None };
        let (mut m, mut b) = (w, w);
        super::answer(req, &mut m, &mut b, &Content::default(), &|| Err("no run dir here".to_string()))
    }

    #[test]
    fn a_row_created_over_the_socket_carries_its_claim_inputs_from_the_content_store() {
        let w = World::new();
        w.file("sp-new", "task", Some("sp-e"), &["incident"]);
        let ans = create_bead_served(&w);
        assert_eq!((ans.code, ans.stderr.as_str()), (0, ""), "{ans:?}");
        let row = {
            let rows: Vec<serde_json::Value> = serde_json::from_str(&w.ok(&["list", "--live"])).unwrap();
            rows.into_iter().find(|r| r["bead_id"] == "sp-new").unwrap()
        };
        assert_eq!(row["claim"], serde_json::json!({"blocks": [], "parent": "sp-e", "labels": ["incident"]}));
        assert_ne!(row["claim_view"]["verdict"], "cannot-tell", "{row}");
    }

    #[test]
    fn a_create_whose_claim_sync_fails_stands_and_names_the_repair() {
        let w = World::new();
        w.file("sp-new", "task", Some("sp-e"), &["incident"]);
        w.plan.borrow_mut().remove("sp-new");
        let ans = create_bead_served(&w);
        assert_eq!(ans.code, 0, "{ans:?}");
        assert!(state(&w, "sp-new").is_some(), "the row stands");
        assert!(ans.stderr.contains("repair: spira-lc sync-claim-inputs sp-new"), "{ans:?}");
    }

    #[test]
    fn sync_claim_inputs_only_if_unset_keeps_a_written_claim_and_fills_a_null_one() {
        let (world, client) = filed_world(&["incident"], Some("sp-e"));
        {
            let w = world.0.lock().unwrap();
            w.file("sp-set", "task", None, &["other"]);
            w.ok(&["create-bead", "sp-set", "--ready"]);
            w.ok(&["set-claim-inputs", "sp-set", "--block", "sp-b"]);
        }
        let reply = client.call(&["sync-claim-inputs", "--only-if-unset", "sp-set", "sp-new"]).unwrap();
        assert_eq!(reply.code, 0, "{}", reply.text());
        assert!(reply.text().contains("kept sp-set: already written") && !reply.text().contains("kept sp-new"), "{}", reply.text());
        assert_eq!(listed_row(&world, "sp-set")["claim"], serde_json::json!({"blocks": ["sp-b"], "parent": null, "labels": []}));
        assert_eq!(listed_row(&world, "sp-new")["claim"], serde_json::json!({"blocks": [], "parent": "sp-e", "labels": ["incident"]}));
    }

    /// The served path's machine: the primitives only, refusing a composed verb as `Local` does.
    /// `stale` names records whose `list` row still reads `claim: null`, as when another writer's
    /// claim lands between the sweep's list and its write.
    struct Primitives<'w> {
        world: &'w World,
        stale: &'w [&'w str],
    }

    impl crate::callers::Machine for Primitives<'_> {
        fn call(&mut self, args: &[String]) -> (i32, String) {
            if args.first().is_some_and(|v| crate::composed::is_served(v)) {
                return (2, "usage: spira-lc show <bead-id> | show-batch ...".to_string());
            }
            let mut w = self.world;
            let (code, out) = w.call(args);
            if args.first().map(String::as_str) != Some("list") || self.stale.is_empty() {
                return (code, out);
            }
            let mut rows: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
            rows.iter_mut().filter(|r| self.stale.contains(&r["bead_id"].as_str().unwrap_or(""))).for_each(|r| r["claim"] = serde_json::Value::Null);
            (code, serde_json::Value::Array(rows).to_string())
        }
    }

    #[test]
    fn a_served_backfill_sets_a_null_claim_and_keeps_a_written_one() {
        let w = World::new();
        w.file("sp-new", "task", Some("sp-e"), &["incident"]);
        w.ok(&["create-bead", "sp-new", "--ready"]);
        w.file("sp-set", "task", None, &["other"]);
        w.ok(&["create-bead", "sp-set", "--ready"]);
        w.ok(&["set-claim-inputs", "sp-set", "--block", "sp-b"]);
        let req = super::Request {
            argv: ["backfill-claim-inputs", "--apply", "sp-new", "sp-set"].iter().map(|a| a.to_string()).collect(),
            stdin: None,
            caller: Default::default(),
            task: None,
            request_id: None,
        };
        let ans = super::answer(req, &mut Primitives { world: &w, stale: &["sp-set"] }, &mut &w, &Content::default(), &|| Err("no run dir here".to_string()));
        assert_eq!(ans.code, 0, "{}{}", ans.stdout, ans.stderr);
        assert!(ans.stdout.contains("1 set, 1 kept, 0 failed"), "{}", ans.stdout);
        let rows: Vec<serde_json::Value> = serde_json::from_str(&w.ok(&["list", "--live"])).unwrap();
        let claim = |id: &str| rows.iter().find(|r| r["bead_id"] == id).unwrap()["claim"].clone();
        assert_eq!(claim("sp-new"), serde_json::json!({"blocks": [], "parent": "sp-e", "labels": ["incident"]}));
        assert_eq!(claim("sp-set"), serde_json::json!({"blocks": ["sp-b"], "parent": null, "labels": []}));
    }

    #[test]
    fn syncing_an_id_with_no_record_names_it_and_makes_none() {
        let world = std::sync::Arc::new(InProcess(std::sync::Mutex::new(World::new())));
        world.0.lock().unwrap().file("sp-ghost", "task", None, &[]);
        let client = spira_store::lc_call::LcClient::over(world.clone(), true);
        let err = spira_store::lifecycle_row::sync_claim_inputs_on(&client, "bdq", &["sp-ghost".to_string()], &spira_store::lifecycle_row::Retry::once()).unwrap_err();
        assert!(err.contains("sp-ghost"), "{err}");
        assert_eq!(world.0.lock().unwrap().ok(&["list", "--live"]).trim(), "[]");
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
            (&["dep", "add", "sp-a"], "`dep add sp-a` is not a content verb"),
            (&["close", "sp-a", "--reason", "r", "--force"], "`close sp-a --reason r --force` is not a content verb"),
            (&["update", "sp-a", "--status", "open"], "update takes only --add-label/--remove-label <label>, --set-metadata <key=value>, --priority 0-4 and --force-claimed <reason> with a body, not `--status`"),
            (&["create", "T", "--body-file", system_paths::ETC_PASSWD], "create takes its body only as --body-file - (the request body)"),
        ] {
            let ((code, out, err), wrote) = content_write(argv, None);
            assert_eq!((code, out.as_str(), err), (3, "", format!("spira-lc content: {why}\n")), "{argv:?}");
            assert!(wrote.is_empty(), "{argv:?} wrote {wrote:?}");
        }
    }

    #[test]
    fn content_writes_keep_the_configured_default_under_the_clients_five() {
        for argv in [&["label", "add", "sp-a", "x"][..], &["close", "sp-a"], &["note", "sp-a", "--stdin"], &["update", "sp-a", "--set-metadata", "k=v"], &["create", "T"]] {
            let full: Vec<String> = std::iter::once("content").chain(argv.iter().copied()).map(str::to_string).collect();
            assert_eq!(dl(&full), test_deadlines().default, "{argv:?}");
            assert!(dl(&full) < crate::client::TIMEOUT, "{argv:?}");
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
        let req = super::Request { argv: vec!["batch-progress".into()], stdin: Some("[{\"bead_id\":\"sp-a\"}]".into()), caller: Default::default(), task: None, request_id: None };
        super::answer(req, &mut rec, &mut bd, &Content::default(), &|| Err(String::new()));
        assert_eq!(rec.0, vec![vec!["batch-progress".to_string(), "--json".into(), "[{\"bead_id\":\"sp-a\"}]".into()]]);
    }

    #[test]
    fn a_work_requests_body_becomes_its_stdin_trailer_before_actor() {
        struct Rec(Vec<Vec<String>>);
        impl Machine for Rec {
            fn call(&mut self, args: &[String]) -> (i32, String) {
                self.0.push(args.to_vec());
                (0, String::new())
            }
        }
        let w = World::new();
        let (mut rec, mut bd) = (Rec(Vec::new()), &w);
        let a = |v: &[&str], body: Option<&str>| super::Request {
            argv: v.iter().map(|s| s.to_string()).collect(),
            stdin: body.map(String::from),
            caller: Default::default(),
            task: None,
            request_id: None,
        };
        let mut run = |req| {
            super::answer(req, &mut rec, &mut bd, &Content::default(), &|| Err(String::new()));
        };
        run(a(&["work", "sp-a", "note", "-", "--actor", "sift"], Some("evidence\nline two")));
        run(a(&["work", "sp-a", "note", "-"], Some("evidence\nline two")));
        run(a(&["work", "sp-a", "note", "-"], None));
        run(a(&["work", "-", "file", "t", "--body-file", "-", "--actor", "ops"], Some("b")));
        run(a(&["work", "sp-a", "note", "-", "--stdin", "x"], Some("y")));
        let sv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(rec.0[0], sv(&["work", "sp-a", "note", "-", "--stdin", "evidence\nline two", "--actor", "sift"]));
        let c = crate::work::split_reserved(&rec.0[0][2..]);
        assert_eq!((c.stdin.as_deref(), c.actor.as_deref()), (Some("evidence\nline two"), Some("sift")));
        assert_eq!(rec.0[1], sv(&["work", "sp-a", "note", "-", "--stdin", "evidence\nline two"]));
        assert_eq!(rec.0[2], sv(&["work", "sp-a", "note", "-"]));
        assert_eq!(rec.0[3], sv(&["work", "-", "file", "t", "--body-file", "-", "--stdin", "b", "--actor", "ops"]));
        assert_eq!(rec.0[4], sv(&["work", "sp-a", "note", "-", "--stdin", "x"]));
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
    fn a_composed_verb_keeps_the_configured_default_and_only_its_sweeps_are_batch_jobs() {
        let d = |v: &str| dl(&[v.to_string(), "x".to_string()]);
        for verb in ["close", "reopen", "reconcile-closed", "close-ask", "hold", "unhold", "drop", "disposition", "held", "reply", "content"] {
            assert_eq!(d(verb), std::time::Duration::from_millis(1000), "{verb}");
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
