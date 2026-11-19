//! Failed spira-* and shared systemd units (sp-niqjl). The Units port gives the CURRENT list; a state file persists the pass each unit was FIRST seen failing,
//! because a unit crash-looping every 30s re-enters `activating` then `failed` on every
//! restart and systemd's own timestamps never age past one restart interval. One escalation
//! per failure, not per pass — a unit still in the state file with its escalated flag set
//! does not re-file; a unit that recovers (absent from the current list) is dropped, so a
//! later, unrelated failure of the same unit starts its own clock.

use std::path::Path;

use runtime::units::{UnitHandle, UnitState, Units};

/// Units no instance owns, so `spira-*` never matches them.
pub const SHARED_UNITS: &[&str] = &[
    "cockpit-ensure.service",
    "concierge.service",
    "dolt-beads.service",
    "dolt-tmp-prune.service",
    "lc-serve.service",
    "sccache-dav.service",
];

/// Every failed unit of the manager's scope, transient ones included, and the shared ones. `None` means the manager could
/// not be asked, rendered `?`, never an empty (all-clear) list. A unit whose file is gone is
/// `Absent`, not failed.
pub fn gather(units: &dyn Units) -> Option<Vec<String>> {
    Some(gather_runs(units)?.into_iter().map(|(name, _)| name).collect())
}

/// `gather`, each failed unit with the `InvocationID` of the run that failed ("" if systemd gave none).
pub fn gather_runs(units: &dyn Units) -> Option<Vec<(String, String)>> {
    let names = gather_names(units)?;
    Some(names.into_iter().map(|n| { let inv = units.detail(&UnitHandle::named(&n)).ok().and_then(|d| d.invocation_id).unwrap_or_default(); (n, inv) }).collect())
}

fn gather_names(units: &dyn Units) -> Option<Vec<String>> {
    let mut failed: Vec<String> = units.list("*").ok()?.into_iter().filter(|d| d.state == UnitState::Failed).map(|d| d.name).collect();
    for name in SHARED_UNITS {
        if !failed.iter().any(|f| f == name) && units.facts(&UnitHandle::named(name)).ok()?.state == UnitState::Failed {
            failed.push((*name).to_string());
        }
    }
    Some(crate::ctrl_gate::without_suspended(failed))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateRow {
    pub unit: String,
    pub first_seen: i64,
    pub escalated: bool,
    pub invocation: String,
}

pub fn parse_state(text: &str) -> Vec<StateRow> {
    let mut out = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() != 3 && parts.len() != 4 {
            continue;
        }
        let Ok(first_seen) = parts[1].parse::<i64>() else {
            continue;
        };
        out.push(StateRow {
            unit: parts[0].to_string(),
            first_seen,
            escalated: parts[2] == "1",
            invocation: parts.get(3).filter(|i| **i != "-").map(|i| i.to_string()).unwrap_or_default(),
        });
    }
    out
}

pub fn render_state(rows: &[StateRow]) -> String {
    let mut s = String::new();
    for r in rows {
        s.push_str(&format!("{} {} {} {}\n", r.unit, r.first_seen, if r.escalated { 1 } else { 0 }, if r.invocation.is_empty() { "-" } else { &r.invocation }));
    }
    s
}

/// The pure per-unit decision: given what the state file had for this unit (or none, a
/// first sighting), what its new row should be and whether this pass must escalate. The age
/// is of one failed invocation: a row of another invocation starts over.
pub fn decide(now: i64, unit: &str, invocation: &str, prev: Option<&StateRow>, warn_mins: i64) -> (StateRow, bool) {
    let (first_seen, escalated) = match prev {
        Some(r) if r.invocation == invocation => (r.first_seen, r.escalated),
        _ => (now, false),
    };
    let age_mins = (now - first_seen) / 60;
    let should_escalate = !escalated && age_mins >= warn_mins;
    let new_escalated = escalated || should_escalate;
    (
        StateRow {
            unit: unit.to_string(),
            first_seen,
            escalated: new_escalated,
            invocation: invocation.to_string(),
        },
        should_escalate,
    )
}

pub fn read_state_file(path: &Path) -> Vec<StateRow> {
    std::fs::read_to_string(path)
        .map(|t| parse_state(&t))
        .unwrap_or_default()
}

