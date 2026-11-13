//! `Host`: how much room and IO the box has left for the gate and the builders, written by the
//! watchtower. Its levers shed and restore one gate slot, and reap the oldest tenth of what is
//! stale; the applier compares what it holds against the intent's `from` before it acts.

use serde::Deserialize;
use serde_json::{json, Value};

use super::{typed, ConditionDecl, FieldShape, SchemaError, SectionKind};
use crate::section::{Numeric, Target};

pub const NAME: &str = "Host";
pub const OWNER: &str = "watchtower";
/// The one Host section.
pub const SECTION: &str = "host";

pub const SCRATCH_SHORT: &str = "ScratchShort";
pub const DISK_SHORT: &str = "DiskShort";
pub const IO_PRESSURE: &str = "IoPressure";
pub const HOST_SHEDDING: &str = "HostShedding";
pub const SHED: &str = "Shed";

pub const SHED_SLOT: &str = "shed-gate-slot";
pub const RESTORE_SLOT: &str = "restore-gate-slot";
pub const REAP_SCRATCH: &str = "reap-stale-scratch";
pub const REAP_TARGETS: &str = "reap-stale-targets";

pub const SCRATCH: &str = "scratch_free_share";
pub const WORKTREES: &str = "worktree_free_share";
pub const ROOT: &str = "root_free_share";
pub const IO: &str = "io_full_avg300";
pub const CERTIFY_PAR: &str = "certify_par";
pub const SHED_FROM: &str = "shed_from";
/// Stale entries each reap would take a tenth of; the reaps' knobs mirror them.
pub const STALE_SCRATCH: &str = "stale_scratch";
pub const STALE_TARGETS: &str = "stale_targets";

pub const SCRATCH_SPEC: &str = "scratch_free_share";
pub const WORKTREES_SPEC: &str = "worktree_free_share";
pub const IO_SPEC: &str = "io_full_avg300";

/// The distance's literals follow the spec's targets (a test holds them together). Each term
/// is its excess over the spec, normalised by the spec; only current readings enter it.
pub const DISTANCE: &str = "max(0, 0.3 - scratch_free_share) / 0.3 + max(0, 0.2 - worktree_free_share) / 0.2 + max(0, io_full_avg300 - 40) / 40";

pub const OPEN: f64 = 0.05;
/// A shed is `Shed` (ready to be undone) once the distance is under this.
pub const CLOSE: f64 = 0.02;

