//! The virtual-time seam: `SPIRA_NOW` (epoch seconds) overrides the wall clock.

use std::time::{SystemTime, UNIX_EPOCH};

pub const VAR: &str = "SPIRA_NOW";

pub fn override_from(raw: Option<&str>) -> Option<u64> {
    raw.and_then(|v| v.trim().parse().ok())
}

pub fn override_epoch() -> Option<u64> {
    override_from(std::env::var(VAR).ok().as_deref())
}

pub fn wall_epoch() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub const GIT_DATE_VARS: [&str; 2] = ["GIT_AUTHOR_DATE", "GIT_COMMITTER_DATE"];

/// What a spawn that clears its environment must still hand down for virtual time to
/// reach the child: `SPIRA_NOW` and the two git date variables, each only if set here.
pub fn passthrough() -> Vec<(&'static str, String)> {
    std::iter::once(VAR)
        .chain(GIT_DATE_VARS)
        .filter_map(|k| std::env::var(k).ok().map(|v| (k, v)))
        .collect()
}

/// The environment the simulator gives an actor run at virtual time `now` (epoch seconds).
pub fn actor_env(now: u64) -> Vec<(&'static str, String)> {
    let date = format!("{now} +0000");
    vec![(VAR, now.to_string()), (GIT_DATE_VARS[0], date.clone()), (GIT_DATE_VARS[1], date)]
}

/// Runs `f` with `SPIRA_NOW` set to `now`, serialised against other callers in this process.
#[doc(hidden)]
pub fn with_now_for_test<R>(now: u64, f: impl FnOnce() -> R) -> R {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var(VAR, now.to_string());
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::env::remove_var(VAR);
    match r {
        Ok(r) => r,
        Err(e) => std::panic::resume_unwind(e),
    }
}

/// Epoch seconds: `SPIRA_NOW` when set and numeric, the wall clock otherwise.
pub fn now_epoch() -> u64 {
    override_epoch().unwrap_or_else(wall_epoch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_parses_numeric_and_rejects_the_rest() {
        assert_eq!(override_from(Some("1700000000")), Some(1_700_000_000));
        assert_eq!(override_from(Some(" 5 ")), Some(5));
        assert_eq!(override_from(Some("soon")), None);
        assert_eq!(override_from(None), None);
    }

    #[test]
    fn now_epoch_reads_the_override_and_falls_back_to_the_wall() {
        assert_eq!(with_now_for_test(42, now_epoch), 42);
        assert!(now_epoch() > 1_700_000_000);
    }
}
