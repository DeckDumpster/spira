//! The conformance suite against a live file: every scenario runs on a [`Scoped`] view of the
//! store under `conformance/<run-id>/`, and the prefix is deleted afterwards. The store's real
//! keys are never read or written.

use std::panic::{catch_unwind, AssertUnwindSafe};

use state_store::conformance::{self as suite, Rig};
use state_store::{Backend, Error, LogEntry, Result, Version, Write};

use crate::SqliteBackend;

pub const SCRATCH_PREFIX: &str = "conformance/";

/// Scenarios a live run leaves out: compaction is store-wide, and would drop the live store's
/// own watch log.
pub const LIVE_EXCLUDED: &[&str] = &["watch_returns_too_old_after_compaction"];

/// One backend seen through a key prefix: every key it reads or writes is `scope` + key.
/// Revisions stay the store's own, and the scope is new at `base`, the head it was made at, so
/// its watch log starts there: a live store's older revisions may be compacted, and are never
/// the scope's. `gc_range` keeps the default, which filters the scope's own `events/`.
#[derive(Clone)]
pub struct Scoped<B: Backend + Clone> {
    inner: B,
    scope: String,
    base: u64,
}

impl<B: Backend + Clone> Scoped<B> {
    pub fn new(inner: B, scope: &str) -> Result<Self> {
        Ok(Scoped {
            base: inner.head()?,
            inner,
            scope: scope.to_string(),
        })
    }

    fn outer(&self, key: &str) -> String {
        format!("{}{key}", self.scope)
    }

    fn strip(&self, rows: Vec<(String, Version)>) -> Vec<(String, Version)> {
        rows.into_iter()
            .filter_map(|(k, v)| k.strip_prefix(&self.scope).map(|k| (k.to_string(), v)))
            .collect()
    }
}

impl<B: Backend + Clone> Backend for Scoped<B> {
    fn head(&self) -> Result<u64> {
        self.inner.head()
    }

    fn get(&self, key: &str, at: u64) -> Result<Option<Version>> {
        self.inner.get(&self.outer(key), at)
    }

    fn range(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        let after = after.map(|a| self.outer(a));
        let rows = self
            .inner
            .range(&self.outer(prefix), after.as_deref(), limit, at)?;
        Ok(self.strip(rows))
    }

    fn commit(&self, expect: u64, batch: &[Write]) -> Result<u64> {
        let batch: Vec<Write> = batch
            .iter()
            .map(|w| match w.clone() {
                Write::Put {
                    key,
                    value,
                    create_rev,
                    prev,
                } => Write::Put {
                    key: self.outer(&key),
                    value,
                    create_rev,
                    prev,
                },
                Write::Delete { key, prev } => Write::Delete {
                    key: self.outer(&key),
                    prev,
                },
            })
            .collect();
        self.inner.commit(expect, &batch)
    }

    /// The scope's entries of the first page of the store's log that has any, so a page of
    /// other keys never reads as caught up.
    fn log(&self, from: u64, max_mains: usize) -> Result<Vec<LogEntry>> {
        let mut from = from.max(self.base);
        loop {
            let page = self.inner.log(from, max_mains)?;
            let Some(last) = page.last().map(|e| e.rev.main) else {
                return Ok(Vec::new());
            };
            let mine: Vec<LogEntry> = page
                .into_iter()
                .filter_map(|mut e| {
                    e.key = e.key.strip_prefix(&self.scope)?.to_string();
                    Some(e)
                })
                .collect();
            if !mine.is_empty() {
                return Ok(mine);
            }
            from = last;
        }
    }

    fn compact(&self, _rev: u64) -> Result<()> {
        Err(Error::Backend(
            "a scoped run never compacts the store it shares".into(),
        ))
    }
}

/// Every scenario of the suite but [`LIVE_EXCLUDED`], by name.
macro_rules! live_scenarios {
    ($($name:ident),* $(,)?) => {
        pub const LIVE_SCENARIOS: &[&str] = &[$(stringify!($name)),*];
        fn scenarios<R: Rig>() -> Vec<(&'static str, fn(&R))> {
            vec![$((stringify!($name), suite::$name::<R> as fn(&R))),*]
        }
    };
}

