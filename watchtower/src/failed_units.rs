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

/// The spira-* units and the shared ones that are failed now. `None` means the manager could
/// not be asked, rendered `?`, never an empty (all-clear) list. A unit whose file is gone is
/// `Absent`, not failed.
pub fn gather(units: &dyn Units) -> Option<Vec<String>> {
    let mut failed: Vec<String> = units.list("spira-*").ok()?.into_iter().filter(|d| d.state == UnitState::Failed).map(|d| d.name).collect();
    for name in SHARED_UNITS {
        if units.facts(&UnitHandle::named(name)).ok()?.state == UnitState::Failed {
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
}

pub fn parse_state(text: &str) -> Vec<StateRow> {
    let mut out = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() != 3 {
            continue;
        }
        let Ok(first_seen) = parts[1].parse::<i64>() else {
            continue;
        };
        out.push(StateRow {
            unit: parts[0].to_string(),
            first_seen,
            escalated: parts[2] == "1",
        });
    }
    out
}

pub fn render_state(rows: &[StateRow]) -> String {
    let mut s = String::new();
    for r in rows {
        s.push_str(&format!("{} {} {}\n", r.unit, r.first_seen, if r.escalated { 1 } else { 0 }));
    }
    s
}

/// The pure per-unit decision: given what the state file had for this unit (or none, a
/// first sighting), what its new row should be and whether this pass must escalate.
pub fn decide(now: i64, unit: &str, prev: Option<&StateRow>, warn_mins: i64) -> (StateRow, bool) {
    let (first_seen, escalated) = match prev {
        Some(r) => (r.first_seen, r.escalated),
        None => (now, false),
    };
    let age_mins = (now - first_seen) / 60;
    let should_escalate = !escalated && age_mins >= warn_mins;
    let new_escalated = escalated || should_escalate;
    (
        StateRow {
            unit: unit.to_string(),
            first_seen,
            escalated: new_escalated,
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
        assert_eq!(gather(&u), Some(vec!["spira-watchtower.service".to_string(), "dolt-beads.service".to_string()]));
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
        let (row, esc) = decide(1_000_000, "spira-x.service", None, 15);
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
        };
        // 14 minutes: not yet.
        let (row, esc) = decide(1_000_000 + 14 * 60, "spira-x.service", Some(&prev), 15);
        assert!(!esc);
        assert!(!row.escalated);
        // 15 minutes: fires, and the row records it as escalated.
        let (row2, esc2) = decide(1_000_000 + 15 * 60, "spira-x.service", Some(&prev), 15);
        assert!(esc2);
        assert!(row2.escalated);
    }

    #[test]
    fn dedup_once_escalated_later_passes_do_not_refire() {
        let prev = StateRow {
            unit: "spira-x.service".into(),
            first_seen: 1_000_000,
            escalated: true,
        };
        let (row, esc) = decide(1_000_000 + 999 * 60, "spira-x.service", Some(&prev), 15);
        assert!(!esc);
        assert!(row.escalated);
    }

    #[test]
    fn state_file_round_trips_through_parse_and_render() {
        let rows = vec![
            StateRow { unit: "a.service".into(), first_seen: 100, escalated: true },
            StateRow { unit: "b.service".into(), first_seen: 200, escalated: false },
        ];
        let text = render_state(&rows);
        assert_eq!(parse_state(&text), rows);
    }
}
