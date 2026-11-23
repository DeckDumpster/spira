//! The control store at `SPIRA_LC_STORE_PATH`, opened once per process on first use and held
//! for the process's life: its opener is exclusive, so in practice lc-serve holds it and any
//! other opener answers "cannot tell". Every lifecycle verb is served from it; with the key
//! unset they answer "cannot tell" rather than fall back to a store that no longer exists.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use runtime::ports::Clock;
use state_store::{Config, Contract, MonotonicClock, Ports};
use state_store_sqlite::{Options, SqliteBackend};

use super::store::Store;
use super::{claimable, readiness, sections, state, verbs};
use crate::{CANNOT_TELL, REFUSED};

/// A failed open is not kept, so the next call tries again.
static OPENED: Mutex<Option<Arc<Store<Served>>>> = Mutex::new(None);

/// The store path the config declares: `None` while the key is unset or the config is
/// unreadable, so the dolt path answers and reports its own config error.
fn store_path() -> Option<Result<String, String>> {
    let path = spira_config::process::cfg("SPIRA_LC_STORE_PATH")
        .ok()
        .filter(|p| !p.is_empty())?;
    Some(confined(&path).map(|()| path))
}

pub fn store_configured() -> bool {
    store_path().is_some()
}

fn confined(path: &str) -> Result<(), String> {
    let instance = spira_config::process::cfg("SPIRA_INSTANCE")?;
    let workspaces = spira_config::process::cfg("SPIRA_WORKSPACES")?;
    spira_config::containment::check_path(&instance, &workspaces, "SPIRA_LC_STORE_PATH", path)
}

/// The shortest lock the contract grants, and so the shortest claim: `SPIRA_LC_LOCK_TTL_MIN_MS`.
fn lock_floor_ms() -> Result<u64, String> {
    match spira_config::process::cfg_parse::<u64>("SPIRA_LC_LOCK_TTL_MIN_MS")? {
        0 => Err("SPIRA_LC_LOCK_TTL_MIN_MS must be at least 1".into()),
        n => Ok(n),
    }
}

/// The longest one read may hold before the store ends it: `SPIRA_LC_MAX_READ_MS`.
fn max_read_ms() -> Result<u64, String> {
    nonzero_read_limit(spira_config::process::cfg_parse::<u64>("SPIRA_LC_MAX_READ_MS")?)
}

fn nonzero_read_limit(ms: u64) -> Result<u64, String> {
    match ms {
        0 => Err("SPIRA_LC_MAX_READ_MS must be at least 1".into()),
        n => Ok(n),
    }
}

/// The wall clock lock expiries are read on: the whole second events are stamped at, pinned with
/// the rest of the process by `SPIRA_NOW`, so a lease reads back as the second it was asked for.
pub(crate) struct Wall;

impl Clock for Wall {
    fn now_ms(&self) -> u64 {
        spira_config::vtime::now_epoch().saturating_mul(1_000)
    }
}

/// The store as this process opens it: SQLite alone, every read served from it.
pub type Served = SqliteBackend;

/// Reader connections on the main file. With the backend's default of four, requests were
/// measured waiting 1.7 to 2.5 s of a 3 s deadline for a reader (`spira.phase.reader_wait_us`)
/// while the server ran ten or more request threads; a WAL reader costs a file handle and its
/// page cache, so there is one for each thread that can ask.
pub(crate) const READERS: usize = 16;

fn backend_at(path: &str, opts: Options) -> Result<Served, String> {
    // Opening creates a missing file, which would answer every read as an empty store.
    if !Path::new(path).is_file() {
        return Err(format!("no control store at {path}"));
    }
    let began = std::time::Instant::now();
    let backend = SqliteBackend::open(Path::new(path), opts).map_err(|e| format!("{path}: {e}"))?;
    eprintln!("{}", crate::serve::startup_line("open-sqlite", began.elapsed(), crate::serve::wall_ms()));
    Ok(backend)
}

