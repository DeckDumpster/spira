//! A bead's rejection standing, read from its facts: the `reopen` causes every path writes,
//! the `rejection` detail where one was recorded, and the `escalated` holds that restart it.

use spira_config::escalation::{self, Standing};

use crate::events::{classify, EventRow, ReturnClass};

/// Rows oldest first. A rejection is a judged `reopen` cause (a red ejection or rework);
/// harness faults and sequencing ejects classify otherwise and do not count. One attempt is
/// rejected once: further judged reopens before the next claim (a batch closing on the red the
/// Concierge also ejected) are the same rejection. Only rejections of attempts that ran on the
/// stronger model count toward the ceiling. A hold the ceiling recorded starts the count again.
pub fn standing(rows: &[EventRow]) -> Standing {
    let mut st = Standing::default();
    let (mut counted, mut on_stronger) = (false, false);
    for r in rows {
        let cause = r.new_value.clone().unwrap_or_default();
        match r.event_type.as_str() {
            "claimed" => (counted, on_stronger) = (false, false),
            "status_changed" if cause.contains("in_progress") => (counted, on_stronger) = (false, false),
            "reopen" if classify(&cause) == ReturnClass::Judged && !counted => {
                counted = true;
                st.rejections += 1;
                st.stronger_rejections += usize::from(on_stronger);
            }
            escalation::REJECTION => st.history.extend(escalation::parse_all(&[cause])),
            escalation::ESCALATED if cause.starts_with("hold") => (st, counted, on_stronger) = (Standing::default(), false, false),
            escalation::ESCALATED if cause.starts_with("model=") => on_stronger = true,
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
        assert_eq!((st.rejections, escalation::tier(&st)), (1, Tier::Normal));
    }

    #[test]
    fn two_rejections_escalate_and_a_recorded_hold_restarts_the_count() {
        let mut rows = vec![f("claimed", ""), f("reopen", "eject-red"), f("claimed", ""), f("reopen", "gate-red")];
        rows.push(f("rejection", &Rejection::new("gate-red", "test-x.sh", "t1", "boom").encode()));
        let st = standing(&rows);
        assert_eq!(escalation::tier(&st), Tier::Stronger);
        assert_eq!(st.history.len(), 1);
        rows.push(f("escalated", "hold rejections=3"));
        assert_eq!(standing(&rows), Standing::default());
    }

    #[test]
    fn two_plain_rejections_and_one_stronger_do_not_hold_and_the_next_claim_stays_stronger() {
        let rows = [f("claimed", ""), f("reopen", "eject-red"), f("claimed", ""), f("reopen", "eject-red"), f("claimed", ""), f("escalated", "model=big rejections=2"), f("reopen", "gate-red")];
        let st = standing(&rows);
        assert_eq!((st.rejections, st.stronger_rejections, escalation::tier(&st)), (3, 1, Tier::Stronger));
    }

    #[test]
    fn three_rejections_on_the_stronger_model_hold() {
        let mut rows = vec![f("claimed", ""), f("reopen", "eject-red"), f("claimed", ""), f("reopen", "eject-red")];
        for _ in 0..3 {
            rows.extend([f("claimed", ""), f("escalated", "model=big rejections=2"), f("reopen", "gate-red")]);
        }
        let st = standing(&rows);
        assert_eq!((st.stronger_rejections, escalation::tier(&st)), (3, Tier::Hold));
    }

    #[test]
    fn an_eject_and_a_batch_close_of_the_same_red_count_once() {
        let rows = [f("claimed", ""), f("reopen", "eject-red"), f("reopen", "gate-red")];
        assert_eq!(standing(&rows).rejections, 1);
    }
}
