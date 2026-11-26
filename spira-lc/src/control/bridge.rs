//! The store bridge: each committed transition and claim as the retained topics
//! `bead/<id>/state` and `bead/<id>/sessions`, and every open bead's both again on each connect
//! to the broker. A store call only hands a message to the publishing thread, because a publish
//! waits while the publisher is connecting.

use std::collections::HashMap;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use lifecycle::bead::LifecycleRow;
use serde_json::{json, Value};
use spira_otel::metrics::{MetricsSink, Point};
use spira_pub::{PubError, Publisher, Republish};
use state_store::Backend;

use super::records::{Audit, LifecycleRecord};
use super::store::{Store, RECORD_CHUNK};
use crate::ops::LIVE;

const PUBLISHER: &str = "lc-serve";
/// Messages held for the publishing thread before one is dropped.
const BACKLOG: usize = 4096;
/// Tries at a message while the publisher is still connecting or draining a republish.
const ATTEMPTS: usize = 5;
const DRAIN: Duration = Duration::from_secs(1);

const REPUBLISHED: &str = "spira_lc_republished";
const REPUBLISH_SECONDS: &str = "spira_lc_republish_seconds";

#[derive(Debug, PartialEq)]
pub(crate) struct Message {
    topic: String,
    payload: Value,
}

fn state_of(id: &str, state: &str, from: Option<&str>, actor: Option<&str>, at: Option<i64>) -> Message {
    Message { topic: format!("bead/{id}/state"), payload: json!({ "state": state, "from": from, "actor": actor, "at": at }) }
}

/// `None` for an audit that moved no bead: a refusal, another machine's, or the same state.
fn transition_message(a: &Audit) -> Option<Message> {
    (a.applied && a.machine == "bead" && a.from_state != a.to_state)
        .then(|| state_of(&a.lc_key, &a.to_state, Some(&a.from_state), Some(&a.actor), Some(a.at)))
}

fn sessions_message(row: &LifecycleRow, at: Option<i64>) -> Message {
    Message {
        topic: format!("bead/{}/sessions", row.bead_id),
        payload: json!({ "sessions": row.builder_claims, "holder": row.holder, "at": at }),
    }
}

/// What a connect republishes of `r`, or nothing once it is terminal. A row records neither who
/// moved it nor from where, so both are null; `at` is when it entered its state.
fn row_messages(r: &LifecycleRecord) -> Option<[Message; 2]> {
    let at = r.row.since.or(Some(r.updated_at));
    (!r.row.state.is_terminal())
        .then(|| [state_of(&r.row.bead_id, r.row.state.as_str(), None, None, at), sessions_message(&r.row, at)])
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
}

/// The revision each topic was last published at. Two request threads can hand over one bead's
/// commits out of order, and the older must not become the retained value.
#[derive(Default)]
struct Newest(HashMap<String, u64>);

impl Newest {
    fn admits(&mut self, topic: &str, rev: u64) -> bool {
        let last = self.0.entry(topic.to_string()).or_default();
        let admitted = rev >= *last;
        *last = rev.max(*last);
        admitted
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
    let weak = Arc::downgrade(store);
    publisher.on_connect(move |again| republish(&weak, again, &metrics));
    let (tx, rx) = sync_channel::<(u64, Message)>(BACKLOG);
    std::thread::Builder::new()
        .name("lc-publish".into())
        .spawn(move || {
            // A failure is already logged; the connect is asked for so a later broker is joined.
            let _ = publisher.connect();
            let mut newest = Newest::default();
            for (rev, m) in rx {
                if newest.admits(&m.topic, rev) {
                    deliver(&publisher, &m);
                }
            }
        })
        .map_err(|e| format!("cannot start the publishing thread: {e}"))?;
    store.set_bridge(Outbox(tx));
    Ok(())
}

fn deliver(publisher: &Publisher, m: &Message) {
    for _ in 0..ATTEMPTS {
        match publisher.publish(&m.topic, &m.payload) {
            Err(PubError::Timeout | PubError::QueueFull { .. }) => drop(publisher.flush(DRAIN)),
            _ => return,
        }
    }
}

fn republish<B: Backend>(store: &Weak<Store<B>>, again: &mut Republish, metrics: &MetricsSink) {
    let Some(store) = store.upgrade() else { return };
    let _holder = state_store::phases::holder_scoped("republish", "");
    let began = Instant::now();
    match republish_open(&store, again) {
        Ok(beads) => metrics.push(
            &[
                Point::Count { name: REPUBLISHED, value: beads },
                Point::Seconds { name: REPUBLISH_SECONDS, value: began.elapsed().as_secs_f64() },
            ],
            &[],
        ),
        Err(e) => eprintln!("spira-lc serve: republish failed: {e}"),
    }
}

/// How many beads were handed to `again`, read a page at a time from one revision.
fn republish_open<B: Backend>(store: &Store<B>, again: &mut Republish) -> state_store::Result<u64> {
    let rev = store.contract().head()?;
    let mut after: Option<String> = None;
    let mut beads = 0;
    loop {
        let page = store.records_page(rev, Some(&LIVE), None, after.as_deref(), Some(RECORD_CHUNK), |_| true)?;
        for messages in page.iter().filter_map(|r| row_messages(&r.value)) {
            beads += 1;
            for m in messages {
                if let Err(e) = again.publish(&m.topic, &m.payload) {
                    eprintln!("spira-lc serve: publish failed ({}): {e}", m.topic);
                }
            }
        }
        if page.len() < RECORD_CHUNK {
            return Ok(beads);
        }
        after = page.last().map(|r| r.value.row.bead_id.clone());
    }
}

#[cfg(test)]
mod tests {
    use lifecycle::bead::BeadState;

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
            Some(Message { topic: "bead/sp-a/state".into(), payload: json!({"state": "WORKING", "from": "READY", "actor": "dwarf-1", "at": 100}) })
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
        assert_eq!(sessions, Message { topic: "bead/sp-a/sessions".into(), payload: json!({"sessions": 3, "holder": "dwarf-1", "at": 140}) });
        for terminal in BeadState::TERMINAL {
            record.row.state = terminal;
            assert_eq!(row_messages(&record), None, "{terminal:?}");
            assert!(!LIVE.contains(&terminal), "{terminal:?} is paged for republish");
        }
    }

    #[test_id(tmu70ay)]
    #[test]
    fn a_commit_handed_over_after_a_later_one_of_the_same_topic_is_not_published() {
        let mut newest = Newest::default();
        assert!(newest.admits("bead/sp-a/state", 7));
        assert!(!newest.admits("bead/sp-a/state", 6));
        assert!(newest.admits("bead/sp-a/state", 7), "one txn moving a bead twice publishes both");
        assert!(newest.admits("bead/sp-b/state", 2));
    }
}
