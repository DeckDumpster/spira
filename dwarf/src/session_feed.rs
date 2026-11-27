//! The session published about itself as retained state: `session/<bead>/state` and
//! `session/<bead>/age/<15m|30m|60m>`, all cleared once the end has been published. A failed
//! publish never changes the session.

use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{json, Value};

use crate::decide::{session_marks, Mark};

/// What one lease renewal said about the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renewal {
    Applied,
    /// The row is no longer this holder's WORKING row.
    Refused,
    /// The machine could not tell; what was last known stands.
    Unjudged,
}

/// Where the feed publishes. A failure is the outlet's to log.
pub trait Outlet: Send + Sync {
    fn publish(&self, topic: &str, payload: &Value);
    /// Removes `topic`'s retained value.
    fn clear(&self, topic: &str);
}

impl Outlet for spira_pub::Publisher {
    fn publish(&self, topic: &str, payload: &Value) {
        let _ = spira_pub::Publisher::publish(self, spira_pub::Class::State, topic, payload);
    }

    fn clear(&self, topic: &str) {
        let _ = spira_pub::Publisher::clear(self, topic);
    }
}

#[derive(Default)]
struct Record {
    started_at: Option<i64>,
    ended: bool,
    sessions: Option<u32>,
    left_working_at: Option<i64>,
    published: Vec<Mark>,
    state: Option<Value>,
    ages: Vec<(String, Value)>,
}

impl std::fmt::Debug for SessionFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionFeed").field("bead", &self.retained.bead).finish_non_exhaustive()
    }
}

/// What a broker that kept nothing must be told again. Its lock is never held across a
/// publish, so the publisher's `on_connect` hook may take it.
#[derive(Clone)]
pub struct Retained {
    bead: String,
    record: Arc<Mutex<Record>>,
}

impl Retained {
    fn lock(&self) -> MutexGuard<'_, Record> {
        self.record.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn state_topic(&self) -> String {
        format!("session/{}/state", self.bead)
    }

    /// The current state, then every age reached, as (topic, payload).
    pub fn messages(&self) -> Vec<(String, Value)> {
        let r = self.lock();
        r.state.iter().map(|v| (self.state_topic(), v.clone())).chain(r.ages.iter().cloned()).collect()
    }
}

pub struct SessionFeed {
    rune: String,
    linger_s: i64,
    retained: Retained,
    out: Box<dyn Outlet>,
    /// Held from deciding a message to publishing it: no age or `lingering` follows `ended`.
    order: Mutex<()>,
}

impl SessionFeed {
    pub fn new(bead: &str, rune: &str, linger_s: i64, out: impl FnOnce(Retained) -> Box<dyn Outlet>) -> SessionFeed {
        let retained = Retained { bead: bead.to_string(), record: Default::default() };
        let out = out(retained.clone());
        SessionFeed { rune: rune.to_string(), linger_s, retained, out, order: Mutex::new(()) }
    }

    /// The feed over the broker the configuration names, publishing as `dwarf-<bead>`.
    pub fn over_broker(bead: &str, rune: &str, linger_s: i64, cfg: &dyn Fn(&str) -> Result<String, String>) -> Result<SessionFeed, spira_pub::PubError> {
        let publisher = spira_pub::Publisher::from_config(&format!("dwarf-{bead}"), cfg)?;
        let feed = SessionFeed::new(bead, rune, linger_s, |retained| {
            publisher.on_connect(move |again| {
                for (topic, payload) in retained.messages() {
                    // Each was accepted when first published; a refusal here is the library's to log.
                    let _ = again.publish(spira_pub::Class::State, &topic, &payload);
                }
            });
            // Connected before there is anything to say, so the first publish is not also
            // republished by the connect it would cause. A failure is the library's to log.
            let _ = publisher.connect();
            Box::new(publisher)
        });
        Ok(feed)
    }

    fn ordered(&self) -> MutexGuard<'_, ()> {
        self.order.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn state(&self, payload: Value) {
        self.retained.lock().state = Some(payload.clone());
        self.out.publish(&self.retained.state_topic(), &payload);
    }

    pub fn started(&self, at: i64, sessions: Option<u32>) {
        let _order = self.ordered();
        {
            let mut r = self.retained.lock();
            r.started_at = Some(at);
            r.sessions = sessions;
        }
        self.state(json!({"state": "started", "bead": self.retained.bead, "rune": self.rune, "started_at": at, "sessions": sessions}));
    }

    pub fn ended(&self, at: i64, rc: i32, wall_s: i64) {
        let _order = self.ordered();
        self.retained.lock().ended = true;
        self.state(json!({"state": "ended", "rc": rc, "wall_s": wall_s, "at": at}));
        // Emptied before the clears are sent: a reconnect must not restore a cleared topic.
        let ages = {
            let mut r = self.retained.lock();
            r.state = None;
            std::mem::take(&mut r.ages)
        };
        self.out.clear(&self.retained.state_topic());
        for (topic, _) in ages {
            self.out.clear(&topic);
        }
    }

    /// The outcome of this beat's lease renewal. The item left WORKING at the first refusal
    /// since a renewal last applied.
    pub fn renewal(&self, now: i64, outcome: Renewal) {
        let mut r = self.retained.lock();
        match outcome {
            Renewal::Applied => r.left_working_at = None,
            Renewal::Refused => r.left_working_at = r.left_working_at.or(Some(now)),
            Renewal::Unjudged => {}
        }
    }

    /// Publishes every mark the running session has reached by `now`.
    pub fn beat(&self, now: i64) {
        let _order = self.ordered();
        let (marks, sessions) = {
            let r = self.retained.lock();
            let Some(started_at) = r.started_at.filter(|_| !r.ended) else { return };
            (session_marks(started_at, now, &r.published, r.left_working_at, self.linger_s), r.sessions)
        };
        for mark in marks {
            self.retained.lock().published.push(mark);
            match mark {
                Mark::Age(minutes) => {
                    let topic = format!("session/{}/age/{minutes}m", self.retained.bead);
                    let payload = json!({"at": now, "sessions": sessions});
                    self.retained.lock().ages.push((topic.clone(), payload.clone()));
                    self.out.publish(&topic, &payload);
                }
                Mark::Lingering => self.state(json!({"state": "lingering", "at": now, "sessions": sessions})),
            }
        }
    }
}
