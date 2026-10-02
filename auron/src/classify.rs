//! Builds the observation JSON exactly as auron.sh's own heredoc did, hands it to
//! `auron-classify.py` unchanged (out of this bead's scope, per sp-zpaq0's own inventory —
//! it is a pure function with its own suite, test-auron-classify.sh, untouched here), and
//! parses back the JSON-lines firing records.
//!
//! `auron.sh gathers -> auron-classify.py classifies -> auron.sh speaks` — this crate is
//! everything on either side of the arrow; the arrow itself is unchanged.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::util::{self, Out, Spec};

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
    pub journal: String,
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
                "delta": a.delta, "window": a.window, "journal": a.journal,
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

/// Runs `auron-classify.py` (bare name, launcher PATH) with the observation on stdin,
/// parsing its JSON-lines stdout. `None` on a classifier failure (unreadable/unparseable
/// output) — a caller must treat that as "nothing known to be firing", never as "confirmed
/// clear", exactly as auron.sh's own `2>/dev/null` swallowed a classifier crash into an
/// empty `firing_json`.
pub fn run(obs: &Observation) -> Vec<Firing> {
    run_with(obs, "auron-classify.py")
}

pub fn run_with(obs: &Observation, prog: &str) -> Vec<Firing> {
    let payload = obs.to_json().to_string();
    let o: Out = util::run(Spec { prog, args: vec![], env: None, cwd: None, stdin: Some(payload.into_bytes()), timeout: None });
    parse_firing_lines(&o.stdout)
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
    fn run_with_feeds_the_observation_on_stdin_and_reads_json_lines_back() {
        // A stand-in classifier: cat's stdin back as one firing record naming the byte count.
        let dir = testkit::TempDir::new("auron-classify");
        let stub = dir.join("stub-classify.py");
        // testkit::write_exe, never fs::write + set_permissions (sp-os3of): a concurrent
        // fork elsewhere in this test binary can inherit a duplicate of this process's
        // own write-fd mid-window and leave the file ETXTBSY for this test's own exec.
        testkit::write_exe(&stub, "#!/usr/bin/env python3\nimport sys, json\nd = sys.stdin.read()\nprint(json.dumps({\"key\": \"echo\", \"title\": \"t\", \"evidence\": str(len(d))}))\n");
        let obs = Observation { now: 1, ..Default::default() };
        let f = run_with(&obs, stub.to_str().unwrap());
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].key, "echo");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
