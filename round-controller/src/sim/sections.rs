//! What a scenario counts of the section and its intents: the sections spira-lc committed, read
//! back through the kind's own distance, and the acks the controller wrote.

use serde_json::Value;
use spira_desired_state::intent::INTENTS_PREFIX;
use spira_desired_state::section::parse_section;
use spira_desired_state::sections::lookup;
use spira_desired_state::sections::round_controller::NAME;
use state_store::Selector;

use crate::outcome::{Kind, Outcome};
use crate::sim::bench::Bench;

/// The ack results a scenario may count.
pub const ACKS: [&str; 3] = ["applied", "already-applied", "refused"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SectionCount {
    /// `section-writes`: sections committed.
    Writes,
    /// `section-refusals`: publishes spira-lc refused.
    Refusals,
    /// `section-raised:<Type>`: committed sections with that condition True.
    Raised(String),
    /// `section-distance:positive`: committed sections whose distance was above 0.
    DistancePositive,
    /// `section-last-distance`: the last committed section's distance, rounded up; `u64::MAX`
    /// when none was committed.
    LastDistance,
    /// `section-green-with-blocker`: the most GREEN rounds one section listed with a blocker.
    GreenWithBlocker,
    /// `acks:<result>`: intents acked so.
    Acks(&'static str),
    /// `acks-stale:<json>`: intents in the store acked refused as stale at that knob value.
    Stale(Value),
    /// `opened-with:<member>`: round records whose members include that bead.
    OpenedWith(String),
}

impl SectionCount {
    pub fn parse(what: &str, of: Option<&str>) -> Option<SectionCount> {
        match (what, of) {
            ("section-writes", None) => Some(SectionCount::Writes),
            ("section-refusals", None) => Some(SectionCount::Refusals),
            ("section-raised", Some(t))
                if lookup(NAME).is_some_and(|k| k.condition(t).is_some()) =>
            {
                Some(SectionCount::Raised(t.to_string()))
            }
            ("section-distance", Some("positive")) => Some(SectionCount::DistancePositive),
            ("section-last-distance", None) => Some(SectionCount::LastDistance),
            ("section-green-with-blocker", None) => Some(SectionCount::GreenWithBlocker),
            ("acks", Some(w)) => ACKS.into_iter().find(|a| *a == w).map(SectionCount::Acks),
            ("acks-stale", Some(v)) => serde_json::from_str(v).ok().map(SectionCount::Stale),
            ("opened-with", Some(m)) if !m.is_empty() => {
                Some(SectionCount::OpenedWith(m.to_string()))
            }
            _ => None,
        }
    }

    pub fn count(&self, b: &Bench, outcomes: &[Outcome]) -> u64 {
        let published = b.published();
        let raised = |s: &Value, t: &str| {
            s.pointer("/status/conditions")
                .and_then(Value::as_array)
                .is_some_and(|cs| cs.iter().any(|c| c["type"] == t && c["status"] == "True"))
        };
        match self {
            SectionCount::Writes => published.len() as u64,
            SectionCount::Refusals => crate::lock(&b.lc_calls)
                .iter()
                .filter(|c| {
                    c.args.get(1).map(String::as_str) == Some("publish-status")
                        && c.code == crate::ports::LC_REFUSED
                })
                .count() as u64,
            SectionCount::Raised(t) => published.iter().filter(|s| raised(s, t)).count() as u64,
            SectionCount::DistancePositive => {
                published.iter().filter(|s| distance(s) > 0.0).count() as u64
            }
            SectionCount::LastDistance => published
                .last()
                .map_or(u64::MAX, |s| distance(s).ceil() as u64),
            SectionCount::GreenWithBlocker => published
                .iter()
                .map(|s| {
                    s.pointer("/status/green_unlanded")
                        .and_then(Value::as_array)
                        .map_or(0, |g| {
                            g.iter().filter(|e| e.get("blocker").is_some()).count()
                        })
                })
                .max()
                .unwrap_or(0) as u64,
            SectionCount::Acks(w) => outcomes
                .iter()
                .filter(|o| o.kind == Kind::Acked && o.cause.word() == *w)
                .count() as u64,
            SectionCount::Stale(v) => listed(b, INTENTS_PREFIX)
                .iter()
                .filter(|i| i.pointer("/ack/result/refused/stale") == Some(v))
                .count() as u64,
            SectionCount::OpenedWith(m) => listed(b, "batches/")
                .iter()
                .filter(|r| {
                    r.get("members")
                        .and_then(Value::as_array)
                        .is_some_and(|ms| ms.iter().any(|x| x["id"] == m.as_str()))
                })
                .count() as u64,
        }
    }
}

fn listed(b: &Bench, prefix: &str) -> Vec<Value> {
    b.store
        .list(&[prefix], &Selector::Everything)
        .expect("the in-memory store lists")
        .0
        .into_iter()
        .map(|i| i.value.data)
        .collect()
}

/// A committed section's distance, through the kind's own expression; a section whose
/// distance does not evaluate reads as infinitely far.
pub fn distance(s: &Value) -> f64 {
    let Some(kind) = lookup(NAME) else {
        return f64::INFINITY;
    };
    parse_section(s)
        .ok()
        .and_then(|s| s.distance_value(kind))
        .and_then(Result::ok)
        .unwrap_or(f64::INFINITY)
}
