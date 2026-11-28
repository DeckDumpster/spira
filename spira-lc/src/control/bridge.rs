//! The store bridge: each committed transition and claim as the retained topics
//! `bead/<id>/state` and `bead/<id>/sessions`, each unfiled incident as `incident/<ref>`
//! (cleared once its row goes), and every open bead's and unfiled incident's again on each
//! connect to the broker. A store call only hands a message to the publishing thread, because a publish
//! waits while the publisher is connecting.

use std::collections::HashMap;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use lifecycle::bead::{BeadState, LifecycleRow};
use serde_json::{json, Value};
use spira_store::incident::Row as IncidentRow;
use spira_otel::metrics::{MetricsSink, Point};
use spira_pub::{Class, Publisher, Republish};
use state_store::{Backend, Selector};

use super::keys;
use super::records::{decode, Audit, LifecycleRecord};
use super::store::{Store, RECORD_CHUNK};
use crate::ops::LIVE;

mod topics;

const PUBLISHER: &str = "lc-serve";
/// Messages held for the publishing thread before one is dropped.
const BACKLOG: usize = 4096;
/// The most the publishing thread waits for what is queued ahead of its next message.
const DRAIN: Duration = Duration::from_secs(1);
/// The most the publishing thread waits out a republish before publishing into it.
const REPUBLISH_BOUND: Duration = Duration::from_secs(30);

const REPUBLISHED: &str = "spira_lc_republished";
const REPUBLISH_SECONDS: &str = "spira_lc_republish_seconds";

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Message {
    topic: String,
    /// Null clears the topic's retained value.
    payload: Value,
}

/// `row`'s topic with what it holds; a deleted row, keyed `key`, is cleared.
fn incident_message(key: &str, row: Option<&IncidentRow>) -> Message {
    let reference = row.map_or_else(|| key.replace("%2F", "/").replace("%25", "%"), |r| r.payload.reference.clone());
    let payload = row.map_or(Value::Null, |r| {
        json!({ "reference": r.payload.reference, "title": r.payload.title, "repo": r.payload.repo, "state": r.state.as_str(), "reason": r.reason, "events": r.events, "first_at": r.first_at, "last_at": r.last_at, "bead": r.bead })
    });
    Message { topic: topics::incident(&spira_pub::segment(&reference)), payload }
}

fn state_of(id: &str, state: &str, from: Option<&str>, actor: Option<&str>, at: Option<i64>) -> Message {
    Message { topic: topics::state(id), payload: json!({ "state": state, "from": from, "actor": actor, "at": at }) }
}

/// `None` for an audit that moved no bead: a refusal, another machine's, or the same state.
fn transition_message(a: &Audit) -> Option<Message> {
    (a.applied && a.machine == "bead" && a.from_state != a.to_state)
        .then(|| state_of(&a.lc_key, &a.to_state, Some(&a.from_state), Some(&a.actor), Some(a.at)))
}

fn sessions_message(row: &LifecycleRow, at: Option<i64>) -> Message {
    Message {
        topic: topics::sessions(&row.bead_id),
        payload: json!({ "sessions": row.builder_claims, "holder": row.holder, "at": at }),
    }
}

/// What a connect republishes of `r`, or nothing once it is terminal. A row records neither who
/// moved it, from where, nor when it was last claimed unless it still is, so those are null.
fn row_messages(r: &LifecycleRecord) -> Option<[Message; 2]> {
    let at = r.row.since.or(Some(r.updated_at));
    let claimed_at = at.filter(|_| r.row.state == BeadState::Working);
    (!r.row.state.is_terminal())
        .then(|| [state_of(&r.row.bead_id, r.row.state.as_str(), None, None, at), sessions_message(&r.row, claimed_at)])
}

/// The store's end of the bridge.
pub(crate) struct Outbox(SyncSender<(u64, Message)>);

impl Outbox {
    fn send(&self, rev: u64, m: Message) {
        if let Err(e) = self.0.try_send((rev, m)) {
            let (std::sync::mpsc::TrySendError::Full((_, m)) | std::sync::mpsc::TrySendError::Disconnected((_, m))) = e;
            eprintln!("spira-lc serve: publish failed ({}): the publishing thread has not kept up", m.topic);
        }
    }

    pub(super) fn transitions(&self, rev: u64, audits: &[Audit]) {
        audits.iter().filter_map(transition_message).for_each(|m| self.send(rev, m));
    }

    pub(super) fn claim(&self, rev: u64, row: &LifecycleRow, at: Option<i64>) {
        self.send(rev, sessions_message(row, at));
    }

    pub(super) fn incident(&self, rev: u64, key: &str, row: Option<&IncidentRow>) {
        self.send(rev, incident_message(key, row));
    }
}

