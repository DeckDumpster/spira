//! Heimdall's in-scheduler checks, for the `reconcile.heimdall` task: the same causes `judge`
//! reads from log tails and unit listings, read instead from what owns each fact — a sibling
//! task's `scheduler status`, the export task's last outcome, the unit manager's restart counts.
//! The scheduler itself is watched from outside by the watchdog, not here. Pure: `now` and the
//! readings in, the firing set out; no clock read, no child, no file.

use std::collections::{BTreeMap, BTreeSet};

use crate::classify::Firing;
use crate::reconcile::{step_keys, Step, Trigger};
use crate::state::KeyState;

/// The readings, each from the component that owns it. Times are epoch seconds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Readings {
    pub now: i64,
    /// The sentinel task is armed in the scheduler.
    pub sentinel_armed: bool,
    /// When the sentinel task last fired, if it ever has.
    pub sentinel_last_fired: Option<i64>,
    /// (unit, restarts in the window) for the units that remain processes.
    pub restart_alerts: Vec<(String, u64)>,
    pub mirror_age_secs: Option<u64>,
    pub world_halted: bool,
    pub world_draining: bool,
    /// Beads holding a lease with no live dwarf past the ghost threshold.
    pub strands: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Thresholds {
    pub pass_stale: i64,
    pub mirror_stale: i64,
    pub restart_threshold: i64,
    pub restart_window: i64,
}

fn age(sec: i64) -> String {
    if sec < 120 {
        format!("{sec}s")
    } else if sec < 7200 {
        format!("{}m", sec.div_euclid(60))
    } else {
        format!("{:.1}h", sec as f64 / 3600.0)
    }
}

/// The firing causes, sorted by key.
pub fn classify(r: &Readings, th: &Thresholds) -> Vec<Firing> {
    let mut out = Vec::new();
    let mut alert = |key: String, title: String, evidence: String| out.push(Firing { key, title, evidence });
    if !r.world_halted && !r.world_draining {
        if !r.sentinel_armed {
            alert("sentinel-stale".into(), "The sentinel task is not armed in the scheduler — the loop is not being run".into(), "ARMED     no\n\nNothing is being claimed, landed, reaped or reclaimed while this holds.".into());
        } else if let Some(at) = r.sentinel_last_fired.filter(|at| r.now - at > th.pass_stale) {
            alert(
                "sentinel-stale".into(),
                format!("The Spira sentinel has not fired in {}", age(r.now - at)),
                format!("LAST FIRED {} ago\nTHRESHOLD {}s\nARMED     yes\n\nNothing is being claimed, landed, reaped or reclaimed while this holds.\nDwarves already running are unaffected and will finish.", age(r.now - at), th.pass_stale),
            );
        }
    }
    if let Some(a) = r.mirror_age_secs.map(|a| a as i64).filter(|a| *a > th.mirror_stale) {
        alert("mirror-stale".into(), format!("The beads mirror has not been rewritten in {}", age(a)), format!("WRITTEN   {} ago\nTHRESHOLD {}\n\nThe mirror is the copy of the database that reaches git, review and the phone.", age(a), age(th.mirror_stale)));
    }
    if !r.strands.is_empty() {
        alert("lease-unreclaimed".into(), format!("{} bead(s) hold a lease with no live dwarf", r.strands.len()), format!("--- ghosts ---\n{}", r.strands.iter().map(|s| format!("  {s}")).collect::<Vec<_>>().join("\n")));
    }
    for (unit, delta) in &r.restart_alerts {
        if (*delta as i64) <= th.restart_threshold {
            continue;
        }
        alert(
            format!("restart-loop:{unit}"),
            format!("{unit} restarted {delta} time(s) in the last {}", age(th.restart_window)),
            format!("UNIT      {unit}\nRESTARTS  {delta} in this {} window (threshold {})\n\nA unit that restarts forever never reaches 'failed' — incident intake misses it.", age(th.restart_window), th.restart_threshold),
        );
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// One pass of the debounce machine over `firing`, with the counters saturated at their
/// thresholds: only `seen >= confirm` and `unseen >= clear` are ever read, so a steady key
/// leaves its state unchanged pass after pass instead of counting forever.
pub fn step(now: i64, confirm: i64, clear_n: i64, refresh: i64, db_reachable: bool, firing: &[Firing], prior: &BTreeMap<String, KeyState>) -> Vec<Step> {
    let keys: BTreeSet<String> = firing.iter().map(|f| f.key.clone()).collect();
    let mut steps = step_keys(now, confirm, clear_n, refresh, db_reachable, &keys, prior);
    for s in &mut steps {
        s.counters.seen = s.counters.seen.min(confirm.max(1));
        s.counters.unseen = s.counters.unseen.min(clear_n.max(1));
    }
    steps
}

/// Whether any step raises: what a pass with an unreachable store must not attempt.
pub fn raises(steps: &[Step]) -> Vec<String> {
    steps.iter().filter(|s| s.trigger == Trigger::ConfirmRaise).map(|s| s.key.clone()).collect()
}

/// An alert's description with its first-seen time already rendered (`when`), so no clock or
/// zone library is needed to write it.
pub fn body_at(key: &str, when: &str, flaps: i64, evidence: &str) -> String {
    crate::alerts::body_with(key, when, flaps, evidence)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TH: Thresholds = Thresholds { pass_stale: 900, mirror_stale: 90_000, restart_threshold: 5, restart_window: 3600 };

    #[test]
    fn a_stale_sentinel_fires_unless_the_world_is_halted() {
        let r = Readings { now: 10_000, sentinel_armed: true, sentinel_last_fired: Some(1_000), ..Readings::default() };
        assert_eq!(classify(&r, &TH).iter().map(|f| f.key.as_str()).collect::<Vec<_>>(), ["sentinel-stale"]);
        assert!(classify(&Readings { world_halted: true, ..r.clone() }, &TH).is_empty());
        assert!(classify(&Readings { sentinel_last_fired: Some(9_500), ..r }, &TH).is_empty());
    }

    #[test]
    fn a_steady_firing_key_stops_counting_at_its_threshold() {
        let f = vec![Firing { key: "k".into(), title: "t".into(), evidence: String::new() }];
        let mut prior = BTreeMap::new();
        for _ in 0..5 {
            let s = step(100, 2, 2, 3600, true, &f, &prior);
            prior.insert("k".into(), s[0].on_success(100));
        }
        assert_eq!(prior["k"].seen, 2);
        assert_eq!(prior["k"].state, "firing");
    }
}