fn served_options(history_bytes: usize, wal_line_bytes: u64, max_read_ms: u64, metrics: Option<spira_otel::metrics::MetricsSink>) -> Options {
    Options { history_bytes, wal_line_bytes, max_read_ms, wall: Arc::new(Wall), metrics, readers: READERS, ..Options::default() }
}

fn open(path: &str) -> Result<Store<Served>, String> {
    let history_bytes = spira_config::process::cfg_parse::<usize>("SPIRA_LC_HISTORY_RING_BYTES")?;
    let wal_line_bytes = spira_config::process::cfg_parse::<u64>("SPIRA_LC_WAL_LINE_BYTES")?;
    let max_read_ms = max_read_ms()?;
    let endpoint = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok();
    let metrics = Some(spira_otel::metrics::MetricsSink::new(endpoint.as_deref(), "spira-lc", std::time::Duration::from_millis(250)));
    let backend = backend_at(path, served_options(history_bytes, wal_line_bytes, max_read_ms, metrics))?;
    let floor = lock_floor_ms()?;
    let ports = Ports {
        mono: Arc::new(MonotonicClock::new()),
        wall: Arc::new(Wall),
    };
    let contract = Contract::open(
        backend,
        ports,
        Config {
            min_lock_ttl_ms: floor,
        },
    )
    .map_err(|e| format!("{path}: {e}"))?;
    let logs = spira_config::process::cfg("SPIRA_VICTORIALOGS_URL").map_err(|e| e.to_string())?;
    if logs.trim().is_empty() {
        return Err("SPIRA_VICTORIALOGS_URL is empty: every transition is shipped there and there is no fallback".into());
    }
    Ok(Store::new(contract, floor)
        .with_shipper(spira_otel::vlogs::Shipper::start(&logs))
        .with_refs(Arc::new(crate::bd::external_ref_of)))
}

fn run_dir(run: std::path::PathBuf) -> &'static sections::lifecycle::RunDir {
    static RUN_DIR: std::sync::OnceLock<sections::lifecycle::RunDir> = std::sync::OnceLock::new();
    RUN_DIR.get_or_init(|| sections::lifecycle::RunDir::new(run))
}

fn opened(path: &str) -> Result<Arc<Store<Served>>, String> {
    let mut slot = OPENED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = slot.as_ref() {
        return Ok(Arc::clone(s));
    }
    let s = Arc::new(open(path)?);
    *slot = Some(Arc::clone(&s));
    prime_dwell(Arc::clone(&s));
    Ok(s)
}

/// Fills the dwell index for the day an observer asks about, off the caller's thread, so the
/// first observer's read finds it built rather than building it inside its deadline.
fn prime_dwell(s: Arc<Store<Served>>) {
    std::thread::spawn(move || {
        for _ in 0..PRIME_CALLS {
            match s.dwell(spira_config::vtime::now_epoch() as i64, PRIME_WINDOW_S, None) {
                Ok(d) if !d.complete => {}
                _ => break,
            }
        }
    });
}

/// The window the index is primed to, and the most calls spent on it (each reads a bounded part).
const PRIME_WINDOW_S: u64 = 86_400;
const PRIME_CALLS: usize = 16;

/// The store the config declares, opened: `None` while the key is unset.
pub fn configured() -> Option<Result<Arc<Store<Served>>, String>> {
    Some(store_path()?.and_then(|p| opened(&p)))
}

/// The store the config declares, or the reason a verb that needs it cannot be answered.
pub fn store() -> Result<Arc<Store<Served>>, String> {
    match configured() {
        Some(r) => r.map_err(|e| format!("the control store: {e}")),
        None => Err("the control store is not configured: SPIRA_LC_STORE_PATH is unset or unreadable".into()),
    }
}

