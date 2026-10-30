//! A landed bead's cycle time, first SUBMITTED to LANDED, cut by the state it was in: derived from
//! the event log, pushed once when the bead lands.

use serde_json::Value;

use crate::db::Conn;
use crate::passes;

pub const CYCLE_SECONDS: &str = "spira_bead_cycle_seconds";
pub const CYCLE_PASSES: &str = "spira_bead_cycle_passes";

#[derive(Debug, Default, PartialEq)]
pub struct Cycle {
    pub gate: i64,
    pub wait_round: i64,
    pub round_passes: i64,
    pub land: i64,
    pub rework: i64,
    pub passes: u32,
    pub landed_at: i64,
}

impl Cycle {
    pub fn seconds(&self) -> [(&'static str, i64); 5] {
        [("gate", self.gate), ("wait_round", self.wait_round), ("round_passes", self.round_passes), ("land", self.land), ("rework", self.rework)]
    }

    pub fn total(&self) -> i64 {
        self.gate + self.wait_round + self.round_passes + self.land + self.rework
    }
}

fn int(v: &Value, k: &str) -> Option<i64> {
    v.get(k).and_then(|x| x.as_i64().or_else(|| x.as_str().and_then(|s| s.parse().ok())))
}

fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// `bead_events`: the bead's applied events in `seq` order. `batches`: for each batch the bead was
/// a member of, that batch's pass events as `passes::fold` takes them. None until the bead has
/// been submitted and landed.
pub fn fold(bead_events: &[Value], batches: &[Vec<Value>]) -> Option<Cycle> {
    let moves: Vec<(i64, &str)> = bead_events
        .iter()
        .filter(|e| text(e, "from_state") != text(e, "to_state"))
        .filter_map(|e| Some((int(e, "at")?, text(e, "to_state"))))
        .collect();
    let first = moves.iter().position(|(_, s)| *s == "SUBMITTED")?;
    let last = first + moves[first..].iter().position(|(_, s)| *s == "LANDED")?;
    let windows: Vec<(i64, i64)> = batches.iter().flat_map(|b| passes::fold(b).0).filter_map(|p| Some((p["started_at"].as_i64()?, p["ended_at"].as_i64().unwrap_or(i64::MAX)))).collect();

    let mut c = Cycle { landed_at: moves[last].0, ..Cycle::default() };
    let mut counted = std::collections::BTreeSet::new();
    for w in moves[first..=last].windows(2) {
        let ((from, state), (to, _)) = (w[0], w[1]);
        let span = to - from;
        match state {
            "SUBMITTED" => c.gate += span,
            "CERTIFIED" => c.wait_round += span,
            "IN_DELIVERY" => {
                let inside: Vec<(i64, i64)> = windows.iter().filter(|(s, e)| *s < to && *e > from).map(|(s, e)| ((*s).max(from), (*e).min(to))).collect();
                for (s, e) in windows.iter().filter(|(s, e)| *s < to && *e > from) {
                    counted.insert((*s, *e));
                }
                match (inside.iter().map(|w| w.0).min(), inside.iter().map(|w| w.1).max()) {
                    (Some(first_pass), Some(last_pass)) => {
                        c.wait_round += first_pass - from;
                        c.round_passes += last_pass - first_pass;
                        c.land += to - last_pass;
                    }
                    _ => c.wait_round += span,
                }
            }
            "READY" | "WORKING" | "REWORK" => c.rework += span,
            _ => {}
        }
    }
    c.passes = counted.len() as u32;
    Some(c)
}

/// Push each landed bead's segments, stamped with its landing time. Never fatal: the landing is
/// already recorded and the metric has no other reader.
pub fn push_landed(conn: &Conn, bead_ids: &[String]) {
    use spira_otel::metrics::{MetricsSink, Series};
    let Ok(endpoint) = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT") else { return };
    let mut series = Vec::new();
    for id in bead_ids {
        let Some(c) = read(conn, id) else { continue };
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(c.landed_at.max(0) as u64);
        let mut push = |name: &str, value: f64, segment: Option<&str>| {
            let mut labels = vec![("bead".to_string(), id.clone())];
            labels.extend(segment.map(|s| ("segment".to_string(), s.to_string())));
            series.push(Series { name: name.into(), value, labels, at });
        };
        for (segment, secs) in c.seconds() {
            push(CYCLE_SECONDS, secs as f64, Some(segment));
        }
        push(CYCLE_SECONDS, c.total() as f64, Some("total"));
        push(CYCLE_PASSES, c.passes as f64, None);
    }
    if !series.is_empty() {
        let _ = MetricsSink::new(Some(&endpoint), "spira-lc", std::time::Duration::from_millis(250)).try_push_series(&series);
    }
}

fn read(conn: &Conn, id: &str) -> Option<Cycle> {
    let key = crate::rows::escape(id);
    let beads = conn.query(&format!("SELECT event, from_state, to_state, at FROM event WHERE machine = 'bead' AND applied = 1 AND lc_key = '{key}' ORDER BY seq")).ok()?;
    let batch_ids = conn.query(&format!("SELECT batch_id FROM batch_member WHERE bead_id = '{key}'")).ok()?;
    let batches = batch_ids
        .iter()
        .filter_map(|r| {
            let b = crate::rows::escape(r.get("batch_id")?.as_str()?);
            conn.query(&format!("SELECT event, evidence, at FROM event WHERE machine = 'batch' AND applied = 1 AND lc_key = '{b}' AND event IN ({}) ORDER BY seq", passes::FETCHED)).ok()
        })
        .collect::<Vec<_>>();
    fold(&beads, &batches)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mv(from: &str, to: &str, at: i64) -> Value {
        json!({"event": "x", "from_state": from, "to_state": to, "at": at})
    }

    fn pass(name: &str, body: Value, at: i64) -> Value {
        json!({"event": name, "evidence": json!({ name: body }).to_string(), "at": at})
    }

    #[test]
    fn a_rework_cycle_and_two_round_passes_are_cut_into_their_segments() {
        let bead = vec![
            mv("READY", "WORKING", 0),
            mv("WORKING", "SUBMITTED", 100),
            mv("SUBMITTED", "CERTIFIED", 160),
            mv("CERTIFIED", "IN_DELIVERY", 400),
            mv("IN_DELIVERY", "REWORK", 700),
            mv("REWORK", "WORKING", 750),
            mv("WORKING", "SUBMITTED", 1000),
            mv("SUBMITTED", "CERTIFIED", 1100),
            mv("CERTIFIED", "IN_DELIVERY", 1200),
            mv("IN_DELIVERY", "LANDED", 2300),
        ];
        let first = vec![pass("PassStarted", json!({"n": 1, "head": "a"}), 450), pass("PassRed", json!({"n": 1, "red_suites": [], "suites_s": 1, "build_s": 1}), 650)];
        let second = vec![
            pass("PassStarted", json!({"n": 1, "head": "b"}), 1300),
            pass("PassRed", json!({"n": 1, "red_suites": [], "suites_s": 1, "build_s": 1}), 1500),
            pass("PassStarted", json!({"n": 2, "head": "c"}), 1600),
            pass("PassGreen", json!({"n": 2, "suites_s": 1, "build_s": 1}), 2200),
        ];
        let c = fold(&bead, &[first, second]).expect("a landed bead folds");
        assert_eq!(
            c,
            Cycle {
                gate: 60 + 100,
                wait_round: 240 + 50 + 100 + 100,
                round_passes: 200 + 900,
                land: 50 + 100,
                rework: 50 + 250,
                passes: 3,
                landed_at: 2300,
            }
        );
        assert_eq!(c.total(), 2300 - 100, "segments sum to first SUBMITTED to LANDED");
    }

    #[test]
    fn a_bead_that_has_not_landed_has_no_cycle() {
        let bead = vec![mv("WORKING", "SUBMITTED", 1), mv("SUBMITTED", "CERTIFIED", 5)];
        assert_eq!(fold(&bead, &[]), None);
    }

    #[test]
    fn an_in_delivery_stretch_with_no_pass_is_all_waiting() {
        let bead = vec![mv("WORKING", "SUBMITTED", 0), mv("SUBMITTED", "CERTIFIED", 10), mv("CERTIFIED", "IN_DELIVERY", 20), mv("IN_DELIVERY", "LANDED", 50)];
        let c = fold(&bead, &[]).unwrap();
        assert_eq!((c.wait_round, c.round_passes, c.land, c.passes), (40, 0, 0, 0));
    }
}
