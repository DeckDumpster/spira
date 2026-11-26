//! pr-notify's publisher: the effects that put `pr/<repo>/<n>` on the broker, off the loop.
//! What it would republish to a broker that comes back is the carry of the latest observe,
//! with every publish since on top.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use spira_pub::Publisher;

use crate::watchers::pr_notify::{carried_topics, Carry, Topics};

pub struct PrTopics {
    publisher: Publisher,
    held: Arc<Mutex<Topics>>,
    asked_to_connect: AtomicBool,
}

fn lock(m: &Mutex<Topics>) -> MutexGuard<'_, Topics> {
    m.lock().unwrap_or_else(|e| e.into_inner()) // loop-safe: taken only by effects on the pool and the publisher thread
}

impl PrTopics {
    pub fn new(publisher: Publisher) -> PrTopics {
        let held = Arc::new(Mutex::new(Topics::new()));
        let republish = held.clone();
        publisher.on_connect(move |again| {
            for (topic, payload) in lock(&republish).iter() {
                // A refusal is the publisher's to log; nothing here can mend it.
                let _ = again.publish(topic, payload);
            }
        });
        PrTopics { publisher, held, asked_to_connect: AtomicBool::new(false) }
    }

    /// Takes the carried state an observe was asked with. The first call that has any connects,
    /// so a restarted process republishes it; the publisher reconnects by itself after that.
    pub fn track(&self, carry: &str) {
        let Ok(carry) = serde_json::from_str::<Carry>(carry) else { return };
        let tracked = carried_topics(&carry);
        let any = !tracked.is_empty();
        *lock(&self.held) = tracked;
        if any && !self.asked_to_connect.swap(true, Ordering::SeqCst) {
            // A failure is logged by the publisher.
            let _ = self.publisher.connect();
        }
    }

    /// Publishes the topics in `input`. A broker that cannot be reached is logged by the
    /// publisher and is not the effect's failure: the state goes out on reconnect.
    pub fn publish(&self, input: &str) -> Result<String, String> {
        let topics: Topics = serde_json::from_str(input).map_err(|e| e.to_string())?;
        let sent = topics.iter().filter(|(topic, payload)| self.publisher.publish(topic, payload).is_ok()).count();
        let total = topics.len();
        // Held only after the publishes: the connect hook takes this lock while they wait.
        lock(&self.held).extend(topics);
        Ok(format!("published {sent} of {total} topic(s)"))
    }

    /// Waits for the broker to take what is queued.
    pub fn flush(&self, deadline: std::time::Duration) -> Result<(), String> {
        self.publisher.flush(deadline).map_err(|e| e.to_string())
    }
}
