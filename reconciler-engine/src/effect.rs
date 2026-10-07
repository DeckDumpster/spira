//! Remedy change telemetry and effect measurement. Every remedy that acts leaves one
//! [`RemedyEvent`]; each declares the invariant metric it should lower, and the passes after
//! it read that metric again. A metric that has not dropped within `max_passes` is a remedy
//! with no effect, and the caller mails both readings. Lower is better: a gap indicator is
//! 1 while open and 0 once closed. Nothing here reads a clock, a file or a socket except
//! [`append_event`], [`load_pending`] and [`save_pending`], the IO seam at the bottom.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The kind of an invariant key: everything before the first `:` (`disk:/var` is kind `disk`).
pub fn kind_of(key: &str) -> &str {
    key.split(':').next().unwrap_or(key)
}

/// True when the kill switch names this kind. `*` names every kind. A shadowed kind is
/// observed and recorded but its remedy does not run.
pub fn is_shadowed(shadow_kinds: &[String], kind: &str) -> bool {
    shadow_kinds.iter().any(|k| k == "*" || k == kind)
}

/// A remedy that acted and has not yet been judged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PendingEffect {
    pub action: String,
    pub metric: String,
    pub before: f64,
    pub acted_at: u64,
    pub passes_seen: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Not enough passes yet to call it either way.
    Pending(PendingEffect),
    Effect { before: f64, after: f64 },
    NoEffect { before: f64, after: f64 },
}

/// Judges one pending remedy against this pass's reading of its metric. A drop is an effect
/// at once; no drop is only called `NoEffect` once `max_passes` passes have read it.
pub fn measure(pending: PendingEffect, after: f64, max_passes: u32) -> Outcome {
    if after < pending.before {
        return Outcome::Effect { before: pending.before, after };
    }
    let seen = pending.passes_seen + 1;
    if seen >= max_passes {
        return Outcome::NoEffect { before: pending.before, after };
    }
    Outcome::Pending(PendingEffect { passes_seen: seen, ..pending })
}

/// The evidence-carrying body mailed to the Concierge for a remedy with no effect.
pub fn compose_no_effect(key: &str, pending: &PendingEffect, after: f64, now: u64, max_passes: u32) -> String {
    format!(
        "remedy: {}\ninvariant: {}\nmetric: {}\nbefore: {}\nafter: {}\nacted: {}s ago\nthe metric did not move within {} passes\n",
        pending.action,
        key,
        pending.metric,
        pending.before,
        after,
        now.saturating_sub(pending.acted_at),
        max_passes
    )
}

/// One line of the remedy time series. `event` is `remedy` when a remedy acts, `shadow`
/// when the kill switch held it back, and `effect` / `no-effect` when it is judged.
#[derive(Serialize)]
pub struct RemedyEvent<'a> {
    pub ts: &'a str,
    pub event: &'a str,
    pub kind: &'a str,
    pub key: &'a str,
    pub action: &'a str,
    pub metric: &'a str,
    pub before: f64,
    pub after: Option<f64>,
    pub release: &'a str,
}

pub fn append_event(path: &Path, event: &RemedyEvent) {
    let Ok(line) = serde_json::to_string(event) else { return };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{}", line);
    }
}

pub type PendingMap = BTreeMap<String, PendingEffect>;

/// A missing or unparsable file is "nothing pending": a remedy forgotten here is never
/// judged, which loses one measurement and never a remedy or an escalation.
pub fn load_pending(path: &Path) -> PendingMap {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_pending(path: &Path, pending: &PendingMap) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(pending).unwrap_or_else(|_| "{}".to_string());
    fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(before: f64) -> PendingEffect {
        PendingEffect { action: "restart x".into(), metric: "gap".into(), before, acted_at: 100, passes_seen: 0 }
    }

    #[test]
    fn kind_is_the_key_prefix() {
        assert_eq!(kind_of("disk:/var"), "disk");
        assert_eq!(kind_of("release"), "release");
    }

    #[test]
    fn the_kill_switch_names_kinds_or_everything() {
        let some = vec!["disk".to_string()];
        assert!(is_shadowed(&some, "disk"));
        assert!(!is_shadowed(&some, "units-timer"));
        assert!(is_shadowed(&["*".to_string()], "units-timer"));
        assert!(!is_shadowed(&[], "disk"));
    }

    #[test]
    fn a_drop_in_the_metric_is_an_effect() {
        assert_eq!(measure(pending(1.0), 0.0, 3), Outcome::Effect { before: 1.0, after: 0.0 });
    }

    #[test]
    fn no_drop_is_pending_until_the_pass_budget_is_spent_then_no_effect() {
        let Outcome::Pending(p1) = measure(pending(1.0), 1.0, 3) else { panic!("pass 1 must stay pending") };
        let Outcome::Pending(p2) = measure(p1, 1.0, 3) else { panic!("pass 2 must stay pending") };
        assert_eq!(measure(p2, 1.0, 3), Outcome::NoEffect { before: 1.0, after: 1.0 });
    }

    #[test]
    fn a_rise_in_the_metric_is_no_effect_not_effect() {
        assert_eq!(measure(pending(1.0), 2.0, 1), Outcome::NoEffect { before: 1.0, after: 2.0 });
    }

    #[test]
    fn the_no_effect_body_carries_both_readings() {
        let body = compose_no_effect("disk:/var", &pending(1.0), 1.0, 160, 2);
        assert!(body.contains("before: 1") && body.contains("after: 1"));
        assert!(body.contains("restart x") && body.contains("disk:/var") && body.contains("60s ago"));
    }

    #[test]
    fn events_and_pending_round_trip_through_files() {
        let dir = testkit::TempDir::new("reconciler-engine-effect-test");
        let path = dir.join("tsd").join("events.jsonl");
        let ev = RemedyEvent {
            ts: "2026-10-07T00:00:00Z",
            event: "remedy",
            kind: "disk",
            key: "disk:/var",
            action: "bash disk-remedy.sh",
            metric: "gap",
            before: 1.0,
            after: None,
            release: "r1",
        };
        append_event(&path, &ev);
        let line = fs::read_to_string(&path).unwrap();
        assert!(line.contains("\"kind\":\"disk\"") && line.contains("\"release\":\"r1\"") && line.contains("\"before\":1.0"));

        let pp = dir.join("pending.json");
        assert!(load_pending(&pp).is_empty());
        let mut m = PendingMap::new();
        m.insert("disk:/var".into(), pending(1.0));
        save_pending(&pp, &m).unwrap();
        assert_eq!(load_pending(&pp), m);
    }
}