/// What each topic was last given, and the oldest revision it still takes.
#[derive(Default)]
struct Ledger {
    republishing: bool,
    topics: HashMap<String, (u64, Value)>,
}

impl Ledger {
    /// Whether `m`, committed at `rev`, is still news. Two request threads can hand over one
    /// bead's commits out of order, and a republish can overtake queued ones; an older value
    /// must not become the retained one.
    fn admits(&mut self, rev: u64, m: &Message) -> bool {
        let (last, payload) = self.topics.entry(m.topic.clone()).or_default();
        if rev < *last {
            return false;
        }
        (*last, *payload) = (rev, m.payload.clone());
        true
    }

    /// The incident topics last told to clear: a clear the broker never received leaves the
    /// old value retained, so a connect says it again.
    fn cleared(&self) -> Vec<String> {
        let mut gone: Vec<String> = self.topics.iter().filter(|(t, (_, p))| t.starts_with(topics::INCIDENTS) && p.is_null()).map(|(t, _)| t.clone()).collect();
        gone.sort();
        gone
    }

    /// What a connect says for `row`, read from a record last written at `rev`: what this
    /// process already said of the same facts, which carries what a row does not record.
    fn republished(&mut self, rev: u64, row: Message) -> Message {
        let (last, payload) = self.topics.entry(row.topic.clone()).or_default();
        if !same_facts(payload, &row.payload) {
            *payload = row.payload;
        }
        *last = rev.max(*last);
        Message { topic: row.topic, payload: payload.clone() }
    }
}

/// A state is the same entry into it; a session count is the same claim, whose holder a row
/// clears once it is let go.
fn same_facts(said: &Value, row: &Value) -> bool {
    let same = |key: &str| said.get(key) == row.get(key);
    if row.get("events").is_some() {
        said == row
    } else if row.get("sessions").is_some() {
        same("sessions") && (row["holder"].is_null() || same("holder"))
    } else {
        same("state") && same("at")
    }
}

/// What the publishing thread and the `on_connect` hook share. Never held across a publish.
#[derive(Default)]
struct Shared {
    ledger: Mutex<Ledger>,
    idle: Condvar,
}

impl Shared {
    fn ledger(&self) -> MutexGuard<'_, Ledger> {
        self.ledger.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_republishing(&self, republishing: bool) {
        self.ledger().republishing = republishing;
        self.idle.notify_all();
    }

    /// The ledger once no republish is running: a publish made during one waits out only a
    /// second of it before it fails.
    fn between_republishes(&self) -> MutexGuard<'_, Ledger> {
        self.idle.wait_timeout_while(self.ledger(), REPUBLISH_BOUND, |l| l.republishing).unwrap_or_else(|e| e.into_inner()).0
    }
}

/// Publishes `store`'s commits as `lc-serve` on the configured broker. A publisher that cannot
/// be built is logged and the store is served without one.
pub(crate) fn serve<B: Backend + Send + Sync + 'static>(store: &Arc<Store<B>>) {
    let endpoint = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok();
    let metrics = MetricsSink::new(endpoint.as_deref(), "spira-lc", Duration::from_millis(250));
    let started = Publisher::new(PUBLISHER).map_err(|e| e.to_string()).and_then(|p| start(store, p, metrics));
    if let Err(e) = started {
        eprintln!("spira-lc serve: transitions are not published: {e}");
    }
}

fn start<B: Backend + Send + Sync + 'static>(store: &Arc<Store<B>>, publisher: Publisher, metrics: MetricsSink) -> Result<(), String> {
    let shared = Arc::new(Shared::default());
    let (weak, hooked, metrics) = (Arc::downgrade(store), shared.clone(), Arc::new(metrics));
    publisher.resident();
    publisher.on_connect(move |again| republish(&weak, &hooked, again, &metrics));
    let (tx, rx) = sync_channel::<(u64, Message)>(BACKLOG);
    std::thread::Builder::new()
        .name("lc-publish".into())
        .spawn(move || {
            // A failure is already logged; the connect is asked for so a later broker is joined.
            let _ = publisher.connect();
            for (rev, m) in rx {
                if !shared.between_republishes().admits(rev, &m) {
                    continue;
                }
                // A republish can fill the publisher's queue. What a flush finds lost, and what
                // a publish fails on, the publisher has already logged; neither is retried.
                let _ = publisher.flush(DRAIN);
                let _ = if m.payload.is_null() { publisher.clear(&m.topic) } else { publisher.publish(Class::State, &m.topic, &m.payload) };
            }
        })
        .map_err(|e| format!("cannot start the publishing thread: {e}"))?;
    store.set_bridge(Outbox(tx));
    Ok(())
}

