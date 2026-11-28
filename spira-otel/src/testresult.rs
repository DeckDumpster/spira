//! The one definition of the `test.result` event: one record per test a run executed. Every
//! producer builds its attributes here, and every query over it is built here.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::logs::Attr;
use crate::vlogs::{number, quote, Logs, LogsError};

pub const EVENT: &str = "test.result";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Unit,
    Integration,
    /// A shell-suite case.
    Suite,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Unit => "unit",
            Tier::Integration => "integration",
            Tier::Suite => "suite",
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
    /// The round's batch id and member beads; empty outside a round, and then not sent.
    pub round: String,
    pub members: Vec<String>,
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
    if !r.round.is_empty() {
        out.push(("round".to_string(), s(&r.round)));
    }
    if !r.members.is_empty() {
        out.push(("members".to_string(), Attr::Strs(r.members.clone())));
    }
    out.extend(r.load1.map(|l| ("load1".to_string(), Attr::Float(l))));
    out
}

/// A test that both passed and went red on one input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Flip {
    pub test_id: String,
    pub name: String,
    pub crate_name: String,
    pub input_hash: String,
    pub passes: u64,
    /// `fail` and `timeout` together.
    pub fails: u64,
    pub first_seen: String,
    pub last_seen: String,
}

/// One test's verdicts over every input; a skip is not a run.
#[derive(Clone, Debug, PartialEq)]
pub struct Rate {
    pub test_id: String,
    pub name: String,
    pub crate_name: String,
    pub runs: u64,
    pub reds: u64,
    pub rate: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// Distinct tests with a record.
    pub tests: u64,
    /// Distinct tests with a `fail` or `timeout`.
    pub red: u64,
    /// The test of the longest single run, and its milliseconds.
    pub slowest: Option<(String, u64)>,
}

fn window(window_secs: u64) -> String {
    format!("_time:{window_secs}s _msg:\"{EVENT}\"")
}

/// Only a test that went red can have flipped, so the store narrows to those before it groups:
/// every test's groups are too large an answer for one call.
pub fn flips_query(window_secs: u64) -> String {
    let all = window(window_secs);
    format!("{all} test_id:* test_id:in({all} outcome:in(fail,timeout) | fields test_id) | stats by (test_id, input_hash, outcome, name, crate) count() n, min(_time) first, max(_time) last")
}

pub fn rates_query(window_secs: u64, test: Option<&str>) -> String {
    let one = test.map(|t| format!(":={}", quote(t)));
    format!("{} test_id{} | stats by (test_id, outcome, name, crate) count() n, max(_time) last", window(window_secs), one.as_deref().unwrap_or(":*"))
}

fn counts_query(window_secs: u64) -> String {
    format!("{} test_id:* | stats count_uniq(test_id) tests, count_uniq(test_id) if (outcome:in(fail,timeout)) red", window(window_secs))
}

fn slowest_query(window_secs: u64) -> String {
    format!("{} test_id:* | stats by (test_id) max(duration_ms) ms | sort by (ms desc) limit 1", window(window_secs))
}

fn field<'a>(row: &'a Value, key: &str) -> std::result::Result<&'a str, LogsError> {
    row.get(key).and_then(Value::as_str).ok_or_else(|| LogsError::Malformed(format!("row has no {key}")))
}

