//! The subscription set and what is delivered for it. No I/O: time is the injected clock, and
//! every method returns the lines to stream and what the connection and the set file owe.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use ports::Clock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use spira_pub::topics::{self, Class};

use crate::render;

/// The filter every client holds for itself: publisher liveness.
pub const LIVENESS: &str = "publisher/+";

/// One entry of the set, as the set file holds it: a topic filter or an instant, never a message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<u64>,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub once: bool,
    /// The interval deliveries are batched at; absent means at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_s: Option<u64>,
    /// Per topic, the digest of the state value last delivered here: never the value.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub delivered: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    /// The next id's number: an id is never reused, even across a restart.
    pub next: u64,
    pub subs: Vec<Entry>,
}

pub(crate) struct Sub {
    pub(crate) entry: Entry,
    pub(crate) last_fired_ms: Option<u64>,
    /// What the next batch holds: the latest value per topic.
    pub(crate) pending: BTreeMap<String, Value>,
    /// When the first of `pending` arrived, and when the batch is due. Set while it holds any.
    since_ms: u64,
    due_ms: Option<u64>,
}

/// What one step owes the outside.
#[derive(Debug, Default, PartialEq)]
pub struct Out {
    /// Each element is one delivery: one line, or a batch's header and lines.
    pub lines: Vec<String>,
    /// Filters no remaining subscription holds.
    pub unsubscribe: Vec<String>,
    /// The set changed and must be written.
    pub changed: bool,
}

pub struct Core {
    clock: Arc<dyn Clock>,
    next: u64,
    pub(crate) subs: Vec<Sub>,
    /// The current value of every subscribed state topic.
    current: BTreeMap<String, Value>,
    /// The chatter message last taken, and how many more copies of it the broker owes.
    echoes: (String, Vec<u8>, usize),
    /// The current value of every `publisher/<name>`.
    pub(crate) publishers: BTreeMap<String, Value>,
}

fn is_state(topic: &str) -> bool {
    topics::family_of(topic).is_some_and(|f| f.class == Class::State)
}

fn is_gone(value: &Value) -> bool {
    value.get("state").and_then(Value::as_str) == Some("gone")
}

/// Whether `filter` watches a family that `publisher` keeps alive.
fn depends_on(filter: &str, publisher: &str) -> bool {
    topics::families_matching(filter).any(|f| f.resident.iter().any(|r| r == publisher))
}

/// `value` with its keys in order: a build may unify serde_json's `preserve_order` in.
fn key_ordered(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let ordered: BTreeMap<&String, &Value> = map.iter().collect();
            Value::Object(ordered.into_iter().map(|(k, v)| (k.clone(), key_ordered(v))).collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(key_ordered).collect()),
        other => other.clone(),
    }
}

fn digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(key_ordered(value).to_string()))
}

/// The next multiple of the interval, so equal intervals fall due together.
fn boundary(now_ms: u64, batch_s: u64) -> u64 {
    let interval = batch_s.saturating_mul(1000).max(1);
    (now_ms / interval + 1).saturating_mul(interval)
}

const NOT_BATCHED: &str = "delivers at once, so it cannot be batched";

impl Sub {
    fn new(entry: Entry) -> Sub {
        Sub { entry, last_fired_ms: None, pending: BTreeMap::new(), since_ms: 0, due_ms: None }
    }

    fn filter(&self) -> Option<&str> {
        self.entry.filter.as_deref()
    }

    fn already_given(&self, topic: &str, value: &Value) -> bool {
        self.entry.delivered.get(topic) == Some(&digest(value))
    }

    /// The line for `topic` = `value`, unless it is the state value last delivered here. The one
    /// place a value counts as given.
    fn deliver(&mut self, topic: &str, value: &Value, state: bool, now: u64, out: &mut Out) -> Option<String> {
        if state {
            if self.already_given(topic, value) {
                return None;
            }
            self.entry.delivered.insert(topic.to_string(), digest(value));
            out.changed = true;
        }
        self.last_fired_ms = Some(now);
        Some(render::line(&self.entry.id, topic, value, &self.entry.note))
    }

    fn hold(&mut self, topic: &str, value: &Value, batch_s: u64, now: u64) {
        if self.due_ms.is_none() {
            self.since_ms = now;
            self.due_ms = Some(boundary(now, batch_s));
        }
        self.pending.insert(topic.to_string(), value.clone());
    }

    /// The topic holds no value any more: there is none to batch, and its next one is news.
    fn clear(&mut self, topic: &str, out: &mut Out) {
        self.pending.remove(topic);
        if self.pending.is_empty() {
            self.due_ms = None;
        }
        out.changed |= self.entry.delivered.remove(topic).is_some();
    }