fn republish<B: Backend>(store: &Weak<Store<B>>, shared: &Shared, again: &mut Republish, metrics: &Arc<MetricsSink>) {
    let Some(store) = store.upgrade() else { return };
    let _holder = state_store::phases::holder_scoped("republish", "");
    let began = Instant::now();
    shared.set_republishing(true);
    let read = open_messages(&store, |rev, row| {
        let m = shared.ledger().republished(rev, row);
        if let Err(e) = again.publish(Class::State, &m.topic, &m.payload) {
            eprintln!("spira-lc serve: publish failed ({}): {e}", m.topic);
        }
    });
    for topic in shared.ledger().cleared() {
        if let Err(e) = again.clear(&topic) {
            eprintln!("spira-lc serve: publish failed ({topic}): {e}");
        }
    }
    shared.set_republishing(false);
    match read {
        Ok(beads) => push_republished(metrics.clone(), beads, began.elapsed()),
        Err(e) => eprintln!("spira-lc serve: republish failed: {e}"),
    }
}

/// From a thread of its own: publishes wait on the connect this is part of.
fn push_republished(metrics: Arc<MetricsSink>, beads: u64, took: Duration) {
    std::thread::spawn(move || {
        metrics.push(
            &[Point::Count { name: REPUBLISHED, value: beads }, Point::Seconds { name: REPUBLISH_SECONDS, value: took.as_secs_f64() }],
            &[],
        );
    });
}

/// Hands `emit` the message of every unfiled incident and both messages of every open bead,
/// each with the revision its record was last written at, the beads read a page at a time from
/// one revision; answers how many beads.
fn open_messages<B: Backend>(store: &Store<B>, mut emit: impl FnMut(u64, Message)) -> state_store::Result<u64> {
    let (incidents, _) = store.contract().list(&[keys::INCIDENTS], &Selector::Everything)?;
    for item in incidents {
        let Some(key) = keys::id_of(keys::INCIDENTS, &item.key) else { continue };
        let row: IncidentRow = decode(&item.key, &item.value)?;
        emit(item.mod_rev.main, incident_message(key, Some(&row)));
    }
    let rev = store.contract().head()?;
    let mut after: Option<String> = None;
    let mut beads = 0;
    loop {
        let page = store.records_page(rev, Some(&LIVE), None, after.as_deref(), Some(RECORD_CHUNK), |_| true)?;
        for r in &page {
            let Some(messages) = row_messages(&r.value) else { continue };
            beads += 1;
            messages.into_iter().for_each(|m| emit(r.mod_rev.main, m));
        }
        if page.len() < RECORD_CHUNK {
            return Ok(beads);
        }
        after = page.last().map(|r| r.value.row.bead_id.clone());
    }
}

#[cfg(test)]
mod tests {
    use runtime::ports::VirtualClock;
    use state_store::{Config, Contract, MemBackend, Ports, Request};

    use super::*;

    fn audit(from: &str, to: &str, applied: bool, machine: &str) -> Audit {
        Audit {
            machine: machine.into(),
            lc_key: "sp-a".into(),
            event: "Claim".into(),
            expect: from.into(),
            from_state: from.into(),
            to_state: to.into(),
            applied,
            refusal: None,
            evidence: Value::Null,
            actor: "dwarf-1".into(),
            at: 100,
        }
    }

    #[test_id(t1wj35u)]
    #[test]
    fn only_a_bead_that_moved_has_a_state_message_and_it_carries_the_audit() {
        assert_eq!(
            transition_message(&audit("READY", "WORKING", true, "bead")),
            Some(Message { topic: topics::state("sp-a"), payload: json!({"state": "WORKING", "from": "READY", "actor": "dwarf-1", "at": 100}) })
        );
        for (from, to, applied, machine) in
            [("READY", "WORKING", false, "bead"), ("WORKING", "WORKING", true, "bead"), ("OPEN", "TESTING", true, "batch")]
        {
            assert_eq!(transition_message(&audit(from, to, applied, machine)), None, "{from} {to} {applied} {machine}");
        }
    }

    #[test_id(ti3w37x)]
    #[test]
    fn a_row_is_republished_as_its_state_and_session_count_unless_it_is_terminal() {
        let mut record = LifecycleRecord::born("sp-a", Some("harness"), None, 100);
        record.row.state = BeadState::Working;
        record.row.builder_claims = 3;
        record.row.holder = Some("dwarf-1".into());
        record.row.since = Some(140);
        let [state, sessions] = row_messages(&record).unwrap();
        assert_eq!(state, state_of("sp-a", "WORKING", None, None, Some(140)));
        assert_eq!(sessions, Message { topic: topics::sessions("sp-a"), payload: json!({"sessions": 3, "holder": "dwarf-1", "at": 140}) });
        record.row.state = BeadState::Submitted;
        let [_, sessions] = row_messages(&record).unwrap();
        assert_eq!(sessions.payload["at"], Value::Null, "a row that left WORKING no longer says when it was claimed");
        for terminal in BeadState::TERMINAL {
            record.row.state = terminal;
            assert_eq!(row_messages(&record), None, "{terminal:?}");
            assert!(!LIVE.contains(&terminal), "{terminal:?} is paged for republish");
        }
    }