live_scenarios!(
    cas_conflict_on_a_stale_mod_rev,
    a_refused_txn_writes_nothing_but_its_events_key_carrying_the_refusal,
    one_main_per_txn_with_main_sub_per_key,
    the_next_main_stays_greater_after_deleting_the_newest_keys_and_reopening,
    create_with_an_equal_value_succeeds_and_a_different_value_conflicts,
    list_pages_and_list_rev_come_from_one_snapshot_while_a_commit_lands_between_pages,
    watch_resumes_from_a_revision,
    a_watch_on_a_quiet_prefix_returns_progress_rev,
    a_lock_expires_on_the_virtual_clock,
    a_paused_holders_fenced_write_is_refused_after_expiry_and_reacquisition,
    reopen_extends_every_lock,
    a_lock_ttl_below_the_minimum_is_refused,
    a_finalizer_blocks_delete_until_gc_removes_the_key_once_cleared,
    an_owner_reference_orphan_is_collected,
    a_request_retry_after_an_ambiguous_commit_reports_already_applied,
    retries_of_lock_create_delete_and_a_refusal_report_the_original_outcome,
    request_keys_expire_at_24h_of_virtual_time,
    a_txn_touching_only_telemetry_writes_no_events_key_and_its_refusal_stores_nothing,
    a_txn_touching_telemetry_and_intents_writes_one_events_key_with_both_sub_events,
    a_put_under_intents_alone_writes_one_events_key,
    a_put_under_telemetry_or_intents_without_ttl_secs_is_refused,
    ttl_keys_are_gone_after_their_ttl_and_present_one_tick_before,
    a_rewrite_restarts_a_keys_ttl,
    reopen_restarts_every_ttl,
    a_gc_expiry_writes_no_events_key,
    a_multi_prefix_list_reads_one_snapshot_while_a_commit_lands_between_prefixes,
    txn_ops_at_the_limit_pass_and_one_over_is_refused,
    txn_compares_at_the_limit_pass_and_one_over_is_refused,
    a_cut_at_the_member_cap_passes_the_limits_and_one_more_is_refused,
    request_bytes_at_the_limit_pass_and_one_over_is_refused,
    a_key_twice_in_one_txn_is_refused,
    caller_writes_to_the_contracts_own_prefixes_are_refused,
    a_txn_that_acquires_a_lock_and_writes_a_fenced_key_applies_whole_or_not_at_all,
    a_txn_acquiring_a_lock_held_by_another_holder_fails_whole,
    lock_check_fails_the_txn_after_the_lock_expired_or_was_retaken,
    get_and_list_return_a_locks_expiry_and_renew_moves_it_forward,
    a_renew_under_a_new_ttl_keeps_the_token_and_moves_the_expiry,
    a_state_filtered_range_reads_the_keys_in_that_state_at_the_head_and_below_it,
);

/// Each scenario's store is its own sub-prefix of the run's scope.
struct LiveRig {
    backend: SqliteBackend,
    scope: String,
    n: std::sync::atomic::AtomicUsize,
}

impl Rig for LiveRig {
    type B = Scoped<SqliteBackend>;
    fn fresh(&self) -> Self::B {
        let n = self.n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Scoped::new(self.backend.clone(), &format!("{}{n}/", self.scope)).expect("head")
    }
    fn reopen(&self, b: &Self::B) -> Self::B {
        b.clone()
    }
}

#[derive(Debug, Default)]
pub struct LiveReport {
    pub passed: Vec<&'static str>,
    pub failed: Vec<(&'static str, String)>,
    /// Keys under the run's scope deleted afterwards.
    pub removed: usize,
}

/// Runs every live scenario on `backend` under `conformance/<run_id>/`, then deletes every key
/// left under that prefix. A failing scenario is reported, never fatal to the cleanup.
pub fn run(backend: &SqliteBackend, run_id: &str) -> Result<LiveReport> {
    let scope = format!("{SCRATCH_PREFIX}{run_id}/");
    if !backend.range(&scope, None, 1, backend.head()?)?.is_empty() {
        return Err(Error::Backend(format!("{scope} already holds keys")));
    }
    let rig = LiveRig {
        backend: backend.clone(),
        scope: scope.clone(),
        n: Default::default(),
    };
    let mut report = LiveReport::default();
    for (name, scenario) in scenarios::<LiveRig>() {
        match catch_unwind(AssertUnwindSafe(|| scenario(&rig))) {
            Ok(()) => report.passed.push(name),
            Err(p) => {
                let why = p
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                report.failed.push((name, why));
            }
        }
    }
    report.removed = remove_prefix(backend, &scope)?;
    Ok(report)
}

/// Deletes every live key under `prefix` in one commit at the head.
fn remove_prefix(backend: &SqliteBackend, prefix: &str) -> Result<usize> {
    loop {
        let head = backend.head()?;
        let keys = backend.range(prefix, None, usize::MAX, head)?;
        if keys.is_empty() {
            return Ok(0);
        }
        let batch: Vec<Write> = keys
            .iter()
            .map(|(k, v)| Write::Delete {
                key: k.clone(),
                prev: v.mod_rev,
            })
            .collect();
        match backend.commit(head, &batch) {
            Ok(_) => return Ok(batch.len()),
            Err(Error::Stale { .. }) => continue,
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_live_run_names_every_scenario_but_the_excluded() {
        let mut named: Vec<&str> = LIVE_SCENARIOS
            .iter()
            .chain(LIVE_EXCLUDED)
            .copied()
            .collect();
        let mut all = suite::SCENARIOS.to_vec();
        named.sort_unstable();
        all.sort_unstable();
        assert_eq!(named, all);
    }
}