/// `admin-create-store`: a new, empty control store at `SPIRA_LC_STORE_PATH`, which no verb
/// creates on open (an absent store opened empty would answer every read as nothing there).
/// Refused on an empty key, a path outside the instance's workspaces, or a file already there.
pub fn create_store() -> (i32, String) {
    let refuse = |why: String| (REFUSED, format!("admin-create-store: {why}"));
    let path = match spira_config::process::cfg("SPIRA_LC_STORE_PATH") {
        Err(e) => return (CANNOT_TELL, format!("cannot tell: admin-create-store: {e}")),
        Ok(p) if p.is_empty() => return refuse("SPIRA_LC_STORE_PATH is empty: there is no store to create".into()),
        Ok(p) => p,
    };
    if let Err(e) = confined(&path) {
        return refuse(e);
    }
    let floor = match lock_floor_ms() {
        Ok(f) => f,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: admin-create-store: {e}")),
    };
    // create_new claims the path atomically, so two creators cannot both believe they made it.
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return refuse(format!("{path} already exists")),
        Err(e) => return (CANNOT_TELL, format!("cannot tell: admin-create-store: {path}: {e}")),
        Ok(_) => {}
    }
    match open(&path) {
        Ok(_) => (0, format!("admin-create-store: created {path} (locks of at least {floor} ms)")),
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            (CANNOT_TELL, format!("cannot tell: admin-create-store: {e}"))
        }
    }
}

thread_local! {
    static CLIENT_REQUEST: std::cell::RefCell<Option<(String, u32)>> = const { std::cell::RefCell::new(None) };
}

/// Binds the id the client sent to this thread's calls until dropped.
pub(crate) struct ClientRequest(Option<(String, u32)>);

pub(crate) fn client_request_scoped(id: Option<String>) -> ClientRequest {
    ClientRequest(CLIENT_REQUEST.with(|c| c.replace(id.map(|i| (i, 0)))))
}

impl Drop for ClientRequest {
    fn drop(&mut self) {
        CLIENT_REQUEST.with(|c| *c.borrow_mut() = self.0.take());
    }
}

/// The id for the next store txn of this call: the client's own for the first, `<id>#<n>` for
/// each further one (a composed verb makes several, and the store dedupes on the id alone),
/// else a minted one.
pub(crate) fn call_request_id() -> String {
    let sent = CLIENT_REQUEST.with(|c| {
        c.borrow_mut().as_mut().map(|(id, n)| {
            *n += 1;
            if *n == 1 { id.clone() } else { format!("{id}#{n}") }
        })
    });
    sent.unwrap_or_else(request_id)
}

/// A request id no other call in any process mints: each call, a retry included, is its own
/// request, so a request that lost its race is never answered again as if it had not.
pub(crate) fn request_id() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "lc/{}/{nanos}/{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

/// The control plane's planning watermark, `planning/watermark {seq}`: `None` while no control
/// store is configured, `Ok(None)` while nothing is stamped.
pub fn planning_watermark() -> Option<Result<Option<u64>, String>> {
    let store = match configured()? {
        Ok(s) => s,
        Err(e) => return Some(Err(e)),
    };
    let item = store.contract().get(crate::skew::CONTROL_KEY);
    Some(match item {
        Err(e) => Err(e.to_string()),
        Ok(None) => Ok(None),
        Ok(Some(i)) => i.value.data["seq"]
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("{} holds no seq", crate::skew::CONTROL_KEY)),
    })
}

/// `planning-watermark`, `plane-watermarks` and `skew-check`, which answer with or without a
/// control store.
fn dispatch_watermark(args: &[String]) -> Option<(i32, String)> {
    match args[0].as_str() {
        crate::skew::BOTH_VERB => {
            let control = match planning_watermark() {
                None => Ok(None),
                Some(r) => r,
            };
            let planning = crate::callers::Bd::planning_watermark(&mut crate::bd::LiveBd);
            let manifest = args.iter().any(|a| a == "--manifest");
            Some(crate::skew::both(control, planning, manifest))
        }
        crate::skew::VERB => Some(match planning_watermark() {
            None | Some(Ok(None)) => (0, "null".into()),
            Some(Ok(Some(seq))) => (0, serde_json::json!({ "seq": seq }).to_string()),
            Some(Err(e)) => (CANNOT_TELL, format!("cannot tell: the control store: {e}")),
        }),
        crate::skew::CHECK_VERB => {
            let max = spira_config::process::cfg_parse::<u64>("SPIRA_PLANE_SKEW_MAX");
            Some(crate::skew::served(planning_watermark(), &mut crate::bd::LiveBd, max))
        }
        _ => None,
    }
}

