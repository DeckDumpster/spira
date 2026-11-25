//! The `VmPool/pool` section: how much of the last hour's certification passes the pool
//! wasted, and the slots in use. The status core is pure; the store is a port. The section
//! declares no action.

use crate::config::Config;
use reconciler_engine::goodput_pass::Lc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spira_desired_state::section::{parse_section, Section};
use spira_desired_state::sections::vm_pool as kind;
use spira_desired_state::store::format_rfc3339;
use tsd::pass::Cause;
use tsd::query::{text, ts_of};

pub const HOUR: u64 = 3_600;
pub const TARGET_LATENCY: &str = "5m";
pub const MAX_AGE: &str = "30m";
pub const MAX_AGE_SECS: u64 = 30 * 60;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

/// The pool words: `proxmox` and `ec2`.
pub fn slots_by_pool(leases: &[crate::schema::Lease]) -> Vec<(String, u64)> {
    let ec2 = leases.iter().filter(|l| crate::ec2::is_ec2_handle(&l.vm.handle)).count() as u64;
    vec![("proxmox".to_string(), leases.len() as u64 - ec2), ("ec2".to_string(), ec2)]
}

/// The family's rows, or none when no pass was ever recorded; any other failure stops the publish.
pub fn pass_rows(read: Result<Vec<Value>, tsd::query::Failure>) -> Result<Vec<Value>, String> {
    match read {
        Err(f) if f.code == tsd::query::ERR_NO_FAMILY => Ok(Vec::new()),
        other => other.map_err(|f| f.message),
    }
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

/// The store port over the socket client; a transport failure is exit -1.
pub struct SpiraLc(pub spira_store::lc_call::LcClient);

impl Lc for SpiraLc {
    fn call(&self, args: &[String]) -> (i32, Value) {
        match self.0.call(args) {
            Ok(r) => (r.code, serde_json::from_str(r.out.trim()).unwrap_or_else(|_| json!({ "cannot_tell": r.why() }))),
            Err(e) => (-1, json!({ "cannot_tell": e.to_string() })),
        }
    }
}

/// The hour's reading from the `cert-pass` rows under the run dir and the pool's leases.
pub fn read(cfg: &Config, now: u64) -> Result<Reading, String> {
    let (rows, slots) = inputs(cfg, now)?;
    Ok(measure(&rows, now, slots))
}

type Inputs = (Vec<Value>, Vec<(String, u64)>);

fn inputs(cfg: &Config, now: u64) -> Result<Inputs, String> {
    let rows = pass_rows(tsd::query::Source { reader: None, root: &cfg.run_dir }.rows(tsd::pass::FAMILY, now.saturating_sub(HOUR)))?;
    let state = crate::pool::read_state(&crate::cli::pool_for(cfg).state_file())?;
    Ok((rows, slots_by_pool(&state.leases)))
}

/// Reads the inputs and publishes the section through `lc`: what `round-vm section` does.
pub fn publish(cfg: &Config, lc: &dyn Lc, now: u64) -> Result<Reading, String> {
    let (rows, slots) = inputs(cfg, now)?;
    run(lc, now, &rows, slots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::sync::Mutex;

    const NOW: u64 = 1_793_422_800;

    struct FakeLc(Mutex<Vec<Vec<String>>>);

    impl Lc for FakeLc {
        fn call(&self, args: &[String]) -> (i32, Value) {
            self.0.lock().unwrap().push(args.to_vec());
            match args[1].as_str() {
                "get" => (1, json!({})),
                _ => (0, json!({ "published": true })),
            }
        }
    }

    fn plant_pass(run: &std::path::Path, lines: &[(u64, &str)]) {
        let path = tsd::family_path(run, tsd::pass::FAMILY);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text: String = lines.iter().map(|(at, cause)| format!("{}\n", json!({ "ts": format_rfc3339(*at), "cause": cause }))).collect();
        std::fs::write(path, text).unwrap();
    }

    #[test_id(tgoba7w)]
    #[test]
    fn publish_writes_the_body_the_cli_publisher_wrote_from_the_run_dirs_passes() {
        let dir = TempDir::new();
        plant_pass(dir.path(), &[(NOW - 600, "green"), (NOW - 300, "infra")]);
        let cfg = crate::config::fixture(dir.path());
        let lc = FakeLc(Mutex::new(vec![]));
        let reading = publish(&cfg, &lc, NOW).unwrap();
        assert_eq!(reading, Reading { passes: 2, infra_incomplete_share: 0.5, over_cap_share: 0.0, slots_in_use: vec![("proxmox".into(), 0), ("ec2".into(), 0)] });
        let calls = lc.0.lock().unwrap();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(&calls[0][..2], ["section", "get"]);
        assert_eq!(calls[1][..6], ["section", "publish-status", "--actor", kind::OWNER, "--expect-rev", "0"]);
        let body: Value = serde_json::from_str(&calls[1][7]).unwrap();
        assert_eq!(body, section_body(&status_of(None, &reading), NOW));
        assert_eq!(body["status"][kind::PASSES]["value"], 2.0);
        assert_eq!(body["status"][kind::INFRA_INCOMPLETE]["value"], 0.5);
    }

    #[test_id(trhvrm3)]
    #[test]
    fn the_max_age_seconds_are_the_max_age_the_section_declares() {
        assert_eq!(MAX_AGE_SECS, MAX_AGE.strip_suffix('m').unwrap().parse::<u64>().unwrap() * 60);
    }
}
