//! Each firing alert as one retained value on `alarm/<key>`, carried in the heartbeat from the
//! pass it first fired until the broker has taken its clear.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use inbox_rules::Bus;
use serde::{Deserialize, Serialize};
use serde_json::json;
use spira_pub::{Class, PubError, Publisher};

use crate::classify::Firing;

/// Well inside the unit's `TimeoutStartSec`.
const FLUSH: Duration = Duration::from_secs(5);
const LINE: &str = "SP_HEIMDALL_ALARMS=";
const KEYS_LINE: &str = "SP_HEIMDALL_KEYS=";

pub fn topic(key: &str) -> String {
    format!("alarm/{}", spira_pub::segment(key))
}

/// An alarm as first raised. A title carries ages, so it is frozen with `since`: a value that
/// moved every pass would wake every subscriber every pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Raised {
    since: i64,
    text: String,
    /// It stopped firing and its clear has not been delivered.
    #[serde(default)]
    clearing: bool,
}

#[derive(Debug, Default)]
struct Carried {
    alarms: BTreeMap<String, Raised>,
    /// The heartbeat's firing keys, which are cleared even if `alarms` cannot be read.
    firing: BTreeSet<String>,
}

fn carried(heartbeat: &str) -> Carried {
    let value = |name: &str| heartbeat.lines().find_map(|l| l.strip_prefix(name)).map(|v| v.trim_matches('\''));
    Carried {
        alarms: value(LINE).and_then(|v| serde_json::from_str(v).ok()).unwrap_or_default(),
        firing: value(KEYS_LINE).into_iter().flat_map(|v| v.split(',')).filter(|k| !k.is_empty()).map(String::from).collect(),
    }
}

/// The heartbeat line. `'` is escaped as JSON, so the single quotes hold.
fn line(alarms: &BTreeMap<String, Raised>) -> String {
    let json = serde_json::to_string(alarms).unwrap_or_else(|_| "{}".into()).replace('\'', "\\u0027");
    format!("{LINE}'{json}'\n")
}

/// Publishes what `firing` holds and clears what `heartbeat`, the previous pass's, held beyond
/// it. Returns the line the next heartbeat carries, and why delivery failed if it did.
pub fn publish(heartbeat: &str, firing: &[Firing], bus: &Bus, now: i64) -> (String, Result<(), PubError>) {
    let before = carried(heartbeat);
    let mut next: BTreeMap<String, Raised> = firing
        .iter()
        .map(|f| {
            let standing = before.alarms.get(&f.key).filter(|r| !r.clearing).cloned();
            (f.key.clone(), standing.unwrap_or_else(|| Raised { since: now, text: f.title.clone(), clearing: false }))
        })
        .collect();
    let gone: BTreeSet<&String> = before.alarms.keys().chain(&before.firing).filter(|k| !next.contains_key(*k)).collect();
    if next.is_empty() && gone.is_empty() {
        return (line(&next), Ok(()));
    }
    let delivered = bus.publisher("heimdall").and_then(|p| send(&p, &next, &gone));
    if delivered.is_err() {
        for key in gone {
            let held = before.alarms.get(key).cloned().unwrap_or_else(|| Raised { since: now, text: String::new(), clearing: true });
            next.insert(key.clone(), Raised { clearing: true, ..held });
        }
    }
    (line(&next), delivered)
}

/// Stops at the first failure, so a broker that is not answering costs the pass one wait.
fn send(p: &Publisher, firing: &BTreeMap<String, Raised>, gone: &BTreeSet<&String>) -> Result<(), PubError> {
    let mut live = BTreeSet::new();
    for (key, r) in firing {
        let topic = topic(key);
        p.publish(Class::State, &topic, &json!({ "text": r.text, "since": r.since, "at": r.since }))?;
        live.insert(topic);
    }
    for topic in gone.iter().map(|k| topic(k)).filter(|t| !live.contains(t)) {
        p.clear(&topic)?;
    }
    p.flush(FLUSH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(t2k19mt)]
    #[test]
    fn a_key_is_one_topic_segment() {
        assert_eq!(topic("gate-red-streak/a+b#c"), "alarm/gate-red-streak-a-b-c");
        assert_eq!(topic("builder-still:x"), "alarm/builder-still:x");
    }

    #[test_id(tl62veb)]
    #[test]
    fn the_heartbeat_line_round_trips_a_title_with_a_quote_inside_its_quotes() {
        let alarms: BTreeMap<String, Raised> = [("k".to_string(), Raised { since: 7, text: "the unit's log".into(), clearing: false })].into();
        let written = line(&alarms);
        assert_eq!(written.matches('\'').count(), 2, "{written}");
        assert_eq!(carried(&written).alarms, alarms);
    }

    #[test_id(t1jgw3p)]
    #[test]
    fn an_undelivered_clear_is_carried_even_when_only_the_firing_keys_were_recorded() {
        let (written, delivered) = publish("SP_HEIMDALL_KEYS='a,b'\n", &[], &Bus::Off, 9);
        assert!(delivered.is_err());
        let held = carried(&written).alarms;
        assert_eq!(held.keys().collect::<Vec<_>>(), ["a", "b"]);
        assert!(held.values().all(|r| r.clearing));
    }
}
