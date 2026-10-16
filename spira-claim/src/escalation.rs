//! A bead's rejection standing, read from its facts: the `reopen` causes every path writes,
//! the `rejection` detail where one was recorded, and the `escalated` holds that restart it.

use spira_config::escalation::{self, Standing};

use crate::events::{classify, EventRow, ReturnClass};

/// Rows oldest first. A rejection is a judged `reopen` cause (a red ejection or rework);
/// harness faults and sequencing ejects classify otherwise and do not count. A hold the
/// ceiling recorded starts the count again, so a bead the Concierge has mended gets its tiers anew.
pub fn standing(rows: &[EventRow]) -> Standing {
    let mut st = Standing::default();
    for r in rows {
        let cause = r.new_value.clone().unwrap_or_default();
        match r.event_type.as_str() {
            "reopen" if classify(&cause) == ReturnClass::Judged => st.rejections += 1,
            escalation::REJECTION => st.history.extend(escalation::parse_all(&[cause])),
            escalation::ESCALATED if cause.starts_with("hold") => st = Standing::default(),
            _ => {}
        }
    }
    st
}

#[cfg(test)]
mod tests {
    use super::*;
    use escalation::{Rejection, Tier};

    fn f(kind: &str, cause: &str) -> EventRow {
        EventRow::new("sp-a", kind, cause, "2026-10-16T10:00:00Z")
    }

    #[test]
    fn only_judged_reopens_count_and_sequencing_ejects_and_gate_faults_do_not() {
        let rows = [f("reopen", "eject"), f("reopen", "gate-no-verdict"), f("reopen", "gate-base-red"), f("reopen", "queue-eject"), f("claimed", ""), f("reopen", "eject-red")];
        let st = standing(&rows);
        assert_eq!((st.rejections, escalation::tier(st.rejections)), (1, Tier::Normal));
    }

    #[test]
    fn two_rejections_escalate_three_hold_and_a_recorded_hold_restarts_the_count() {
        let mut rows = vec![f("reopen", "eject-red"), f("reopen", "gate-red")];
        rows.push(f("rejection", &Rejection::new("gate-red", "test-x.sh", "t1", "boom").encode()));
        let st = standing(&rows);
        assert_eq!(escalation::tier(st.rejections), Tier::Stronger);
        assert_eq!(st.history.len(), 1);
        rows.push(f("reopen", "eject-red"));
        assert_eq!(escalation::tier(standing(&rows).rejections), Tier::Hold);
        rows.push(f("escalated", "hold rejections=3"));
        assert_eq!(standing(&rows), Standing::default());
    }
}
