//! The subscription set and what is delivered for it. No I/O: time is the injected clock, and
//! every method returns the lines to stream and what the connection and the set file owe.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use ports::Clock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use spira_pub::topics;

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
    /// What the next batch holds: per topic, the latest value and whether it is state.
    pub(crate) pending: BTreeMap<String, (Value, bool)>,
    /// When the first of `pending` arrived, and when the batch is due. Set while it holds any.
    since_ms: u64,
    due_ms: Option<u64>,
    /// Per topic, the line carrying a state value that is not yet acked, and the value's digest.
    unacked_given: BTreeMap<String, (u64, String)>,
    /// The unacked line that spends this `--once` or `--at`: it sends nothing more meanwhile.
    spent_by: Option<u64>,
}

/// Who a line would reach, which decides when it counts as delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Nobody,
    /// A line counts as delivered as it is sent.
    Spending,
    /// A line counts once [`Core::ack`] says so.
    Acking,
}

/// What a reader that acks is told about a line besides its text.
#[derive(Debug, Clone, PartialEq)]
pub struct Receipt {
    pub seq: u64,
    pub subs: Vec<String>,
    /// Counts as delivered only once acked.
    pub held: bool,
}

/// The seq of every line made, and the subscriptions of each one still to be acked.
#[derive(Default)]
struct Ledger {
    last: u64,
    unacked: BTreeMap<u64, Vec<String>>,
}

impl Ledger {
    fn next(&mut self) -> u64 {
        self.last += 1;
        self.last
    }

    /// A dropped subscription's lines are no longer waited for: nothing would send them again.
    fn forget(&mut self, id: &str) {
        self.unacked.retain(|_, subs| {
            subs.retain(|s| s != id);
            !subs.is_empty()
        });
    }

    fn send(&mut self, mode: Mode, seq: u64, subs: Vec<String>, text: String, out: &mut Out) {
        let held = mode == Mode::Acking;
        if held {
            self.unacked.insert(seq, subs.clone());
        }
        out.lines.push(text);
        out.receipts.push(Receipt { seq, subs, held });
    }
}

/// What the store's topic record says of one family that has been published.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TopicRow {
    /// An MQTT filter: `+` stands for any one level.
    pub family: String,
    /// The publisher names that live as long as what they publish: one going is news to this
    /// family's subscribers.
    pub resident: Vec<String>,
}

/// What one step owes the outside.
#[derive(Debug, Default, PartialEq)]
pub struct Out {
    /// Each element is one delivery: one line, or a batch's header and lines.
    pub lines: Vec<String>,
    /// One for each of `lines`, in order.
    pub receipts: Vec<Receipt>,
    /// Filters no remaining subscription holds.
    pub unsubscribe: Vec<String>,
    /// The set changed and must be written.
    pub changed: bool,
    /// What the caller of an add should know about the filter it gave.
    pub warning: Option<String>,
}

pub struct Core {
    clock: Arc<dyn Clock>,
    next: u64,
    pub(crate) subs: Vec<Sub>,
    /// The current value of every subscribed state topic.
    current: BTreeMap<String, Value>,
    /// The chatter message last taken, and how many more copies of it the broker owes.
    echoes: (String, Vec<u8>, usize),
    /// The state topics that hold a value as far as this connection has heard.
    heard: BTreeSet<String>,
    /// The current value of every `publisher/<name>`.
    pub(crate) publishers: BTreeMap<String, Value>,
    /// The topic record as last read.
    pub(crate) topics: Vec<TopicRow>,
    /// Whether the latest read of the record succeeded.
    record_read: bool,
    ledger: Ledger,
}

fn is_gone(value: &Value) -> bool {
    value.get("state").and_then(Value::as_str) == Some("gone")
}

/// Whether `filter` watches a recorded family that `publisher` keeps alive.
fn depends_on(record: &[TopicRow], filter: &str, publisher: &str) -> bool {
    record.iter().any(|row| row.resident.iter().any(|r| r == publisher) && topics::filter_meets_family(filter, &row.family))
}

/// Why `filter` is refused.
pub fn malformed(filter: &str) -> String {
    format!("filter {filter:?} is not an MQTT topic filter: `+` and `#` are each a whole level, and `#` is the last")
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
        Sub { entry, last_fired_ms: None, pending: BTreeMap::new(), since_ms: 0, due_ms: None, unacked_given: BTreeMap::new(), spent_by: None }
    }

    fn filter(&self) -> Option<&str> {
        self.entry.filter.as_deref()
    }

    /// Whether `value` is the one last sent here, acked or still waiting to be.
    fn already_given(&self, topic: &str, value: &Value) -> bool {
        let last = self.unacked_given.get(topic).map(|(_, digest)| digest).or_else(|| self.entry.delivered.get(topic));
        last == Some(&digest(value))
    }

    /// The line `seq` for `topic` = `value`, unless it is the state value last sent here or this
    /// subscription is waiting to be spent. With [`Core::ack`], the one place a value counts as
    /// given.
    #[allow(clippy::too_many_arguments)]
    fn deliver(&mut self, topic: &str, value: &Value, state: bool, now: u64, mode: Mode, seq: u64, out: &mut Out) -> Option<String> {
        if self.spent_by.is_some() {
            return None;
        }
        if state {
            if self.already_given(topic, value) {
                return None;
            }
            if mode == Mode::Acking {
                self.unacked_given.insert(topic.to_string(), (seq, digest(value)));
                // Which value the reader holds is unknown until the ack, so an older one kept
                // here would silence a return to it.
                out.changed |= self.entry.delivered.remove(topic).is_some();
            } else {
                self.unacked_given.remove(topic);
                self.entry.delivered.insert(topic.to_string(), digest(value));
                out.changed = true;
            }
        }
        self.last_fired_ms = Some(now);
        Some(render::line(&self.entry.id, topic, value, &self.entry.note))
    }

    /// Spends this `--once` or `--at` on the line `seq`: true when it is to be dropped now,
    /// false when only that line's ack drops it.
    fn spend(&mut self, mode: Mode, seq: u64) -> bool {
        if mode == Mode::Acking {
            self.spent_by = Some(seq);
        }
        mode != Mode::Acking
    }

    fn hold(&mut self, topic: &str, value: &Value, state: bool, batch_s: u64, now: u64) {
        if self.due_ms.is_none() {
            self.since_ms = now;
            self.due_ms = Some(boundary(now, batch_s));
        }
        self.pending.insert(topic.to_string(), (value.clone(), state));
    }

    /// The topic holds no value any more: there is none to batch, and its next one is news.
    fn clear(&mut self, topic: &str, out: &mut Out) {
        self.pending.remove(topic);
        self.unacked_given.remove(topic);
        if self.pending.is_empty() {
            self.due_ms = None;
        }
        out.changed |= self.entry.delivered.remove(topic).is_some();
    }

    /// A line for each gone publisher the filter depends on but does not itself select.
    #[allow(clippy::too_many_arguments)]
    fn notify_gone(&mut self, publishers: &BTreeMap<String, Value>, record: &[TopicRow], now: u64, mode: Mode, ledger: &mut Ledger, out: &mut Out) {
        let Some(filter) = self.entry.filter.clone() else { return };
        for (topic, value) in publishers {
            if is_gone(value) && !topics::filter_selects(&filter, topic) && depends_on(record, &filter, topic.trim_start_matches("publisher/")) {
                let seq = ledger.next();
                if let Some(line) = self.deliver(topic, value, true, now, mode, seq, out) {
                    ledger.send(mode, seq, vec![self.entry.id.clone()], line, out);
                }
            }
        }
    }

    /// Ends the batch being gathered and returns what it held.
    fn take_pending(&mut self) -> BTreeMap<String, (Value, bool)> {
        self.due_ms = None;
        std::mem::take(&mut self.pending)
    }
}