pub fn write_state_file(path: &Path, rows: &[StateRow]) {
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, render_state(rows)).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use runtime::ports::VirtualClock;
    use runtime::units::{ScriptedUnits, UnitDetail, UnitFacts};
    use std::sync::Arc;

    fn units() -> ScriptedUnits {
        ScriptedUnits::new(Arc::new(VirtualClock::new(0)))
    }

    fn at(u: &ScriptedUnits, name: &str, state: UnitState) {
        let mut d = UnitDetail::from_facts(name, &UnitFacts { state, main_pid: None, exit: None, since_ms: None });
        d.load_state = Some("loaded".into());
        u.install_detail(d);
    }

    #[test]
    fn a_failed_spira_unit_and_a_failed_shared_unit_are_both_named() {
        let u = units();
        at(&u, "spira-watchtower.service", UnitState::Failed);
        at(&u, "spira-ok.service", UnitState::Active);
        at(&u, "dolt-beads.service", UnitState::Failed);
        at(&u, "concierge.service", UnitState::Active);
        assert_eq!(gather(&u), Some(vec!["dolt-beads.service".to_string(), "spira-watchtower.service".to_string()]));
    }

    #[test]
    fn a_failed_transient_unit_outside_the_spira_prefix_is_named() {
        let u = units();
        at(&u, "concierge-hel-catchup.service", UnitState::Failed);
        at(&u, "dolt-beads.service", UnitState::Failed);
        assert_eq!(gather(&u), Some(vec!["concierge-hel-catchup.service".to_string(), "dolt-beads.service".to_string()]));
    }

    #[test]
    fn a_unit_whose_file_is_gone_is_absent_not_failed() {
        let u = units();
        u.install_detail(UnitDetail::absent("spira-watch-refresh-prod.service"));
        u.install_detail(UnitDetail::absent("lc-serve.service"));
        assert_eq!(gather(&u), Some(vec![]));
    }

    #[test]
    fn a_manager_that_cannot_be_asked_is_unknown_never_all_clear() {
        struct Down;
        impl Units for Down {
            fn start(&self, _: &runtime::units::UnitSpec) -> runtime::Result<UnitHandle> {
                Err(runtime::RuntimeError::new("down"))
            }
            fn stop(&self, _: &UnitHandle) -> runtime::Result<()> {
                Err(runtime::RuntimeError::new("down"))
            }
            fn restart(&self, _: &UnitHandle) -> runtime::Result<()> {
                Err(runtime::RuntimeError::new("down"))
            }
            fn facts(&self, _: &UnitHandle) -> runtime::Result<UnitFacts> {
                Err(runtime::RuntimeError::new("down"))
            }
        }
        assert_eq!(gather(&Down), None);
    }

    #[test]
    fn first_sighting_never_escalates_immediately() {
        let (row, esc) = decide(1_000_000, "spira-x.service", "i1", None, 15);
        assert!(!esc);
        assert_eq!(row.first_seen, 1_000_000);
        assert!(!row.escalated);
    }

    #[test]
    fn escalates_once_age_crosses_the_threshold_on_a_later_pass() {
        let prev = StateRow {
            unit: "spira-x.service".into(),
            first_seen: 1_000_000,
            escalated: false,
            invocation: "i1".into(),
        };
        // 14 minutes: not yet.
        let (row, esc) = decide(1_000_000 + 14 * 60, "spira-x.service", "i1", Some(&prev), 15);
        assert!(!esc);
        assert!(!row.escalated);
        // 15 minutes: fires, and the row records it as escalated.
        let (row2, esc2) = decide(1_000_000 + 15 * 60, "spira-x.service", "i1", Some(&prev), 15);
        assert!(esc2);
        assert!(row2.escalated);
    }

    #[test]
    fn dedup_once_escalated_later_passes_do_not_refire() {
        let prev = StateRow {
            unit: "spira-x.service".into(),
            first_seen: 1_000_000,
            escalated: true,
            invocation: "i1".into(),
        };
        let (row, esc) = decide(1_000_000 + 999 * 60, "spira-x.service", "i1", Some(&prev), 15);
        assert!(!esc);
        assert!(row.escalated);
    }

    #[test]
    fn state_file_round_trips_through_parse_and_render() {
        let rows = vec![
            StateRow { unit: "a.service".into(), first_seen: 100, escalated: true, invocation: "i1".into() },
            StateRow { unit: "b.service".into(), first_seen: 200, escalated: false, invocation: String::new() },
        ];
        let text = render_state(&rows);
        assert_eq!(parse_state(&text), rows);
    }

    #[test]
    fn a_new_invocation_restarts_the_age_and_clears_escalated() {
        let prev = StateRow { unit: "u".into(), first_seen: 1_000_000, escalated: true, invocation: "i1".into() };
        let now = 1_000_000 + 29 * 60;
        let (row, esc) = decide(now, "u", "i2", Some(&prev), 15);
        assert!(!esc);
        assert_eq!((row.first_seen, row.escalated, row.invocation.as_str()), (now, false, "i2"));
    }

    #[test]
    fn one_invocation_escalates_once_at_16_minutes_and_not_again_at_46() {
        let prev = StateRow { unit: "u".into(), first_seen: 1_000_000, escalated: false, invocation: "i1".into() };
        let (row, esc) = decide(1_000_000 + 16 * 60, "u", "i1", Some(&prev), 15);
        assert!(esc && row.escalated);
        let (_, again) = decide(1_000_000 + 46 * 60, "u", "i1", Some(&row), 15);
        assert!(!again);
    }

    #[test]
    fn a_state_row_written_before_invocations_reads_as_another_invocation() {
        let rows = parse_state("u 1000000 1\n");
        assert_eq!(rows[0].invocation, "");
        let (row, esc) = decide(1_000_000 + 60 * 60, "u", "i1", Some(&rows[0]), 15);
        assert!(!esc);
        assert_eq!(row.first_seen, 1_000_000 + 60 * 60);
    }

    #[test]
    fn gather_runs_names_each_failed_units_invocation() {
        let u = units();
        let mut d = UnitDetail::from_facts("spira-a.service", &UnitFacts { state: UnitState::Failed, main_pid: None, exit: None, since_ms: None });
        d.load_state = Some("loaded".into());
        d.invocation_id = Some("abc".into());
        u.install_detail(d);
        assert_eq!(gather_runs(&u), Some(vec![("spira-a.service".to_string(), "abc".to_string())]));
    }
}