    /// Ends the batch being gathered and returns what it held.
    fn take_pending(&mut self) -> BTreeMap<String, Value> {
        self.due_ms = None;
        std::mem::take(&mut self.pending)
    }
}

impl Core {
    pub fn new(clock: Arc<dyn Clock>, saved: Saved) -> Core {
        let subs = saved.subs.into_iter().map(Sub::new).collect();
        Core { clock, next: saved.next.max(1), subs, current: BTreeMap::new(), publishers: BTreeMap::new(), echoes: Default::default() }
    }

    pub fn saved(&self) -> Saved {
        Saved { next: self.next, subs: self.subs.iter().map(|s| s.entry.clone()).collect() }
    }

    pub fn len(&self) -> usize {
        self.subs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.subs.is_empty()
    }

    pub(crate) fn now_ms(&self) -> u64 {
        self.clock.now_ms()
    }

    /// A new connection: who is up is unknown until the broker says, since a restarted one
    /// holds nothing about a publisher that does not come back.
    pub fn reconnected(&mut self) {
        self.publishers.clear();
    }

    /// Every topic filter the set holds.
    pub fn filters(&self) -> BTreeSet<String> {
        self.subs.iter().filter_map(|s| s.filter().map(str::to_string)).collect()
    }

    fn push(&mut self, filter: Option<String>, at_ms: Option<u64>, note: &str, once: bool) -> String {
        let id = format!("s{}", self.next);
        self.next += 1;
        let entry = Entry { id: id.clone(), filter, at_ms, note: note.to_string(), once, batch_s: None, delivered: BTreeMap::new() };
        self.subs.push(Sub::new(entry));
        id
    }

    /// A new subscription delivered at once. See [`Core::add_classed`].
    pub fn add(&mut self, filter: &str, note: &str, once: bool, listening: bool) -> Result<(String, Out), String> {
        self.add_classed(filter, note, once, None, listening)
    }

    /// A new subscription, batched when `batch_s` is given. The caller subscribes its filter, which is what brings the retained
    /// values; a publisher already known to be gone is said here, while someone is `listening`.
    pub fn add_classed(&mut self, filter: &str, note: &str, once: bool, batch_s: Option<u64>, listening: bool) -> Result<(String, Out), String> {
        if once && batch_s.is_some() {
            return Err("--batch conflicts with --once: a --once subscription delivers at once".to_string());
        }
        if !topics::filter_matches_any(filter) {
            return Err(format!("filter {filter:?} matches no topic family declared in spira/topics.toml"));
        }
        let id = self.push(Some(filter.to_string()), None, note, once);
        let mut out = Out { changed: true, ..Out::default() };
        if listening {
            let now = self.now_ms();
            let sub = self.subs.last_mut().expect("just pushed");
            for (topic, value) in &self.publishers {
                if is_gone(value) && depends_on(filter, topic.trim_start_matches("publisher/")) {
                    let line = sub.deliver(topic, value, true, now, &mut out);
                    out.lines.extend(line);
                }
            }
        }
        self.subs.last_mut().expect("just pushed").entry.batch_s = batch_s;
        Ok((id, out))
    }

    /// A subscription that fires once, at `at_ms`.
    pub fn add_at(&mut self, at_ms: u64, note: &str) -> (String, Out) {
        let id = self.push(None, Some(at_ms), note, true);
        (id, Out { changed: true, ..Out::default() })
    }

    fn position(&self, id: &str) -> Result<usize, String> {
        self.subs.iter().position(|s| s.entry.id == id).ok_or_else(|| format!("no subscription {id:?}; `spira-sub list` shows them"))
    }

    /// Changes how `id` is delivered. Becoming at once, it is given what its batch held. With
    /// nobody `listening`, the next reader's replay has the state values, and the chatter,
    /// which no replay holds, stays pending and due.
    pub fn set_class(&mut self, id: &str, batch_s: Option<u64>, listening: bool) -> Result<Out, String> {
        let at = self.position(id)?;
        let now = self.now_ms();
        let sub = &mut self.subs[at];
        if batch_s.is_some() && sub.entry.once {
            let kind = if sub.filter().is_some() { "--once" } else { "--at" };
            return Err(format!("{id} is {kind}: it {NOT_BATCHED}"));
        }
        let mut out = Out { changed: true, ..Out::default() };
        sub.entry.batch_s = batch_s;
        match batch_s {
            Some(batch_s) => sub.due_ms = sub.due_ms.map(|_| boundary(now, batch_s)),
            None if listening => {
                for (topic, value) in sub.take_pending() {
                    let line = sub.deliver(&topic, &value, is_state(&topic), now, &mut out);
                    out.lines.extend(line);
                }
            }
            None => {
                sub.pending.retain(|topic, _| !is_state(topic));
                sub.due_ms = (!sub.pending.is_empty()).then_some(now);
            }
        }
        Ok(out)
    }

