//! The replay-only Lifecycle publisher: the one `Lifecycle/all` section, with goodput computed
//! by `spira_lc::goodput` from the typed events the replayed world has seen, over the trailing
//! hour and day of unhalted time, in total and per repo, published through `section
//! publish-status`. A test fixture, never installed; the real section is spira-lc's.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use spira_desired_state::section::{ConditionStatus, Numeric, Unobservable, WarmingUp, Windowed, API_VERSION};
use spira_desired_state::sections::lifecycle::{Goodput as Fields, Status, BELOW_FLOOR, NAME, OWNER, SECTION};
use spira_desired_state::store::format_rfc3339;
use spira_lc::goodput::{self, Goodput, Measure, Scope, Window, DAY_SECS};

use super::round_controller::{KEEPALIVE_MS, MAX_AGE, PERIOD_MS, TARGET_LATENCY};
use super::{Replay, ReplayWorld, Store};

pub const PUBLISHER: &str = "lifecycle";
/// Landings per day the replayed loop is measured against.
pub const FLOOR: f64 = 100.0;
const HOUR_SECS: u64 = 3_600;

#[derive(Default)]
pub struct Publisher {
    last: Option<(Value, u64, u64)>,
    pub writes: usize,
}

fn windowed(m: &Measure, window: &str) -> Windowed {
    match m {
        Measure::Value(v) => Windowed::Value(Numeric { value: *v, window: window.into() }),
        Measure::Unobservable(why) => Windowed::Unobservable(Unobservable { unobservable: (*why).into(), window: window.into() }),
        Measure::WarmingUp => Windowed::WarmingUp(WarmingUp::WarmingUp),
    }
}

fn fields(hour: &Goodput, day: &Goodput) -> Fields {
    Fields {
        goodput_ratio_1h: Some(windowed(&hour.ratio, "1h")),
        goodput_rate_1h: Some(windowed(&hour.rate, "1h")),
        goodput_ratio_24h: Some(windowed(&day.ratio, "24h")),
        goodput_rate_24h: Some(windowed(&day.rate, "24h")),
        landed_1h: Some(windowed(&hour.landed, "1h")),
        landed_24h: Some(windowed(&day.landed, "24h")),
    }
}

/// The section's status at `now_s` for the events in `w`: the total and each repo that landed
/// in the day.
pub fn status(w: &ReplayWorld, now_s: u64) -> Status {
    let events = &w.loop_events;
    let (hour, day) = (Window::new(events, now_s, HOUR_SECS), Window::new(events, now_s, DAY_SECS));
    let measure = |scope| (goodput::goodput(events, &hour, scope, &w.bead_repo), goodput::goodput(events, &day, scope, &w.bead_repo));
    let (h, d) = measure(Scope::All);
    let mut out = Status::default();
    out.set_total(fields(&h, &d));
    let repos: BTreeSet<&str> = events
        .iter()
        .filter_map(|e| match e {
            goodput::LoopEvent::Landed { repo, at, .. } if day.contains(*at) => Some(repo.as_str()),
            _ => None,
        })
        .collect();
    for repo in repos {
        let (h, d) = measure(Scope::Repo(repo));
        out.repos.insert(repo.to_string(), fields(&h, &d));
    }
    out.warming_up_until = day.warming_until.max(hour.warming_until).map(format_rfc3339);
    out
}

impl Publisher {
    pub fn publish(&mut self, w: &ReplayWorld, store: &mut Store, now_ms: u64) {
        let now_s = now_ms / 1000;
        let status = status(w, now_s);
        let mut status_json = serde_json::to_value(&status).expect("a status serialises");
        let rate = match &status.goodput_rate_24h {
            Some(Windowed::Value(n)) => Some(n.value),
            _ => None,
        };
        let condition = match rate {
            Some(r) => (
                if r < FLOOR { ConditionStatus::True } else { ConditionStatus::False },
                format!("goodput rate {r:.1}/day against a floor of {FLOOR}"),
            ),
            None => (ConditionStatus::Unknown, "the 24h goodput rate is not a value".to_string()),
        };
        status_json["observedGeneration"] = json!(0);
        status_json["conditions"] = json!([{
            "type": BELOW_FLOOR,
            "status": condition.0,
            "reason": condition.1,
            "lastTransitionTime": format_rfc3339(now_s),
        }]);
        let due = self.last.as_ref().is_none_or(|(prev, at, _)| {
            let mut a = prev.clone();
            let mut b = status_json.clone();
            a["conditions"][0]["lastTransitionTime"] = Value::Null;
            b["conditions"][0]["lastTransitionTime"] = Value::Null;
            a != b || now_ms >= at + KEEPALIVE_MS
        });
        if !due {
            return;
        }
        let body = json!({
            "apiVersion": API_VERSION,
            "kind": NAME,
            "metadata": {
                "name": SECTION, "owner": OWNER,
                "observedAt": format_rfc3339(now_s), "target_latency": TARGET_LATENCY, "max_age": MAX_AGE,
            },
            "status": status_json,
            "flow": { "consumes": ["lifecycle/landed"], "produces": [] },
            "distance": { "fn": format!("max(0, 1 - goodput_rate_24h / {FLOOR})"), "open": 0.05, "close": 0.02 },
        });
        let rev = self.last.as_ref().map_or(0, |(_, _, rev)| *rev).to_string();
        let answer: Value = store.ok(&["section", "publish-status", "--actor", OWNER, "--expect-rev", &rev], Some(&body));
        let rev = answer["resourceVersion"].as_str().and_then(|r| r.parse().ok()).expect("publish-status answers the resourceVersion it wrote");
        self.writes += 1;
        self.last = Some((status_json, now_ms, rev));
    }
}

pub fn register(replay: &mut Replay) {
    let mut p = Publisher::default();
    replay.register_publisher(PUBLISHER, PERIOD_MS, move |w, store, now| p.publish(w, store, now));
}