impl Core {
    pub fn new(clock: Arc<dyn Clock>, saved: Saved) -> Core {
        let subs = saved.subs.into_iter().map(Sub::new).collect();
        Core { clock, next: saved.next.max(1), subs, current: BTreeMap::new(), heard: BTreeSet::new(), publishers: BTreeMap::new(), echoes: Default::default(), topics: Vec::new(), record_read: false, ledger: Ledger::default() }
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

    /// The topic record, just read.
    pub fn set_topics(&mut self, rows: Vec<TopicRow>) {
        self.topics = rows;
        self.record_read = true;
    }

    /// The latest read of the record failed: what it last said still names the residents.
    pub fn topics_unread(&mut self) {
        self.record_read = false;
    }

    /// Tells each subscription of the gone publishers it depends on and has not been told of.
    pub fn gone_notices(&mut self, mode: Mode) -> Out {
        let mut out = Out::default();
        if mode == Mode::Nobody {
            return out;
        }
        let now = self.now_ms();
        for sub in &mut self.subs {
            sub.notify_gone(&self.publishers, &self.topics, now, mode, &mut self.ledger, &mut out);
        }
        out
    }

    /// A new connection: who is up is unknown until the broker says, since a restarted one
    /// holds nothing about a publisher that does not come back.
    pub fn reconnected(&mut self) {
        self.publishers.clear();
        self.heard.clear();
    }

    /// The broker has sent all it retains and kept it since the last connection, so a state
    /// topic not heard was cleared in between. Wrong to call for a restarted broker, whose
    /// publishers have yet to say their values again.
    pub fn synced(&mut self) -> Out {
        let mut out = Out::default();
        for sub in &mut self.subs {
            let pending = sub.pending.iter().filter(|(_, (_, state))| *state).map(|(t, _)| t);
            let unheard: BTreeSet<String> = sub.entry.delivered.keys().chain(sub.unacked_given.keys()).chain(pending).filter(|t| !self.heard.contains(*t)).cloned().collect();
            for topic in unheard {
                sub.clear(&topic, &mut out);
            }
        }
        self.current.retain(|topic, _| self.heard.contains(topic));
        out
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
    pub fn add(&mut self, filter: &str, note: &str, once: bool, mode: Mode) -> Result<(String, Out), String> {
        self.add_classed(filter, note, once, None, mode)
    }

    /// A new subscription, batched when `batch_s` is given. The caller subscribes its filter, which is what brings the retained
    /// values; a publisher already known to be gone is said here, unless there is [`Mode::Nobody`].
    pub fn add_classed(&mut self, filter: &str, note: &str, once: bool, batch_s: Option<u64>, mode: Mode) -> Result<(String, Out), String> {
        if once && batch_s.is_some() {
            return Err("--batch conflicts with --once: a --once subscription delivers at once".to_string());
        }
        if !topics::filter_is_well_formed(filter) {
            return Err(malformed(filter));
        }
        let warning = if !self.record_read {
            Some("cannot check the filter against the record; subscribed anyway".to_string())
        } else if !self.topics.iter().any(|row| topics::filter_meets_family(filter, &row.family)) {
            Some(format!("filter \"{filter}\" matches no topic ever published; subscribed anyway"))
        } else {
            None
        };
        let id = self.push(Some(filter.to_string()), None, note, once);
        let mut out = Out { changed: true, warning, ..Out::default() };
        if mode != Mode::Nobody {
            let now = self.now_ms();
            let sub = self.subs.last_mut().expect("just pushed");
            sub.notify_gone(&self.publishers, &self.topics, now, mode, &mut self.ledger, &mut out);
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
    /// [`Mode::Nobody`], the next reader's replay has the state values, and the chatter,
    /// which no replay holds, stays pending and due.
    pub fn set_class(&mut self, id: &str, batch_s: Option<u64>, mode: Mode) -> Result<Out, String> {
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
            None if mode != Mode::Nobody => {
                for (topic, (value, state)) in sub.take_pending() {
                    let seq = self.ledger.next();
                    if let Some(line) = sub.deliver(&topic, &value, state, now, mode, seq, &mut out) {
                        self.ledger.send(mode, seq, vec![id.to_string()], line, &mut out);
                    }
                }
            }
            None => {
                sub.pending.retain(|_, (_, state)| !*state);
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
            let entry = self.subs.remove(at).entry;
            self.ledger.forget(&entry.id);
            gone.extend(entry.filter);
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

    /// A message from the broker, `retained` being the publisher's retain flag: set, the value
    /// is state. With [`Mode::Nobody`] nothing counts as delivered, so a
    /// `--once` subscription waits for a reader; the value is kept for its replay, and a
    /// batched subscription's in its pending batch.
    pub fn message(&mut self, topic: &str, payload: &[u8], retained: bool, mode: Mode) -> Out {
        let mut out = Out::default();
        let liveness = topic.starts_with("publisher/");
        if payload.is_empty() {
            self.heard.remove(topic);
            self.current.remove(topic);
            self.publishers.remove(topic);
            self.subs.iter_mut().for_each(|s| s.clear(topic, &mut out));
            return out;
        }
        let value = serde_json::from_slice(payload).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(payload).into_owned()));
        let state = retained;
        if !state && self.echo(topic, payload) {
            return out;
        }
        if state {
            self.heard.insert(topic.to_string());
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
            if liveness && !direct && !is_gone(&value) {
                // Back up: its next going is news again, whatever the record says by now.
                sub.clear(topic, &mut out);
                continue;
            }
            let notice = !direct && liveness && depends_on(&self.topics, &filter, topic.trim_start_matches("publisher/"));
            if let Some(batch_s) = sub.entry.batch_s.filter(|_| direct) {
                sub.hold(topic, &value, state, batch_s, now);
                continue;
            }
            if mode == Mode::Nobody || !(direct || notice) {
                continue;
            }
            let seq = self.ledger.next();
            let Some(line) = sub.deliver(topic, &value, state, now, mode, seq, &mut out) else { continue };
            self.ledger.send(mode, seq, vec![sub.entry.id.clone()], line, &mut out);
            if direct && sub.entry.once && sub.spend(mode, seq) {
                spent.push(at);
            }
        }
        self.discard(&spent, &mut out);
        out
    }

    /// What a newly attached reader is told first: each subscribed state value not yet
    /// delivered, every batch already due and every timer already due.
    pub fn attach(&mut self, mode: Mode) -> Out {
        let mut out = Out::default();
        let now = self.now_ms();
        let mut spent = Vec::new();
        for (at, sub) in self.subs.iter_mut().enumerate() {
            let Some(filter) = sub.entry.filter.clone() else { continue };
            sub.notify_gone(&self.publishers, &self.topics, now, mode, &mut self.ledger, &mut out);
            for (topic, value) in self.current.iter().filter(|(t, _)| topics::filter_selects(&filter, t)) {
                if let Some(batch_s) = sub.entry.batch_s {
                    // Unheard while it was delivered at once, so no batch holds it yet.
                    if !sub.already_given(topic, value) {
                        sub.hold(topic, value, true, batch_s, now);
                    }
                    continue;
                }
                let seq = self.ledger.next();
                let Some(line) = sub.deliver(topic, value, true, now, mode, seq, &mut out) else { continue };
                self.ledger.send(mode, seq, vec![sub.entry.id.clone()], line, &mut out);
                if sub.entry.once {
                    if sub.spend(mode, seq) {
                        spent.push(at);
                    }
                    break;
                }
            }
        }
        self.discard(&spent, &mut out);
        for due in [self.flush_due(mode), self.fire_due(mode)] {
            out.lines.extend(due.lines);
            out.receipts.extend(due.receipts);
            out.unsubscribe.extend(due.unsubscribe);
            out.changed |= due.changed;
        }
        out
    }

    /// When the earliest timer or batch is due.
    pub fn next_due_ms(&self) -> Option<u64> {
        // A fired timer awaiting its ack stays in the set, and must not be woken for again.
        self.subs.iter().flat_map(|s| [s.entry.at_ms.filter(|_| s.spent_by.is_none()), s.due_ms]).flatten().min()
    }

    /// Delivers every due batch as one delivery, unless there is [`Mode::Nobody`], whose batch
    /// goes on gathering. A value back to what was last delivered is left out. What a
    /// subscription since set to at once still holds goes as ordinary lines.
    pub fn flush_due(&mut self, mode: Mode) -> Out {
        let mut out = Out::default();
        if mode == Mode::Nobody {
            return out;
        }
        let now = self.now_ms();
        // First what no longer batches: its lines go ahead of the batch, so are numbered first.
        for sub in self.subs.iter_mut().filter(|s| s.entry.batch_s.is_none() && s.due_ms.is_some_and(|due| due <= now)) {
            for (topic, (value, state)) in sub.take_pending() {
                let seq = self.ledger.next();
                if let Some(line) = sub.deliver(&topic, &value, state, now, mode, seq, &mut out) {
                    self.ledger.send(mode, seq, vec![sub.entry.id.clone()], line, &mut out);
                }
            }
        }
        let batch_seq = self.ledger.next();
        let mut block = Vec::new();
        let mut held_by = Vec::new();
        let mut since = u64::MAX;
        for sub in self.subs.iter_mut().filter(|s| s.due_ms.is_some_and(|due| due <= now)) {
            for (topic, (value, state)) in sub.take_pending() {
                let Some(line) = sub.deliver(&topic, &value, state, now, mode, batch_seq, &mut out) else { continue };
                since = since.min(sub.since_ms);
                block.push(line);
                if !held_by.contains(&sub.entry.id) {
                    held_by.push(sub.entry.id.clone());
                }
            }
        }
        if !block.is_empty() {
            let header = render::batch_line(block.len(), since);
            block.insert(0, header);
            self.ledger.send(mode, batch_seq, held_by, block.join("\n"), &mut out);
        }
        out
    }

    /// Fires each due timer, once, unless there is [`Mode::Nobody`], for which it stays due.
    pub fn fire_due(&mut self, mode: Mode) -> Out {
        let mut out = Out::default();
        if mode == Mode::Nobody {
            return out;
        }
        let now = self.now_ms();
        let mut spent = Vec::new();
        for (at, sub) in self.subs.iter_mut().enumerate() {
            let Some(due) = sub.entry.at_ms.filter(|due| *due <= now && sub.spent_by.is_none()) else { continue };
            let seq = self.ledger.next();
            let line = render::at_line(&sub.entry.id, due, &sub.entry.note);
            self.ledger.send(mode, seq, vec![sub.entry.id.clone()], line, &mut out);
            if sub.spend(mode, seq) {
                spent.push(at);
            }
        }
        self.discard(&spent, &mut out);
        out
    }

    /// Whether the line `seq` is still waiting for its ack.
    pub fn awaits(&self, seq: u64) -> bool {
        self.ledger.unacked.contains_key(&seq)
    }

    /// The lines waiting for an ack.
    pub fn unacked(&self) -> usize {
        self.ledger.unacked.len()
    }

    /// The lines waiting for an ack that carry something of `id`'s.
    pub(crate) fn unacked_of(&self, id: &str) -> usize {
        self.ledger.unacked.values().filter(|subs| subs.iter().any(|s| s == id)).count()
    }

    /// The line `seq` reached its reader: what it gave is now given, and what it spent is
    /// dropped. Nothing happens for a seq that is not waiting.
    pub fn ack(&mut self, seq: u64) -> Out {
        let mut out = Out::default();
        let Some(subs) = self.ledger.unacked.remove(&seq) else { return out };
        let mut spent = Vec::new();
        for (at, sub) in self.subs.iter_mut().enumerate().filter(|(_, s)| subs.contains(&s.entry.id)) {
            let delivered = &mut sub.entry.delivered;
            sub.unacked_given.retain(|topic, (by, digest)| {
                if *by != seq {
                    return true;
                }
                delivered.insert(topic.clone(), std::mem::take(digest));
                out.changed = true;
                false
            });
            if sub.spent_by == Some(seq) {
                spent.push(at);
            }
        }
        self.discard(&spent, &mut out);
        out
    }

    /// The acking reader went: every line it had not acked is forgotten. Nothing was recorded
    /// for them, so the next [`Core::attach`] gives again what can be given again.
    pub fn released(&mut self) {
        self.ledger.unacked.clear();
        for sub in &mut self.subs {
            sub.unacked_given.clear();
            sub.spent_by = None;
        }
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

    fn row(family: &str, resident: &[&str]) -> TopicRow {
        TopicRow { family: family.to_string(), resident: resident.iter().map(|r| r.to_string()).collect() }
    }

    fn record() -> Vec<TopicRow> {
        vec![
            row("bead/+/state", &["lc-serve"]),
            row("gate/+/+", &[]),
            row("host/pressure", &["watchtower"]),
            row("progress/+", &[]),
            row("publisher/+", &["lc-serve", "watchtower"]),
            row("release/active", &[]),
            row("round/+/+", &[]),
            row("session/+/age/+", &[]),
        ]
    }

    fn recorded(clock: &VirtualClock, saved: Saved) -> Core {
        let mut c = Core::new(Arc::new(clock.clone()), saved);
        c.set_topics(record());
        c
    }

    fn core() -> (Core, VirtualClock) {
        let clock = VirtualClock::new(1_000_000);
        (recorded(&clock, Saved::default()), clock)
    }

    /// A state publish: retained.
    fn say(c: &mut Core, topic: &str, value: Value) -> Out {
        c.message(topic, value.to_string().as_bytes(), true, Mode::Spending)
    }

    fn chat(c: &mut Core, topic: &str, value: Value) -> Out {
        c.message(topic, value.to_string().as_bytes(), false, Mode::Spending)
    }

    #[test_id(t8oxs9z)]
    #[test]
    fn a_malformed_filter_is_refused_and_one_matching_nothing_published_is_taken_with_a_warning() {
        let (mut c, _) = core();
        for bad in ["a/#/b", ""] {
            let why = c.add(bad, "", false, Mode::Spending).unwrap_err();
            assert!(why.contains(&format!("{bad:?}")), "{why}");
        }
        assert!(c.is_empty());
        let (_, out) = c.add("nosuch/#", "", false, Mode::Spending).unwrap();
        assert_eq!(out.warning.as_deref(), Some(r#"filter "nosuch/#" matches no topic ever published; subscribed anyway"#));
        assert_eq!(c.add("round/#", "", false, Mode::Spending).unwrap().1.warning, None, "positive control");
        c.topics_unread();
        let warning = c.add("round/#", "", false, Mode::Spending).unwrap().1.warning.unwrap();
        assert!(warning.contains("cannot check the filter against the record"), "{warning}");
    }

    #[test_id(t82299p)]
    #[test]
    fn the_delivered_retain_flag_alone_says_whether_an_equal_value_is_news() {
        let (mut c, _) = core();
        c.add("unlisted/x", "", false, Mode::Spending).unwrap();
        assert_eq!(say(&mut c, "unlisted/x", json!({"n": 1})).lines.len(), 1);
        assert!(say(&mut c, "unlisted/x", json!({"n": 1})).lines.is_empty(), "retained: state");
        for _ in 0..2 {
            assert_eq!(chat(&mut c, "unlisted/x", json!({"n": 1})).lines, ["[sub:s1] unlisted/x n=1"], "not retained: chatter");
        }
    }

    #[test_id(t97s1d6)]
    #[test]
    fn a_resident_named_by_the_record_going_follows_its_familys_last_value() {
        let clock = VirtualClock::new(1_000_000);
        let mut c = Core::new(Arc::new(clock), Saved::default());
        c.add("bead/+/state", "", false, Mode::Spending).unwrap();
        assert_eq!(say(&mut c, "bead/sp-a/state", json!({"state": "WORKING"})).lines, ["[sub:s1] bead/sp-a/state state=WORKING"]);
        assert!(say(&mut c, "publisher/lc-serve", json!({"state": "gone"})).lines.is_empty(), "no record, no resident");
        c.set_topics(vec![row("bead/+/state", &["lc-serve"])]);
        assert_eq!(c.gone_notices(Mode::Spending).lines, ["[sub:s1] publisher/lc-serve state=gone"]);
        assert!(c.gone_notices(Mode::Spending).lines.is_empty(), "told once");
    }

    #[test_id(t4nc54b)]
    #[test]
    fn a_publisher_back_up_before_the_record_is_read_makes_its_next_going_news() {
        let (mut before, clock) = core();
        before.add("bead/+/state", "", false, Mode::Spending).unwrap();
        assert_eq!(say(&mut before, "publisher/lc-serve", json!({"state": "gone"})).lines.len(), 1, "positive control");
        let mut c = Core::new(Arc::new(clock), before.saved());
        say(&mut c, "publisher/lc-serve", json!({"state": "up", "since_ms": 2}));
        c.set_topics(record());
        assert_eq!(say(&mut c, "publisher/lc-serve", json!({"state": "gone"})).lines, ["[sub:s1] publisher/lc-serve state=gone"]);
    }

    #[test_id(ts3ekw7)]
    #[test]
    fn fields_print_in_the_payloads_key_order_with_nulls_left_out() {
        let (mut c, _) = core();
        c.add("bead/+/state", "", false, Mode::Spending).unwrap();
        // Neither sorted nor any declared order, so only the bytes can have given it.
        let said = br#"{"from":"READY","state":"WORKING","actor":null,"at":100}"#;
        assert_eq!(c.message("bead/sp-a/state", said, true, Mode::Spending).lines, ["[sub:s1] bead/sp-a/state from=READY state=WORKING at=100"]);
    }

    #[test_id(tfa7rf1)]
    #[test]
    fn once_drops_the_subscription_after_one_delivery() {
        let (mut c, _) = core();
        let (id, _) = c.add("bead/+/state", "landed?", true, Mode::Spending).unwrap();
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
        c.add("bead/sp-a/state", "", true, Mode::Nobody).unwrap();
        let unheard = c.message("bead/sp-a/state", br#"{"state":"LANDED"}"#, true, Mode::Nobody);
        assert!(unheard.lines.is_empty() && c.len() == 1, "{unheard:?}");
        assert_eq!(c.attach(Mode::Spending).lines, ["[sub:s1] bead/sp-a/state state=LANDED"]);
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
        assert_eq!(c.fire_due(Mode::Spending), Out::default());
        clock.advance_to(at);
        assert_eq!(c.fire_due(Mode::Spending).lines, [format!("[sub:{id}] at 1970-01-01T00:17:40Z — look again")]);
        clock.advance(60_000);
        assert_eq!(c.fire_due(Mode::Spending), Out::default());
        assert_eq!(c.next_due_ms(), None);
    }

    #[test_id(tylt1hd)]
    #[test]
    fn a_timer_due_with_no_reader_is_delivered_when_one_attaches() {
        let (mut c, clock) = core();
        c.add_at(clock.now_ms(), "n");
        assert_eq!(c.fire_due(Mode::Nobody), Out::default());
        assert_eq!(c.attach(Mode::Spending).lines.len(), 1);
        assert!(c.is_empty());
    }

    #[test_id(te2npbl)]
    #[test]
    fn an_equal_retained_value_is_not_delivered_again_and_a_changed_one_is() {
        let (mut c, _) = core();
        c.add("release/active", "", false, Mode::Spending).unwrap();
        assert_eq!(say(&mut c, "release/active", json!({"release": "r1"})).lines.len(), 1);
        assert!(say(&mut c, "release/active", json!({"release": "r1"})).lines.is_empty());
        assert_eq!(say(&mut c, "release/active", json!({"release": "r2"})).lines, ["[sub:s1] release/active release=r2"]);
    }

    #[test_id(tphihze)]
    #[test]
    fn a_second_subscription_is_handed_a_value_the_first_already_had() {
        let (mut c, _) = core();
        c.add("release/active", "one", false, Mode::Spending).unwrap();
        say(&mut c, "release/active", json!({"release": "r1"}));
        c.add("release/#", "two", false, Mode::Spending).unwrap();
        assert_eq!(say(&mut c, "release/active", json!({"release": "r1"})).lines, ["[sub:s2] release/active release=r1 — two"]);
    }

    #[test_id(tttuj3p)]
    #[test]
    fn chatter_is_delivered_every_time() {
        let (mut c, _) = core();
        c.add("progress/+", "", false, Mode::Spending).unwrap();
        for _ in 0..2 {
            assert_eq!(chat(&mut c, "progress/sp-a", json!({"note": "still compiling"})).lines, [r#"[sub:s1] progress/sp-a note="still compiling""#]);
        }
        assert!(c.attach(Mode::Spending).lines.is_empty(), "chatter is not retained");
    }

    #[test_id(t8ua73w)]
    #[test]
    fn the_copies_of_one_chatter_message_sent_for_overlapping_filters_are_one_delivery() {
        let (mut c, _) = core();
        c.add("progress/+", "", false, Mode::Spending).unwrap();
        c.add("progress/#", "", false, Mode::Spending).unwrap();
        let said = json!({"note": "n"});
        for publish in 0..2 {
            assert_eq!(chat(&mut c, "progress/sp-a", said.clone()).lines.len(), 2, "publish {publish}: one line each");
            assert!(chat(&mut c, "progress/sp-a", said.clone()).lines.is_empty(), "publish {publish}: the broker's second copy");
        }
    }

    #[test_id(t9eqtde)]
    #[test]
    fn a_resident_publisher_going_is_told_to_its_familys_subscriptions_once_per_going() {
        let (mut c, _) = core();
        c.add("bead/+/state", "n", true, Mode::Spending).unwrap();
        c.add("release/active", "", false, Mode::Spending).unwrap();
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
        c.add("host/pressure", "n", false, Mode::Spending).unwrap();
        let out = say(&mut c, "publisher/watchtower", json!({"state": "gone"}));
        assert_eq!(out.lines, ["[sub:s1] publisher/watchtower state=gone — n"]);
    }

    #[test_id(twwgm2e)]
    #[test]
    fn a_subscription_added_after_the_publisher_went_is_told_at_once() {
        let (mut c, _) = core();
        say(&mut c, "publisher/lc-serve", json!({"state": "gone"}));
        let (_, out) = c.add("bead/sp-a/state", "", false, Mode::Spending).unwrap();
        assert_eq!(out.lines, ["[sub:s1] publisher/lc-serve state=gone"]);
    }

    #[test_id(t88gcae)]
    #[test]
    fn a_dropped_subscription_is_silent_and_a_shared_filter_is_kept() {
        let (mut c, _) = core();
        c.add("round/#", "", false, Mode::Spending).unwrap();
        c.add("round/#", "", false, Mode::Spending).unwrap();
        assert!(c.remove("s1").unwrap().unsubscribe.is_empty());
        assert_eq!(c.remove("s2").unwrap().unsubscribe, ["round/#"]);
        assert!(say(&mut c, "round/r/1", json!({"event": "green"})).lines.is_empty());
        assert!(c.remove("s2").unwrap_err().contains("s2"));
    }

    #[test_id(temo6m2)]
    #[test]
    fn the_saved_set_holds_subscriptions_and_never_reuses_an_id() {
        let (mut c, clock) = core();
        c.add("round/#", "n", false, Mode::Spending).unwrap();
        say(&mut c, "round/r/1", json!({"event": "SECRET"}));
        c.add("gate/#", "", false, Mode::Spending).unwrap();
        c.remove("s2").unwrap();
        let text = serde_json::to_string(&c.saved()).unwrap();
        assert!(!text.contains("SECRET"), "the set file holds no message: {text}");
        let mut again = recorded(&clock, serde_json::from_str(&text).unwrap());
        assert_eq!(again.filters().into_iter().collect::<Vec<_>>(), ["round/#"]);
        assert_eq!(again.add("gate/#", "", false, Mode::Spending).unwrap().0, "s3");
    }

    const TEN_MINUTES: Option<u64> = Some(600);

    fn batched(c: &mut Core, filter: &str) -> String {
        c.add_classed(filter, "", false, TEN_MINUTES, Mode::Spending).unwrap().0
    }

    /// Moves the clock to the pending batch's instant and flushes it.
    fn at_due(c: &mut Core, clock: &VirtualClock) -> Out {
        clock.advance_to(c.next_due_ms().expect("a batch is pending"));
        c.flush_due(Mode::Spending)
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
        assert_eq!(c.flush_due(Mode::Spending), Out::default());
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
        c.message("progress/sp-a", br#"{"note":"one"}"#, false, Mode::Nobody);
        clock.advance(3_600_000);
        assert_eq!(c.flush_due(Mode::Nobody), Out::default());
        c.message("progress/sp-a", br#"{"note":"two"}"#, false, Mode::Nobody);
        assert_eq!(c.attach(Mode::Spending).lines, ["[batch] n=1 since 1970-01-01T00:16:40Z\n[sub:s1] progress/sp-a note=two"]);
    }

    #[test_id(t6c8cpc)]
    #[test]
    fn chatter_held_when_set_to_at_once_with_no_reader_reaches_the_next_one() {
        let (mut c, _) = core();
        batched(&mut c, "progress/+");
        batched(&mut c, "release/active");
        c.message("progress/sp-a", br#"{"note":"held"}"#, false, Mode::Nobody);
        c.message("release/active", br#"{"release":"r1"}"#, true, Mode::Nobody);
        assert!(c.set_class("s1", None, Mode::Nobody).unwrap().lines.is_empty());
        assert!(c.set_class("s2", None, Mode::Nobody).unwrap().lines.is_empty());
        assert_eq!(c.attach(Mode::Spending).lines, ["[sub:s2] release/active release=r1", "[sub:s1] progress/sp-a note=held"]);
        assert_eq!(c.next_due_ms(), None);
    }

    #[test_id(t4lutwq)]
    #[test]
    fn a_state_value_republished_with_its_keys_reordered_is_not_news() {
        let (mut c, _) = core();
        c.add("release/active", "", false, Mode::Spending).unwrap();
        assert_eq!(c.message("release/active", br#"{"a":1,"b":{"c":2,"d":3}}"#, true, Mode::Spending).lines.len(), 1);
        assert!(c.message("release/active", br#"{"b":{"d":3,"c":2},"a":1}"#, true, Mode::Spending).lines.is_empty());
    }

    #[test_id(tqfvk56)]
    #[test]
    fn a_value_that_arrived_unheard_before_batching_still_reaches_a_batch() {
        let (mut c, clock) = core();
        c.add("release/active", "", false, Mode::Nobody).unwrap();
        c.message("release/active", br#"{"release":"r1"}"#, true, Mode::Nobody);
        c.set_class("s1", TEN_MINUTES, Mode::Nobody).unwrap();
        assert!(c.attach(Mode::Spending).lines.is_empty(), "it waits for its batch");
        assert_eq!(at_due(&mut c, &clock).lines.len(), 1);
    }

    #[test_id(t0aftrt)]
    #[test]
    fn a_cleared_topic_leaves_its_batch_and_its_next_value_is_news() {
        let (mut c, clock) = core();
        batched(&mut c, "release/active");
        say(&mut c, "release/active", json!({"release": "r1"}));
        assert!(!c.message("release/active", b"", true, Mode::Spending).changed);
        assert_eq!(c.next_due_ms(), None);
        say(&mut c, "release/active", json!({"release": "r1"}));
        assert_eq!(at_due(&mut c, &clock).lines.len(), 1);
        assert!(c.message("release/active", b"", true, Mode::Spending).changed, "what was given is forgotten on disk too");
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
        let out = c.set_class(&id, None, Mode::Spending).unwrap();
        assert_eq!(out.lines, ["[sub:s1] release/active release=r1"]);
        assert!(out.changed && out.unsubscribe.is_empty());
        assert_eq!(c.next_due_ms(), None);
        assert_eq!(say(&mut c, "release/active", json!({"release": "r2"})).lines.len(), 1);
        assert!(c.set_class(&id, TEN_MINUTES, Mode::Spending).unwrap().lines.is_empty());
        clock.advance(1000);
        assert!(say(&mut c, "release/active", json!({"release": "r3"})).lines.is_empty());
        c.set_class(&id, Some(60), Mode::Spending).unwrap();
        assert_eq!(c.next_due_ms(), Some(1_020_000), "a pending batch takes the new interval");
        assert!(c.set_class("s9", None, Mode::Spending).unwrap_err().contains("no subscription \"s9\""));
    }

    #[test_id(tpw9cb7)]
    #[test]
    fn a_restarted_core_does_not_repeat_what_the_saved_set_says_was_given() {
        let (mut c, clock) = core();
        c.add("release/active", "", false, Mode::Spending).unwrap();
        assert!(say(&mut c, "release/active", json!({"release": "SECRET"})).changed, "what was given must be written");
        let text = serde_json::to_string(&c.saved()).unwrap();
        assert!(!text.contains("SECRET"), "{text}");
        let mut again = Core::new(Arc::new(clock), serde_json::from_str(&text).unwrap());
        assert!(again.message("release/active", br#"{"release":"SECRET"}"#, true, Mode::Nobody).lines.is_empty());
        assert!(again.attach(Mode::Spending).lines.is_empty());
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
        let why = c.add_classed("release/active", "", true, TEN_MINUTES, Mode::Spending).unwrap_err();
        assert!(why.contains("--batch") && why.contains("--once"), "{why}");
        assert!(c.is_empty());
        c.add("release/active", "", true, Mode::Spending).unwrap();
        c.add_at(clock.now_ms() + 1000, "n");
        assert!(c.set_class("s1", TEN_MINUTES, Mode::Spending).unwrap_err().contains("--once"));
        assert!(c.set_class("s2", TEN_MINUTES, Mode::Spending).unwrap_err().contains("--at"));
        assert!(c.saved().subs.iter().all(|s| s.batch_s.is_none()));
    }

    #[test_id(t2qin32)]
    #[test]
    fn the_list_screen_fits_a_phone_and_says_which_publishers_are_up() {
        let (mut c, clock) = core();
        c.add("bead/+/state", &"a long note ".repeat(12), true, Mode::Spending).unwrap();
        c.add("session/+/age/+", "", false, Mode::Spending).unwrap();
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
        c.add("bead/+/state", "", false, Mode::Spending).unwrap();
        say(&mut c, "publisher/lc-serve", json!({"state": "up", "since_ms": 1}));
        assert!(c.list("concierge", true, 1).contains("lc-serve up"), "positive control");
        c.reconnected();
        let screen = c.list("concierge", true, 1);
        assert!(!screen.contains("lc-serve up"), "{screen}");
    }

    #[test_id(t9suoj5)]
    #[test]
    fn a_topic_not_heard_again_loses_its_digest_and_replay_value_and_a_heard_one_keeps_them() {
        let (mut c, _) = core();
        c.add("release/active", "", false, Mode::Spending).unwrap();
        c.add("host/pressure", "", false, Mode::Spending).unwrap();
        say(&mut c, "release/active", json!({"release": "r1"}));
        say(&mut c, "host/pressure", json!({"level": "ok"}));

        c.reconnected();
        assert!(say(&mut c, "host/pressure", json!({"level": "ok"})).lines.is_empty());
        let out = c.synced();

        assert!(out.changed && out.lines.is_empty() && out.unsubscribe.is_empty(), "{out:?}");
        let saved = c.saved();
        assert!(saved.subs[0].delivered.is_empty(), "{saved:?}");
        assert!(saved.subs[1].delivered.contains_key("host/pressure"), "{saved:?}");
        c.add("release/#", "", false, Mode::Nobody).unwrap();
        assert!(c.attach(Mode::Spending).lines.is_empty(), "the cleared value is not replayed");
        assert!(say(&mut c, "host/pressure", json!({"level": "ok"})).lines.is_empty());
        assert_eq!(say(&mut c, "release/active", json!({"release": "r1"})).lines.len(), 2);
    }

    #[test_id(tasv4qb)]
    #[test]
    fn a_pending_state_value_not_heard_again_leaves_its_batch_and_chatter_stays() {
        let (mut c, clock) = core();
        batched(&mut c, "release/active");
        say(&mut c, "release/active", json!({"release": "r1"}));
        c.reconnected();
        c.synced();
        assert_eq!(c.next_due_ms(), None);

        batched(&mut c, "progress/+");
        chat(&mut c, "progress/sp-a", json!({"note": "n"}));
        c.reconnected();
        c.synced();
        assert_eq!(at_due(&mut c, &clock).lines.len(), 1, "chatter is not retained, so not being resent says nothing");
    }

    #[test_id(t415935)]
    #[test]
    fn a_topic_cleared_after_it_was_heard_is_not_counted_as_heard() {
        let (mut c, _) = core();
        c.add("release/active", "", false, Mode::Nobody).unwrap();
        c.message("release/active", br#"{"release":"r1"}"#, true, Mode::Nobody);
        c.message("release/active", b"", true, Mode::Nobody);
        c.synced();
        assert!(c.attach(Mode::Spending).lines.is_empty());
        assert!(c.heard.is_empty());
    }

    /// A state publish heard only by a reader that acks.
    fn say_acking(c: &mut Core, topic: &str, value: Value) -> Out {
        c.message(topic, value.to_string().as_bytes(), true, Mode::Acking)
    }

    fn only_seq(out: &Out) -> u64 {
        assert_eq!(out.receipts.len(), 1, "{out:?}");
        out.receipts[0].seq
    }

    #[test_id(t0oan8n)]
    #[test]
    fn an_acking_once_subscription_is_spent_by_the_ack_and_by_nothing_before_it() {
        let (mut c, _) = core();
        c.add("bead/+/state", "landed?", true, Mode::Acking).unwrap();

        let sent = say_acking(&mut c, "bead/sp-a/state", json!({"state": "LANDED"}));

        assert_eq!(sent.lines, ["[sub:s1] bead/sp-a/state state=LANDED — landed?"]);
        assert_eq!(c.saved().subs.len(), 1, "no reader has confirmed anything");
        assert!(sent.unsubscribe.is_empty() && !sent.changed, "{sent:?}");
        let screen = c.list("concierge", true, 1);
        assert!(screen.contains("1 undelivered"), "{screen}");
        for state in ["LANDED", "DONE"] {
            assert!(say_acking(&mut c, "bead/sp-a/state", json!({"state": state})).lines.is_empty(), "{state}: it waits to be spent");
        }
        assert!(say(&mut c, "bead/sp-b/state", json!({"state": "LANDED"})).lines.is_empty(), "nor does a follow that joins get more");

        let acked = c.ack(only_seq(&sent));

        assert_eq!(acked.unsubscribe, ["bead/+/state"]);
        assert!(acked.changed && c.saved().subs.is_empty() && c.unacked() == 0, "{acked:?}");
        assert_eq!(c.ack(only_seq(&sent)), Out::default(), "an ack counts once");
    }

    #[test_id(tsq80ao)]
    #[test]
    fn an_unacked_timer_is_not_woken_for_again_until_its_reader_goes() {
        let (mut c, clock) = core();
        c.add_at(clock.now_ms(), "look again");

        let sent = c.fire_due(Mode::Acking);

        assert_eq!(sent.lines, ["[sub:s1] at 1970-01-01T00:16:40Z — look again"]);
        assert_eq!(c.next_due_ms(), None, "the timer thread would spin on it");
        assert_eq!(c.fire_due(Mode::Acking), Out::default());
        assert_eq!(c.saved().subs.len(), 1);

        c.released();

        assert_eq!(c.next_due_ms(), Some(clock.now_ms()));
        let again = c.fire_due(Mode::Acking);
        assert_eq!(again.lines, sent.lines);
        assert!(only_seq(&again) > only_seq(&sent), "a seq rises");
        assert!(c.ack(only_seq(&sent)) == Out::default() && c.saved().subs.len() == 1, "a released line's ack is nothing");
        assert!(c.ack(only_seq(&again)).changed && c.saved().subs.is_empty());
    }

    #[test_id(tyqzplt)]
    #[test]
    fn a_state_value_counts_as_given_only_once_acked_and_an_unacked_one_is_replayed() {
        let (mut c, _) = core();
        c.add("release/active", "", false, Mode::Acking).unwrap();
        let sent = say_acking(&mut c, "release/active", json!({"release": "r1"}));
        assert!(c.saved().subs[0].delivered.is_empty());
        assert!(say_acking(&mut c, "release/active", json!({"release": "r1"})).lines.is_empty(), "already on its way");
        assert!(c.attach(Mode::Acking).lines.is_empty());

        c.released();
        let replayed = c.attach(Mode::Acking);
        assert_eq!(replayed.lines, sent.lines);

        assert!(c.ack(only_seq(&replayed)).changed);
        assert!(c.saved().subs[0].delivered.contains_key("release/active"));
        c.released();
        assert!(c.attach(Mode::Acking).lines.is_empty(), "acked: not replayed");
    }

    #[test_id(tk1iqmn)]
    #[test]
    fn a_return_to_the_acked_value_is_sent_again_when_a_newer_one_was_never_acked() {
        let (mut c, _) = core();
        c.add("release/active", "", false, Mode::Acking).unwrap();
        let first = say_acking(&mut c, "release/active", json!({"release": "a"}));
        c.ack(only_seq(&first));
        let newer = say_acking(&mut c, "release/active", json!({"release": "b"}));
        assert!(newer.changed && c.saved().subs[0].delivered.is_empty(), "what the reader holds is unknown");
        let back = say_acking(&mut c, "release/active", json!({"release": "a"}));
        assert_eq!(back.lines.len(), 1);
        c.ack(only_seq(&back));
        c.ack(only_seq(&newer));
        assert!(say_acking(&mut c, "release/active", json!({"release": "a"})).lines.is_empty(), "a late ack of an older line records nothing");
    }

    #[test_id(tnp56tx)]
    #[test]
    fn a_batch_gives_its_state_values_and_names_its_subscriptions_only_on_its_ack() {
        let (mut c, clock) = core();
        batched(&mut c, "release/active");
        batched(&mut c, "bead/+/state");
        say_acking(&mut c, "release/active", json!({"release": "r1"}));
        say_acking(&mut c, "bead/sp-a/state", json!({"state": "LANDED"}));
        clock.advance_to(c.next_due_ms().unwrap());

        let sent = c.flush_due(Mode::Acking);

        let seq = only_seq(&sent);
        assert_eq!(sent.receipts[0].subs, ["s1", "s2"]);
        assert_eq!((c.unacked(), c.unacked_of("s1"), c.unacked_of("s2")), (1, 1, 1));
        let screen = c.list("pane", true, 1);
        assert!(screen.replace("\n    ", "").starts_with("pane: broker up, readers 1, undelivered 1\n"), "{screen}");
        assert!(screen.lines().all(|l| l.chars().count() <= 40), "{screen}");
        assert!(c.saved().subs.iter().all(|s| s.delivered.is_empty()));
        say_acking(&mut c, "release/active", json!({"release": "r1"}));
        clock.advance_to(c.next_due_ms().unwrap());
        assert!(c.flush_due(Mode::Acking).lines.is_empty(), "the same value is already on its way");

        c.ack(seq);

        assert!(c.saved().subs.iter().all(|s| s.delivered.len() == 1));
        assert_eq!(c.unacked(), 0);
    }

    #[test_id(tqvltnl)]
    #[test]
    fn chatter_waits_for_an_ack_to_be_counted_and_is_not_replayed() {
        let (mut c, _) = core();
        c.add("progress/+", "", false, Mode::Acking).unwrap();
        let sent = c.message("progress/sp-a", br#"{"note":"n"}"#, false, Mode::Acking);
        assert_eq!((c.unacked(), sent.receipts[0].held), (1, true));
        assert_eq!(c.ack(only_seq(&sent)), Out::default());
        c.message("progress/sp-a", br#"{"note":"m"}"#, false, Mode::Acking);
        c.released();
        assert_eq!(c.unacked(), 0);
        assert!(c.attach(Mode::Acking).lines.is_empty());
    }

    #[test_id(ttrdiph)]
    #[test]
    fn a_topic_cleared_while_its_value_is_unacked_is_news_when_said_again() {
        let (mut c, _) = core();
        c.add("release/active", "", false, Mode::Acking).unwrap();
        let sent = say_acking(&mut c, "release/active", json!({"release": "r1"}));
        c.message("release/active", b"", true, Mode::Acking);
        assert_eq!(say_acking(&mut c, "release/active", json!({"release": "r1"})).lines.len(), 1);
        c.reconnected();
        c.synced();
        c.ack(only_seq(&sent));
        assert!(c.saved().subs[0].delivered.is_empty(), "an ack records nothing for a topic that was cleared");
    }

    #[test_id(t9j403s)]
    #[test]
    fn seqs_rise_down_the_stream_when_unbatched_chatter_falls_due_beside_a_batch() {
        let (mut c, clock) = core();
        batched(&mut c, "release/active");
        batched(&mut c, "progress/+");
        c.message("release/active", br#"{"release":"r1"}"#, true, Mode::Nobody);
        c.message("progress/sp-a", br#"{"note":"held"}"#, false, Mode::Nobody);
        c.set_class("s2", None, Mode::Nobody).unwrap();
        clock.advance(600_000);

        let out = c.flush_due(Mode::Acking);

        assert_eq!(out.lines.len(), 2, "{out:?}");
        assert!(out.lines[1].starts_with("[batch] "), "{out:?}");
        assert!(out.receipts[0].seq < out.receipts[1].seq, "{out:?}");
    }

    #[test_id(tlody5v)]
    #[test]
    fn dropping_a_subscription_stops_its_lines_being_counted_as_undelivered() {
        let (mut c, clock) = core();
        batched(&mut c, "release/active");
        batched(&mut c, "bead/+/state");
        c.add_at(clock.now_ms(), "n");
        say_acking(&mut c, "release/active", json!({"release": "r1"}));
        say_acking(&mut c, "bead/sp-a/state", json!({"state": "LANDED"}));
        let timer = c.fire_due(Mode::Acking);
        clock.advance_to(c.next_due_ms().unwrap());
        let batch = c.flush_due(Mode::Acking);
        assert_eq!(c.unacked(), 2, "positive control");

        c.remove("s3").unwrap();
        c.remove("s1").unwrap();

        assert_eq!((c.unacked(), c.awaits(only_seq(&timer))), (1, false));
        assert!(c.ack(only_seq(&batch)).changed, "the shared batch is still acked for the one left");
        assert!(c.saved().subs[0].delivered.contains_key("bead/sp-a/state"));
        assert_eq!(c.unacked(), 0);
    }
}