    pub fn remove(&mut self, id: &str) -> Result<Out, String> {
        let at = self.position(id)?;
        let mut out = Out { changed: true, ..Out::default() };
        self.discard(&[at], &mut out);
        Ok(out)
    }

    /// Drops the subscriptions at `positions` (ascending), freeing what only they held.
    fn discard(&mut self, positions: &[usize], out: &mut Out) {
        let mut gone = Vec::new();
        for &at in positions.iter().rev() {
            gone.extend(self.subs.remove(at).entry.filter);
        }
        if positions.is_empty() {
            return;
        }
        out.changed = true;
        let held = self.filters();
        for filter in gone {
            if !held.contains(&filter) && filter != LIVENESS && !out.unsubscribe.contains(&filter) {
                out.unsubscribe.push(filter);
            }
        }
        self.current.retain(|topic, _| held.iter().any(|f| topics::filter_selects(f, topic)));
    }

    /// Whether this is another copy of the chatter just taken: the broker sends one for each
    /// held filter that selects the topic, one after another. A state value needs no such
    /// count, since an equal one is skipped anyway.
    fn echo(&mut self, topic: &str, payload: &[u8]) -> bool {
        let (last, said, owed) = &mut self.echoes;
        if *owed > 0 && last == topic && said == payload {
            *owed -= 1;
            return true;
        }
        let copies = self.filters().iter().filter(|f| topics::filter_selects(f, topic)).count();
        self.echoes = (topic.to_string(), payload.to_vec(), copies.saturating_sub(1));
        false
    }

