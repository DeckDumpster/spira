//! The `Supply/supply` section: how much landable work was filed in the last day. The status
//! core is pure; the store is a port (`Lc`, which is spira-lc). The section declares no action:
//! a filing quota would invite filler beads.

use std::collections::BTreeSet;

use reconciler_engine::goodput_pass::Lc;
use serde_json::{json, Value};
use spira_desired_state::section::{parse_section, Section};
use spira_desired_state::sections::supply as kind;
use spira_desired_state::store::format_rfc3339;

pub const DAY: u64 = 86_400;
/// `max_age` is this many publish intervals.
pub const MAX_AGE_INTERVALS: u64 = 4;

/// One lifecycle transition of a bead, as the lifecycle stamps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transition {
    pub at: u64,
    pub id: String,
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    pub filed: u64,
    pub landable: u64,
    pub dropped_share: f64,
    pub claimable_ready: u64,
}

/// The day's filings over transitions stamped in `(now - 24h, now]`: a bead is filed when it
/// reaches READY from another state or is dropped; it is landable when it reached READY and was
/// not dropped in the window. No filing is a dropped share of 0.
pub fn measure(transitions: &[Transition], now: u64, claimable_ready: u64) -> Reading {
    let from = now.saturating_sub(DAY);
    let in_window = || transitions.iter().filter(|t| t.at > from && t.at <= now);
    let ready: BTreeSet<&str> = in_window().filter(|t| t.to == "READY" && t.from != "READY").map(|t| t.id.as_str()).collect();
    let dropped: BTreeSet<&str> = in_window().filter(|t| t.to == "DROPPED").map(|t| t.id.as_str()).collect();
    let filed = ready.union(&dropped).count() as u64;
    Reading {
        filed,
        landable: ready.difference(&dropped).count() as u64,
        dropped_share: if filed == 0 { 0.0 } else { dropped.len() as f64 / filed as f64 },
        claimable_ready,
    }
}

/// Transitions from `spira-lc list` rows, which carry a bead's current state and the time it
/// entered it but not where it came from: a bead that reached READY and left it within the day
/// is not seen, so a live `landable` is a lower bound.
pub fn transitions_of_rows(rows: &Value) -> Vec<Transition> {
    let num = |v: &Value| v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()).map(|f| f as i64));
    rows.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    Some(Transition {
                        at: u64::try_from(num(r.get("updated_at")?)?).ok()?,
                        id: r.get("bead_id")?.as_str()?.to_string(),
                        from: String::new(),
                        to: r.get("state")?.as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn status_of(prev: Option<&Section>, r: &Reading) -> Value {
    let day = |v: f64| json!({ "value": v, "window": "24h" });
    json!({
        "observedGeneration": prev.and_then(|s| s.metadata.generation).unwrap_or(0),
        kind::FILED: day(r.filed as f64),
        kind::LANDABLE: day(r.landable as f64),
        kind::DROPPED_SHARE: day(r.dropped_share),
        kind::CLAIMABLE_READY: { "value": r.claimable_ready as f64, "window": "level" },
    })
}

pub fn section_body(status: &Value, now: u64, interval_secs: u64) -> Value {
    json!({
        "apiVersion": "spira/v1",
        "kind": kind::NAME,
        "metadata": {
            "name": kind::SECTION,
            "owner": kind::OWNER,
            "observedAt": format_rfc3339(now),
            "target_latency": format!("{interval_secs}s"),
            "max_age": format!("{}s", interval_secs * MAX_AGE_INTERVALS),
        },
        "status": status,
        "flow": { "consumes": [], "produces": ["supply/landable"] },
        "distance": { "fn": kind::DISTANCE, "open": kind::OPEN, "close": 0.0 },
        "actions": [],
    })
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn lc_ok(lc: &dyn Lc, what: &str, args: &[String]) -> Result<Value, String> {
    match lc.call(args) {
        (0, v) => Ok(v),
        (code, v) => Err(format!("{what}: exit {code}: {v}")),
    }
}

/// The READY work a claimer can take: the rows `ready-set` judged claimable.
pub fn claimable_ready(lc: &dyn Lc) -> Result<u64, String> {
    let v = lc_ok(lc, "ready-set", &argv(&["ready-set"]))?;
    Ok(v["lifecycle"].as_array().map_or(0, Vec::len) as u64)
}

/// The rows that entered READY or DROPPED, as transitions.
pub fn filed_transitions(lc: &dyn Lc) -> Result<Vec<Transition>, String> {
    Ok(transitions_of_rows(&lc_ok(lc, "list", &argv(&["list", "--state", "READY,DROPPED"]))?))
}

/// Publishes the section for this reading, compared on the revision the store holds. A section
/// that moved under the write, or a refusal, is an `Err`: the next groom pass measures again.
pub fn run(lc: &dyn Lc, now: u64, interval_secs: u64, transitions: &[Transition], claimable_ready: u64) -> Result<Reading, String> {
    let name = format!("{}/{}", kind::NAME, kind::SECTION);
    let (code, v) = lc.call(&argv(&["section", "get", &name]));
    let prev = match code {
        0 => Some(parse_section(&v["section"]).map_err(|r| format!("{name} is unreadable: {r:?}"))?),
        1 => None,
        _ => return Err(format!("section get {name}: exit {code}: {v}")),
    };
    let rev = prev.as_ref().and_then(|s| s.metadata.resource_version.clone()).unwrap_or_else(|| "0".into());
    let reading = measure(transitions, now, claimable_ready);
    let body = section_body(&status_of(prev.as_ref(), &reading), now, interval_secs).to_string();
    lc_ok(lc, &format!("publish {name}"), &argv(&["section", "publish-status", "--actor", kind::OWNER, "--expect-rev", &rev, "--json", &body]))?;
    Ok(reading)
}

/// A groom pass's publish: reads the day's filings and the claimable set, then [`run`].
pub fn publish(lc: &dyn Lc, now: u64, interval_secs: u64) -> Result<Reading, String> {
    let transitions = filed_transitions(lc)?;
    let ready = claimable_ready(lc)?;
    run(lc, now, interval_secs, &transitions, ready)
}

/// spira-lc over its door, as the port the publisher calls.
pub struct SpiraLc(pub spira_store::lc_call::LcClient);

impl Lc for SpiraLc {
    fn call(&self, args: &[String]) -> (i32, Value) {
        match self.0.call(args) {
            Ok(r) => (r.code, serde_json::from_str(r.out.trim()).unwrap_or_else(|_| json!({ "cannot_tell": r.why() }))),
            Err(e) => (-1, json!({ "cannot_tell": e.to_string() })),
        }
    }
}
