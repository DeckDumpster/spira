//! Raising, re-raising and clearing one alert, and the write probe, against the `Conditions`
//! port so the whole thing is testable with an in-memory fake. An alert is the control-plane
//! condition `conditions/heimdall/<key>`, never a bd item.

use crate::conditions::{Condition, Conditions, Failure, ALERT, PROBE, PROBE_KEY};
use crate::util;

/// The description, REGENERATED WHOLE on every write, never patched
/// (law-regenerate-derived-summaries): a patched alert body accumulates the readings that
/// were true at each past firing, and the reader cannot tell which one is now.
///
/// NO MARKDOWN IN THE TRAILER. It is read in a tmux pane, which renders no emphasis.
pub fn body_of(key: &str, first: i64, flaps: i64, evidence: &str, tz: &str) -> String {
    body_with(key, &util::iso_display(first, tz), flaps, evidence)
}

/// [`body_of`] with the first-seen time already rendered as `first_seen`.
pub fn body_with(key: &str, first_seen: &str, flaps: i64, evidence: &str) -> String {
    format!(
        "**Condition:** `{key}`\n**First seen:** {first_seen}  ·  **Fired:** {flaps} time(s)\n\n```\n{}\n```\n\nRaised by Heimdall (bin/heimdall; replaces spira/heimdall.sh), which watches the loop and only\nspeaks - it repairs nothing. It clears this itself when the condition passes,\nso nothing is owed. Clearing it by hand acknowledges it: Heimdall will not raise\nit again until the condition has cleared and returned.\n",
        evidence.trim(),
    )
}

/// The close reason for a cleared alert, with the raise time already rendered as `raised`.
pub fn clear_reason_with(raised: &str, flaps: i64, clear_n: i64) -> String {
    format!(
        "Cleared by Heimdall: the condition stopped holding for {clear_n} consecutive checks.\nRaised {raised}, fired {flaps} time(s).\nNothing was repaired by Heimdall — it only reports. Either the loop recovered on its own\nor something else fixed it.\n",
    )
}

/// This key's condition, from the last `condition list`.
pub fn find_by_key<'a>(rows: &'a [Condition], key: &str) -> Option<&'a Condition> {
    rows.iter().find(|r| r.key == key)
}

/// What a raise writes: the title is the message's first line, so the pane reads both from one
/// field.
pub fn message_of(title: &str, body: &str) -> String {
    format!("{}\n\n{}", title.lines().next().unwrap_or(""), body)
}

/// Raises or re-raises the alert for `key`. The store counts the flaps and forgets an
/// acknowledgement when the condition returns. Returns the key on success.
pub fn alert_write(ops: &dyn Conditions, key: &str, title: &str, body: &str) -> Result<String, Failure> {
    ops.set(key, ALERT, "Firing", &message_of(title, body), true)?;
    Ok(key.to_string())
}

/// Clears the alert for `key`: a no-op success when there is none or it is not firing.
pub fn alert_clear_with(ops: &dyn Conditions, existing: Option<&Condition>, reason: &str) -> Result<(), Failure> {
    let Some(c) = existing.filter(|c| c.firing()) else { return Ok(()) };
    ops.clear(&c.key, "Cleared", reason)
}

// ---- the write probe --------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOutcome {
    pub id: Option<String>,
    /// `None` when unattempted (the store unreachable, so the probe never ran at all).
    pub write_ok: Option<bool>,
    pub saturated: bool,
}

/// Exercises the write path on every pass: one fixed condition, written False with the time,
/// so it never reads as firing and there is nothing to duplicate. A timeout is contention,
/// not unavailability.
pub fn maintain_probe(ops: &dyn Conditions) -> ProbeOutcome {
    match ops.set(PROBE_KEY, PROBE, "Probe", &util::now_epoch().to_string(), false) {
        Ok(()) => ProbeOutcome { id: Some(PROBE_KEY.into()), write_ok: Some(true), saturated: false },
        Err(f) => ProbeOutcome { id: None, write_ok: Some(false), saturated: f == Failure::Saturated },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Fake, Mode};

    #[test]
    fn a_clear_with_nothing_firing_writes_nothing() {
        let ops = Fake::default();
        alert_clear_with(&ops, None, "r").unwrap();
        let off = Condition { key: "k".into(), kind: ALERT.into(), status: "False".into(), flaps: 1 };
        alert_clear_with(&ops, Some(&off), "r").unwrap();
        assert!(ops.calls().is_empty());
    }

    #[test]
    fn the_message_leads_with_the_title() {
        assert_eq!(message_of("Loop stalled\nmore", "evidence"), "Loop stalled\n\nevidence");
    }

    #[test]
    fn the_probe_writes_a_condition_that_never_fires_and_a_timeout_is_saturation() {
        let ops = Fake::default();
        assert_eq!(maintain_probe(&ops), ProbeOutcome { id: Some(PROBE_KEY.into()), write_ok: Some(true), saturated: false });
        assert_eq!(ops.status(PROBE_KEY).as_deref(), Some("False"));
        let busy = Fake::in_mode(Mode::WriteFails(Failure::Saturated));
        assert_eq!(maintain_probe(&busy), ProbeOutcome { id: None, write_ok: Some(false), saturated: true });
        let down = Fake::in_mode(Mode::WriteFails(Failure::Other));
        assert!(!maintain_probe(&down).saturated);
    }
}
