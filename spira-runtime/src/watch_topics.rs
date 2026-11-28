//! The watch rows' conditions on `watch/<row>/<subject>`: [`settle`] is what a row's handler
//! says changed, [`WatchTopics`] the effects' publisher, off the loop. What it would republish
//! to a broker that comes back is what each row's latest observe carried, with every publish
//! since on top.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use runtime::event::{fnv1a, EffectSpec, Event, Projection};
use serde::{Deserialize, Serialize};
use serde_json::json;
use spira_pub::{Class, Publisher};

use crate::watchers::{answered_spec, lost_at_restart, WATCH_PUBLISH};

const PUBLISH_DEADLINE_MS: u64 = 10_000;
/// How long the broker is given to take a clear before it is kept to be sent again.
const CLEAR_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

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

/// The effect again when `ev` says a `watch.publish` was lost with a stopping process, less
/// whatever `published` has moved past. A lost clear is in no carry, so nothing else resends it.
pub fn again(view: &Projection, ev: &Event, published: &Published) -> Option<EffectSpec> {
    let lost = answered_spec(view, ev).filter(|s| s.name == WATCH_PUBLISH && lost_at_restart(ev))?;
    let mut change: Change = serde_json::from_str(&lost.input).ok()?;
    change.set.retain(|subject, value| published.get(subject) == Some(value));
    change.clear.retain(|subject| !published.contains_key(subject));
    (!change.set.is_empty() || !change.clear.is_empty()).then(|| effect(&change, ev.at))
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

#[derive(Default)]
struct State {
    held: BTreeMap<String, Published>,
    /// Topics cleared here that the broker has not been seen to take.
    uncleared: BTreeSet<String>,
}

pub struct WatchTopics {
    publisher: Publisher,
    state: Arc<Mutex<State>>,
    asked_to_connect: AtomicBool,
}

fn lock(m: &Mutex<State>) -> MutexGuard<'_, State> {
    m.lock().unwrap_or_else(|e| e.into_inner()) // loop-safe: taken only by effects on the pool and the publisher thread
}

impl WatchTopics {
    pub fn new(publisher: Publisher) -> WatchTopics {
        let state = Arc::new(Mutex::new(State::default()));
        let republish = state.clone();
        publisher.on_connect(move |again| {
            let state = lock(&republish);
            for (row, published) in &state.held {
                for (subject, value) in published {
                    // A refusal is the publisher's to log; nothing here can mend it.
                    let _ = again.publish(Class::State, &topic(row, subject), &json!(value));
                }
            }
            for cleared in &state.uncleared {
                let _ = again.clear(cleared);
            }
        });
        WatchTopics { publisher, state, asked_to_connect: AtomicBool::new(false) }
    }

    /// Takes the carried state an observe was asked with. Whatever of it is not already held is
    /// published, so a restarted process says it again.
    pub fn track(&self, input: &str) {
        let Ok(Carried { row, published }) = serde_json::from_str(input) else { return };
        let held = lock(&self.state).held.get(&row).cloned().unwrap_or_default();
        let unsaid: Vec<(&String, &Condition)> = published.iter().filter(|(s, v)| held.get(*s) != Some(*v)).collect();
        if !unsaid.is_empty() && !self.asked_to_connect.swap(true, Ordering::SeqCst) {
            // A failure is logged by the publisher, which connects when the broker appears.
            let _ = self.publisher.connect();
        }
        for (subject, value) in unsaid {
            let _ = self.publisher.publish(Class::State, &topic(&row, subject), &json!(value));
        }
        let mut state = lock(&self.state);
        for subject in published.keys() {
            state.uncleared.remove(&topic(&row, subject));
        }
        // Held only after the publishes: the connect hook takes this lock while they wait.
        state.held.insert(row, published);
        drop(state);
        self.send_clears();
    }

    /// Publishes the [`Change`] in `input`. A broker that cannot be reached is logged by the
    /// publisher and is not the effect's failure: the state goes out on reconnect.
    pub fn publish(&self, input: &str) -> Result<String, String> {
        let Change { row, set, clear } = serde_json::from_str(input).map_err(|e| e.to_string())?;
        {
            // A clear is recorded before anything is sent, so a connect on the way cannot put
            // the old value back.
            let mut state = lock(&self.state);
            for subject in &clear {
                state.uncleared.insert(topic(&row, subject));
                if let Some(published) = state.held.get_mut(&row) {
                    published.remove(subject);
                }
            }
            for subject in set.keys() {
                state.uncleared.remove(&topic(&row, subject));
            }
        }
        let sent = set.iter().filter(|(s, v)| self.publisher.publish(Class::State, &topic(&row, s), &json!(v)).is_ok()).count();
        let said = format!("published {sent} of {} value(s), {} clear(s)", set.len(), clear.len());
        // Held only after the publishes: the connect hook takes this lock while they wait.
        lock(&self.state).held.entry(row).or_default().extend(set);
        self.send_clears();
        Ok(said)
    }

    /// Sends every clear not yet seen to arrive, and forgets those the broker then takes.
    fn send_clears(&self) {
        let owed: Vec<String> = lock(&self.state).uncleared.iter().cloned().collect();
        let queued: Vec<&String> = owed.iter().filter(|t| self.publisher.clear(t).is_ok()).collect();
        if queued.is_empty() || self.publisher.flush(CLEAR_DEADLINE).is_err() {
            return;
        }
        let mut state = lock(&self.state);
        for taken in queued {
            state.uncleared.remove(taken);
        }
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
