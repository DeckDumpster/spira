//! The one definition of the `test.result` event: one record per test a run executed. Every
//! producer builds its attributes here.

use crate::logs::Attr;

pub const EVENT: &str = "test.result";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Unit,
    Integration,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Unit => "unit",
            Tier::Integration => "integration",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail,
    Timeout,
    Skipped,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Pass => "pass",
            Outcome::Fail => "fail",
            Outcome::Timeout => "timeout",
            Outcome::Skipped => "skipped",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Result {
    /// Empty for a test without an id.
    pub test_id: String,
    pub name: String,
    pub crate_name: String,
    pub tier: Tier,
    /// Equal while nothing the test's crate builds from changes; empty when it could not be read.
    pub input_hash: String,
    pub outcome: Outcome,
    pub duration_ms: u64,
    pub runner: String,
    pub load1: Option<f64>,
    pub source: &'static str,
    pub gate: String,
    pub bead: String,
    pub repo: String,
    pub tree: String,
}

pub fn attrs(r: &Result) -> Vec<(String, Attr)> {
    let s = |v: &str| Attr::Str(v.to_string());
    let mut out: Vec<(String, Attr)> = [
        ("test_id", s(&r.test_id)),
        ("name", s(&r.name)),
        ("crate", s(&r.crate_name)),
        ("tier", s(r.tier.as_str())),
        ("input_hash", s(&r.input_hash)),
        ("outcome", s(r.outcome.as_str())),
        ("duration_ms", Attr::Int(i64::try_from(r.duration_ms).unwrap_or(i64::MAX))),
        ("runner", s(&r.runner)),
        ("source", s(r.source)),
        ("gate", s(&r.gate)),
        ("bead", s(&r.bead)),
        ("repo", s(&r.repo)),
        ("tree", s(&r.tree)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    out.extend(r.load1.map(|l| ("load1".to_string(), Attr::Float(l))));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(t6e6bag)]
    #[test]
    fn the_event_carries_every_field_under_its_pinned_name() {
        let r = Result {
            test_id: "t0aaaaa".into(),
            name: "a::t0aaaaa".into(),
            crate_name: "queue".into(),
            tier: Tier::Integration,
            input_hash: "h".into(),
            outcome: Outcome::Timeout,
            duration_ms: 1500,
            runner: "box".into(),
            load1: Some(2.5),
            source: "gate",
            gate: "k".into(),
            bead: "sp-a".into(),
            repo: "r".into(),
            tree: "m".into(),
        };
        let s = |v: &str| Attr::Str(v.into());
        let want = vec![
            ("test_id", s("t0aaaaa")), ("name", s("a::t0aaaaa")), ("crate", s("queue")), ("tier", s("integration")),
            ("input_hash", s("h")), ("outcome", s("timeout")), ("duration_ms", Attr::Int(1500)), ("runner", s("box")),
            ("source", s("gate")), ("gate", s("k")), ("bead", s("sp-a")), ("repo", s("r")), ("tree", s("m")), ("load1", Attr::Float(2.5)),
        ];
        assert_eq!(attrs(&r), want.into_iter().map(|(k, v)| (k.to_string(), v)).collect::<Vec<_>>());
        assert!(attrs(&Result { load1: None, ..r }).iter().all(|(k, _)| k != "load1"), "an unread load is absent, never a number");
    }
}
