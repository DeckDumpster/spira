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
    flush_to(&MetricsSink::for_component(endpoint, "spira-runtime"));
}

fn flush_to(sink: &MetricsSink) {
    let (calls, rates) = with(|l| (std::mem::take(&mut l.calls), l.rate.values().copied().collect::<Vec<_>>()));
    for ((caller, outcome), n) in calls {
        sink.push(&[Point::Count { name: CALLS, value: n }], &[("caller", &caller), ("outcome", outcome)]);
    }
    if let Some(r) = rates.iter().min_by_key(|r| r.remaining) {
        sink.push(&[Point::Gauge { name: REMAINING, value: r.remaining as f64 }, Point::Gauge { name: LIMIT, value: r.limit as f64 }], &[]);
    }
}

/// The core budget out of `gh api rate_limit` (a read GitHub does not charge).
pub fn core_rate(json: &str) -> Option<Rate> {
    let v: Value = serde_json::from_str(json).ok()?;
    let core = v.get("resources")?.get("core")?;
    let n = |k: &str| core.get(k).and_then(Value::as_u64);
    Some(Rate { limit: n("limit")?, remaining: n("remaining")?, reset: n("reset")? })
}

/// The caller that spent most, and how much, among the exported `spira_gh_api_calls` series.
pub fn top_spender(series: &[spira_otel::query::Exported]) -> Option<(String, u64)> {
    let mut by: BTreeMap<&str, f64> = BTreeMap::new();
    for s in series {
        let Some(caller) = s.labels.get("caller") else { continue };
        if !matches!(s.labels.get("outcome").map(String::as_str), Some(FRESH | FAILED)) {
            continue;
        }
        *by.entry(caller).or_default() += s.samples.iter().map(|(_, v)| v).sum::<f64>();
    }
    by.into_iter().max_by(|a, b| a.1.total_cmp(&b.1)).map(|(c, n)| (c.to_string(), n as u64))
}

pub fn clock_of(epoch: u64) -> String {
    let d = epoch % 86_400;
    format!("{:02}:{:02}:{:02}Z", d / 3600, d % 3600 / 60, d % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_otel::metrics::fixture;

    #[test]
    fn a_flush_pushes_each_callers_count_by_outcome_and_the_lowest_budget_seen() {
        note_rate("flush-a", Rate { limit: 5000, remaining: 4000, reset: 1 });
        note_rate("flush-b", Rate { limit: 5000, remaining: 12, reset: 1 });
        count("flush-caller", FRESH);
        count("flush-caller", FRESH);
        count("flush-caller", CACHED);
        let (_, got) = fixture::capture(flush_to);
        let calls: Vec<(u64, String, String)> = got.iter().filter(|s| s.name == CALLS && s.label("caller") == Some("flush-caller")).map(|s| (s.value as u64, s.label("outcome").unwrap().to_string(), "".into())).collect();
        assert!(calls.iter().any(|c| c.0 == 2 && c.1 == FRESH), "{calls:?}");
        assert!(calls.iter().any(|c| c.0 == 1 && c.1 == CACHED), "{calls:?}");
        let remaining = got.iter().find(|s| s.name == REMAINING).expect("remaining pushed");
        assert!(remaining.value <= 12.0);
        assert!(got.iter().any(|s| s.name == LIMIT));
        assert_eq!(calls_after_flush(), 0, "a flush leaves nothing to count twice");
    }

    #[test]
    fn the_core_budget_is_read_from_a_rate_limit_answer_and_nothing_else_is_taken_for_it() {
        let body = r#"{"resources":{"core":{"limit":5000,"remaining":0,"reset":1793003600},"graphql":{"limit":5000,"remaining":4999,"reset":1}}}"#;
        assert_eq!(core_rate(body), Some(Rate { limit: 5000, remaining: 0, reset: 1_793_003_600 }));
        assert_eq!(core_rate(r#"{"message":"Bad credentials"}"#), None);
        assert_eq!(core_rate("not json"), None);
    }

    #[test]
    fn the_top_spender_is_the_caller_with_most_charged_calls() {
        let with = |caller: &str, outcome: &str, vals: &[f64]| spira_otel::query::Exported {
            labels: [("caller".to_string(), caller.to_string()), ("outcome".to_string(), outcome.to_string())].into(),
            samples: vals.iter().enumerate().map(|(i, v)| (i as i64, *v)).collect(),
        };
        let series = |caller: &str, vals: &[f64]| with(caller, FRESH, vals);
        let got = top_spender(&[series("a", &[3.0, 4.0]), series("b", &[20.0]), series("c", &[1.0])]);
        assert_eq!(got, Some(("b".to_string(), 20)));
        let erroring = [with("e", FAILED, &[9.0]), with("e", CACHED, &[500.0]), series("a", &[5.0])];
        assert_eq!(top_spender(&erroring), Some(("e".to_string(), 9)));
        let unlabelled = spira_otel::query::Exported { labels: Default::default(), samples: vec![(0, 99.0)] };
        assert_eq!(top_spender(&[unlabelled, series("a", &[2.0])]), Some(("a".to_string(), 2)));
        assert_eq!(top_spender(&[]), None);
    }

    fn calls_after_flush() -> u64 {
        calls("flush-caller", FRESH)
    }
}
