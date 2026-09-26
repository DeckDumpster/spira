//! The IO seam: persists [`crate::core::HysteresisState`] across passes (one JSON file,
//! replacing a flat marker file per invariant) and appends each pass's verdict to a JSONL
//! time series. Every function here does exactly one read or one write — the diff and
//! hysteresis logic that decides what to persist lives in [`crate::core`], not here.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use serde::Serialize;

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

#[derive(Serialize)]
struct StatusRecord<'a> {
    ts: &'a str,
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

/// Appends one line per invariant per pass to the time series — every resource's status is
/// recorded every pass, not only when it changes, so a gap in the series itself is visible.
pub fn append_status(path: &Path, now_iso: &str, key: &str, verdict: &Verdict) {
    let (status, desired, observed, reason) = match &verdict.status {
        RawStatus::Satisfied => ("satisfied", None, None, None),
        RawStatus::Gap { desired, observed, .. } => ("gap", Some(desired.as_str()), Some(observed.as_str()), None),
        RawStatus::Unobservable { reason } => ("unobservable", None, None, Some(reason.as_str())),
    };
    let record = StatusRecord {
        ts: now_iso,
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
    let line = match serde_json::to_string(&record) {
        Ok(l) => l,
        Err(_) => return,
    };
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{}", line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::step;

    #[test]
    fn round_trips_through_a_file() {
        let dir = std::env::temp_dir().join(format!("reconciler-engine-io-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("state.json");

        let mut state = load_state(&state_path);
        assert!(state.is_empty(), "missing file loads as empty, not an error");

        let raw = RawStatus::Gap { desired: "d".into(), observed: "o".into(), since_hint: None };
        let (verdict, next) = step(100, raw, 0, HysteresisState::default());
        state.insert("k".to_string(), next);
        save_state(&state_path, &state).unwrap();

        let reloaded = load_state(&state_path);
        assert_eq!(reloaded.get("k"), state.get("k"));

        let status_path = dir.join("status.jsonl");
        append_status(&status_path, "2026-09-25T00:00:00Z", "k", &verdict);
        let contents = fs::read_to_string(&status_path).unwrap();
        assert!(contents.contains("\"status\":\"gap\""));
        assert_eq!(contents.lines().count(), 1);

        fs::remove_dir_all(&dir).ok();
    }
}
