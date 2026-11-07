//! The replay-only Lifecycle publisher: the one `Lifecycle/all` section, with the goodput rate
//! over the trailing 24 virtual hours computed from the landings the replayed world has seen,
//! published through `section publish-status`. A test fixture, never installed; the real
//! section is spira-lc's.

use std::collections::VecDeque;

use serde_json::{json, Value};
use spira_desired_state::section::{ConditionStatus, API_VERSION};
use spira_desired_state::sections::lifecycle::{BELOW_FLOOR, NAME, OWNER, SECTION};
use spira_desired_state::store::format_rfc3339;

use super::round_controller::{KEEPALIVE_MS, MAX_AGE, PERIOD_MS, TARGET_LATENCY};
use super::{Replay, ReplayWorld, Store};

pub const PUBLISHER: &str = "lifecycle";
/// Landings per day the replayed loop is measured against.
pub const FLOOR: f64 = 100.0;
const DAY_MS: u64 = 24 * 3_600_000;
/// No rate is published until this much of the window has been seen.
const WARM_MS: u64 = 3_600_000;

#[derive(Default)]
pub struct Publisher {
    samples: VecDeque<(u64, u32)>,
    last: Option<(f64, u64, u64)>,
    pub writes: usize,
}

impl Publisher {
    pub fn publish(&mut self, w: &ReplayWorld, store: &mut Store, now_ms: u64) {
        let landed: u32 = w.lands.values().sum();
        self.samples.push_back((now_ms, landed));
        while self.samples.len() > 1 && now_ms - self.samples[1].0 >= DAY_MS {
            self.samples.pop_front();
        }
        let (since, before) = self.samples[0];
        if now_ms - since < WARM_MS {
            return;
        }
        let rate = f64::from(landed - before) * DAY_MS as f64 / (now_ms - since) as f64;
        let due = self.last.is_none_or(|(r, at, _)| (r - rate).abs() > 0.5 || now_ms >= at + KEEPALIVE_MS);
        if !due {
            return;
        }
        let now_s = now_ms / 1000;
        let below = rate < FLOOR;
        let body = json!({
            "apiVersion": API_VERSION,
            "kind": NAME,
            "metadata": {
                "name": SECTION, "owner": OWNER,
                "observedAt": format_rfc3339(now_s), "target_latency": TARGET_LATENCY, "max_age": MAX_AGE,
            },
            "status": {
                "observedGeneration": 0,
                "goodput_rate_24h": { "value": rate, "window": "24h" },
                "conditions": [{
                    "type": BELOW_FLOOR,
                    "status": if below { ConditionStatus::True } else { ConditionStatus::False },
                    "reason": format!("goodput rate {rate:.1}/day against a floor of {FLOOR}"),
                    "lastTransitionTime": format_rfc3339(now_s),
                }],
            },
            "flow": { "consumes": ["lifecycle/landed"], "produces": [] },
            "distance": { "fn": format!("max(0, 1 - goodput_rate_24h / {FLOOR})"), "open": 0.05, "close": 0.02 },
        });
        let rev = self.last.map_or(0, |(_, _, rev)| rev).to_string();
        let answer: Value = store.ok(&["section", "publish-status", "--actor", OWNER, "--expect-rev", &rev], Some(&body));
        let rev = answer["resourceVersion"].as_str().and_then(|r| r.parse().ok()).expect("publish-status answers the resourceVersion it wrote");
        self.writes += 1;
        self.last = Some((rate, now_ms, rev));
    }
}

pub fn register(replay: &mut Replay) {
    let mut p = Publisher::default();
    replay.register_publisher(PUBLISHER, PERIOD_MS, move |w, store, now| p.publish(w, store, now));
}