/// The config key the gate actions step, as `spira-config` names it.
pub const CERTIFY_PAR_KEY: &str = "spira.certify_par";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct Spec {
    pub scratch_free_share: Option<Target<f64>>,
    pub worktree_free_share: Option<Target<f64>>,
    pub io_full_avg300: Option<Target<f64>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct Status {
    pub scratch_free_share: Option<Numeric>,
    pub worktree_free_share: Option<Numeric>,
    pub root_free_share: Option<Numeric>,
    pub io_full_avg300: Option<Numeric>,
    pub certify_par: Option<Numeric>,
    pub shed_from: Option<Numeric>,
    pub stale_scratch: Option<Numeric>,
    pub stale_targets: Option<Numeric>,
}

/// The levers the owner publishes with every status.
pub fn actions() -> Value {
    json!([
        { "name": SHED_SLOT, "class": "capacity-lower", "step": "-1", "moves": IO,
          "addresses": [IO_PRESSURE, SCRATCH_SHORT], "expected_effect": -5.0, "horizon": "15m" },
        { "name": RESTORE_SLOT, "class": "capacity-raise", "step": "+1", "moves": CERTIFY_PAR,
          "addresses": [SHED], "expected_effect": 1.0, "horizon": "15m" },
        { "name": REAP_SCRATCH, "class": "destructive", "step": "10% fewer", "moves": SCRATCH,
          "addresses": [SCRATCH_SHORT, DISK_SHORT], "expected_effect": 0.01, "horizon": "15m" },
        { "name": REAP_TARGETS, "class": "destructive", "step": "10% fewer", "moves": WORKTREES,
          "addresses": [SCRATCH_SHORT, DISK_SHORT], "expected_effect": 0.01, "horizon": "15m" },
    ])
}

fn check_spec(v: &Value) -> Result<(), SchemaError> {
    typed::<Spec>(v)
}

fn check_status(v: &Value) -> Result<(), SchemaError> {
    typed::<Status>(v)
}

pub const KIND: SectionKind = SectionKind {
    name: NAME,
    owner: OWNER,
    escalation: None,
    conditions: &[
        ConditionDecl { type_: SCRATCH_SHORT, page_on_raise: false },
        ConditionDecl { type_: DISK_SHORT, page_on_raise: false },
        ConditionDecl { type_: IO_PRESSURE, page_on_raise: false },
        ConditionDecl { type_: HOST_SHEDDING, page_on_raise: false },
        ConditionDecl { type_: SHED, page_on_raise: false },
    ],
    fields: &[
        (SCRATCH, FieldShape::Numeric),
        (WORKTREES, FieldShape::Numeric),
        (ROOT, FieldShape::Numeric),
        (IO, FieldShape::Numeric),
        (CERTIFY_PAR, FieldShape::Numeric),
        (SHED_FROM, FieldShape::Numeric),
        (STALE_SCRATCH, FieldShape::Numeric),
        (STALE_TARGETS, FieldShape::Numeric),
    ],
    check_spec,
    check_status,
};

#[cfg(test)]
mod tests {

    use super::*;
    use crate::section::{admit_publish, parse_section};

    fn section(status: Value) -> Value {
        json!({
            "apiVersion": "spira/v1",
            "kind": NAME,
            "metadata": { "name": SECTION, "owner": OWNER, "observedAt": "2026-10-31T04:10:00Z", "target_latency": "5m", "max_age": "15m" },
            "status": status,
            "flow": { "consumes": [], "produces": ["host/scratch", "host/io"] },
            "distance": { "fn": DISTANCE, "open": OPEN, "close": CLOSE },
            "actions": actions(),
        })
    }

    fn status() -> Value {
        json!({
            "observedGeneration": 0,
            SCRATCH: { "value": 0.15, "window": "level" },
            WORKTREES: { "value": 0.30, "window": "level" },
            ROOT: { "value": 0.40, "window": "level" },
            IO: { "value": 60.0, "window": "5m" },
            CERTIFY_PAR: { "value": 2.0, "window": "level" },
        })
    }

    #[test]
    fn the_kind_admits_its_section_and_its_distance_sums_each_excess_over_its_spec() {
        let s = parse_section(&section(status())).unwrap();
        let kind = admit_publish(&s).unwrap();
        assert_eq!(kind.name, NAME);
        // scratch 0.15 of 0.30 => 0.5; worktrees over; IO 60 of 40 => 0.5.
        assert!((s.distance_value(kind).unwrap().unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_host_inside_every_spec_has_no_distance() {
        let mut st = status();
        st[SCRATCH]["value"] = json!(0.9);
        st[IO]["value"] = json!(3.0);
        let s = parse_section(&section(st)).unwrap();
        assert_eq!(s.distance_value(admit_publish(&s).unwrap()).unwrap().unwrap(), 0.0);
    }

    #[test]
    fn a_field_the_kind_does_not_declare_is_refused_by_name() {
        let mut st = status();
        st["io_full_avg60"] = json!({ "value": 1.0, "window": "1m" });
        let r = admit_publish(&parse_section(&section(st)).unwrap()).unwrap_err();
        assert!(r.rule.contains("io_full_avg60"), "{r:?}");
    }

    #[test]
    fn the_shipped_spec_seeds_the_three_targets_and_the_distance_follows_them() {
        let fragment = crate::producer::parse_fragment(include_str!("../../examples/host.toml")).unwrap();
        let composite = crate::compose::compose(&[fragment]).unwrap();
        let specs = crate::compose::section_specs(&composite);
        let s = specs.iter().find(|s| s["kind"] == NAME).expect("a Host spec is seeded");
        assert_eq!(s["metadata"]["name"], SECTION);
        for (key, want) in [(SCRATCH_SPEC, 0.30), (WORKTREES_SPEC, 0.20), (IO_SPEC, 40.0)] {
            let target = s["spec"][key]["value"].as_f64().unwrap();
            assert_eq!(target, want);
            assert!(DISTANCE.contains(&target.to_string()), "the distance literal {DISTANCE:?} follows the spec's {target}");
        }
        assert!(check_spec(&json!({ "scratch_free": { "value": 0.3, "statute": "x" } })).is_err());
    }
}
