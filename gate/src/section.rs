//! The `Gate/gate` section: the hour's share of gates that ended NO_VERDICT, the share of
//! gate-hours they used, and the count by cause, read from the typed `gate-run` rows. The
//! status core is pure; the control store is a port. The section declares no action: the
//! gate's concurrency is `host`'s shedding lever.

use serde_json::{json, Value};
use spira_desired_state::section::{parse_section, Section};
use spira_desired_state::sections::gate as kind;
use spira_desired_state::store::format_rfc3339;
use tsd::query::{num, text, ts_of};

pub const HOUR: u64 = 3_600;
pub const TARGET_LATENCY: &str = "10m";
pub const MAX_AGE: &str = "1h";
pub const UNSPECIFIED: &str = "unspecified";

#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    pub gates: u64,
    pub no_verdict_share: f64,
    pub no_verdict_hours_share: f64,
    pub by_cause: Vec<(String, u64)>,
}

/// The hour's figures over the `gate-run` rows stamped in `(now - 1h, now]`. A row whose status
/// is no gate outcome is no gate. An hour with no gate, or with no run time, is a share of 0:
/// nothing was wasted.
pub fn measure(rows: &[Value], now: u64) -> Reading {
    let from = now.saturating_sub(HOUR);
    let gates: Vec<&Value> = rows
        .iter()
        .filter(|r| ts_of(r).is_some_and(|t| t > from && t <= now))
        .filter(|r| matches!(text(r, "status"), Some("PASS" | "FAIL" | "NO_VERDICT" | "BASE_FAIL")))
        .collect();
    let ran = |r: &Value| num(r, "ran_secs").unwrap_or(0.0);
    let lost: Vec<&&Value> = gates.iter().filter(|r| text(r, "status") == Some("NO_VERDICT")).collect();
    let share = |part: f64, whole: f64| if whole > 0.0 { part / whole } else { 0.0 };
    let mut by_cause: Vec<(String, u64)> = Vec::new();
    for r in &lost {
        let cause = text(r, "reason").filter(|c| !c.is_empty()).unwrap_or(UNSPECIFIED);
        match by_cause.iter_mut().find(|(c, _)| c == cause) {
            Some((_, n)) => *n += 1,
            None => by_cause.push((cause.to_string(), 1)),
        }
    }
    by_cause.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Reading {
        gates: gates.len() as u64,
        no_verdict_share: share(lost.len() as f64, gates.len() as f64),
        no_verdict_hours_share: share(lost.iter().map(|r| ran(r)).sum(), gates.iter().map(|r| ran(r)).sum()),
        by_cause,
    }
}

/// The family's rows, or none when no gate ever recorded one; any other failure stops the publish.
pub fn gate_rows(read: Result<Vec<Value>, tsd::query::Failure>) -> Result<Vec<Value>, String> {
    match read {
        Err(f) if f.code == tsd::query::ERR_NO_FAMILY => Ok(Vec::new()),
        other => other.map_err(|f| f.message),
    }
}

pub fn status_of(prev: Option<&Section>, r: &Reading) -> Value {
    let hour = |v: f64| json!({ "value": v, "window": "1h" });
    json!({
        "observedGeneration": prev.and_then(|s| s.metadata.generation).unwrap_or(0),
        kind::NO_VERDICT_SHARE: hour(r.no_verdict_share),
        kind::NO_VERDICT_HOURS_SHARE: hour(r.no_verdict_hours_share),
        kind::GATES: hour(r.gates as f64),
        kind::NO_VERDICT_BY_CAUSE: r.by_cause.iter().map(|(cause, count)| json!({ "cause": cause, "count": count })).collect::<Vec<_>>(),
    })
}

pub fn section_body(status: &Value, now: u64) -> Value {
    json!({
        "apiVersion": "spira/v1",
        "kind": kind::NAME,
        "metadata": { "name": kind::SECTION, "owner": kind::OWNER, "observedAt": format_rfc3339(now), "target_latency": TARGET_LATENCY, "max_age": MAX_AGE },
        "status": status,
        "flow": { "consumes": ["host/scratch", "host/io"], "produces": ["gate/verdicts"] },
        "distance": { "fn": kind::DISTANCE, "open": kind::OPEN, "close": 0.0 },
        "actions": [],
    })
}

/// The control store, as the `spira-lc` verbs answer it: an exit code and a JSON body.
pub trait Lc {
    fn call(&self, args: &[String]) -> (i32, Value);
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// Publishes the section for the rows, compared on the revision the store holds. A section that
/// moved under the write, or a refusal, is an `Err`: the next run measures again.
pub fn run(lc: &dyn Lc, now: u64, rows: &[Value]) -> Result<Reading, String> {
    let name = format!("{}/{}", kind::NAME, kind::SECTION);
    let (code, v) = lc.call(&argv(&["section", "get", &name]));
    let prev = match code {
        0 => Some(parse_section(&v["section"]).map_err(|r| format!("{name} is unreadable: {r:?}"))?),
        1 => None,
        _ => return Err(format!("section get {name}: exit {code}: {v}")),
    };
    let rev = prev.as_ref().and_then(|s| s.metadata.resource_version.clone()).unwrap_or_else(|| "0".into());
    let reading = measure(rows, now);
    let body = section_body(&status_of(prev.as_ref(), &reading), now).to_string();
    match lc.call(&argv(&["section", "publish-status", "--actor", kind::OWNER, "--expect-rev", &rev, "--json", &body])) {
        (0, _) => Ok(reading),
        (code, v) => Err(format!("publish {name}: exit {code}: {v}")),
    }
}
