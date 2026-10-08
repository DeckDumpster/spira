//! Failed spira-* systemd units (sp-niqjl). `systemctl --user list-units --state=failed`
//! gives the CURRENT list; a state file persists the pass each unit was FIRST seen failing,
//! because a unit crash-looping every 30s re-enters `activating` then `failed` on every
//! restart and systemd's own timestamps never age past one restart interval. One escalation
//! per failure, not per pass — a unit still in the state file with its escalated flag set
//! does not re-file; a unit that recovers (absent from the current list) is dropped, so a
//! later, unrelated failure of the same unit starts its own clock.

use std::path::Path;

/// `systemctl --user list-units --state=failed --no-legend 'spira-*'`, one unit name per
/// line with the leading bullet and trailing columns stripped. `None` means the probe
/// itself failed — rendered `?`, never an empty (all-clear) list.
pub fn gather(systemctl: &str) -> Option<Vec<String>> {
    let out = crate::deadline::output(
        "failed units",
        std::process::Command::new(systemctl).args(["--user", "list-units", "--state=failed", "--no-legend", "spira-*"]),
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut units = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if is_not_found(line) {
            continue;
        }
        if let Some(u) = extract_unit_name(line) {
            units.push(u);
        }
    }
    Some(units)
}

/// `sed -E 's/^[^[:alnum:]]+ //; s/ .*//'` — drop the leading run of non-alphanumeric
/// characters (the bullet and the space after it), then keep only the first remaining
/// whitespace-delimited token.
fn extract_unit_name(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && !bytes[i].is_ascii_alphanumeric() {
        i += 1;
    }
    // Consume through the first space after the bullet run, matching `[^alnum]+ ` (the
    // pattern requires the run to be followed by exactly one space it also consumes).
    let after_bullet = &line[i..];
    let token = after_bullet.split(' ').next().unwrap_or("");
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

/// A unit whose file is gone keeps a residual failed state; LoadState=not-found is absent,
/// not failing.
fn is_not_found(line: &str) -> bool {
    extract_unit_name(line).is_some_and(|u| {
        line.split_whitespace().skip_while(|t| *t != u).nth(1) == Some("not-found")
    })
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

    #[test]
    fn extract_unit_name_strips_the_bullet_and_trailing_columns() {
        assert_eq!(
            extract_unit_name("● spira-watchtower.service loaded failed failed Spira watchtower"),
            Some("spira-watchtower.service".to_string())
        );
    }

    /// Regression for sp-ezeiy: watchtower.sh once extracted "●" itself as the unit name.
    /// Ported verbatim from `test-watchtower-unit-extraction-sp-ezeiy.sh`, which tested a
    /// hand-copied re-implementation of the sed pipeline rather than the real script; this
    /// exercises the actual function, so the bash suite retires (DESIGN.md §4).
    #[test]
    fn sp_ezeiy_never_extracts_the_bullet_character_and_handles_a_double_leading_space() {
        let lines = [
            "● spira-watch-answers-prod.service loaded failed failed Spira watcher answers",
            "● beads-push.service loaded failed failed Push Spira's beads",
            "  ● another-unit.service loaded failed failed Another unit",
        ];
        let expected = [
            "spira-watch-answers-prod.service",
            "beads-push.service",
            "another-unit.service",
        ];
        for (line, want) in lines.iter().zip(expected.iter()) {
            let got = extract_unit_name(line);
            assert_eq!(got.as_deref(), Some(*want));
            assert_ne!(got.as_deref(), Some("\u{25cf}"));
        }
    }

    #[test]
    fn a_not_found_unit_is_absent_not_failed() {
        assert!(is_not_found("○ spira-watch-refresh-prod.service not-found failed failed spira-watch-refresh-prod.service"));
        assert!(!is_not_found("● spira-x.service loaded failed failed Spira x"));
        assert!(!is_not_found("● spira-x.service"));
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
