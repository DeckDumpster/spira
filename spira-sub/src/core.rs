//! The subscription set and what is delivered for it. No I/O: time is the injected clock, and
//! every method returns the lines to stream and what the connection and the set file owe.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use ports::Clock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
    /// The last state value delivered here, per topic.
    seen: HashMap<String, Value>,
}

/// What one step owes the outside.
#[derive(Debug, Default, PartialEq)]
pub struct Out {
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

impl Sub {
    fn filter(&self) -> Option<&str> {
        self.entry.filter.as_deref()
    }

    /// The line for `topic` = `value`, unless it is the state value last delivered here.
    fn deliver(&mut self, topic: &str, value: &Value, state: bool, now: u64) -> Option<String> {
        if state {
            if self.seen.get(topic) == Some(value) {
                return None;
            }
            self.seen.insert(topic.to_string(), value.clone());
        }
        self.last_fired_ms = Some(now);
        Some(render::line(&self.entry.id, topic, value, &self.entry.note))
    }
}

impl Core {
    pub fn new(clock: Arc<dyn Clock>, saved: Saved) -> Core {
        let subs = saved.subs.into_iter().map(|entry| Sub { entry, last_fired_ms: None, seen: HashMap::new() }).collect();
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

    /// Every topic filter the set holds.
    pub fn filters(&self) -> BTreeSet<String> {
        self.subs.iter().filter_map(|s| s.filter().map(str::to_string)).collect()
    }

    fn push(&mut self, filter: Option<String>, at_ms: Option<u64>, note: &str, once: bool) -> String {
        let id = format!("s{}", self.next);
        self.next += 1;
        let entry = Entry { id: id.clone(), filter, at_ms, note: note.to_string(), once };
        self.subs.push(Sub { entry, last_fired_ms: None, seen: HashMap::new() });
        id
    }

    /// A new subscription. The caller subscribes its filter, which is what brings the retained
    /// values; a publisher already known to be gone is said here, while someone is `listening`.
    pub fn add(&mut self, filter: &str, note: &str, once: bool, listening: bool) -> Result<(String, Out), String> {
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
                    out.lines.extend(sub.deliver(topic, value, true, now));
                }
            }
        }
        Ok((id, out))
    }

    /// A subscription that fires once, at `at_ms`.
    pub fn add_at(&mut self, at_ms: u64, note: &str) -> (String, Out) {
        let id = self.push(None, Some(at_ms), note, true);
        (id, Out { changed: true, ..Out::default() })
    }

    pub fn remove(&mut self, id: &str) -> Result<Out, String> {
        let at = self.subs.iter().position(|s| s.entry.id == id).ok_or_else(|| format!("no subscription {id:?}; `spira-sub list` shows them"))?;
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
    /// `--once` subscription waits for a reader; the value is kept for its replay.
    pub fn message(&mut self, topic: &str, payload: &[u8], listening: bool) -> Out {
        let mut out = Out::default();
        let liveness = topic.starts_with("publisher/");
        if payload.is_empty() {
            self.current.remove(topic);
            self.publishers.remove(topic);
            self.subs.iter_mut().for_each(|s| drop(s.seen.remove(topic)));
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
                sub.seen.remove(topic);
                continue;
            }
            if !listening || !(direct || notice) {
                continue;
            }
            let Some(line) = sub.deliver(topic, &value, state, now) else { continue };
            out.lines.push(line);
            if direct && sub.entry.once {
                spent.push(at);
            }
        }
        self.discard(&spent, &mut out);
        out
    }

    /// What a newly attached reader is told first: the current value of every state topic each
    /// subscription selects, and every timer already due.
    pub fn attach(&mut self) -> Out {
        let mut out = Out::default();
        let now = self.now_ms();
        let mut spent = Vec::new();
        for (at, sub) in self.subs.iter_mut().enumerate() {
            let Some(filter) = sub.entry.filter.clone() else { continue };
            sub.seen.clear();
            for (topic, value) in &self.publishers {
                if !topics::filter_selects(&filter, topic) && is_gone(value) && depends_on(&filter, topic.trim_start_matches("publisher/")) {
                    out.lines.extend(sub.deliver(topic, value, true, now));
                }
            }
            for (topic, value) in self.current.iter().filter(|(t, _)| topics::filter_selects(&filter, t)) {
                out.lines.extend(sub.deliver(topic, value, true, now));
                if sub.entry.once {
                    spent.push(at);
                    break;
                }
            }
        }
        self.discard(&spent, &mut out);
        let due = self.fire_due(true);
        out.lines.extend(due.lines);
        out.changed |= due.changed;
        out
    }

    /// When the earliest timer is due.
    pub fn next_due_ms(&self) -> Option<u64> {
        self.subs.iter().filter_map(|s| s.entry.at_ms).min()
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
}
