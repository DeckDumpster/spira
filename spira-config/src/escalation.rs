//! Escalation after repeated rejection: a bead's red ejections are recorded as `rejection`
//! facts; the second makes its next claim run on the persona's stronger model with the history
//! in the brief, the third holds it for the Concierge instead of another claim.

use serde::{Deserialize, Serialize};

pub const REJECTION: &str = "rejection";
pub const ESCALATED: &str = "escalated";
pub const ESCALATE_AT: usize = 2;
pub const HOLD_AT: usize = 3;
pub const HOLD_ACTOR: &str = "escalation";

const REASON_MAX: usize = 1500;
const FIELD_MAX: usize = 300;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejection {
    pub cause: String,
    pub suites: String,
    pub tip: String,
    pub reason: String,
}

impl Rejection {
    pub fn new(cause: &str, suites: &str, tip: &str, reason: &str) -> Self {
        let cut = |s: &str, n: usize| s.chars().take(n).collect::<String>();
        Rejection { cause: cut(cause, FIELD_MAX), suites: cut(suites, FIELD_MAX), tip: cut(tip, FIELD_MAX), reason: cut(reason, REASON_MAX) }
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// The `rejection` facts' causes, oldest first; a cause that does not parse is dropped.
pub fn parse_all(causes: &[String]) -> Vec<Rejection> {
    causes.iter().filter_map(|c| serde_json::from_str(c).ok()).collect()
}

/// What `spira-claim rejections` answers: how many times the bead has been rejected and the
/// detail recorded for them.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Standing {
    pub rejections: usize,
    pub history: Vec<Rejection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Normal,
    Stronger,
    Hold,
}

pub fn tier(rejections: usize) -> Tier {
    match rejections {
        n if n >= HOLD_AT => Tier::Hold,
        n if n >= ESCALATE_AT => Tier::Stronger,
        _ => Tier::Normal,
    }
}

pub fn hold_reason(rejections: usize) -> String {
    format!("escalation-ceiling: {rejections} rejections — Concierge: mend it, split it or report it, then lift this hold")
}

/// The brief section of an escalated claim: every rejection, so the aeon starts from what went wrong.
pub fn history_brief(history: &[Rejection], model: &str) -> String {
    if history.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "\n\n## Rejection history — this claim runs on {model}\n\nThis bead was rejected {} times. Start from what went wrong, not from scratch: reproduce each failing check below at your tip before you submit, and say in your submission what you changed to make it pass.\n",
        history.len()
    );
    for (i, r) in history.iter().enumerate() {
        let shown = |v: &str| if v.is_empty() { "unrecorded".to_string() } else { v.to_string() };
        out.push_str(&format!("\n{}. cause: {}\n   failing checks: {}\n   tip: {}\n   reason: {}\n", i + 1, r.cause, shown(&r.suites), shown(&r.tip), shown(&r.reason)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_second_rejection_escalates_and_the_third_holds() {
        assert_eq!([0, 1, 2, 3, 9].map(tier), [Tier::Normal, Tier::Normal, Tier::Stronger, Tier::Hold, Tier::Hold]);
    }

    #[test]
    fn a_rejection_round_trips_and_a_foreign_cause_is_dropped() {
        let r = Rejection::new("eject-red", "test-x.sh", "abc", "line one\nline \"two\"");
        assert_eq!(parse_all(&[r.encode(), "not json".into()]), vec![r]);
    }

    #[test]
    fn the_brief_names_every_rejection_with_checks_tip_and_reason() {
        let h = vec![Rejection::new("eject-red", "test-x.sh", "t1", "five tests failed"), Rejection::new("gate-red", "", "t2", "did not compile")];
        let b = history_brief(&h, "big-model");
        for want in ["big-model", "rejected 2 times", "test-x.sh", "t1", "five tests failed", "t2", "did not compile", "unrecorded"] {
            assert!(b.contains(want), "{want}: {b}");
        }
        assert_eq!(history_brief(&[], "m"), "");
    }

    #[test]
    fn a_reason_is_bounded() {
        assert_eq!(Rejection::new("c", "s", "t", &"x".repeat(9000)).reason.len(), REASON_MAX);
    }
}
