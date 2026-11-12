//! The control store at `SPIRA_LC_STORE_PATH`, opened once per process on first use and held
//! for the process's life: its opener is exclusive, so in practice lc-serve holds it and any
//! other opener answers "cannot tell". Every lifecycle verb is served from it; with the key
//! unset they answer "cannot tell" rather than fall back to a store that no longer exists.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use runtime::ports::Clock;
use state_store::{Config, Contract, Modeled, MonotonicClock, Ports};
use state_store_sqlite::{Options, SqliteBackend};

use super::store::Store;
use super::{readiness, sections, state, verbs};
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

/// The wall clock lock expiries are read on: the whole second events are stamped at, pinned with
/// the rest of the process by `SPIRA_NOW`, so a lease reads back as the second it was asked for.
struct Wall;

impl Clock for Wall {
    fn now_ms(&self) -> u64 {
        spira_config::vtime::now_epoch().saturating_mul(1_000)
    }
}

/// The store as this process opens it: SQLite under an in-memory copy of everything but the
/// keyspaces that grow without bound, kept by the store's own commits ([`Modeled`]), so an
/// observer's read never waits on the database and a compare-and-set stays SQLite's.
pub type Served = Modeled<SqliteBackend>;

/// What the copy leaves to SQLite: the audit history, each record's history entries and the
/// request keys, which grow with every write and which only `history`, `event-seq`, a watch
/// and a retry's idempotency check read.
pub const UNMODELED: &[&str] = &[
    state_store::EVENTS_PREFIX,
    state_store::REQUESTS_PREFIX,
    super::keys::HISTORY,
];

/// Revisions behind the head the copy still answers a snapshot read at: a list paging one
/// snapshot while this many commits land keeps reading from memory.
pub const MODEL_WINDOW: u64 = 1_024;

/// The longest the cold read-model build may read: it scans every modeled key once, which a
/// cold page cache stretches well past the steady-state read limit.
const BUILD_READ_MS: u64 = 120_000;

/// The store's backend under a read model built within `build_ms`, then held to `opts`' own limit.
fn modeled(path: &str, opts: Options, build_ms: u64) -> Result<Served, String> {
    // Opening creates a missing file, which would answer every read as an empty store.
    if !Path::new(path).is_file() {
        return Err(format!("no control store at {path}"));
    }
    let steady = opts.max_read_ms;
    let backend = SqliteBackend::open(Path::new(path), Options { max_read_ms: build_ms.max(steady), ..opts })
        .map_err(|e| format!("{path}: {e}"))?;
    let backend = Modeled::open(backend, UNMODELED, MODEL_WINDOW)
        .map_err(|e| format!("{path}: the read model: {e}"))?;
    backend.inner().set_max_read_ms(steady);
    Ok(backend)
}

fn open(path: &str) -> Result<Store<Served>, String> {
    let backend = modeled(path, Options::default(), BUILD_READ_MS)?;
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
    Ok(Store::new(contract, floor).with_shipper(spira_otel::vlogs::Shipper::start(&logs)))
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
    let lifecycle = state::is_verb(verb) || readiness::VERBS.contains(&verb.as_str());
    if !lifecycle && !verbs::VERBS.contains(&verb.as_str()) {
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
            let request = request_id();
            let call = state::Call {
                now: crate::db::now_epoch(),
                request: &request,
            };
            if readiness::VERBS.contains(&verb.as_str()) {
                readiness::run(&store, args, &call)
            } else {
                state::run(&store, args, &call)
            }
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
            let request = request_id();
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
                Ok(run) => sections::lifecycle::run(&store, args, &call, &sections::lifecycle::RunDir(run)),
                Err(e) => sections::cannot_tell(&format!("the run directory: {e}")),
            }
        }
        Err(e) => sections::cannot_tell(&format!("the control store: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::VirtualClock;

    fn slow_reads(steady_ms: u64) -> Options {
        let clock = VirtualClock::new(0);
        let tick = clock.clone();
        Options {
            max_read_ms: steady_ms,
            clock: Arc::new(clock),
            read_probe: Some(Arc::new(move || tick.advance(5 * steady_ms))),
            ..Options::default()
        }
    }

    #[test]
    fn a_read_model_build_slower_than_the_steady_read_limit_still_opens() {
        let dir = testkit::TempDir::new("lc-served-cold");
        let path = dir.join("control.db");
        let seed = SqliteBackend::open(&path, Options::default()).unwrap();
        seed.bulk_load(2, vec![("k".to_string(), b"v".to_vec())]).unwrap();
        drop(seed);
        let p = path.to_str().unwrap();

        let err = modeled(p, slow_reads(1_000), 0).expect_err("a build held to the steady limit must end");
        assert!(err.contains(state_store::READ_ENDED), "{err}");

        let built = modeled(p, slow_reads(1_000), BUILD_READ_MS).expect("the build has its own budget");
        assert_eq!(built.size().0, 1);

        let later = state_store::Backend::log(built.inner(), 0, 10).expect_err("a read after the build is held to the steady limit again");
        assert!(state_store_sqlite::is_read_ended(&later), "{later}");
    }

    #[test]
    fn the_new_verbs_take_no_name_an_existing_verb_answers() {
        for v in sections::VERBS.iter().chain([&sections::lifecycle::VERB, &spira_lc::rc::VERB]) {
            assert!(!state::VERBS.contains(v), "{v}");
            assert!(!verbs::VERBS.contains(v), "{v}");
            assert!(!crate::callers::is_verb(v), "{v}");
        }
    }
}
