//! The scheduler's incident as the retained `alarm/watchdog`. It publishes in-process, never
//! through the scheduler, which is what is down, and apart from the inbox pager.

use std::time::Duration;

use serde_json::json;
use spira_pub::{Class, Publisher};

use crate::{load, pass, secs, Config, Deps, Outcome};

const TOPIC: &str = "alarm/watchdog";
/// Well inside the unit's `TimeoutStartSec`.
const FLUSH: Duration = Duration::from_secs(5);

/// The publisher [`watch`] is handed, on the broker `cfg` names. Without one the passes run on.
pub fn publisher(cfg: &dyn Fn(&str) -> Result<String, String>) -> Option<Publisher> {
    Publisher::from_config("spira-watchdog", cfg).inspect_err(|e| eprintln!("spira-watchdog: no alarm topic: {e}")).ok()
}

/// [`pass`], then `alarm/watchdog` made to match it. A publish that fails is logged, never the
/// pass's failure.
pub fn watch(cfg: &Config, d: &Deps, alarm: Option<&Publisher>) -> Result<Outcome, String> {
    let outcome = pass(cfg, d)?;
    if let Some(p) = alarm {
        announce(cfg, &outcome, p);
    }
    Ok(outcome)
}

fn announce(cfg: &Config, outcome: &Outcome, p: &Publisher) {
    let queued = match outcome {
        Outcome::Down { since_ms, .. } => {
            // The opening verdict: a later one counts a growing age, and the value must not move.
            let text = match load(&cfg.incident) {
                Ok(Some(i)) => i.reason,
                Ok(None) => return eprintln!("spira-watchdog: {TOPIC} not published: the incident file is gone"),
                Err(e) => return eprintln!("spira-watchdog: {TOPIC} not published: {e}"),
            };
            let since = secs(*since_ms);
            p.publish(Class::State, TOPIC, &json!({ "text": text, "since": since, "at": since }))
        }
        // Every healthy pass, so a clear the broker missed is sent again.
        Outcome::Healthy | Outcome::Recovered { .. } => p.clear(TOPIC),
        Outcome::Unknown(_) => return,
    };
    // A refused publish is already logged by the publisher; a failed flush is not.
    if queued.is_ok() {
        if let Err(e) = p.flush(FLUSH) {
            eprintln!("spira-watchdog: {TOPIC} not delivered: {e}");
        }
    }
}
