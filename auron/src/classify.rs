//! Builds the observation JSON exactly as auron.sh's own heredoc did, classifies it with
//! `judge` (the pure function) and parses back the firing records.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};


#[derive(Debug, Clone, Default)]
pub struct Mirror {
    pub configured: bool,
    pub exists: bool,
    pub mtime: i64,
    pub path: String,
    pub exporter: String,
}

#[derive(Debug, Clone, Default)]
pub struct RestartAlert {
    pub unit: String,
    pub current: i64,
    pub baseline: i64,
    pub delta: i64,
    pub window: i64,
}

#[derive(Debug, Clone, Default)]
pub struct Thresholds {
    pub pass_stale: i64,
    pub starve_passes: i64,
    pub mirror_stale: i64,
    pub ghost_stale: i64,
    pub restart_threshold: i64,
    pub restart_window: i64,
}

#[derive(Debug, Clone, Default)]
pub struct Observation {
    pub now: i64,
    pub auron_first: i64,
    pub sentinel_log: String,
    pub sentinel_log_readable: bool,
    pub sentinel_log_error: String,
    pub sentinel_log_mtime: i64,
    pub sentinel_log_path: String,
    pub sentinel_timer: String,
    pub db_reachable: bool,
    pub db_error: String,
    pub db_path: String,
    pub fallback_path: String,
    pub mirror: Mirror,
    /// Opaque — strand.sh's own episode JSON, passed through verbatim (auron.sh read it
    /// straight off disk and never inspected its shape).
    pub strands: Value,
    pub restart_alerts: Vec<RestartAlert>,
    pub world_halted: bool,
    pub world_halt_at: i64,
    pub world_draining: bool,
    pub world_drain_at: i64,
    pub thresholds: Thresholds,
}

impl Observation {
    pub fn to_json(&self) -> Value {
        json!({
            "now": self.now,
            "auron_first": self.auron_first,
            "sentinel_log": self.sentinel_log,
            "sentinel_log_readable": self.sentinel_log_readable,
            "sentinel_log_error": self.sentinel_log_error,
            "sentinel_log_mtime": self.sentinel_log_mtime,
            "sentinel_log_path": self.sentinel_log_path,
            "sentinel_timer": self.sentinel_timer,
            "db_reachable": self.db_reachable,
            "db_error": self.db_error,
            "db_path": self.db_path,
            "fallback_path": self.fallback_path,
            "mirror": {
                "configured": self.mirror.configured,
                "exists": self.mirror.exists,
                "mtime": self.mirror.mtime,
                "path": self.mirror.path,
                "exporter": self.mirror.exporter,
            },
            "strands": self.strands,
            "restart_alerts": self.restart_alerts.iter().map(|a| json!({
                "unit": a.unit, "current": a.current, "baseline": a.baseline,
                "delta": a.delta, "window": a.window,
            })).collect::<Vec<_>>(),
            "world_halted": self.world_halted,
            "world_halt_at": self.world_halt_at,
            "world_draining": self.world_draining,
            "world_drain_at": self.world_drain_at,
            "thresholds": {
                "pass_stale": self.thresholds.pass_stale,
                "starve_passes": self.thresholds.starve_passes,
                "mirror_stale": self.thresholds.mirror_stale,
                "ghost_stale": self.thresholds.ghost_stale,
                "restart_threshold": self.thresholds.restart_threshold,
                "restart_window": self.thresholds.restart_window,
            },
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Firing {
    pub key: String,
    pub title: String,
    pub evidence: String,
}

/// Classifies the observation in-process. An unusable observation yields nothing — a
/// caller must treat that as "nothing known to be firing", never as "confirmed clear".
pub fn run(obs: &Observation) -> Vec<Firing> {
    crate::judge::classify(&obs.to_json()).unwrap_or_default()
}

/// The `auron classify` entry: one observation object on stdin, one sorted-key JSON alert per
/// line on stdout.
pub fn classify_stdin(input: &str) -> Result<String, String> {
    let o: Value = serde_json::from_str(input).map_err(|e| format!("unreadable observations: {e}"))?;
    let firing = crate::judge::classify(&o)?;
    let mut out = String::new();
    for f in firing {
        let v: std::collections::BTreeMap<&str, &str> = [("key", f.key.as_str()), ("title", f.title.as_str()), ("evidence", f.evidence.as_str())].into_iter().collect();
        out.push_str(&serde_json::to_string(&v).unwrap_or_default());
        out.push('\n');
    }
    Ok(out)
}

pub fn parse_firing_lines(text: &str) -> Vec<Firing> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(f) = serde_json::from_str::<Firing>(line) {
            out.push(f);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_json_shape_matches_the_classifier_contract() {
        let obs = Observation { now: 1000, auron_first: 500, thresholds: Thresholds { pass_stale: 600, starve_passes: 5, mirror_stale: 90000, ghost_stale: 1800, restart_threshold: 5, restart_window: 3600 }, ..Default::default() };
        let v = obs.to_json();
        assert_eq!(v["now"], 1000);
        assert_eq!(v["auron_first"], 500);
        assert_eq!(v["thresholds"]["pass_stale"], 600);
        assert_eq!(v["mirror"]["configured"], false);
        assert_eq!(v["restart_alerts"], serde_json::json!([]));
    }

    #[test]
    fn parses_multiple_firing_lines() {
        let text = "{\"key\":\"a\",\"title\":\"A\",\"evidence\":\"e1\"}\n{\"key\":\"b\",\"title\":\"B\",\"evidence\":\"e2\"}\n";
        let f = parse_firing_lines(text);
        assert_eq!(f, vec![Firing { key: "a".into(), title: "A".into(), evidence: "e1".into() }, Firing { key: "b".into(), title: "B".into(), evidence: "e2".into() }]);
    }

    #[test]
    fn a_malformed_line_is_skipped_not_fatal() {
        let text = "not json at all\n{\"key\":\"a\",\"title\":\"A\",\"evidence\":\"e\"}\n";
        assert_eq!(parse_firing_lines(text), vec![Firing { key: "a".into(), title: "A".into(), evidence: "e".into() }]);
    }

    #[test]
    fn empty_output_is_nothing_firing() {
        assert_eq!(parse_firing_lines(""), Vec::<Firing>::new());
    }

    #[test]
    fn classify_stdin_emits_sorted_key_json_lines() {
        let out = classify_stdin("{\"now\": 1, \"sentinel_log_readable\": false}").unwrap();
        let v: Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(v["key"], "sentinel-unobservable");
        assert!(out.starts_with("{\"evidence\""));
        assert!(classify_stdin("not json").is_err());
    }
}
