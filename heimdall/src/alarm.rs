//! Each firing alert as one retained value on `alarm/<key>`, carried from the pass it first
//! fired until the broker has taken its clear: in the hosted task's journaled carry, and in the
//! heartbeat of the binary's pass.

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
pub struct Raised {
    pub since: i64,
    pub text: String,
    /// It stopped firing and its clear has not been delivered.
    #[serde(default)]
    pub clearing: bool,
}

/// By firing key.
pub type Alarms = BTreeMap<String, Raised>;

/// The alarms after a pass that found `firing`: a key that stopped is marked for clearing.
/// With `gate_unread`, a streak that was standing stands: its check did not run.
pub fn step(before: &Alarms, firing: &[Firing], gate_unread: bool, now: i64) -> Alarms {
    let standing = |key: &str| before.get(key).filter(|r| !r.clearing).cloned();
    let mut next: Alarms = firing
        .iter()
        .map(|f| (f.key.clone(), standing(&f.key).unwrap_or_else(|| Raised { since: now, text: f.title.clone(), clearing: false })))
        .collect();
    for (key, held) in before {
        if !next.contains_key(key) {
            let stands = gate_unread && !held.clearing && streak::is_streak(key);
            next.insert(key.clone(), Raised { clearing: !stands, ..held.clone() });
        }
    }
    next
}

/// [`send`], answering the keys whose clear the broker took. A failure is logged here or by the
/// publisher, and takes none.
pub fn deliver(p: &Publisher, alarms: &Alarms) -> Vec<String> {
    match send(p, alarms) {
        Ok(()) => alarms.iter().filter(|(_, r)| r.clearing).map(|(k, _)| k.clone()).collect(),
        Err(e) => {
            eprintln!("heimdall alarm topics: {e}");
            Vec::new()
        }
    }
}

/// Forgets the clears [`deliver`] answered. A key that has fired again since is kept.
pub fn delivered(alarms: &mut Alarms, cleared: &[String]) {
    alarms.retain(|k, r| !(r.clearing && cleared.contains(k)));
}

#[derive(Debug, Default)]
struct Carried {
    alarms: Alarms,
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
fn line(alarms: &Alarms) -> String {
    let json = serde_json::to_string(alarms).unwrap_or_else(|_| "{}".into()).replace('\'', "\\u0027");
    format!("{LINE}'{json}'\n")
}

/// What one pass leaves for the next, and what went wrong on the way.
pub struct Published {
    /// The line the next heartbeat carries.
    pub line: String,
    pub errors: Vec<String>,
}

/// [`step`] from the previous pass's heartbeat, then published.
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
    let Carried { alarms: mut before, firing: recorded, unreadable } = carried(&text);
    errors.extend(unreadable);
    // A key known only by name has no value to hold: it is cleared unless it fires, and a
    // streak is left alone when its check did not run.
    for key in recorded.into_iter().filter(|k| !(gate_unread && streak::is_streak(k))) {
        before.entry(key).or_insert(Raised { since: now, text: String::new(), clearing: true });
    }
    let mut next = step(&before, firing, gate_unread, now);
    if !next.is_empty() {
        match bus.publisher("heimdall").and_then(|p| send(&p, &next)) {
            Ok(()) => next.retain(|_, r| !r.clearing),
            Err(e) => errors.push(e.to_string()),
        }
    }
    Published { line: line(&next), errors }
}

/// Stops at the first failure, so a broker that is not answering costs the pass one wait.
fn send(p: &Publisher, alarms: &Alarms) -> Result<(), PubError> {
    let mut live = BTreeSet::new();
    for (key, r) in alarms.iter().filter(|(_, r)| !r.clearing) {
        let topic = topic(key);
        p.publish(Class::State, &topic, &json!({ "text": r.text, "since": r.since, "at": r.since }))?;
        live.insert(topic);
    }
    for topic in alarms.iter().filter(|(_, r)| r.clearing).map(|(k, _)| topic(k)).filter(|t| !live.contains(t)) {
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

    #[test_id(tmi4ls5)]
    #[test]
    fn a_streak_known_only_by_name_is_not_cleared_while_its_check_is_unread() {
        let dir = testkit::TempDir::new("heimdall-alarm-damaged-streak");
        let heartbeat = dir.join("heimdall.status");
        std::fs::write(&heartbeat, "SP_HEIMDALL_KEYS='a,gate-red-streak/r'\nSP_HEIMDALL_ALARMS='{'\n").unwrap();
        let published = publish(&heartbeat, &[], true, &Bus::Off, 9);
        assert_eq!(carried(&published.line).alarms.keys().collect::<Vec<_>>(), ["a"]);
    }
}
