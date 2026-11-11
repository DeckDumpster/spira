//! The `VmPool/pool` section: how much of the last hour's certification passes the pool
//! wasted, and the slots in use. The status core is pure; the store is a port. The section
//! declares no action: VM capacity has one knob, the round controller's `concurrency`.

use reconciler_engine::goodput_pass::Lc;
use serde_json::{json, Value};
use spira_desired_state::section::{parse_section, Section};
use spira_desired_state::sections::vm_pool as kind;
use spira_desired_state::store::format_rfc3339;
use tsd::pass::Cause;
use tsd::query::{text, ts_of};

pub const HOUR: u64 = 3_600;
pub const TARGET_LATENCY: &str = "5m";
pub const MAX_AGE: &str = "30m";

#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    pub passes: u64,
    pub infra_incomplete_share: f64,
    pub over_cap_share: f64,
    pub slots_in_use: Vec<(String, u64)>,
}

/// The hour's shares over the `cert-pass` rows stamped in `(now - 1h, now]`. A row whose cause
/// is not one the pass family names is not a pass. No pass in the hour is a share of 0: nothing
/// was wasted.
pub fn measure(rows: &[Value], now: u64, slots_in_use: Vec<(String, u64)>) -> Reading {
    let from = now.saturating_sub(HOUR);
    let causes: Vec<Cause> = rows
        .iter()
        .filter(|r| ts_of(r).is_some_and(|t| t > from && t <= now))
        .filter_map(|r| Cause::parse(text(r, "cause")?).ok())
        .collect();
    let share = |wanted: &[Cause]| match causes.len() {
        0 => 0.0,
        n => causes.iter().filter(|c| wanted.contains(c)).count() as f64 / n as f64,
    };
    Reading {
        passes: causes.len() as u64,
        infra_incomplete_share: share(&[Cause::Infra, Cause::VmLost]),
        over_cap_share: share(&[Cause::OverCap]),
        slots_in_use,
    }
}

/// Leases held per pool, every pool named even at zero.
pub fn slots_by_pool(leases: &[crate::schema::Lease]) -> Vec<(String, u64)> {
    let ec2 = leases.iter().filter(|l| crate::ec2::is_ec2_handle(&l.vm.handle)).count() as u64;
    vec![("local".to_string(), leases.len() as u64 - ec2), ("ec2".to_string(), ec2)]
}

pub fn status_of(prev: Option<&Section>, r: &Reading) -> Value {
    let hour = |v: f64| json!({ "value": v, "window": "1h" });
    json!({
        "observedGeneration": prev.and_then(|s| s.metadata.generation).unwrap_or(0),
        kind::INFRA_INCOMPLETE: hour(r.infra_incomplete_share),
        kind::OVER_CAP: hour(r.over_cap_share),
        kind::PASSES: hour(r.passes as f64),
        kind::SLOTS_IN_USE: r.slots_in_use.iter().map(|(pool, n)| json!({ "pool": pool, "in_use": n })).collect::<Vec<_>>(),
    })
}

pub fn section_body(status: &Value, now: u64) -> Value {
    json!({
        "apiVersion": "spira/v1",
        "kind": kind::NAME,
        "metadata": { "name": kind::SECTION, "owner": kind::OWNER, "observedAt": format_rfc3339(now), "target_latency": TARGET_LATENCY, "max_age": MAX_AGE },
        "status": status,
        "flow": { "consumes": ["host/io"], "produces": ["vm-pool/slots"] },
        "distance": { "fn": kind::DISTANCE, "open": 0.05, "close": 0.0 },
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

/// Publishes the section for this reading, compared on the revision the store holds. A section
/// that moved under the write, or a refusal, is an `Err`: the next tick measures again.
pub fn run(lc: &dyn Lc, now: u64, rows: &[Value], slots_in_use: Vec<(String, u64)>) -> Result<Reading, String> {
    let name = format!("{}/{}", kind::NAME, kind::SECTION);
    let (code, v) = lc.call(&argv(&["section", "get", &name]));
    let prev = match code {
        0 => Some(parse_section(&v["section"]).map_err(|r| format!("{name} is unreadable: {r:?}"))?),
        1 => None,
        _ => return Err(format!("section get {name}: exit {code}: {v}")),
    };
    let rev = prev.as_ref().and_then(|s| s.metadata.resource_version.clone()).unwrap_or_else(|| "0".into());
    let reading = measure(rows, now, slots_in_use);
    let body = section_body(&status_of(prev.as_ref(), &reading), now).to_string();
    lc_ok(lc, &format!("publish {name}"), &argv(&["section", "publish-status", "--actor", kind::OWNER, "--expect-rev", &rev, "--json", &body]))?;
    Ok(reading)
}
