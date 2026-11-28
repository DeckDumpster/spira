//! Each firing alert as one retained value on `alarm/<key>`, carried in the heartbeat from the
//! pass it first fired until the broker has taken its clear.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use inbox_rules::Bus;
use serde::{Deserialize, Serialize};
use serde_json::json;
use spira_pub::{Class, PubError, Publisher};

use crate::classify::Firing;
use crate::streak;

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
    /// Why the alarms line could not be read.
    unreadable: Option<String>,
}

fn carried(heartbeat: &str) -> Carried {
    let value = |name: &str| heartbeat.lines().find_map(|l| l.strip_prefix(name)).map(|v| v.trim_matches('\''));
    let (alarms, unreadable) = match value(LINE).map(serde_json::from_str).transpose() {
        Ok(alarms) => (alarms.unwrap_or_default(), None),
        Err(e) => (BTreeMap::new(), Some(format!("{LINE} in the last heartbeat is unreadable: {e}"))),
    };
    Carried {
        alarms,
        firing: value(KEYS_LINE).into_iter().flat_map(|v| v.split(',')).filter(|k| !k.is_empty()).map(String::from).collect(),
        unreadable,
    }
}

/// The heartbeat line. `'` is escaped as JSON, so the single quotes hold.
fn line(alarms: &BTreeMap<String, Raised>) -> String {
    let json = serde_json::to_string(alarms).unwrap_or_else(|_| "{}".into()).replace('\'', "\\u0027");
    format!("{LINE}'{json}'\n")
}

/// What one pass leaves for the next, and what went wrong on the way.
pub struct Published {
    /// The line the next heartbeat carries.
    pub line: String,
    pub errors: Vec<String>,
}

/// Publishes what `firing` holds and clears what the previous pass's heartbeat held beyond it.
/// With `gate_unread`, a streak that was standing stands: its check did not run.
pub fn publish(heartbeat: &Path, firing: &[Firing], gate_unread: bool, bus: &Bus, now: i64) -> Published {
    let mut errors = Vec::new();
    let text = match std::fs::read_to_string(heartbeat) {
        Ok(text) => text,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                errors.push(format!("the last heartbeat is unreadable: {e}"));
            }
            String::new()
        }
    };
    let before = carried(&text);
    errors.extend(before.unreadable.clone());
    let standing = |key: &str| before.alarms.get(key).filter(|r| !r.clearing).cloned();
    let mut next: BTreeMap<String, Raised> = firing
        .iter()
        .map(|f| (f.key.clone(), standing(&f.key).unwrap_or_else(|| Raised { since: now, text: f.title.clone(), clearing: false })))
        .collect();
    if gate_unread {
        for key in before.alarms.keys().filter(|k| streak::is_streak(k)) {
            if let Some(held) = standing(key) {
                next.entry(key.clone()).or_insert(held);
            }
        }
    }
    let gone: BTreeSet<&String> = before.alarms.keys().chain(&before.firing).filter(|k| !next.contains_key(*k)).collect();
    if !next.is_empty() || !gone.is_empty() {
        if let Err(e) = bus.publisher("heimdall").and_then(|p| send(&p, &next, &gone)) {
            errors.push(e.to_string());
            for key in gone {
                let held = before.alarms.get(key).cloned().unwrap_or_else(|| Raised { since: now, text: String::new(), clearing: true });
                next.insert(key.clone(), Raised { clearing: true, ..held });
            }
        }
    }
    Published { line: line(&next), errors }
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
        let dir = testkit::TempDir::new("heimdall-alarm-keys");
        let heartbeat = dir.join("heimdall.status");
        std::fs::write(&heartbeat, "SP_HEIMDALL_KEYS='a,b'\n").unwrap();
        let published = publish(&heartbeat, &[], false, &Bus::Off, 9);
        assert_eq!(published.errors.len(), 1, "{:?}", published.errors);
        let held = carried(&published.line).alarms;
        assert_eq!(held.keys().collect::<Vec<_>>(), ["a", "b"]);
        assert!(held.values().all(|r| r.clearing));
    }

    #[test_id(tremh9o)]
    #[test]
    fn a_damaged_alarms_line_is_reported_and_the_firing_keys_are_still_cleared() {
        let dir = testkit::TempDir::new("heimdall-alarm-damaged");
        let heartbeat = dir.join("heimdall.status");
        std::fs::write(&heartbeat, "SP_HEIMDALL_KEYS='a'\nSP_HEIMDALL_ALARMS='{'\n").unwrap();
        let published = publish(&heartbeat, &[], false, &Bus::Off, 9);
        assert!(published.errors.iter().any(|e| e.contains("unreadable")), "{:?}", published.errors);
        assert_eq!(carried(&published.line).alarms.keys().collect::<Vec<_>>(), ["a"]);
    }
}
