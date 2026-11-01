//! The control store at `SPIRA_LC_STORE_PATH`, opened once per process on first use and held
//! for the process's life: its opener is exclusive, so in practice lc-serve holds it and any
//! other opener answers "cannot tell". Unset, no verb reaches it and every verb keeps its dolt
//! path.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use runtime::ports::Clock;
use state_store::{Config, Contract, MonotonicClock, Ports};
use state_store_sqlite::{Options, SqliteBackend};

use super::store::Store;
use super::{sections, state, verbs};
use crate::CANNOT_TELL;

/// A failed open is not kept, so the next call tries again.
static OPENED: Mutex<Option<Arc<Store<SqliteBackend>>>> = Mutex::new(None);

/// The store path the config declares: `None` while the key is unset or the config is
/// unreadable, so the dolt path answers and reports its own config error.
fn store_path() -> Option<Result<String, String>> {
    let path = spira_config::process::cfg("SPIRA_LC_STORE_PATH")
        .ok()
        .filter(|p| !p.is_empty())?;
    let confined = || -> Result<(), String> {
        let instance = spira_config::process::cfg("SPIRA_INSTANCE")?;
        let workspaces = spira_config::process::cfg("SPIRA_WORKSPACES")?;
        spira_config::containment::check_path(&instance, &workspaces, "SPIRA_LC_STORE_PATH", &path)
    };
    Some(confined().map(|()| path.clone()))
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

fn open(path: &str) -> Result<Store<SqliteBackend>, String> {
    // Opening creates a missing file, which would answer every read as an empty store.
    if !Path::new(path).is_file() {
        return Err(format!("no control store at {path}"));
    }
    let backend = SqliteBackend::open(Path::new(path), Options::default())
        .map_err(|e| format!("{path}: {e}"))?;
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
    Ok(Store::new(contract, floor))
}

fn opened(path: &str) -> Result<Arc<Store<SqliteBackend>>, String> {
    let mut slot = OPENED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = slot.as_ref() {
        return Ok(Arc::clone(s));
    }
    let s = Arc::new(open(path)?);
    *slot = Some(Arc::clone(&s));
    Ok(s)
}

/// The store the config declares, opened: `None` while the key is unset, so lc-serve serves
/// from dolt alone.
pub fn configured() -> Option<Result<Arc<Store<SqliteBackend>>, String>> {
    Some(store_path()?.and_then(|p| opened(&p)))
}

/// A request id no other call in any process mints: each call, a retry included, is its own
/// request, so a request that lost its race is never answered again as if it had not.
fn request_id() -> String {
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

/// The control store's answer to `args`, or `None` when the dolt path answers it: a verb this
/// store does not serve, or any verb while the key is unset. `watch` has no dolt path.
pub fn dispatch(args: &[String]) -> Option<(i32, String)> {
    let verb = args.first()?;
    if sections::VERBS.contains(&verb.as_str()) {
        return Some(dispatch_sections(args));
    }
    let lifecycle = state::VERBS.contains(&verb.as_str());
    if !lifecycle && !verbs::VERBS.contains(&verb.as_str()) {
        return None;
    }
    let path = match store_path() {
        Some(Ok(p)) => p,
        Some(Err(e)) => return Some((CANNOT_TELL, format!("cannot tell: {e}"))),
        None if verb == "watch" => return Some((
            CANNOT_TELL,
            "cannot tell: watch is served by the control store, and SPIRA_LC_STORE_PATH is unset or unreadable"
                .into(),
        )),
        None => return None,
    };
    Some(match opened(&path) {
        Ok(store) if lifecycle => {
            let request = request_id();
            let call = state::Call {
                now: crate::db::now_epoch(),
                request: &request,
            };
            state::run(&store, args, &call)
        }
        Ok(store) => verbs::run(&store, args),
        Err(e) => (CANNOT_TELL, format!("cannot tell: the control store: {e}")),
    })
}

/// `section`, `intent`, `lock` and `publish-section`, which only the control store serves: JSON
/// on every path.
fn dispatch_sections(args: &[String]) -> (i32, String) {
    let path = match store_path() {
        Some(Ok(p)) => p,
        Some(Err(e)) => return sections::cannot_tell(&e),
        None => {
            return sections::cannot_tell(&format!(
                "{} is served by the control store, and SPIRA_LC_STORE_PATH is unset or unreadable",
                args[0]
            ))
        }
    };
    match opened(&path) {
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
                return sections::run(&store, args, &call);
            }
            match crate::lifecycle_run_dir() {
                Ok(run) => sections::lifecycle::run(&store, args, &call, &sections::lifecycle::RunDir(run)),
                Err(e) => sections::cannot_tell(&format!("the run directory: {e}")),
            }
        }
        Err(e) => sections::cannot_tell(&format!("the control store: {e}")),
    }
}
