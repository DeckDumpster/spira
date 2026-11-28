//! The watch rows' conditions on `watch/<row>/<subject>`: [`settle`] is what a row's handler
//! says changed, [`WatchTopics`] the effects' publisher, off the loop. What it would republish
//! to a broker that comes back is what each row's latest observe carried, with every publish
//! since on top.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use runtime::event::{fnv1a, EffectSpec};
use serde::{Deserialize, Serialize};
use serde_json::json;
use spira_pub::{Class, Publisher};

use crate::watchers::WATCH_PUBLISH;

const PUBLISH_DEADLINE_MS: u64 = 10_000;

/// One held condition: the payload of its topic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Condition {
    pub level: String,
    pub text: String,
    /// Epoch seconds the subject first held a condition, kept while it goes on holding one.
    pub since: u64,
    /// Epoch seconds of the pass that last changed the value.
    pub at: u64,
}

/// What one row has published, by subject.
pub type Published = BTreeMap<String, Condition>;

/// The input of one `watch.publish`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub row: String,
    pub set: Published,
    pub clear: Vec<String>,
}

/// The input of a row's observe: what it last published.
#[derive(Serialize, Deserialize)]
struct Carried {
    row: String,
    published: Published,
}

/// The line's own priority word, else `P2`.
fn level(text: &str) -> &str {
    text.split_whitespace().find(|w| matches!(*w, "P0" | "P1" | "P2" | "P3")).unwrap_or("P2")
}

/// The pass at `now_ms` over `row`'s current conditions (subject → line): what the row has
/// published after it, and the change that takes the broker there, if any. A condition whose
/// line is unchanged keeps its value, `at` included.
pub fn settle(row: &str, published: &Published, conditions: BTreeMap<String, String>, now_ms: u64) -> (Published, Option<Change>) {
    let now = now_ms / 1000;
    let (mut next, mut change) = (Published::new(), Change { row: row.to_string(), ..Change::default() });
    for (subject, text) in conditions {
        let held = published.get(&subject);
        let value = match held {
            Some(held) if held.text == text => held.clone(),
            _ => {
                let value = Condition { level: level(&text).to_string(), since: held.map_or(now, |h| h.since), at: now, text };
                change.set.insert(subject.clone(), value.clone());
                value
            }
        };
        next.insert(subject, value);
    }
    change.clear = published.keys().filter(|s| !next.contains_key(*s)).cloned().collect();
    let changed = !change.set.is_empty() || !change.clear.is_empty();
    (next, changed.then_some(change))
}

/// The effect that publishes `change`. Its key is the change's own: a shared one would have the
/// engine refuse a change while an earlier one is in flight, with the carry already past it.
pub fn effect(change: &Change, now_ms: u64) -> EffectSpec {
    let input = serde_json::to_string(change).unwrap_or_default();
    EffectSpec::keyed(WATCH_PUBLISH, format!("watch/{}/{now_ms}/{:016x}", change.row, fnv1a(&input)), input, PUBLISH_DEADLINE_MS)
}

/// What `row`'s observe is asked with; empty when it has published nothing.
pub fn carried(row: &str, published: &Published) -> String {
    if published.is_empty() {
        return String::new();
    }
    serde_json::to_string(&Carried { row: row.to_string(), published: published.clone() }).unwrap_or_default()
}

fn topic(row: &str, subject: &str) -> String {
    format!("watch/{row}/{}", spira_pub::segment(subject))
}

type Held = BTreeMap<String, Published>;

pub struct WatchTopics {
    publisher: Publisher,
    held: Arc<Mutex<Held>>,
    asked_to_connect: AtomicBool,
}

fn lock(m: &Mutex<Held>) -> MutexGuard<'_, Held> {
    m.lock().unwrap_or_else(|e| e.into_inner()) // loop-safe: taken only by effects on the pool and the publisher thread
}

impl WatchTopics {
    pub fn new(publisher: Publisher) -> WatchTopics {
        let held = Arc::new(Mutex::new(Held::new()));
        let republish = held.clone();
        publisher.on_connect(move |again| {
            for (row, published) in lock(&republish).iter() {
                for (subject, value) in published {
                    // A refusal is the publisher's to log; nothing here can mend it.
                    let _ = again.publish(Class::State, &topic(row, subject), &json!(value));
                }
            }
        });
        WatchTopics { publisher, held, asked_to_connect: AtomicBool::new(false) }
    }

    /// Takes the carried state an observe was asked with. Whatever of it is not already held is
    /// published, so a restarted process says it again.
    pub fn track(&self, input: &str) {
        let Ok(Carried { row, published }) = serde_json::from_str(input) else { return };
        let held = lock(&self.held).get(&row).cloned().unwrap_or_default();
        let unsaid: Vec<(&String, &Condition)> = published.iter().filter(|(s, v)| held.get(*s) != Some(*v)).collect();
        if !unsaid.is_empty() && !self.asked_to_connect.swap(true, Ordering::SeqCst) {
            // A failure is logged by the publisher, which connects when the broker appears.
            let _ = self.publisher.connect();
        }
        for (subject, value) in unsaid {
            let _ = self.publisher.publish(Class::State, &topic(&row, subject), &json!(value));
        }
        // Held only after the publishes: the connect hook takes this lock while they wait.
        lock(&self.held).insert(row, published);
    }

    /// Publishes the [`Change`] in `input`. A broker that cannot be reached is logged by the
    /// publisher and is not the effect's failure: the state goes out on reconnect.
    pub fn publish(&self, input: &str) -> Result<String, String> {
        let Change { row, set, clear } = serde_json::from_str(input).map_err(|e| e.to_string())?;
        let total = set.len() + clear.len();
        let sent = set.iter().filter(|(s, v)| self.publisher.publish(Class::State, &topic(&row, s), &json!(v)).is_ok()).count()
            + clear.iter().filter(|s| self.publisher.clear(&topic(&row, s)).is_ok()).count();
        let mut held = lock(&self.held);
        let published = held.entry(row).or_default();
        published.retain(|subject, _| !clear.contains(subject));
        published.extend(set);
        Ok(format!("published {sent} of {total} change(s)"))
    }

    /// Waits for the broker to take what is queued.
    pub fn flush(&self, deadline: std::time::Duration) -> Result<(), String> {
        self.publisher.flush(deadline).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tk2kbpc)]
    #[test]
    fn a_condition_takes_its_lines_priority_word_and_p2_without_one() {
        assert_eq!(level("P0 BLIND: the store"), "P0");
        assert_eq!(level("SUSPECT sp-r REWORK P1 WAITING"), "P1");
        assert_eq!(level("CLAIMS sp-a: claimed 4 times"), "P2");
        assert_eq!(level("sp-P0 and P4 are not priority words"), "P2");
    }
}