    /// A message from the broker. With nobody `listening` nothing counts as delivered, so a
    /// `--once` subscription waits for a reader; the value is kept for its replay, and a
    /// batched subscription's in its pending batch.
    pub fn message(&mut self, topic: &str, payload: &[u8], listening: bool) -> Out {
        let mut out = Out::default();
        let liveness = topic.starts_with("publisher/");
        if payload.is_empty() {
            self.current.remove(topic);
            self.publishers.remove(topic);
            self.subs.iter_mut().for_each(|s| s.clear(topic, &mut out));
            return out;
        }
        let value = serde_json::from_slice(payload).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(payload).into_owned()));
        let state = is_state(topic);
        if !state && self.echo(topic, payload) {
            return out;
        }
        if liveness {
            self.publishers.insert(topic.to_string(), value.clone());
        }
        if state && self.subs.iter().any(|s| s.filter().is_some_and(|f| topics::filter_selects(f, topic))) {
            self.current.insert(topic.to_string(), value.clone());
        }
        let now = self.now_ms();
        let mut spent = Vec::new();
        for (at, sub) in self.subs.iter_mut().enumerate() {
            let Some(filter) = sub.entry.filter.clone() else { continue };
            let direct = topics::filter_selects(&filter, topic);
            let notice = !direct && liveness && depends_on(&filter, topic.trim_start_matches("publisher/"));
            if notice && !is_gone(&value) {
                // Back up: its next going is news again.
                sub.clear(topic, &mut out);
                continue;
            }
            if let Some(batch_s) = sub.entry.batch_s.filter(|_| direct) {
                sub.hold(topic, &value, batch_s, now);
                continue;
            }
            if !listening || !(direct || notice) {
                continue;
            }
            let Some(line) = sub.deliver(topic, &value, state, now, &mut out) else { continue };
            out.lines.push(line);
            if direct && sub.entry.once {
                spent.push(at);
            }
        }
        self.discard(&spent, &mut out);
        out
    }

    /// What a newly attached reader is told first: each subscribed state value not yet
    /// delivered, every batch already due and every timer already due.
    pub fn attach(&mut self) -> Out {
        let mut out = Out::default();
        let now = self.now_ms();
        let mut spent = Vec::new();
        for (at, sub) in self.subs.iter_mut().enumerate() {
            let Some(filter) = sub.entry.filter.clone() else { continue };
            for (topic, value) in &self.publishers {
                if !topics::filter_selects(&filter, topic) && is_gone(value) && depends_on(&filter, topic.trim_start_matches("publisher/")) {
                    let line = sub.deliver(topic, value, true, now, &mut out);
                    out.lines.extend(line);
                }
            }
            for (topic, value) in self.current.iter().filter(|(t, _)| topics::filter_selects(&filter, t)) {
                if let Some(batch_s) = sub.entry.batch_s {
                    // Unheard while it was delivered at once, so no batch holds it yet.
                    if !sub.already_given(topic, value) {
                        sub.hold(topic, value, batch_s, now);
                    }
                    continue;
                }
                let Some(line) = sub.deliver(topic, value, true, now, &mut out) else { continue };
                out.lines.push(line);
                if sub.entry.once {
                    spent.push(at);
                    break;
                }
            }
        }
        self.discard(&spent, &mut out);
        for due in [self.flush_due(true), self.fire_due(true)] {
            out.lines.extend(due.lines);
            out.unsubscribe.extend(due.unsubscribe);
            out.changed |= due.changed;
        }
        out
    }

    /// When the earliest timer or batch is due.
    pub fn next_due_ms(&self) -> Option<u64> {
        self.subs.iter().flat_map(|s| [s.entry.at_ms, s.due_ms]).flatten().min()
    }

    /// Delivers every due batch as one delivery, while someone is `listening`; an unheard one
    /// goes on gathering. A value back to what was last delivered is left out. What a
    /// subscription since set to at once still holds goes as ordinary lines.
    pub fn flush_due(&mut self, listening: bool) -> Out {
        let mut out = Out::default();
        if !listening {
            return out;
        }
        let now = self.now_ms();
        let mut block = Vec::new();
        let mut since = u64::MAX;
        for sub in self.subs.iter_mut().filter(|s| s.due_ms.is_some_and(|due| due <= now)) {
            for (topic, value) in sub.take_pending() {
                let Some(line) = sub.deliver(&topic, &value, is_state(&topic), now, &mut out) else { continue };
                if sub.entry.batch_s.is_none() {
                    out.lines.push(line);
                    continue;
                }
                since = since.min(sub.since_ms);
                block.push(line);
            }
        }
        if !block.is_empty() {
            let header = render::batch_line(block.len(), since);
            block.insert(0, header);
            out.lines.push(block.join("\n"));
        }
        out
    }

    /// Fires each due timer, once, while someone is `listening`; an unheard one stays due.
    pub fn fire_due(&mut self, listening: bool) -> Out {
        let mut out = Out::default();
        if !listening {
            return out;
        }
        let now = self.now_ms();
        let mut spent = Vec::new();
        for (at, sub) in self.subs.iter().enumerate() {
            if let Some(due) = sub.entry.at_ms.filter(|due| *due <= now) {
                out.lines.push(render::at_line(&sub.entry.id, due, &sub.entry.note));
                spent.push(at);
            }
        }
        self.discard(&spent, &mut out);
        out
    }

    /// The `list` screen.
    pub fn list(&self, name: &str, connected: bool, readers: usize) -> String {
        render::list(self, name, connected, readers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ports::VirtualClock;
    use serde_json::json;

    fn core() -> (Core, VirtualClock) {
        let clock = VirtualClock::new(1_000_000);
        (Core::new(Arc::new(clock.clone()), Saved::default()), clock)
    }

    fn say(c: &mut Core, topic: &str, value: Value) -> Out {
        c.message(topic, value.to_string().as_bytes(), true)
    }

    #[test_id(tlo5svu)]
    #[test]
    fn an_add_whose_filter_matches_no_family_is_refused_and_changes_nothing() {
        let (mut c, _) = core();
        let why = c.add("nosuch/#", "", false, true).unwrap_err();
        assert!(why.contains("nosuch/#"), "{why}");
        assert!(c.is_empty());
        assert!(c.add("round/#", "", false, true).is_ok(), "positive control");
    }

    #[test_id(tfa7rf1)]
    #[test]
    fn once_drops_the_subscription_after_one_delivery() {
        let (mut c, _) = core();
        let (id, _) = c.add("bead/+/state", "landed?", true, true).unwrap();
        let first = say(&mut c, "bead/sp-a/state", json!({"state": "LANDED"}));
        assert_eq!(first.lines, [format!("[sub:{id}] bead/sp-a/state state=LANDED — landed?")]);
        assert_eq!(first.unsubscribe, ["bead/+/state"]);
        assert!(first.changed && c.is_empty());
        assert_eq!(say(&mut c, "bead/sp-b/state", json!({"state": "LANDED"})), Out::default());
    }

    #[test_id(t3vsp7h)]
    #[test]
    fn once_waits_for_a_reader_and_is_spent_by_the_replay() {
        let (mut c, _) = core();
        c.add("bead/sp-a/state", "", true, false).unwrap();
        let unheard = c.message("bead/sp-a/state", br#"{"state":"LANDED"}"#, false);
        assert!(unheard.lines.is_empty() && c.len() == 1, "{unheard:?}");
        assert_eq!(c.attach().lines, ["[sub:s1] bead/sp-a/state state=LANDED"]);
        assert!(c.is_empty());
    }

    #[test_id(te1stx6)]
    #[test]
    fn an_at_timer_fires_once_at_its_instant_and_not_again() {
        let (mut c, clock) = core();
        let at = clock.now_ms() + 60_000;
        let (id, _) = c.add_at(at, "look again");
        assert_eq!(c.next_due_ms(), Some(at));
        clock.advance_to(at - 1);
        assert_eq!(c.fire_due(true), Out::default());
        clock.advance_to(at);
        assert_eq!(c.fire_due(true).lines, [format!("[sub:{id}] at 1970-01-01T00:17:40Z — look again")]);
        clock.advance(60_000);
        assert_eq!(c.fire_due(true), Out::default());
        assert_eq!(c.next_due_ms(), None);
    }

    #[test_id(tylt1hd)]
    #[test]
    fn a_timer_due_with_no_reader_is_delivered_when_one_attaches() {
        let (mut c, clock) = core();
        c.add_at(clock.now_ms(), "n");
        assert_eq!(c.fire_due(false), Out::default());
        assert_eq!(c.attach().lines.len(), 1);
        assert!(c.is_empty());
    }

    #[test_id(te2npbl)]
    #[test]
    fn an_equal_retained_value_is_not_delivered_again_and_a_changed_one_is() {
        let (mut c, _) = core();
        c.add("release/active", "", false, true).unwrap();
        assert_eq!(say(&mut c, "release/active", json!({"release": "r1"})).lines.len(), 1);
        assert!(say(&mut c, "release/active", json!({"release": "r1"})).lines.is_empty());
        assert_eq!(say(&mut c, "release/active", json!({"release": "r2"})).lines, ["[sub:s1] release/active release=r2"]);
    }

    #[test_id(tphihze)]
    #[test]
    fn a_second_subscription_is_handed_a_value_the_first_already_had() {
        let (mut c, _) = core();
        c.add("release/active", "one", false, true).unwrap();
        say(&mut c, "release/active", json!({"release": "r1"}));
        c.add("release/#", "two", false, true).unwrap();
        assert_eq!(say(&mut c, "release/active", json!({"release": "r1"})).lines, ["[sub:s2] release/active release=r1 — two"]);
    }

    #[test_id(tttuj3p)]
    #[test]
    fn chatter_is_delivered_every_time() {
        let (mut c, _) = core();
        c.add("progress/+", "", false, true).unwrap();
        for _ in 0..2 {
            assert_eq!(say(&mut c, "progress/sp-a", json!({"note": "still compiling"})).lines, [r#"[sub:s1] progress/sp-a note="still compiling""#]);
        }
        assert!(c.attach().lines.is_empty(), "chatter is not retained");
    }

    #[test_id(t8ua73w)]
    #[test]
    fn the_copies_of_one_chatter_message_sent_for_overlapping_filters_are_one_delivery() {
        let (mut c, _) = core();
        c.add("progress/+", "", false, true).unwrap();
        c.add("progress/#", "", false, true).unwrap();
        let said = json!({"note": "n"});
        for publish in 0..2 {
            assert_eq!(say(&mut c, "progress/sp-a", said.clone()).lines.len(), 2, "publish {publish}: one line each");
            assert!(say(&mut c, "progress/sp-a", said.clone()).lines.is_empty(), "publish {publish}: the broker's second copy");
        }
    }

    #[test_id(t9eqtde)]
    #[test]
    fn a_resident_publisher_going_is_told_to_its_familys_subscriptions_once_per_going() {
        let (mut c, _) = core();
        c.add("bead/+/state", "n", true, true).unwrap();
        c.add("release/active", "", false, true).unwrap();
        let gone = json!({"state": "gone"});
        let first = say(&mut c, "publisher/lc-serve", gone.clone());
        assert_eq!(first.lines, ["[sub:s1] publisher/lc-serve state=gone — n"]);
        assert_eq!(c.len(), 2, "a notice does not spend --once");
        assert!(say(&mut c, "publisher/lc-serve", gone.clone()).lines.is_empty());
        assert!(say(&mut c, "publisher/lc-serve", json!({"state": "up", "since_ms": 1})).lines.is_empty());
        assert_eq!(say(&mut c, "publisher/lc-serve", gone.clone()).lines.len(), 1);
        assert!(say(&mut c, "publisher/queue", gone).lines.is_empty(), "a publisher that exits after each message is not news");
    }

    #[test_id(telhlp2)]
    #[test]
    fn host_pressure_subscribers_are_told_that_watchtower_went() {
        let (mut c, _) = core();
        c.add("host/pressure", "n", false, true).unwrap();
        let out = say(&mut c, "publisher/watchtower", json!({"state": "gone"}));
        assert_eq!(out.lines, ["[sub:s1] publisher/watchtower state=gone — n"]);
    }

    #[test_id(twwgm2e)]
    #[test]
    fn a_subscription_added_after_the_publisher_went_is_told_at_once() {
        let (mut c, _) = core();
        say(&mut c, "publisher/lc-serve", json!({"state": "gone"}));
        let (_, out) = c.add("bead/sp-a/state", "", false, true).unwrap();
        assert_eq!(out.lines, ["[sub:s1] publisher/lc-serve state=gone"]);
    }

    #[test_id(t88gcae)]
    #[test]
    fn a_dropped_subscription_is_silent_and_a_shared_filter_is_kept() {
        let (mut c, _) = core();
        c.add("round/#", "", false, true).unwrap();
        c.add("round/#", "", false, true).unwrap();
        assert!(c.remove("s1").unwrap().unsubscribe.is_empty());
        assert_eq!(c.remove("s2").unwrap().unsubscribe, ["round/#"]);
        assert!(say(&mut c, "round/r/1", json!({"event": "green"})).lines.is_empty());
        assert!(c.remove("s2").unwrap_err().contains("s2"));
    }

    #[test_id(temo6m2)]
    #[test]
    fn the_saved_set_holds_subscriptions_and_never_reuses_an_id() {
        let (mut c, clock) = core();
        c.add("round/#", "n", false, true).unwrap();
        say(&mut c, "round/r/1", json!({"event": "SECRET"}));
        c.add("gate/#", "", false, true).unwrap();
        c.remove("s2").unwrap();
        let text = serde_json::to_string(&c.saved()).unwrap();
        assert!(!text.contains("SECRET"), "the set file holds no message: {text}");
        let mut again = Core::new(Arc::new(clock), serde_json::from_str(&text).unwrap());
        assert_eq!(again.filters().into_iter().collect::<Vec<_>>(), ["round/#"]);
        assert_eq!(again.add("gate/#", "", false, true).unwrap().0, "s3");
    }

    const TEN_MINUTES: Option<u64> = Some(600);

    fn batched(c: &mut Core, filter: &str) -> String {
        c.add_classed(filter, "", false, TEN_MINUTES, true).unwrap().0
    }

    /// Moves the clock to the pending batch's instant and flushes it.
    fn at_due(c: &mut Core, clock: &VirtualClock) -> Out {
        clock.advance_to(c.next_due_ms().expect("a batch is pending"));
        c.flush_due(true)
    }

    #[test_id(trd8907)]
    #[test]
    fn ten_changes_inside_one_interval_are_one_delivery_of_the_last_value() {
        let (mut c, clock) = core();
        batched(&mut c, "bead/+/state");
        assert_eq!(c.next_due_ms(), None, "nothing pending arms no timer");
        for n in 1..=10 {
            assert!(say(&mut c, "bead/sp-a/state", json!({"state": format!("S{n}")})).lines.is_empty());
            clock.advance(1000);
        }
        assert_eq!(c.next_due_ms(), Some(1_200_000), "a multiple of the interval");
        assert!(c.list("concierge", true, 1).contains("batch 10m, 1 pending"));
        clock.advance_to(1_199_999);
        assert_eq!(c.flush_due(true), Out::default());
        assert_eq!(at_due(&mut c, &clock).lines, ["[batch] n=1 since 1970-01-01T00:16:40Z\n[sub:s1] bead/sp-a/state state=S10"]);
        assert_eq!(c.next_due_ms(), None);
        assert!(!c.list("concierge", true, 1).contains("pending"));
    }

    #[test_id(tbujcct)]
    #[test]
    fn subscriptions_with_one_interval_fall_due_together_in_one_delivery() {
        let (mut c, clock) = core();
        batched(&mut c, "release/active");
        batched(&mut c, "bead/+/state");
        say(&mut c, "release/active", json!({"release": "r1"}));
        clock.advance(60_000);
        say(&mut c, "bead/sp-a/state", json!({"state": "LANDED"}));
        let out = at_due(&mut c, &clock);
        assert_eq!(out.lines, ["[batch] n=2 since 1970-01-01T00:16:40Z\n[sub:s1] release/active release=r1\n[sub:s2] bead/sp-a/state state=LANDED"]);
    }

    #[test_id(t935pt3)]
    #[test]
    fn a_value_back_to_what_was_delivered_flushes_nothing() {
        let (mut c, clock) = core();
        batched(&mut c, "release/active");
        say(&mut c, "release/active", json!({"release": "a"}));
        assert_eq!(at_due(&mut c, &clock).lines.len(), 1, "positive control");
        say(&mut c, "release/active", json!({"release": "b"}));
        say(&mut c, "release/active", json!({"release": "a"}));
        assert!(at_due(&mut c, &clock).lines.is_empty());
        assert_eq!(c.next_due_ms(), None);
    }

    #[test_id(tq2bwdh)]
    #[test]
    fn an_unheard_batch_keeps_gathering_and_is_given_to_the_next_reader() {
        let (mut c, clock) = core();
        batched(&mut c, "progress/+");
        c.message("progress/sp-a", br#"{"note":"one"}"#, false);
        clock.advance(3_600_000);
        assert_eq!(c.flush_due(false), Out::default());
        c.message("progress/sp-a", br#"{"note":"two"}"#, false);
        assert_eq!(c.attach().lines, ["[batch] n=1 since 1970-01-01T00:16:40Z\n[sub:s1] progress/sp-a note=two"]);
    }

    #[test_id(t6c8cpc)]
    #[test]
    fn chatter_held_when_set_to_at_once_with_no_reader_reaches_the_next_one() {
        let (mut c, _) = core();
        batched(&mut c, "progress/+");
        batched(&mut c, "release/active");
        c.message("progress/sp-a", br#"{"note":"held"}"#, false);
        c.message("release/active", br#"{"release":"r1"}"#, false);
        assert!(c.set_class("s1", None, false).unwrap().lines.is_empty());
        assert!(c.set_class("s2", None, false).unwrap().lines.is_empty());
        assert_eq!(c.attach().lines, ["[sub:s2] release/active release=r1", "[sub:s1] progress/sp-a note=held"]);
        assert_eq!(c.next_due_ms(), None);
    }

    #[test_id(t4lutwq)]
    #[test]
    fn a_state_value_republished_with_its_keys_reordered_is_not_news() {
        let (mut c, _) = core();
        c.add("release/active", "", false, true).unwrap();
        assert_eq!(c.message("release/active", br#"{"a":1,"b":{"c":2,"d":3}}"#, true).lines.len(), 1);
        assert!(c.message("release/active", br#"{"b":{"d":3,"c":2},"a":1}"#, true).lines.is_empty());
    }

    #[test_id(tqfvk56)]
    #[test]
    fn a_value_that_arrived_unheard_before_batching_still_reaches_a_batch() {
        let (mut c, clock) = core();
        c.add("release/active", "", false, false).unwrap();
        c.message("release/active", br#"{"release":"r1"}"#, false);
        c.set_class("s1", TEN_MINUTES, false).unwrap();
        assert!(c.attach().lines.is_empty(), "it waits for its batch");
        assert_eq!(at_due(&mut c, &clock).lines.len(), 1);
    }

    #[test_id(t0aftrt)]
    #[test]
    fn a_cleared_topic_leaves_its_batch_and_its_next_value_is_news() {
        let (mut c, clock) = core();
        batched(&mut c, "release/active");
        say(&mut c, "release/active", json!({"release": "r1"}));
        assert!(!c.message("release/active", b"", true).changed);
        assert_eq!(c.next_due_ms(), None);
        say(&mut c, "release/active", json!({"release": "r1"}));
        assert_eq!(at_due(&mut c, &clock).lines.len(), 1);
        assert!(c.message("release/active", b"", true).changed, "what was given is forgotten on disk too");
        assert!(c.saved().subs[0].delivered.is_empty());
    }

    #[test_id(tdnhxi4)]
    #[test]
    fn a_publisher_going_is_told_to_a_batched_subscription_at_once() {
        let (mut c, _) = core();
        batched(&mut c, "bead/+/state");
        assert_eq!(say(&mut c, "publisher/lc-serve", json!({"state": "gone"})).lines, ["[sub:s1] publisher/lc-serve state=gone"]);
    }

    #[test_id(trisuar)]
    #[test]
    fn setting_a_batched_subscription_to_at_once_gives_what_it_held() {
        let (mut c, clock) = core();
        let id = batched(&mut c, "release/active");
        say(&mut c, "release/active", json!({"release": "r1"}));
        let out = c.set_class(&id, None, true).unwrap();
        assert_eq!(out.lines, ["[sub:s1] release/active release=r1"]);
        assert!(out.changed && out.unsubscribe.is_empty());
        assert_eq!(c.next_due_ms(), None);
        assert_eq!(say(&mut c, "release/active", json!({"release": "r2"})).lines.len(), 1);
        assert!(c.set_class(&id, TEN_MINUTES, true).unwrap().lines.is_empty());
        clock.advance(1000);
        assert!(say(&mut c, "release/active", json!({"release": "r3"})).lines.is_empty());
        c.set_class(&id, Some(60), true).unwrap();
        assert_eq!(c.next_due_ms(), Some(1_020_000), "a pending batch takes the new interval");
        assert!(c.set_class("s9", None, true).unwrap_err().contains("no subscription \"s9\""));
    }

    #[test_id(tpw9cb7)]
    #[test]
    fn a_restarted_core_does_not_repeat_what_the_saved_set_says_was_given() {
        let (mut c, clock) = core();
        c.add("release/active", "", false, true).unwrap();
        assert!(say(&mut c, "release/active", json!({"release": "SECRET"})).changed, "what was given must be written");
        let text = serde_json::to_string(&c.saved()).unwrap();
        assert!(!text.contains("SECRET"), "{text}");
        let mut again = Core::new(Arc::new(clock), serde_json::from_str(&text).unwrap());
        assert!(again.message("release/active", br#"{"release":"SECRET"}"#, false).lines.is_empty());
        assert!(again.attach().lines.is_empty());
        assert_eq!(say(&mut again, "release/active", json!({"release": "r2"})).lines.len(), 1);
    }

    #[test_id(trzjhod)]
    #[test]
    fn an_old_set_file_loads_as_delivered_at_once() {
        let saved: Saved = serde_json::from_str(r#"{"next":2,"subs":[{"id":"s1","filter":"release/active","note":"","once":false}]}"#).unwrap();
        let mut c = Core::new(Arc::new(VirtualClock::new(0)), saved);
        assert_eq!(say(&mut c, "release/active", json!({"release": "r1"})).lines.len(), 1);
    }

    #[test_id(tqv67vu)]
    #[test]
    fn batch_with_once_or_on_a_timer_is_refused_and_changes_nothing() {
        let (mut c, clock) = core();
        let why = c.add_classed("release/active", "", true, TEN_MINUTES, true).unwrap_err();
        assert!(why.contains("--batch") && why.contains("--once"), "{why}");
        assert!(c.is_empty());
        c.add("release/active", "", true, true).unwrap();
        c.add_at(clock.now_ms() + 1000, "n");
        assert!(c.set_class("s1", TEN_MINUTES, true).unwrap_err().contains("--once"));
        assert!(c.set_class("s2", TEN_MINUTES, true).unwrap_err().contains("--at"));
        assert!(c.saved().subs.iter().all(|s| s.batch_s.is_none()));
    }

    #[test_id(t2qin32)]
    #[test]
    fn the_list_screen_fits_a_phone_and_says_which_publishers_are_up() {
        let (mut c, clock) = core();
        c.add("bead/+/state", &"a long note ".repeat(12), true, true).unwrap();
        c.add("session/+/age/+", "", false, true).unwrap();
        c.add_at(clock.now_ms() + 1000, "later");
        say(&mut c, "publisher/lc-serve", json!({"state": "up", "since_ms": 1}));
        say(&mut c, "session/sp-a/age/15m", json!({"sessions": 1}));
        clock.advance(120_000);
        let screen = c.list("concierge", true, 1);
        assert!(screen.lines().all(|l| l.chars().count() <= 40), "{screen}");
        for want in ["s1 bead/+/state", "once", "fired 2m ago", "fired never", "lc-serve up", "on demand", "s3 at 1970-01-01T00:16:41Z"] {
            assert!(screen.contains(want), "{want}: {screen}");
        }
    }

    #[test_id(tzpz0yg)]
    #[test]
    fn a_new_connection_forgets_which_publishers_were_up() {
        let (mut c, _) = core();
        c.add("bead/+/state", "", false, true).unwrap();
        say(&mut c, "publisher/lc-serve", json!({"state": "up", "since_ms": 1}));
        assert!(c.list("concierge", true, 1).contains("lc-serve up"), "positive control");
        c.reconnected();
        let screen = c.list("concierge", true, 1);
        assert!(!screen.contains("lc-serve up"), "{screen}");
    }
}