/// The control store's answer to `args`, or `None` for a verb it does not serve.
pub fn dispatch(args: &[String]) -> Option<(i32, String)> {
    let verb = args.first()?;
    if let Some(answer) = dispatch_watermark(args) {
        return Some(answer);
    }
    if sections::VERBS.contains(&verb.as_str()) || verb == sections::lifecycle::VERB || verb == spira_lc::rc::VERB {
        return Some(dispatch_sections(args));
    }
    let closeout = verb == super::gh_closeout::VERB;
    let git_fact = verb == super::git_facts::VERB;
    let work_request = verb == super::work_requests::VERB;
    let lifecycle = state::is_verb(verb) || readiness::VERBS.contains(&verb.as_str()) || claimable::VERBS.contains(&verb.as_str());
    if !lifecycle && !closeout && !git_fact && !work_request && !verbs::VERBS.contains(&verb.as_str()) {
        return None;
    }
    let path = match store_path() {
        Some(Ok(p)) => p,
        Some(Err(e)) => return Some((CANNOT_TELL, format!("cannot tell: {e}"))),
        None => {
            return Some((
                CANNOT_TELL,
                format!("cannot tell: {verb} is served by the control store, and SPIRA_LC_STORE_PATH is unset or unreadable"),
            ))
        }
    };
    Some(match opened(&path) {
        Ok(store) if lifecycle => {
            let request = call_request_id();
            let call = state::Call {
                now: crate::db::now_epoch(),
                request: &request, inputs: &*crate::control::claimable::LIVE,
            };
            if claimable::VERBS.contains(&verb.as_str()) {
                claimable::run(&store, args, &*claimable::LIVE, &call)
            } else if readiness::VERBS.contains(&verb.as_str()) {
                readiness::run(&store, args, &call)
            } else {
                state::run(&store, args, &call)
            }
        }
        Ok(store) if closeout => super::gh_closeout::run(&store, args, &call_request_id()),
        Ok(store) if git_fact => super::git_facts::run(&store, args, &call_request_id(), &crate::slow::current_caller()),
        Ok(store) if work_request => super::work_requests::run(&store, args, &call_request_id(), crate::db::now_epoch()),
        Ok(store) if args.iter().any(|a| a == "--rows") => {
            let request = call_request_id();
            let call = state::Call { now: crate::db::now_epoch(), request: &request, inputs: &*crate::control::claimable::LIVE };
            verbs::run_with_rows(&store, args, &call)
        }
        Ok(store) => verbs::run(&store, args),
        Err(e) => (CANNOT_TELL, format!("cannot tell: the control store: {e}")),
    })
}

