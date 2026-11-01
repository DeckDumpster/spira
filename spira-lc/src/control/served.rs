//! The control store at `SPIRA_LC_STORE_PATH`, opened once per process on first use and held
//! for the process's life: its opener is exclusive, so in practice lc-serve holds it and any
//! other opener answers "cannot tell". Unset, no verb reaches it and every verb keeps its dolt
//! path.

use std::path::Path;
use std::sync::{Arc, Mutex};

use state_store::{Config, Contract, MonotonicClock, Ports};
use state_store_sqlite::{Options, SqliteBackend};

use super::store::Store;
use super::verbs;
use crate::CANNOT_TELL;

/// No verb served here takes a lock; the floor only satisfies the contract's config.
const LOCK_FLOOR_MS: u64 = 1_000;

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

fn open(path: &str) -> Result<Store<SqliteBackend>, String> {
    // Opening creates a missing file, which would answer every read as an empty store.
    if !Path::new(path).is_file() {
        return Err(format!("no control store at {path}"));
    }
    let backend = SqliteBackend::open(Path::new(path), Options::default())
        .map_err(|e| format!("{path}: {e}"))?;
    let ports = Ports {
        mono: Arc::new(MonotonicClock::new()),
        wall: Arc::new(runtime::ports::WallClock),
    };
    let contract = Contract::open(
        backend,
        ports,
        Config {
            min_lock_ttl_ms: LOCK_FLOOR_MS,
        },
    )
    .map_err(|e| format!("{path}: {e}"))?;
    Ok(Store::new(contract, LOCK_FLOOR_MS))
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

/// The control store's answer to `args`, or `None` when the dolt path answers it: a verb this
/// store does not serve, or any verb while the key is unset. `watch` has no dolt path.
pub fn dispatch(args: &[String]) -> Option<(i32, String)> {
    let verb = args.first()?;
    if !verbs::VERBS.contains(&verb.as_str()) {
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
        Ok(store) => verbs::run(&store, args),
        Err(e) => (CANNOT_TELL, format!("cannot tell: the control store: {e}")),
    })
}
