//! The IO seam: persists [`crate::core::HysteresisState`] across passes (one JSON file,
//! replacing a flat marker file per invariant) and appends each pass's verdict to a JSONL
//! time series. Every function here does exactly one read or one write — the diff and
//! hysteresis logic that decides what to persist lives in [`crate::core`], not here.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;
use tsd::store::Pusher;

use crate::core::{HysteresisState, RawStatus, Verdict};

pub type StateMap = BTreeMap<String, HysteresisState>;

/// Loads the persisted hysteresis state. A missing or unparsable file is an empty map —
/// the first pass after a fresh install or a corrupt file starts every invariant's streak
/// over, which is safe: it can only delay an escalation by one grace period, never hide one.
pub fn load_state(path: &Path) -> StateMap {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_state(path: &Path, state: &StateMap) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(state).unwrap_or_else(|_| "{}".to_string());
    fs::write(path, json)
}

/// Which streak (`since`) each invariant last alerted for — the persisted half of
/// [`crate::alert::should_alert`]'s dedup, shared by every caller of the alert path
/// (reconciler-alert, reconciler-flow) instead of each keeping its own copy of this file
/// format.
pub type AlertedSinceMap = BTreeMap<String, u64>;

/// A missing or unparsable file is "nothing alerted yet" — the same fail-safe reading
/// [`load_state`] gives HysteresisState: it can only cost one redundant alert, never
/// suppress one that was owed.
pub fn load_alerted(path: &Path) -> AlertedSinceMap {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_alerted(path: &Path, state: &AlertedSinceMap) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(state).unwrap_or_else(|_| "{}".to_string());
    fs::write(path, json)
}

#[derive(Serialize)]
struct StatusRecord<'a> {
    key: &'a str,
    status: &'static str,
    desired: Option<&'a str>,
    observed: Option<&'a str>,
    reason: Option<&'a str>,
    since: Option<u64>,
    is_gap: bool,
    just_closed: bool,
    remedy_failed: bool,
}

/// Pushes one row per invariant per pass to the `reconciler-status` family — every resource's
/// status is recorded every pass, not only when it changes, so a gap in the series itself is
/// visible. A push that fails is dropped: the pass does not wait on its own record.
pub fn append_status(store: &Pusher, now_iso: &str, key: &str, verdict: &Verdict) {
    let (status, desired, observed, reason) = match &verdict.status {
        RawStatus::Satisfied => ("satisfied", None, None, None),
        RawStatus::Gap { desired, observed, .. } => ("gap", Some(desired.as_str()), Some(observed.as_str()), None),
        RawStatus::Unobservable { reason } => ("unobservable", None, None, Some(reason.as_str())),
        RawStatus::Deliberate { reason } => ("deliberate", None, None, Some(reason.as_str())),
    };
    let record = StatusRecord {
        key,
        status,
        desired,
        observed,
        reason,
        since: verdict.since,
        is_gap: verdict.is_gap,
        just_closed: verdict.just_closed,
        remedy_failed: verdict.remedy_failed,
    };
    push(store, STATUS_FAMILY, now_iso, &record);
}

pub const STATUS_FAMILY: &str = "reconciler-status";

/// One `family` row from a serializable record, through `store`; the record's own fields are the row's.
pub fn push<T: Serialize>(store: &Pusher, family: &str, now_iso: &str, record: &T) {
    let Ok(Value::Object(fields)) = serde_json::to_value(record) else { return };
    let fields: Vec<(String, Value)> = fields.into_iter().collect();
    let _ = store.push(family, now_iso, &fields);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::step;

    #[test]
    fn round_trips_through_a_file() {
        let dir = testkit::TempDir::new("reconciler-engine-io-test");
        let state_path = dir.join("state.json");

        let mut state = load_state(&state_path);
        assert!(state.is_empty(), "missing file loads as empty, not an error");

        let raw = RawStatus::Gap { desired: "d".into(), observed: "o".into(), since_hint: None };
        let (verdict, next) = step(100, raw, 0, HysteresisState::default());
        state.insert("k".to_string(), next);
        save_state(&state_path, &state).unwrap();

        let reloaded = load_state(&state_path);
        assert_eq!(reloaded.get("k"), state.get("k"));

        let store = spira_otel::fixture::FakeStore::start();
        append_status(&Pusher::new(Some(store.addr().to_string())), "2026-09-25T00:00:00Z", "k", &verdict);
        let rows = tsd::store::Reader::new(Some(store.addr())).unwrap().rows(STATUS_FAMILY, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0]["status"].as_str(), rows[0]["key"].as_str(), rows[0]["since"].as_u64()), (Some("gap"), Some("k"), verdict.since));
        assert_eq!(rows[0]["ts"], "2026-09-25T00:00:00Z");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn alerted_since_round_trips_through_a_file() {
        let dir = testkit::TempDir::new("reconciler-engine-io-alerted-test");
        let alerted_path = dir.join("alerted.json");

        let mut alerted = load_alerted(&alerted_path);
        assert!(alerted.is_empty(), "missing file loads as empty, not an error");

        alerted.insert("flow:backlog".to_string(), 100);
        save_alerted(&alerted_path, &alerted).unwrap();

        let reloaded = load_alerted(&alerted_path);
        assert_eq!(reloaded.get("flow:backlog"), Some(&100));

        fs::remove_dir_all(&dir).ok();
    }
}
