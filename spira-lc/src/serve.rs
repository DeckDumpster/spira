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
//! stdin the CLI would have read — then one `{"exit_code", "len"[, "err_len"][, "read_ended"]}` line out,
//! followed by the stdout bytes and any stderr bytes. `read_ended: true` marks a non-zero answer
//! whose request had a store read ended at the read limit; an absent field is false. The composed verbs (composed.rs) are
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
/// Revisions of `events/` one sweep tick folds into the edge tally.
const EDGES_REVS_PER_TICK: usize = 1_000;
const REFUSALS_REVS_PER_TICK: usize = 1_000;
/// Revisions of `events/` one sweep tick folds into the landed list.
const LANDED_REVS_PER_TICK: usize = 1_000;
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
/// The receiver stops 3 s after receiving (law-calls-have-deadlines); no configured deadline may exceed it.
/// `spira/testlib/complete.toml` carries it for every test-started serve; `a_test_started_serve_runs_under_the_ceiling_the_complete_fixture_carries` pins the two together.
pub(crate) const MAX_CONFIGURED_MS: u64 = 3000;

/// The deadlines a store call is served under, pinned at serve start.
#[derive(Clone, Debug)]
pub(crate) struct Deadlines {
    pub(crate) default: Duration,
}

pub(crate) fn deadlines_from(read: impl Fn(&str) -> Result<String, String>) -> Result<Deadlines, String> {
    let default = read(DEADLINE_KEY).map_err(|e| format!("spira.lc_request_deadline_ms: {e}"))?;
    let default: u64 = spira_config::process::parse_declared(DEADLINE_KEY, &default).map_err(|e| format!("spira.lc_request_deadline_ms: {e}"))?;
    if default == 0 || default > MAX_CONFIGURED_MS {
        return Err(format!("spira.lc_request_deadline_ms: {default} is outside 1..=3000 ms"));
    }
    Ok(Deadlines { default: Duration::from_millis(default) })
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

pub(crate) fn deadline_answer() -> Answer {
    Answer::out(2, spira_store::lc_call::DEADLINE_ANSWER.to_string())
}

impl Ledger {
    pub(crate) fn new(ttl: Duration) -> Ledger {
        Ledger(Arc::new(LedgerState { ttl, slots: Default::default(), changed: Default::default() }))
    }

    #[cfg(test)]
    pub(crate) fn await_done(&self, id: &str, limit: Duration) -> bool {
        let end = Instant::now() + limit;
        let mut slots = self.0.slots.lock().unwrap_or_else(|e| e.into_inner());
        while !matches!(slots.get(id), Some(Entry::Done(..))) {
            let Some(left) = end.checked_duration_since(Instant::now()) else {
                return false;
            };
            slots = self.0.changed.wait_timeout(slots, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        true
    }

    /// The answer to the request `id`: the recorded one, the in-flight one's once it ends, or
    /// `work`'s own, run at most once however many sends arrive. No answer within `wait` is a
    /// `None` (the caller makes the deadline answer), and a worker still running keeps its slot until it has a real one.
    fn answer_in_time(&self, id: &str, wait: Duration, work: impl FnOnce() -> Answer + Send + 'static) -> Option<Answer> {
        let end = Instant::now() + wait;
        let mut slots = self.0.slots.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let ttl = self.0.ttl;
            slots.retain(|_, s| !matches!(s, Entry::Done(_, at) if at.elapsed() >= ttl));
            match slots.get(id) {
                Some(Entry::Done(ans, _)) => return Some(Answer { owed: Box::new([]), cert_log: None, ..ans.clone() }),
                Some(Entry::InFlight) => {
                    let left = end.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return None;
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
    }
}

pub(crate) type Work = Arc<dyn Fn(Request) -> Answer + Send + Sync>;

/// What a request is served with: the ledger, the work that answers it and the deadline it keeps.
pub(crate) struct Door {
    pub(crate) ledger: Ledger,
    pub(crate) work: Work,
    pub(crate) deadlines: Deadlines,
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

/// `args` without `--loop-socket <path>`, and the path.
fn split_loop_socket(args: &[String]) -> Result<(Vec<String>, Option<String>), String> {
    let mut rest = Vec::new();
    let mut path = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a != "--loop-socket" {
            rest.push(a.clone());
            continue;
        }
        match it.next() {
            Some(p) if path.is_none() && !p.is_empty() => path = Some(p.clone()),
            _ => return Err("--loop-socket takes one path, once".to_string()),
        }
    }
    Ok((rest, path))
}

pub fn run(args: &[String]) -> i32 {
    let (args, loop_socket) = match split_loop_socket(args) {
        Ok(split) => split,
        Err(e) => {
            eprintln!("spira-lc serve: {e}");
            return 2;
        }
    };
    let args = &args[..];
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
    let spans = spira_config::process::cfg("SPIRA_RUN").and_then(|run| {
        let keep = spira_config::process::cfg_parse::<u32>("SPIRA_OTEL_KEEP_DAYS")?;
        let day_max_mb = spira_config::process::cfg_parse::<u32>("SPIRA_OTEL_DAY_MAX_MB")?;
        Ok((run, keep, day_max_mb))
    });
    match spans {
        Ok((run, keep, day_max_mb)) => {
            let sink = spira_otel::Sink::buffered(spira_otel::dir_of(std::path::Path::new(&run)), "spira-lc", keep, u64::from(day_max_mb) << 20);
            let _ = SPANS.set(sink);
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
    record_writer_grants();
    startup.mark("writer-grants");
    // A request may spawn a child (`work blocked` runs `mail`) that calls back into this
    // socket; serving one connection at a time deadlocks on that.
    if crate::control::served::configured().is_none() {
        eprintln!("spira-lc serve: SPIRA_LC_STORE_PATH is unset: every lifecycle verb answers cannot-tell");
    }
    {
        if let Some(mut shipping) = shipping {
            let snapshotter = shipping.snapshotter.clone();
            let every = shipping.every;
            std::thread::spawn(move || loop {
                if let Err(e) = snapshotter.step() {
                    eprintln!("spira-lc serve: snapshot: {e}");
                }
                std::thread::sleep(every);
            });
            std::thread::spawn(move || {
                let endpoint = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok();
                let metrics = spira_otel::metrics::MetricsSink::new(endpoint.as_deref(), "spira-lc", Duration::from_millis(250));
                loop {
                    std::thread::sleep(shipping.every);
                    crate::durability::tick(&mut shipping, &metrics);
                }
            });
        }
        std::thread::spawn(|| {
            let endpoint = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok();
            let metrics = spira_otel::metrics::MetricsSink::new(endpoint.as_deref(), "spira-lc", Duration::from_millis(250));
            let mut pushed = Default::default();
            let mut pushed_reads = Default::default();
            let mut pushed_followups = Default::default();
            let mut pushed_children = 0;
            loop {
                std::thread::sleep(crate::slow::METRICS_EVERY);
                crate::slow::push_counts(&metrics, &mut pushed);
                crate::slow::push_reads(&metrics, &mut pushed_reads);
                crate::slow::push_gate_waits(&metrics);
                crate::slow::push_children_started(&metrics, &mut pushed_children);
                crate::followups::push(&metrics, &mut pushed_followups);
                if let Some(Ok(store)) = crate::control::served::configured() {
                    match crate::store_gauges::file_series(store.contract().backend(), SystemTime::now()) {
                        Ok(series) => crate::slow::hold_gauges(series),
                        Err(e) => eprintln!("spira-lc serve: store file gauges: {e}"),
                    }
                }
                crate::slow::push_held(&metrics);
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
        std::thread::spawn(|| {
            let mut walk = crate::store_gauges::Walk::default();
            loop {
                std::thread::sleep(SWEEP_EVERY);
                let Some(Ok(store)) = crate::control::served::configured() else { continue };
                let _holder = state_store::phases::holder_scoped("keyspace-walk", "");
                let limit = crate::control::verbs::USAGE_DEFAULT_LIMIT as usize;
                match walk.step(store.contract().backend(), limit, Instant::now()) {
                    Ok(Some(pass)) => crate::slow::replace_gauges(crate::store_gauges::PASS_GAUGES, pass.series(SystemTime::now())),
                    Ok(None) => {}
                    Err(e) => eprintln!("spira-lc serve: keyspace walk: {e}"),
                }
            }
        });
        std::thread::spawn(crate::plan::run_mirror);
        std::thread::spawn(crate::content_write::run_committer);
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
                    let _holder = state_store::phases::holder_scoped("refusal-tally", "");
                    if let Err(e) = store.fold_refusals(crate::db::now_epoch(), REFUSALS_REVS_PER_TICK) {
                        eprintln!("spira-lc serve: refusal tally: {e}");
                    }
                    let _holder = state_store::phases::holder_scoped("landed-list", "");
                    if let Err(e) = store.fold_landed(crate::db::now_epoch(), LANDED_REVS_PER_TICK) {
                        eprintln!("spira-lc serve: landed list: {e}");
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
    let loop_thread = match loop_socket.as_deref().map(start_loop) {
        None => None,
        Some(Ok(t)) => Some(t),
        Some(Err(e)) => {
            eprintln!("spira-lc serve: {e}");
            return 2;
        }
    };
    let flights = Arc::new(Flights::default());
    let (done, finished) = std::sync::mpsc::channel::<()>();
    let (running, accepting) = std::sync::mpsc::channel::<()>();
    {
        let flights = flights.clone();
        let spawned = std::thread::Builder::new().name("lc-accept".into()).spawn(move || {
            let _ = running.send(());
            std::thread::scope(|scope| {
                accept_until_stopped(&listener, &socket_path, scope, &STOPPING, |stream| flights.serve(stream, |stream| handle(stream, Instant::now())))
            });
            let _ = done.send(());
        });
        if let Err(e) = spawned {
            eprintln!("spira-lc serve: cannot start the accept thread: {e}");
            return 2;
        }
    }
    let _ = accepting.recv();
    startup.ready(SPANS.get());
    if let Err(e) = notify_ready(std::env::var("NOTIFY_SOCKET").ok().as_deref()) {
        eprintln!("spira-lc serve: {e}");
    }
    let (drained, answered) = drain(&flights, &STOPPING, &finished, DRAIN_BOUND);
    if let Some(spans) = SPANS.get() {
        spans.flush();
    }
    eprintln!("spira-lc serve: stopped after draining {drained}, answered {answered} at the bound");
    if let Some(t) = loop_thread {
        match t.join() {
            Ok(cut) => eprintln!("spira-lc serve: the loop stopped, answered {cut} at the bound"),
            Err(_) => eprintln!("spira-lc serve: the loop thread panicked"),
        }
    }
    0
}

/// Binds the loop socket and starts the `lc-loop` thread, returning once it runs; the thread
/// ends, with the connections it cut at the drain bound, after SIGTERM.
fn start_loop(path: &str) -> Result<std::thread::JoinHandle<usize>, String> {
    let listener = bind_socket(path).map_err(|e| format!("cannot bind {path}: {e}"))?;
    listener.set_nonblocking(true).map_err(|e| format!("cannot bind {path}: {e}"))?;
    let door = Door {
        ledger: LEDGER.get().expect("the ledger is set before the socket is bound").clone(),
        work: live_work(),
        deadlines: DEADLINES.get().expect("the deadlines are set before the socket is bound").clone(),
    };
    let pool = runtime::offload::Pool::new(crate::control::served::READERS, runtime::offload::QUEUE_DEPTH);
    let shared = Arc::new(crate::serve_loop::Shared::new(Arc::new(door), pool, SPANS.get()));
    let (running, started) = mpsc::channel::<()>();
    let shown = path.to_string();
    let path = shown.clone();
    let thread = std::thread::Builder::new()
        .name("lc-loop".into())
        .spawn(move || {
            let _ = running.send(());
            let gone = path.clone();
            run_loop(listener, shared, &STOPPING, DRAIN_BOUND, move || {
                if let Err(e) = std::fs::remove_file(&gone) {
                    eprintln!("spira-lc serve: cannot remove {gone}: {e}");
                }
            })
        })
        .map_err(|e| format!("cannot start the loop thread: {e}"))?;
    let _ = started.recv();
    eprintln!("spira-lc serve: loop listening on {shown}");
    Ok(thread)
}

/// The loop server on this thread, current-thread tokio, until `stopping` and its drain.
pub(crate) fn run_loop(listener: UnixListener, shared: Arc<crate::serve_loop::Shared>, stopping: &AtomicBool, drain_bound: Duration, on_stop: impl FnOnce()) -> usize {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a current-thread runtime builds");
    runtime.block_on(async {
        let listener = tokio::net::UnixListener::from_std(listener).expect("the loop listener registers");
        crate::serve_loop::serve(listener, shared, stopping, drain_bound, on_stop).await
    })
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
            before_work: Duration::ZERO,
            answer_wait: Duration::ZERO,
            after_answer: Duration::ZERO,
            cut: false,
            answered: None,
            cut_at: None,
            phases: &us,
            commits_us: &[],
            reads: 0,
            read_max_us: 0,
            reads_ended: 0,
            reads_overran: 0,
            open_phase: None,
            content_queries: 0,
            content_query_max_us: 0,
            content_query_max_shape: "",
            task: None,
            children: 0,
            programs: &[],
            subverb: None,
        });
    }
}

/// Leaves 30 s of the unit's 90 s `TimeoutStopSec` to write the refusals and exit.
pub const DRAIN_BOUND: Duration = Duration::from_secs(60);

pub(crate) fn stopped_for_restart() -> String {
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
    fn serve(&self, stream: UnixStream, f: impl FnOnce(UnixStream)) -> Option<Arc<Slot>> {
        struct Leave<'a>(&'a Flights, u64);
        impl Drop for Leave<'_> {
            fn drop(&mut self) {
                CURRENT.with(|c| *c.borrow_mut() = None);
                self.0.open.lock().unwrap().retain(|(i, _)| *i != self.1);
            }
        }
        let Ok(clone) = stream.try_clone() else {
            eprintln!("spira-lc serve: cannot register a connection for the drain; dropped unanswered");
            return None;
        };
        let slot = Arc::new(Slot { stream: clone, answered: std::sync::Mutex::new(false) });
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        self.open.lock().unwrap().push((id, slot.clone()));
        CURRENT.with(|c| *c.borrow_mut() = Some(slot.clone()));
        let _leave = Leave(self, id);
        let _busy = Busy::enter();
        f(stream);
        Some(slot)
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
                let _ = write_raw(&slot.stream, 2, &stopped_for_restart(), "", false);
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

/// Hands each connection to an idle worker, and starts a worker only when none is idle, so a
/// request that calls back into the socket always finds a thread to serve the callback.
struct Workers<'scope, 'env, F, R> {
    scope: &'scope std::thread::Scope<'scope, 'env>,
    serve: F,
    queue: std::sync::mpsc::Sender<UnixStream>,
    inbox: Arc<std::sync::Mutex<std::sync::mpsc::Receiver<UnixStream>>>,
    idle: Arc<AtomicUsize>,
    started: usize,
    kept: std::marker::PhantomData<fn() -> R>,
}

impl<'scope, 'env, F, R> Workers<'scope, 'env, F, R>
where
    F: Fn(UnixStream) -> R + Send + Sync + Clone + 'scope,
{
    fn new(scope: &'scope std::thread::Scope<'scope, 'env>, serve: F, idle: Arc<AtomicUsize>) -> Self {
        let (queue, inbox) = std::sync::mpsc::channel();
        Workers { scope, serve, queue, inbox: Arc::new(std::sync::Mutex::new(inbox)), idle, started: 0, kept: std::marker::PhantomData }
    }

    fn dispatch(&mut self, stream: UnixStream) {
        let claimed = self.idle.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_ok();
        if claimed {
            let _ = self.queue.send(stream);
            return;
        }
        self.started += 1;
        let (serve, inbox, idle) = (self.serve.clone(), self.inbox.clone(), self.idle.clone());
        let spawned = std::thread::Builder::new().name("lc-conn".into()).spawn_scoped(self.scope, move || {
            let mut next = stream;
            loop {
                let held = serve(next);
                idle.fetch_add(1, Ordering::SeqCst);
                drop(held);
                match inbox.lock().unwrap().recv() {
                    Ok(stream) => next = stream,
                    Err(_) => return,
                }
            }
        });
        spawned.expect("spawn lc-conn thread");
    }
}

/// Accept on `listener` until `stopping`, serving connections on reused worker threads and
/// returning how many threads that took. On the stop the path is removed first, so a new
/// connect is refused at once; connections already queued are then accepted and served, and
/// only afterwards does the listener drop and the workers end.
fn accept_until_stopped<'scope, 'env, F, R>(
    listener: &UnixListener,
    socket_path: &str,
    scope: &'scope std::thread::Scope<'scope, 'env>,
    stopping: &AtomicBool,
    serve: F,
) -> usize
where
    F: Fn(UnixStream) -> R + Send + Sync + Clone + 'scope,
{
    accept_counting_idle(listener, socket_path, scope, stopping, serve, Arc::default())
}

/// `accept_until_stopped` with the idle-worker count shared with the caller, which a test reads
/// to know a worker has finished its reply's aftermath.
fn accept_counting_idle<'scope, 'env, F, R>(
    listener: &UnixListener,
    socket_path: &str,
    scope: &'scope std::thread::Scope<'scope, 'env>,
    stopping: &AtomicBool,
    serve: F,
    idle: Arc<AtomicUsize>,
) -> usize
where
    F: Fn(UnixStream) -> R + Send + Sync + Clone + 'scope,
{
    use std::os::unix::io::AsRawFd;
    let mut workers = Workers::new(scope, serve, idle);
    let mut ready = PollFd { fd: listener.as_raw_fd(), events: POLLIN, revents: 0 };
    while !stopping.load(Ordering::SeqCst) {
        ready.revents = 0;
        // SAFETY: one valid pollfd for a descriptor this function borrows.
        if unsafe { poll(&mut ready, 1, 100) } <= 0 || ready.revents & POLLIN == 0 {
            continue;
        }
        match listener.accept() {
            Ok((stream, _)) => workers.dispatch(stream),
            Err(e) => eprintln!("spira-lc serve: accept error: {e}"),
        }
    }
    if let Err(e) = std::fs::remove_file(socket_path) {
        eprintln!("spira-lc serve: cannot remove {socket_path}: {e}");
    }
    if let Err(e) = listener.set_nonblocking(true) {
        eprintln!("spira-lc serve: cannot drain the queue: {e}");
        return workers.started;
    }
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                workers.dispatch(stream);
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
    workers.started
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
    let call = crate::control::state::Call { now, request: &request, inputs: &*crate::control::claimable::LIVE };
    let argv: Vec<String> = ["mending", "sweep", "--actor", "spira-lc.sweep"].iter().map(|s| s.to_string()).collect();
    match crate::control::state::run(store, &argv, &call) {
        (0, out) => out.trim().parse().map_err(|_| format!("sweep answered {out:?}")),
        (code, out) => Err(format!("exit {code}: {out}")),
    }
}

/// The widest window any caller of the audit index asks: the transitions task's, from the same
/// bounds it reads, and the lifecycle section's two-day lookback.
fn dwell_window_s() -> Result<u64, String> {
    let keys = ["SPIRA_TRANSITIONS_SUBMIT_BOUND", "SPIRA_TRANSITIONS_CUT_BOUND", "SPIRA_TRANSITIONS_PR_BOUND", "SPIRA_TRANSITIONS_DELIVER_BOUND", "SPIRA_TRANSITIONS_ACTIVATE_BOUND"];
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
    let started = SystemTime::now();
    let received = Instant::now();
    let mut reader = BufReader::new(stream.try_clone().expect("clone unix stream"));
    let read_at;
    let (req, size) = match read_request(&mut reader) {
        Ok(None) => return,
        Ok(Some(r)) => {
            read_at = Instant::now();
            r
        }
        Err((size, msg)) => {
            let _ = write_response(&stream, 2, &msg, "", false);
            record_unread(spans, size, &msg, started, Instant::now().saturating_duration_since(accepted));
            return;
        }
    };
    let called = call_request(req, peer_of(&stream), door, received, accepted, Cut::Within);
    if crate::slow::first_of_verb(&called.verb) {
        log_rss(&format!("after-first-{}", called.verb));
    }
    let mut answered = Duration::ZERO;
    reply_then_settle(
        &stream,
        &called.ans,
        called.read_ended(),
        || {
            answered = read_at.elapsed();
            count_answer(&called, answered);
        },
        &mut |id, reason| crate::reconcile_content::settle_owed(&mut crate::bd::LiveBd, id, reason),
    );
    record_served(spans, &called, size, started, received, answered);
}

pub(crate) fn record_unread(spans: Option<&spira_otel::Sink>, size: usize, msg: &str, started: SystemTime, queue_wait: Duration) {
    let none = state_store::phases::Snapshot::default();
    let call = spira_otel::Call {
        method: "-",
        caller: "-",
        request_size: size as u64,
        response_size: msg.len() as u64,
        exit_code: 2,
        start: started,
        end: SystemTime::now(),
        queue_wait,
        before_work: Duration::ZERO,
        answer_wait: Duration::ZERO,
        after_answer: Duration::ZERO,
        cut: false,
        answered: None,
        cut_at: None,
        phases: &none.phases,
        commits_us: &none.commits_us,
        reads: none.reads,
        read_max_us: none.read_max_us,
        reads_ended: none.reads_ended,
        reads_overran: none.reads_overran,
        open_phase: none.open,
        content_queries: none.content_queries,
        content_query_max_us: none.content_query_max_us,
        content_query_max_shape: &none.content_query_max_shape,
        task: None,
        children: none.children,
        programs: &none.programs,
        subverb: None,
    };
    record_span(spans, &call);
}

/// The socket peer's process and user.
#[derive(Clone, Copy)]
pub(crate) struct Peer {
    pub(crate) pid: Option<i32>,
    pub(crate) uid: Option<u32>,
}

#[cfg(test)]
pub(crate) fn peer_uid(stream: &UnixStream) -> Option<u32> {
    peer_of(stream).uid
}

#[cfg(test)]
fn peer_pid(stream: &UnixStream) -> Option<i32> {
    peer_of(stream).pid
}

fn peer_of(stream: &UnixStream) -> Peer {
    let cred = peer_cred(stream);
    Peer { pid: cred.as_ref().map(|c| c.pid), uid: cred.map(|c| c.uid) }
}

/// Where a request's deadline is kept: by `within`, on a runner thread of its own, or by the
/// caller, which has already put the request on a thread it can stop waiting for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cut {
    Within,
    ByCaller,
}

/// One request worked: the answer and what the counters and the span are made from.
pub(crate) struct Called {
    pub(crate) ans: Answer,
    pub(crate) caller: String,
    pub(crate) verb: String,
    argv: Vec<String>,
    task: Option<String>,
    pub(crate) cut_at: Option<Duration>,
    accepted: Instant,
    worked: (Instant, Instant),
    began: Arc<std::sync::Mutex<Option<Instant>>>,
    recorder: Arc<state_store::phases::Recorder>,
}

impl Called {
    /// A request answered without being worked: refused, or cut at `cut_at` while its work
    /// was still running.
    pub(crate) fn unworked(req: &Request, caller: String, ans: Answer, cut_at: Option<Duration>, accepted: Instant) -> Called {
        let now = Instant::now();
        Called {
            ans,
            caller,
            verb: req.argv.first().cloned().unwrap_or_default(),
            argv: req.argv.clone(),
            task: req.task.clone(),
            cut_at,
            accepted,
            worked: (now, now),
            began: Default::default(),
            recorder: Default::default(),
        }
    }

    pub(crate) fn read_ended(&self) -> bool {
        let ended = self.recorder.snapshot();
        self.ans.code != 0 && self.cut_at.is_none() && ended.reads_ended + ended.reads_overran + ended.reads_ended_by_reset > 0
    }

    fn method(&self) -> String {
        crate::slow::method_of(&self.argv)
    }
}

/// `req` worked as the one function both servers call: who the caller is, the persona its
/// uid is bound to, the request's deadline and store scopes, then the work.
pub(crate) fn call_request(req: Request, peer: Peer, door: &Door, received: Instant, accepted: Instant, cut: Cut) -> Called {
    let caller = peer.pid.map(crate::slow::caller_of_pid).unwrap_or_else(|| "-".to_string());
    let verb = req.argv.first().cloned().unwrap_or_default();
    let map = std::fs::read_to_string(personas_path()).map(|t| crate::work::parse_persona_uids(&t)).unwrap_or_default();
    let argv_seen = req.argv.clone();
    let task = req.task.clone();
    let recorder = Arc::new(state_store::phases::Recorder::default());
    let started_work = Arc::new(std::sync::Mutex::new(None::<Instant>));
    let mut cut_at = None;
    let worked;
    let ans = match crate::work::bind_peer(&req.argv, peer.uid, &map) {
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
            let handed = Instant::now();
            let in_time = match (ledgered, cut) {
                (Some(id), _) => door.ledger.answer_in_time(&id, wait, run),
                (None, Cut::Within) => within(wait, run),
                (None, Cut::ByCaller) => Some(run()),
            };
            worked = (handed, Instant::now());
            in_time.unwrap_or_else(|| {
                cut_at = Some(limit);
                deadline_answer()
            })
        }
        Err((code, out)) => {
            let now = Instant::now();
            worked = (now, now);
            Answer::out(code, out)
        }
    };
    Called { ans, caller, verb, argv: argv_seen, task, cut_at, accepted, worked, began: started_work, recorder }
}

/// The slow-call counters for an answer that went out `answered` after its request was read.
pub(crate) fn count_answer(called: &Called, answered: Duration) {
    let method = called.method();
    let measured = if called.verb == "ops-view" { method.as_str() } else { called.verb.as_str() };
    crate::slow::request_done(measured, &method, &called.caller, called.task.as_deref(), answered, called.recorder.snapshot().children);
    crate::slow::sample(&method, &called.caller, answered);
}

/// The reads and followups the answer's request made, and its span.
pub(crate) fn record_served(spans: Option<&spira_otel::Sink>, called: &Called, size: usize, started: SystemTime, received: Instant, answered: Duration) {
    let method = called.method();
    let measured = if called.verb == "ops-view" { method.as_str() } else { called.verb.as_str() };
    let (caller, ans) = (&called.caller, &called.ans);
    let began = *called.began.lock().unwrap_or_else(|e| e.into_inner());
    let waited = began.unwrap_or_else(Instant::now).saturating_duration_since(called.accepted);
    let store = called.recorder.snapshot();
    crate::slow::reads_done(measured, store.reads_ended, store.reads_overran, store.reads_ended_by_reset);
    crate::slow::wal_pages(measured, caller, answered, store.reads_reset_ended, state_store::phases::wal_over_line());
    let gate_wait = store.phases.iter().find(|p| p.0 == "gate_wait");
    if let Some((_, us)) = gate_wait {
        crate::slow::gate_waited(measured, Duration::from_micros(*us));
    }
    crate::followups::request_done(caller, measured, store.reads_ended > 0 || store.reads_overran > 0 || store.reads_ended_by_reset > 0 || gate_wait.is_some(), ans.code);
    let (handed, returned) = called.worked;
    let before_span = Instant::now();
    let call = spira_otel::Call {
        method: measured,
        caller,
        request_size: size as u64,
        response_size: (ans.stdout.len() + ans.stderr.len()) as u64,
        exit_code: ans.code,
        start: started,
        end: SystemTime::now(),
        queue_wait: waited,
        before_work: handed.saturating_duration_since(received),
        answer_wait: returned.saturating_duration_since(handed),
        after_answer: before_span.saturating_duration_since(returned),
        cut: called.cut_at.is_some(),
        answered: Some(answered),
        cut_at: called.cut_at,
        phases: &store.phases,
        commits_us: &store.commits_us,
        reads: store.reads,
        read_max_us: store.read_max_us,
        reads_ended: store.reads_ended,
        reads_overran: store.reads_overran,
        open_phase: store.open,
        content_queries: store.content_queries,
        content_query_max_us: store.content_query_max_us,
        content_query_max_shape: &store.content_query_max_shape,
        task: called.task.as_deref(),
        children: store.children,
        programs: &store.programs,
        subverb: subverb_of(&called.argv),
    };
    record_span(spans, &call);
}

/// One request: the argv `env::args().skip(1)` would have produced, and the bytes the CLI's
/// stdin would have held, when the client sent any.
#[derive(Debug, Clone)]
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

/// A request's first line, parsed: what follows it on the wire is `len` bytes of stdin.
pub(crate) struct Head {
    argv: Vec<String>,
    pub(crate) len: Option<u64>,
    caller: std::collections::BTreeMap<String, String>,
    task: Option<String>,
    request_id: Option<String>,
    pub(crate) line_len: usize,
}

/// The refusal for a request that cannot be read, with the bytes read so far.
pub(crate) type Unread = (usize, String);

pub(crate) fn body_unread(line_len: usize, n: u64, why: &dyn std::fmt::Display) -> Unread {
    (line_len, format!("bad request: the {n}-byte body: {why}"))
}

/// The pure half of reading a request: its first line, a JSON argv array or an
/// `{"argv"[, "len"][, "caller"][, "task"][, "request_id"]}` object.
pub(crate) fn parse_head(line: &str) -> Result<Head, Unread> {
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
    Ok(Head { argv, len, caller, task, request_id, line_len: line.len() })
}

/// The pure half of reading a request's body: `head` and the `len` bytes that followed it.
pub(crate) fn finish_request(head: Head, body: Option<Vec<u8>>) -> Result<(Request, usize), Unread> {
    let Head { argv, caller, task, request_id, line_len, .. } = head;
    let stdin = match body {
        None => None,
        Some(b) => Some(String::from_utf8(b).map_err(|e| (line_len, format!("bad request: {e}")))?),
    };
    let size = line_len + stdin.as_ref().map_or(0, String::len);
    Ok((Request { argv, stdin, caller, task, request_id }, size))
}

/// The request on `reader`: one line (`parse_head`), then exactly `len` bytes of stdin when it
/// names one. `Ok(None)` when the client sent nothing; `Err` for a request that cannot be read.
fn read_request(reader: &mut impl BufRead) -> Result<Option<(Request, usize)>, Unread> {
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return Ok(None);
    }
    let head = parse_head(&line)?;
    let body = match head.len {
        None => None,
        Some(n) => {
            let mut body = vec![0u8; n as usize];
            std::io::Read::read_exact(reader, &mut body).map_err(|e| body_unread(head.line_len, n, &e))?;
            Some(body)
        }
    };
    finish_request(head, body).map(Some)
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

/// The reply goes out first, then `answered` runs; the owed store closes are queued to the settler, or settled here when none is installed.
fn reply_then_settle(stream: &UnixStream, ans: &Answer, read_ended: bool, answered: impl FnOnce(), settle: &mut dyn FnMut(&str, &str) -> Result<(), String>) {
    let _ = write_response(stream, ans.code, &ans.stdout, &ans.stderr, read_ended);
    answered();
    if !crate::settle::owe_closes(&ans.owed) {
        crate::callers::settle_owed_with(&ans.owed, settle);
    }
}

/// `argv[1]` when it is a plain lowercase word, so bead ids and free text are never emitted.
fn subverb_of(argv: &[String]) -> Option<&str> {
    let word = argv.get(1)?;
    let mut chars = word.chars();
    let plain = chars.next().is_some_and(|c| c.is_ascii_lowercase()) && word.len() <= 32 && chars.all(|c| c.is_ascii_lowercase() || c == '-');
    plain.then_some(word.as_str())
}

fn record_span(spans: Option<&spira_otel::Sink>, call: &spira_otel::Call) {
    if let Some(sink) = spans {
        sink.emit(call);
    }
}

// batch-job: just under the 310 s the `work` client waits for a tool-running verb.
pub const TOOL_DEADLINE: Duration = Duration::from_secs(305);

const WATCH_MARGIN: Duration = Duration::from_millis(500);

/// The request's deadline: the configured default, for every store call but a `watch`, which keeps its own hold. `work`'s lane verbs are
/// batch jobs this service brokers, not calls: they run a harness tool (groomer sweep, bead
/// filing, mail) for the dwarf, and keep the bound they had.
pub(crate) fn request_deadline(argv: &[String], deadlines: &Deadlines) -> Duration {
    match argv.first().map(String::as_str) {
        // batch-job: a work verb that runs a brokered tool waits the client's tool bound less a margin.
        // A recorded verb is a plain store write; the program it names runs in the scheduler.
        Some("work") if argv.get(2).is_some_and(|v| crate::work::exec_verb(v)) => crate::db::QUERY_DEADLINE,
        Some("work") if argv.get(2).is_some_and(|v| crate::work::brokers_tool(v)) => TOOL_DEADLINE,
        Some("work") => crate::db::QUERY_DEADLINE,
        // batch-job: a watch holds as long as its caller asked, so the cutoff is that hold plus a margin, not the request default.
        Some("watch") => {
            let hold = crate::flag(argv, "--wait-ms").and_then(|v| v.parse::<u64>().ok()).map_or(state_store::MAX_WATCH_WAIT_MS, |ms| ms.min(state_store::MAX_WATCH_WAIT_MS));
            Duration::from_millis(hold) + WATCH_MARGIN
        }
        // batch-job: a repair sweep over every row, bounded at the query deadline like census's.
        Some(v) if crate::composed::SWEEPS.contains(&v) => crate::db::QUERY_DEADLINE,
        _ => deadlines.default,
    }
}

/// Binds the worker thread to the request deadline: the control store refuses a write begun past it.
fn bind_deadline(received: Instant, limit: Duration) -> state_store::phases::DeadlineScope {
    state_store::phases::deadline_scoped(received + limit)
}

/// `f`'s answer, or `None` once `deadline` passes; the abandoned work finishes unobserved.
fn within<T: Send + 'static>(deadline: Duration, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    RUNNERS.run(Box::new(move || {
        let answer = f();
        Box::new(move || {
            let _ = tx.send(answer);
        })
    }));
    rx.recv_timeout(deadline).ok()
}

/// Runs the work and returns the delivery of its answer, which the runner makes only once it is idle again.
type Job = Box<dyn FnOnce() -> Box<dyn FnOnce()> + Send + 'static>;

/// Process-wide threads that run request work: a job takes an idle runner, and a runner starts
/// only when none is idle, so one still finishing abandoned work is never waited for.
struct Runners {
    queue: std::sync::mpsc::Sender<Job>,
    inbox: Arc<std::sync::Mutex<std::sync::mpsc::Receiver<Job>>>,
    idle: Arc<AtomicUsize>,
}

static RUNNERS: std::sync::LazyLock<Runners> = std::sync::LazyLock::new(|| {
    let (queue, inbox) = std::sync::mpsc::channel();
    Runners { queue, inbox: Arc::new(std::sync::Mutex::new(inbox)), idle: Arc::new(AtomicUsize::new(0)) }
});

impl Runners {
    fn run(&self, job: Job) {
        if self.idle.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_ok() {
            let _ = self.queue.send(job);
            return;
        }
        let (inbox, idle) = (self.inbox.clone(), self.idle.clone());
        let started = std::thread::Builder::new().name("lc-run".into()).spawn(move || {
            let mut next = job;
            loop {
                let delivery = {
                    let _busy = Busy::enter();
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(next))
                };
                idle.fetch_add(1, Ordering::SeqCst);
                if let Ok(deliver) = delivery {
                    deliver();
                }
                match inbox.lock().unwrap().recv() {
                    Ok(job) => next = job,
                    Err(_) => return,
                }
            }
        });
        started.expect("spawn lc-run thread");
    }
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

/// A header line, then the stdout bytes; a non-empty stderr follows them, its length in the
/// header's `err_len` (a client that predates it reads `len` bytes and stops).
fn write_response(stream: &UnixStream, exit_code: i32, stdout: &str, stderr: &str, read_ended: bool) -> std::io::Result<()> {
    match CURRENT.with(|c| c.borrow().clone()) {
        None => write_raw(stream, exit_code, stdout, stderr, read_ended),
        Some(slot) => {
            let mut answered = slot.answered.lock().unwrap();
            if *answered {
                return Ok(());
            }
            *answered = true;
            write_raw(stream, exit_code, stdout, stderr, read_ended)
        }
    }
}

/// The bytes of one answer: the header line, then stdout, then stderr.
pub(crate) fn encode_response(exit_code: i32, stdout: &str, stderr: &str, read_ended: bool) -> Vec<u8> {
    let mut buf = Vec::with_capacity(stdout.len() + stderr.len() + 64);
    let mut head = serde_json::json!({"exit_code": exit_code, "len": stdout.len()});
    if !stderr.is_empty() {
        head["err_len"] = stderr.len().into();
    }
    if read_ended {
        head["read_ended"] = true.into();
    }
    buf.extend_from_slice(format!("{head}\n").as_bytes());
    buf.extend_from_slice(stdout.as_bytes());
    buf.extend_from_slice(stderr.as_bytes());
    buf
}

fn write_raw(mut stream: &UnixStream, exit_code: i32, stdout: &str, stderr: &str, read_ended: bool) -> std::io::Result<()> {
    stream.write_all(&encode_response(exit_code, stdout, stderr, read_ended))
}

/// sd_notify `READY=1` to `$NOTIFY_SOCKET` (`@` prefix: abstract namespace); nothing when unset.
pub fn notify_ready(socket: Option<&str>) -> Result<(), String> {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixDatagram};
    let Some(s) = socket.filter(|s| !s.is_empty()) else { return Ok(()) };
    let fail = |e: std::io::Error| format!("cannot tell systemd READY=1 at {s}: {e}");
    let addr = match s.strip_prefix('@') {
        Some(name) => SocketAddr::from_abstract_name(name.as_bytes()),
        None => SocketAddr::from_pathname(s),
    }
    .map_err(fail)?;
    UnixDatagram::unbound().and_then(|d| d.send_to_addr(b"READY=1", &addr)).map(|_| ()).map_err(fail)
}

#[cfg(test)]
mod tests;

/// Runs the writer's grant check once, says on stderr what it found, and keeps the verdict for
/// the `writer-grants` verb. Starts no process and changes no grant.
fn record_writer_grants() {
    match crate::admin_writer::record_check() {
        Some(Ok(missing)) if !missing.is_empty() => eprintln!("spira-lc serve: the content writer lacks write grants on {}: run spira-lc admin-provision-writer", missing.join(", ")),
        Some(Err(e)) => eprintln!("spira-lc serve: cannot check the content writer's grants: {e}"),
        _ => {}
    }
}
