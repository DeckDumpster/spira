//! What the forge client spends of GitHub's hourly API budget: calls counted by caller, the
//! budget the last response reported, and the ETag-validated bodies that let an unchanged poll
//! cost nothing (a 304 is not charged against the budget).

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use serde_json::Value;
use spira_otel::metrics::{MetricsSink, Point};

pub const CALLS: &str = "spira_gh_api_calls";
pub const REMAINING: &str = "spira_gh_api_remaining";
pub const LIMIT: &str = "spira_gh_api_limit";

/// A call that was answered with a body (charged), one the ETag answered with a 304 (free),
/// one not made because the budget was at zero, and one that failed.
pub const FRESH: &str = "fresh";
pub const CACHED: &str = "cached";
pub const REFUSED: &str = "refused";
pub const FAILED: &str = "failed";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rate {
    pub limit: u64,
    pub remaining: u64,
    pub reset: u64,
}

#[derive(Default)]
struct Ledger {
    calls: BTreeMap<(String, &'static str), u64>,
    rate: BTreeMap<String, Rate>,
    etags: HashMap<String, (String, Value)>,
    announced: BTreeMap<String, u64>,
}

static LEDGER: Mutex<Option<Ledger>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Ledger) -> R) -> R {
    let mut g = LEDGER.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Ledger::default))
}

pub fn count(caller: &str, outcome: &'static str) {
    with(|l| *l.calls.entry((caller.to_string(), outcome)).or_default() += 1);
}

pub fn note_rate(api: &str, rate: Rate) {
    with(|l| {
        l.rate.insert(api.to_string(), rate);
    });
}

pub fn rate(api: &str) -> Option<Rate> {
    with(|l| l.rate.get(api).copied())
}

/// The reset time (epoch seconds) when `api`'s budget is known to be spent and has not reset yet.
pub fn exhausted_until(api: &str, now: u64) -> Option<u64> {
    rate(api).filter(|r| r.remaining == 0 && r.reset > now).map(|r| r.reset)
}

/// True the first time it is asked for this reset, so a spent budget is reported once per window.
pub fn first_report_of(api: &str, reset: u64) -> bool {
    with(|l| l.announced.insert(api.to_string(), reset) != Some(reset))
}

pub fn etag(url: &str) -> Option<String> {
    with(|l| l.etags.get(url).map(|(t, _)| t.clone()))
}

pub fn body_for(url: &str) -> Option<Value> {
    with(|l| l.etags.get(url).map(|(_, v)| v.clone()))
}

pub fn keep(url: &str, etag: String, body: Value) {
    with(|l| {
        l.etags.insert(url.to_string(), (etag, body));
    });
}

pub fn calls(caller: &str, outcome: &'static str) -> u64 {
    with(|l| l.calls.get(&(caller.to_string(), outcome)).copied().unwrap_or(0))
}

/// Pushes the calls counted since the last flush, by caller and outcome, and the budget last seen.
pub fn flush(endpoint: Option<&str>) {
    let (calls, rates) = with(|l| (std::mem::take(&mut l.calls), l.rate.values().copied().collect::<Vec<_>>()));
    let sink = MetricsSink::for_component(endpoint, "spira-runtime");
    for ((caller, outcome), n) in calls {
        sink.push(&[Point::Count { name: CALLS, value: n }], &[("caller", &caller), ("outcome", outcome)]);
    }
    if let Some(r) = rates.iter().min_by_key(|r| r.remaining) {
        sink.push(&[Point::Gauge { name: REMAINING, value: r.remaining as f64 }, Point::Gauge { name: LIMIT, value: r.limit as f64 }], &[]);
    }
}

pub fn clock_of(epoch: u64) -> String {
    let d = epoch % 86_400;
    format!("{:02}:{:02}:{:02}Z", d / 3600, d % 3600 / 60, d % 60)
}
