//! `incident alarm` — a detector's condition as one note in the Concierge inbox, never a bead.
//! The state file maps a condition's reference to when it was last noted; a condition seen
//! again is silent until the reminder interval passes.

use std::path::Path;

use inbox_rules::Sink;

use crate::intake::{now_epoch, now_iso};

pub const REMINDER_S: i64 = 86_400;

/// The state after seeing `reference` at `now`, and whether to write a note. Unrelated rows
/// are kept; a malformed row is dropped.
pub fn decide(state: &str, reference: &str, now: i64) -> (bool, String) {
    let mut rows: Vec<(String, i64)> = state
        .lines()
        .filter_map(|l| {
            let (r, t) = l.rsplit_once('\t')?;
            Some((r.to_string(), t.parse().ok()?))
        })
        .collect();
    let due = match rows.iter_mut().find(|(r, _)| r == reference) {
        Some((_, t)) if now - *t < REMINDER_S => false,
        Some((_, t)) => {
            *t = now;
            true
        }
        None => {
            rows.push((reference.to_string(), now));
            true
        }
    };
    let out = rows.iter().map(|(r, t)| format!("{r}\t{t}\n")).collect();
    (due, out)
}

/// Notes `reference` in the Concierge inbox when it is due, and remembers it only once the line
/// is there. `Ok(false)` is a condition not due; `Err` a due one that was not noted. The caller
/// holds the incident lock.
pub fn note(sink: &Sink, state_path: &Path, reference: &str, title: &str) -> Result<bool, String> {
    let state = std::fs::read_to_string(state_path).unwrap_or_default();
    let (due, next) = decide(&state, reference, now_epoch());
    if !due {
        return Ok(false);
    }
    sink.record(&format!("{} {}", now_iso(), line(title, reference)))?;
    std::fs::write(state_path, next).map_err(|e| format!("{}: {e}", state_path.display()))?;
    Ok(true)
}

/// One inbox line: single-line, so the triage reads it as one event.
pub fn line(title: &str, reference: &str) -> String {
    let flat = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("ALARM {} [{}]", flat(title), flat(reference))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(te7cza3)]
    #[test]
    fn a_new_condition_notes_once_and_a_repeat_is_silent() {
        let (a, s) = decide("", "incident:x", 1000);
        assert!(a);
        let (b, _) = decide(&s, "incident:x", 1000 + 60);
        assert!(!b);
    }

    #[test_id(tnl54uz)]
    #[test]
    fn a_persisting_condition_reminds_once_per_interval() {
        let (_, s) = decide("", "incident:x", 1000);
        let (due, s) = decide(&s, "incident:x", 1000 + REMINDER_S);
        assert!(due);
        let (again, _) = decide(&s, "incident:x", 1000 + REMINDER_S + 1);
        assert!(!again);
    }

    #[test_id(ts0usbq)]
    #[test]
    fn another_condition_is_not_silenced_by_the_first() {
        let (_, s) = decide("", "incident:x", 1000);
        let (due, s) = decide(&s, "incident:y", 1001);
        assert!(due);
        assert!(s.contains("incident:x\t1000") && s.contains("incident:y\t1001"));
    }

    #[test_id(tr4pe04)]
    #[test]
    fn a_line_is_one_line() {
        assert_eq!(line("A\nB  C", "r"), "ALARM A B C [r]");
    }
}