    fn moved(to: &str, from: &str) -> Message {
        state_of("sp-a", to, Some(from), Some("dwarf-1"), Some(100))
    }

    #[test_id(tij2fxs)]
    #[test]
    fn a_cleared_incident_topic_is_said_again_on_connect_until_it_is_raised_again() {
        let mut ledger = Ledger::default();
        let row = IncidentRow { payload: spira_store::incident::Payload { reference: "incident:a".into(), ..Default::default() }, ..Default::default() };
        let gone = incident_message("incident:a", None);
        assert!(ledger.admits(3, &incident_message("incident:a", Some(&row))));
        assert!(ledger.cleared().is_empty());
        assert!(ledger.admits(4, &gone));
        assert!(ledger.admits(4, &Message { topic: topics::state("sp-a"), payload: Value::Null }));
        assert_eq!(ledger.cleared(), ["incident/incident:a"]);
        assert!(ledger.admits(5, &incident_message("incident:a", Some(&row))));
        assert!(ledger.cleared().is_empty());
    }

    #[test_id(td3baj6)]
    #[test]
    fn a_commit_handed_over_after_a_later_one_of_the_same_topic_is_not_published() {
        let mut ledger = Ledger::default();
        assert!(ledger.admits(7, &moved("SUBMITTED", "WORKING")));
        assert!(!ledger.admits(6, &moved("WORKING", "READY")));
        assert!(ledger.admits(7, &moved("CERTIFIED", "SUBMITTED")), "one txn moving a bead twice publishes both");
    }

    #[test_id(tcorsc4)]
    #[test]
    fn a_commit_still_queued_when_a_republish_read_a_later_one_is_not_published() {
        let mut ledger = Ledger::default();
        let row = state_of("sp-a", "SUBMITTED", None, None, Some(140));
        assert_eq!(ledger.republished(9, row.clone()), row);
        assert!(!ledger.admits(8, &moved("WORKING", "READY")), "the record was written again after that commit");
        assert!(ledger.admits(9, &moved("SUBMITTED", "WORKING")), "the record's own last commit says who moved it");
    }

    #[test_id(tyjmbxh)]
    #[test]
    fn a_republish_repeats_what_was_said_of_the_same_facts_and_otherwise_says_the_row() {
        let mut ledger = Ledger::default();
        let said = moved("WORKING", "READY");
        let claimed = Message { topic: topics::sessions("sp-a"), payload: json!({"sessions": 1, "holder": "dwarf-1", "at": 100}) };
        assert!(ledger.admits(3, &said) && ledger.admits(3, &claimed));
        assert_eq!(ledger.republished(3, state_of("sp-a", "WORKING", None, None, Some(100))), said);
        let let_go = Message { topic: topics::sessions("sp-a"), payload: json!({"sessions": 1, "holder": null, "at": null}) };
        assert_eq!(ledger.republished(5, let_go), claimed);
        let later = state_of("sp-a", "SUBMITTED", None, None, Some(140));
        assert_eq!(ledger.republished(5, later.clone()), later);
        let again = Message { topic: topics::sessions("sp-a"), payload: json!({"sessions": 2, "holder": "dwarf-2", "at": 200}) };
        assert_eq!(ledger.republished(8, again.clone()), again);
    }

    #[test_id(teecjvl)]
    #[test]
    fn every_open_bead_past_one_page_is_read_for_a_republish() {
        let clock = VirtualClock::new(0);
        let contract = Contract::open(MemBackend::new(), Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        let store = Store::new(contract, 1_000);
        let open = RECORD_CHUNK * 2 + 1;
        for n in 0..open {
            let id = format!("sp-{n:04}");
            store.create(&id, None, None, 100, &Request::new(&id, "t", "t")).unwrap();
        }
        let mut seen = std::collections::BTreeSet::new();
        let beads = open_messages(&store, |_, m| assert!(seen.insert(m.topic.clone()), "{} twice", m.topic)).unwrap();
        assert_eq!((beads, seen.len()), (open as u64, open * 2));
    }

    #[test_id(tdf1nx5)]
    #[test]
    fn a_full_backlog_drops_the_message_without_holding_the_store_call() {
        let (tx, rx) = sync_channel(1);
        let outbox = Outbox(tx);
        outbox.transitions(1, &[audit("READY", "WORKING", true, "bead")]);
        outbox.transitions(2, &[audit("WORKING", "SUBMITTED", true, "bead")]);
        assert_eq!(rx.try_iter().map(|(rev, _)| rev).collect::<Vec<_>>(), [1]);
    }
}
