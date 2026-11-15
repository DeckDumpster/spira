//! The one definition of a round event's log record: what the queue and spira-lc write when a
//! round is opened, ejected, abandoned, landed, and the rest, and the query that finds the
//! rounds a hand was on.

use std::path::Path;
use std::time::SystemTime;

use crate::logs::{Attr, LogSink};

pub const EVENT: &str = "round.event";
pub const ACTORS: [&str; 3] = ["batcher", "concierge", "operator"];

pub fn is_hand(by: &str) -> bool {
    matches!(by, "concierge" | "operator")
}

/// Best effort, like `landing.log`: a failed write is counted by the sink and never returned.
#[allow(clippy::too_many_arguments)]
pub fn emit(
    run: &Path,
    service: &str,
    kind: &str,
    repo: &str,
    batch: &str,
    by: &str,
    fields: &[(&str, String)],
    now: SystemTime,
) {
    let s = |v: &str| Attr::Str(v.to_string());
    let mut attrs = vec![
        ("kind".to_string(), s(kind)),
        ("repo".to_string(), s(repo)),
        ("batch".to_string(), s(batch)),
        ("by".to_string(), s(by)),
    ];
    attrs.extend(fields.iter().map(|(k, v)| (k.to_string(), s(v))));
    let _ = LogSink::new(crate::dir_of(run), service).write(EVENT, &attrs, now);
}

pub fn hand_managed_query() -> String {
    format!("_msg:\"{EVENT}\" by:in(\"concierge\",\"operator\") | stats by (repo, batch) count() hand_events")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_writes_a_record_that_reads_back_with_its_actor_and_kind() {
        let d = testkit::TempDir::new("otel-rounds");
        emit(
            d.path(),
            "queue",
            "eject",
            "r",
            "b1",
            "concierge",
            &[("id", "sp-a".into())],
            SystemTime::now(),
        );
        let recs = crate::logs::read_all(&crate::dir_of(d.path()), "queue");
        assert_eq!(recs.len(), 1);
        let get = |k: &str| {
            recs[0]
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v["stringValue"].as_str().unwrap().to_string())
        };
        assert_eq!(get("by").as_deref(), Some("concierge"));
        assert_eq!(get("kind").as_deref(), Some("eject"));
        assert_eq!(get("batch").as_deref(), Some("b1"));
        assert_eq!(get("id").as_deref(), Some("sp-a"));
    }

    #[test]
    fn only_concierge_and_operator_are_hands() {
        for a in ACTORS {
            assert_eq!(is_hand(a), a != "batcher", "{a}");
        }
        assert!(!is_hand("spira-lc"));
    }

    #[test]
    fn the_hand_managed_query_is_pinned() {
        assert_eq!(
            hand_managed_query(),
            r#"_msg:"round.event" by:in("concierge","operator") | stats by (repo, batch) count() hand_events"#
        );
    }

    #[test]
    fn the_shipped_panel_runs_the_pinned_query() {
        let dash: serde_json::Value = serde_json::from_str(include_str!(
            "../../spira/metrics/grafana/dashboards/rounds.json"
        ))
        .unwrap();
        let panel = dash["panels"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["title"] == "hand-managed rounds")
            .expect("the panel");
        assert_eq!(panel["targets"][0]["expr"], hand_managed_query());
        assert_eq!(panel["datasource"]["uid"], "${DS_VL}");
    }
}