/// `section`, `intent`, `lock`, `pass`, `publish-section` and `rc`, which only the control store
/// serves: JSON on every path.
fn dispatch_sections(args: &[String]) -> (i32, String) {
    let path = match store_path() {
        Some(Ok(p)) => p,
        Some(Err(e)) => return sections::cannot_tell(&e),
        // A lock lives only in the control store, so with none configured nothing holds one; an
        // unreadable config is not "none configured".
        None if args.get(1).map(String::as_str) == Some("show") && args[0] == "lock" => {
            return match spira_config::process::cfg("SPIRA_LC_STORE_PATH") {
                Ok(p) if p.is_empty() => sections::unheld(args.get(2).map_or("", String::as_str)),
                Ok(_) => sections::cannot_tell("SPIRA_LC_STORE_PATH is set and did not resolve"),
                Err(e) => sections::cannot_tell(&e),
            };
        }
        None => {
            return sections::cannot_tell(&format!(
                "{} is served by the control store, and SPIRA_LC_STORE_PATH is unset or unreadable",
                args[0]
            ))
        }
    };
    match opened(&path) {
        Ok(store) if args[0] == spira_lc::rc::VERB => spira_lc::rc::run(store.contract(), args),
        Ok(store) => {
            let request = call_request_id();
            let endpoint = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok();
            let metrics = spira_otel::metrics::MetricsSink::new(
                endpoint.as_deref(),
                "spira-lc",
                std::time::Duration::from_millis(250),
            );
            let call = sections::Call {
                now_ms: Wall.now_ms(),
                request: &request,
                metrics: &metrics,
            };
            if args[0] != sections::lifecycle::VERB {
                return sections::run(store.contract(), args, &call);
            }
            match crate::lifecycle_run_dir() {
                Ok(run) => sections::lifecycle::run(&store, args, &call, run_dir(run)),
                Err(e) => sections::cannot_tell(&format!("the run directory: {e}")),
            }
        }
        Err(e) => sections::cannot_tell(&format!("the control store: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steps_to_open(tag: &str, keys: usize) -> (u64, testkit::TempDir, Contract<Served>) {
        let dir = testkit::TempDir::new(tag);
        let path = dir.join("control.db");
        let seed = SqliteBackend::open(&path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
        let rows: Vec<_> = (0..keys).map(|i| (format!("{}sp-{i:05}", crate::control::keys::LIFECYCLE), b"{}".to_vec())).collect();
        seed.bulk_load(2, rows).unwrap();
        drop(seed);
        let backend = backend_at(path.to_str().unwrap(), Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
        let clock = runtime::ports::VirtualClock::new(0);
        let contract = Contract::open(backend, Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        (contract.backend().take_read_steps(), dir, contract)
    }

    struct Steered(AtomicU64);

    impl Clock for Steered {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    #[test_id(tuu6gpz)]
    #[test]
    fn a_read_of_150_ms_is_ended_for_every_verb_on_the_store_as_served() {
        let dir = testkit::TempDir::new("lc-served-limit");
        let path = dir.join("control.db");
        drop(SqliteBackend::open(&path, Options::default()).unwrap());
        let clock = Arc::new(Steered(AtomicU64::new(0)));
        let (probe_clock, armed) = (clock.clone(), Arc::new(std::sync::atomic::AtomicBool::new(false)));
        let probe_armed = armed.clone();
        let opts = Options {
            clock: clock.clone(),
            read_probe: Some(Arc::new(move || {
                if probe_armed.load(Ordering::SeqCst) {
                    probe_clock.0.fetch_add(150, Ordering::SeqCst);
                }
            })),
            ..served_options(1 << 20, 8 << 20, 100, None)
        };
        let backend = backend_at(path.to_str().unwrap(), opts).unwrap();
        let vclock = runtime::ports::VirtualClock::new(0);
        let contract = Contract::open(backend, Ports::virtual_clock(&vclock), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        let store = Store::new(contract, 1_000);
        armed.store(true, Ordering::SeqCst);
        let call = state::Call { now: 0, request: "r", inputs: &claimable::FIXED };
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let answers = [
            ("audits", verbs::run(&store, &argv(&["audits", "--limit", "5"]))),
            ("history", verbs::run(&store, &argv(&["history", "p/"]))),
            ("list", state::run(&store, &argv(&["list", "--ids", "sp-a"]), &call)),
            ("show", state::run(&store, &argv(&["show", "sp-a"]), &call)),
        ];
        for (verb, (code, out)) in answers {
            assert_eq!(code, CANNOT_TELL, "{verb}: {out}");
            assert!(out.contains(state_store::READ_ENDED), "{verb}: {out}");
        }
    }

    #[test_id(tpsnny7)]
    #[test]
    fn opening_a_store_holding_20000_keys_reads_no_lifecycle_row() {
        let (empty, _d, _c) = steps_to_open("lc-served-restart-empty", 0);
        let (full, _dir, contract) = steps_to_open("lc-served-restart-full", 20_000);
        assert_eq!(full, empty, "open reads the same few rows however many keys the file holds");
        assert!(full < 100, "{full}");
        let head = state_store::Backend::head(contract.backend()).unwrap();
        let listed = state_store::Backend::range(contract.backend(), crate::control::keys::LIFECYCLE, None, 10, head).unwrap();
        assert_eq!(listed.len(), 10);
        assert!(contract.backend().take_read_steps() >= 10, "the counter sees a range read, so the low count above is real");
    }

    #[test_id(ty5aq3e)]
    #[test]
    fn open_refuses_a_config_that_lacks_the_wal_line_naming_the_key() {
        let dir = testkit::TempDir::new("lc-served-wal-line");
        let toml = spira_config::process::fixture_toml(dir.path(), &[]);
        let text = std::fs::read_to_string(&toml).unwrap();
        assert!(text.contains("lc_wal_line_bytes"), "the complete fixture declares the key");
        let without: String = text.lines().filter(|l| !l.contains("lc_wal_line_bytes")).map(|l| format!("{l}\n")).collect();
        std::fs::write(&toml, without).unwrap();
        let _env = testkit::env(&[("SPIRA_TOML", Some(toml.to_str().unwrap()))]);
        let err = open(dir.path().join("control.db").to_str().unwrap()).err().expect("a missing key is refused");
        assert!(err.contains("SPIRA_LC_WAL_LINE_BYTES"), "{err}");
    }

    #[test_id(t89ptjp)]
    #[test]
    fn the_served_store_reads_under_the_fixture_allowance() {
        let dir = testkit::TempDir::new("lc-served-max-read");
        let toml = spira_config::process::fixture_toml(dir.path(), &[]);
        let doc = spira_config::validate(&std::fs::read_to_string(toml).unwrap()).unwrap();
        let declared = doc.spira.as_ref().unwrap().get_u64("SPIRA_LC_MAX_READ_MS").unwrap().expect("the fixture declares the key");
        let opts = served_options(1, 1, nonzero_read_limit(declared).unwrap(), None);
        assert_eq!(opts.max_read_ms, testkit::LOADED_HOST_MAX_READ_MS);
    }

    #[test_id(ts2crs5)]
    #[test]
    fn open_refuses_a_config_that_lacks_the_read_limit_naming_the_key() {
        let dir = testkit::TempDir::new("lc-served-read-limit-absent");
        let toml = spira_config::process::fixture_toml(dir.path(), &[]);
        let text = std::fs::read_to_string(&toml).unwrap();
        assert!(text.contains("lc_max_read_ms = 120000"), "the complete fixture declares the key");
        let without: String = text.lines().filter(|l| !l.starts_with("lc_max_read_ms")).map(|l| format!("{l}\n")).collect();
        std::fs::write(&toml, without).unwrap();
        let _env = testkit::env(&[("SPIRA_TOML", Some(toml.to_str().unwrap()))]);
        let err = open(dir.path().join("control.db").to_str().unwrap()).err().expect("a missing key is refused");
        assert!(err.contains("SPIRA_LC_MAX_READ_MS"), "{err}");
    }

    #[test_id(tab8s4z)]
    #[test]
    fn a_zero_read_limit_is_refused_naming_the_key() {
        let err = nonzero_read_limit(0).unwrap_err();
        assert!(err.contains("SPIRA_LC_MAX_READ_MS"), "{err}");
        assert_eq!(nonzero_read_limit(100), Ok(100));
    }

    #[test_id(t2odk9h)]
    #[test]
    fn a_call_under_a_client_id_spends_it_on_its_first_txn_and_suffixes_each_further_one() {
        let _bound = client_request_scoped(Some("req-1".to_string()));
        assert_eq!((call_request_id(), call_request_id(), call_request_id()), ("req-1".into(), "req-1#2".into(), "req-1#3".into()));
        drop(_bound);
        let (a, b) = (call_request_id(), call_request_id());
        assert!(a.starts_with("lc/") && a != b, "unbound calls mint their own: {a} {b}");
    }

    #[test_id(tgcirlp)]
    #[test]
    fn the_new_verbs_take_no_name_an_existing_verb_answers() {
        for v in sections::VERBS.iter().chain([&sections::lifecycle::VERB, &spira_lc::rc::VERB]) {
            assert!(!state::VERBS.contains(v), "{v}");
            assert!(!verbs::VERBS.contains(v), "{v}");
            assert!(!crate::callers::is_verb(v), "{v}");
        }
    }
}