/// A record without the field groups under no value.
fn label(row: &Value, key: &str) -> String {
    row.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn red(outcome: &str) -> std::result::Result<Option<bool>, LogsError> {
    match outcome {
        "pass" => Ok(Some(false)),
        "fail" | "timeout" => Ok(Some(true)),
        "skipped" => Ok(None),
        other => Err(LogsError::Malformed(format!("unknown outcome {other:?}"))),
    }
}

/// An RFC 3339 UTC stamp as a sort key: without its `Z`, a whole second sorts before its fractions.
fn when(stamp: &str) -> &str {
    stamp.trim_end_matches('Z')
}

/// Passes, reds, and the name and crate of the newest record, over one key's groups.
#[derive(Default)]
struct Seen {
    passes: u64,
    reds: u64,
    first: String,
    last: String,
    name: String,
    crate_name: String,
}

impl Seen {
    fn add(&mut self, row: &Value, is_red: bool, first: Option<&str>) -> std::result::Result<(), LogsError> {
        let n = number(row, "n")?;
        if is_red {
            self.reds += n;
        } else {
            self.passes += n;
        }
        if let Some(first) = first {
            if self.first.is_empty() || when(first) < when(&self.first) {
                self.first = first.to_string();
            }
        }
        let last = field(row, "last")?;
        if self.last.is_empty() || when(last) > when(&self.last) {
            self.last = last.to_string();
            self.name = label(row, "name");
            self.crate_name = label(row, "crate");
        }
        Ok(())
    }
}

impl Logs {
    /// The flips of the last `window_secs`. A record with no test id names no test, and one with
    /// no input hash cannot be shown to share an input with another.
    pub fn flips(&self, window_secs: u64) -> std::result::Result<Vec<Flip>, LogsError> {
        let mut seen: BTreeMap<(String, String), Seen> = BTreeMap::new();
        for row in self.rows(&flips_query(window_secs))? {
            let (id, hash, first) = (field(&row, "test_id")?, label(&row, "input_hash"), field(&row, "first")?);
            let Some(is_red) = red(field(&row, "outcome")?)? else { continue };
            if id.is_empty() || hash.is_empty() {
                continue;
            }
            seen.entry((id.to_string(), hash)).or_default().add(&row, is_red, Some(first))?;
        }
        Ok(seen
            .into_iter()
            .filter(|(_, s)| s.passes > 0 && s.reds > 0)
            .map(|((test_id, input_hash), s)| Flip { test_id, name: s.name, crate_name: s.crate_name, input_hash, passes: s.passes, fails: s.reds, first_seen: s.first, last_seen: s.last })
            .collect())
    }

    /// Each test's runs and reds over the last `window_secs`, worst failure rate first.
    pub fn rates(&self, window_secs: u64, test: Option<&str>) -> std::result::Result<Vec<Rate>, LogsError> {
        let mut seen: BTreeMap<String, Seen> = BTreeMap::new();
        for row in self.rows(&rates_query(window_secs, test))? {
            let id = field(&row, "test_id")?;
            let Some(is_red) = red(field(&row, "outcome")?)? else { continue };
            if !id.is_empty() {
                seen.entry(id.to_string()).or_default().add(&row, is_red, None)?;
            }
        }
        let mut out: Vec<Rate> = seen
            .into_iter()
            .map(|(test_id, s)| {
                let runs = s.passes + s.reds;
                Rate { test_id, name: s.name, crate_name: s.crate_name, runs, reds: s.reds, rate: s.reds as f64 / runs as f64 }
            })
            .collect();
        out.sort_by(|a, b| b.rate.total_cmp(&a.rate).then(b.reds.cmp(&a.reds)).then_with(|| a.test_id.cmp(&b.test_id)));
        Ok(out)
    }

    /// The headline counts of the last `window_secs`, computed by the store.
    pub fn test_summary(&self, window_secs: u64) -> std::result::Result<Summary, LogsError> {
        let counts = self.rows(&counts_query(window_secs))?;
        let counts = counts.first().ok_or_else(|| LogsError::Malformed("a count answered no row".into()))?;
        let longest = self.rows(&slowest_query(window_secs))?;
        let slowest = longest.first().map(|r| Ok::<_, LogsError>((field(r, "test_id")?.to_string(), number(r, "ms")?))).transpose()?;
        Ok(Summary { tests: number(counts, "tests")?, red: number(counts, "red")?, slowest })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vlogs::fake::{FakeLogs, Script};

    fn group(id: &str, hash: &str, outcome: &str, n: u32, first: &str, last: &str) -> String {
        format!(r#"{{"test_id":"{id}","input_hash":"{hash}","outcome":"{outcome}","name":"a::{id}","crate":"queue","n":"{n}","first":"{first}","last":"{last}"}}"#)
    }

    fn two_runs() -> FakeLogs {
        let (a, b) = ("2026-11-27T01:00:00.5Z", "2026-11-27T02:00:00Z");
        let rows = [
            group("tm1a9x4", "h1", "pass", 1, b, b),
            group("tm1a9x4", "h1", "fail", 1, a, a),
            group("tm1a9x4", "h2", "pass", 1, b, b),
            group("tabc123", "h1", "fail", 1, a, a),
            group("tabc123", "h3", "pass", 1, b, b),
            group("", "h1", "pass", 1, a, a),
            group("", "h1", "fail", 1, a, a),
            group("tnohash", "", "pass", 1, a, a),
            group("tnohash", "", "fail", 1, a, a),
            group("tskip00", "h1", "pass", 1, a, a),
            group("tskip00", "h1", "skipped", 4, a, a),
        ];
        let fake = FakeLogs::start();
        fake.answer("test.result", Script { status: 200, body: rows.join("\n"), hang: false });
        fake
    }

    #[test_id(t4izwy1)]
    #[test]
    fn a_flip_is_a_pass_and_a_red_on_one_input_hash_and_never_across_two() {
        let fake = two_runs();
        let got = Logs::new(&fake.url()).unwrap().flips(3600).unwrap();
        let want = Flip {
            test_id: "tm1a9x4".into(),
            name: "a::tm1a9x4".into(),
            crate_name: "queue".into(),
            input_hash: "h1".into(),
            passes: 1,
            fails: 1,
            first_seen: "2026-11-27T01:00:00.5Z".into(),
            last_seen: "2026-11-27T02:00:00Z".into(),
        };
        assert_eq!(got, [want]);
        assert_eq!(fake.queries(), [flips_query(3600)]);
        assert_eq!(flips_query(3600), "_time:3600s _msg:\"test.result\" test_id:* test_id:in(_time:3600s _msg:\"test.result\" outcome:in(fail,timeout) | fields test_id) | stats by (test_id, input_hash, outcome, name, crate) count() n, min(_time) first, max(_time) last");
    }

    #[test_id(t6adah8)]
    #[test]
    fn a_timeout_is_a_red_and_a_renamed_test_keeps_one_flip_with_its_newest_name() {
        let fake = FakeLogs::start();
        let rows = [
            group("t1", "h", "pass", 2, "2026-11-27T01:00:00Z", "2026-11-27T01:00:00Z"),
            group("t1", "h", "timeout", 1, "2026-11-27T01:00:00.1Z", "2026-11-27T01:00:00.1Z").replace("a::t1", "b::t1"),
        ];
        fake.answer("test.result", Script { status: 200, body: rows.join("\n"), hang: false });
        let got = Logs::new(&fake.url()).unwrap().flips(60).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].passes, got[0].fails, got[0].name.as_str()), (2, 1, "b::t1"));
        assert_eq!((got[0].first_seen.as_str(), got[0].last_seen.as_str()), ("2026-11-27T01:00:00Z", "2026-11-27T01:00:00.1Z"), "a whole second is earlier than its own fraction");
    }

    #[test_id(tbcub4q)]
    #[test]
    fn rates_count_every_run_that_reached_a_verdict_across_input_hashes() {
        let fake = two_runs();
        let got = Logs::new(&fake.url()).unwrap().rates(3600, None).unwrap();
        let seen: Vec<(&str, u64, u64)> = got.iter().map(|r| (r.test_id.as_str(), r.runs, r.reds)).collect();
        assert_eq!(seen, [("tabc123", 2, 1), ("tnohash", 2, 1), ("tm1a9x4", 3, 1), ("tskip00", 1, 0)], "worst rate first; a skip is not a run");
        assert!((got[2].rate - 1.0 / 3.0).abs() < 1e-9);
        assert!(rates_query(60, Some("a\"b")).contains("test_id:=\"a\\\"b\""), "{}", rates_query(60, Some("a\"b")));
        assert_eq!(rates_query(60, None), "_time:60s _msg:\"test.result\" test_id:* | stats by (test_id, outcome, name, crate) count() n, max(_time) last");
    }

    #[test_id(t666mdj)]
    #[test]
    fn a_summary_is_the_distinct_tests_the_red_ones_and_the_slowest() {
        let fake = FakeLogs::start();
        fake.answer("count_uniq", Script { status: 200, body: "{\"tests\":\"10914\",\"red\":\"4\"}\n".into(), hang: false });
        fake.answer("max(duration_ms)", Script { status: 200, body: "{\"test_id\":\"a.sh::x\",\"ms\":\"4250\"}\n".into(), hang: false });
        let got = Logs::new(&fake.url()).unwrap().test_summary(3600).unwrap();
        assert_eq!(got, Summary { tests: 10914, red: 4, slowest: Some(("a.sh::x".into(), 4250)) });
        assert_eq!(fake.queries(), [
            "_time:3600s _msg:\"test.result\" test_id:* | stats count_uniq(test_id) tests, count_uniq(test_id) if (outcome:in(fail,timeout)) red",
            "_time:3600s _msg:\"test.result\" test_id:* | stats by (test_id) max(duration_ms) ms | sort by (ms desc) limit 1",
        ]);
    }

    #[test_id(t5a4c5s)]
    #[test]
    fn a_read_that_fails_is_an_error_never_an_empty_list() {
        let fake = FakeLogs::start();
        fake.answer("test.result", Script { status: 500, body: "down".into(), hang: false });
        let logs = Logs::new(&fake.url()).unwrap();
        assert!(matches!(logs.flips(60), Err(LogsError::Status(500, _))));
        assert!(matches!(logs.rates(60, None), Err(LogsError::Status(500, _))));
        assert!(matches!(logs.test_summary(60), Err(LogsError::Status(500, _))));
        let gone = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", gone.local_addr().unwrap());
        drop(gone);
        assert!(matches!(Logs::new(&url).unwrap().flips(60), Err(LogsError::Unreachable(_))));
        let garbled = FakeLogs::start();
        garbled.answer("test.result", Script { status: 200, body: "{\"test_id\":\"t1\",\"n\":\"1\"}\n".into(), hang: false });
        assert!(matches!(Logs::new(&garbled.url()).unwrap().flips(60), Err(LogsError::Malformed(_))), "a row without its fields is not skipped");
    }

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
            round: String::new(),
            members: Vec::new(),
        };
        let s = |v: &str| Attr::Str(v.into());
        let want = vec![
            ("test_id", s("t0aaaaa")), ("name", s("a::t0aaaaa")), ("crate", s("queue")), ("tier", s("integration")),
            ("input_hash", s("h")), ("outcome", s("timeout")), ("duration_ms", Attr::Int(1500)), ("runner", s("box")),
            ("source", s("gate")), ("gate", s("k")), ("bead", s("sp-a")), ("repo", s("r")), ("tree", s("m")), ("load1", Attr::Float(2.5)),
        ];
        assert_eq!(attrs(&r), want.into_iter().map(|(k, v)| (k.to_string(), v)).collect::<Vec<_>>());
        let in_round = attrs(&Result { round: "b1".into(), members: vec!["sp-x".into()], ..r.clone() });
        assert!(in_round.contains(&("round".to_string(), s("b1"))) && in_round.contains(&("members".to_string(), Attr::Strs(vec!["sp-x".into()]))), "{in_round:?}");
        assert!(attrs(&r).iter().all(|(k, _)| k != "round" && k != "members"), "outside a round neither is sent");
        assert!(attrs(&Result { load1: None, ..r }).iter().all(|(k, _)| k != "load1"), "an unread load is absent, never a number");
    }
}
