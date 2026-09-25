//! The alert path's pure half: when a [`crate::core::Verdict`] should raise a fresh wake to
//! the Concierge (once per gap streak, never once per pass), what that wake says, and which
//! three classes an operator escalation is allowed to name. The IO half — actually sending
//! mail, checking whether the Concierge is running — is a caller's job (sp-fufyb); nothing
//! here reads a clock, a file or a socket.

use crate::core::{RawStatus, Verdict};

/// Decides whether this pass should raise a fresh alert for one invariant, given the
/// `since` it was last alerted for (`None` if never alerted, or if its last streak already
/// closed). Returns `(fire, next_alerted_since)` — the caller persists `next_alerted_since`
/// and passes it back in as `prev_alerted_since` on the following pass.
///
/// DEDUPED PER GAP, NOT PER PASS: a streak is identified by `verdict.since`, so a gap that
/// persists across many passes alerts once, on the pass it first crosses grace; once it
/// closes (`is_gap` false clears the record) a *new* streak — a new `since` — alerts again.
pub fn should_alert(verdict: &Verdict, prev_alerted_since: Option<u64>) -> (bool, Option<u64>) {
    if !verdict.is_gap {
        return (false, None);
    }
    if prev_alerted_since == verdict.since {
        return (false, prev_alerted_since);
    }
    (true, verdict.since)
}

/// The evidence-carrying body a wake to the Concierge must always include: the invariant,
/// desired vs observed, how long it has been true, and the last remedy tried (if any).
pub fn compose_alert(invariant: &str, now: u64, verdict: &Verdict, last_remedy: Option<&str>) -> String {
    let (desired, observed) = match &verdict.status {
        RawStatus::Satisfied => ("satisfied".to_string(), "satisfied".to_string()),
        RawStatus::Gap { desired, observed, .. } => (desired.clone(), observed.clone()),
        RawStatus::Unobservable { reason } => ("(unobservable)".to_string(), reason.clone()),
    };
    let since_desc = match verdict.since {
        Some(s) => format!("{}s ago", now.saturating_sub(s)),
        None => "unknown".to_string(),
    };
    let remedy_desc = match last_remedy {
        Some(r) if verdict.remedy_failed => format!("{} — did not close the gap", r),
        Some(r) => r.to_string(),
        None => "none attempted".to_string(),
    };
    format!(
        "invariant: {invariant}\ndesired:   {desired}\nobserved:  {observed}\nsince:     {since_desc}\nlast remedy tried: {remedy_desc}\n"
    )
}

/// The three classes an operator escalation may name (law-escalate-decisions-not-policy,
/// amended 2026-09-25 to add DESTRUCTIVE). Anything else is refused by construction —
/// [`classify_escalation`] returns `None` and the caller routes the ask back to the
/// Concierge instead of the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationClass {
    Permissions,
    Policy,
    Destructive,
}

pub fn classify_escalation(label: &str) -> Option<EscalationClass> {
    match label.trim().to_ascii_lowercase().as_str() {
        "permissions" => Some(EscalationClass::Permissions),
        "policy" => Some(EscalationClass::Policy),
        "destructive" => Some(EscalationClass::Destructive),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{step, HysteresisState};

    fn gap_verdict(now: u64, grace: u64, prev: HysteresisState) -> (Verdict, HysteresisState) {
        let raw = RawStatus::Gap { desired: "8 aeons".into(), observed: "3 aeons".into(), since_hint: None };
        step(now, raw, grace, prev)
    }

    // law-absence-needs-a-positive-control: a gap inside its grace period raises nothing.
    #[test]
    fn a_gap_inside_grace_does_not_alert() {
        let (v, _) = gap_verdict(0, 60, HysteresisState::default());
        assert!(!v.is_gap, "fixture must actually be inside grace");
        let (fire, next) = should_alert(&v, None);
        assert!(!fire);
        assert_eq!(next, None);
    }

    #[test]
    fn a_gap_past_grace_alerts_once() {
        let (_, st1) = gap_verdict(0, 0, HysteresisState::default());
        let (v2, _) = gap_verdict(10, 0, st1);
        assert!(v2.is_gap);
        let (fire, since) = should_alert(&v2, None);
        assert!(fire, "first pass past grace must alert");
        assert_eq!(since, v2.since);
    }

    #[test]
    fn the_same_streak_does_not_alert_twice() {
        let (_, st1) = gap_verdict(0, 0, HysteresisState::default());
        let (v2, st2) = gap_verdict(10, 0, st1);
        let (_, alerted_since) = should_alert(&v2, None);

        // Next pass, same unresolved streak: same `since`, must not re-fire.
        let (v3, _) = gap_verdict(20, 0, st2);
        assert_eq!(v3.since, v2.since, "fixture must still be the same streak");
        let (fire, since) = should_alert(&v3, alerted_since);
        assert!(!fire, "an ongoing streak must not re-alert every pass");
        assert_eq!(since, alerted_since);
    }

    #[test]
    fn a_closed_then_reopened_gap_alerts_again() {
        let (_, st1) = gap_verdict(0, 0, HysteresisState::default());
        let (v2, st2) = gap_verdict(10, 0, st1);
        let (_, alerted_since) = should_alert(&v2, None);

        // Streak closes: one clean satisfied pass.
        let (closed, st3) = step(20, RawStatus::Satisfied, 0, st2);
        assert!(closed.just_closed);
        let (fire, since) = should_alert(&closed, alerted_since);
        assert!(!fire, "satisfied never alerts");
        assert_eq!(since, None, "a closed streak clears the dedup record");

        // A brand-new streak — different `since` — must alert again, not be suppressed by
        // the old record.
        let (v4, _) = gap_verdict(30, 0, st3);
        let (fire2, since2) = should_alert(&v4, since);
        assert!(fire2, "a fresh streak after a close must alert");
        assert_eq!(since2, v4.since);
    }

    // An unreadable input is never satisfied, and never silently skipped either — it drives
    // the exact same alert path as a `Gap` once its own grace period has passed.
    #[test]
    fn unobservable_past_grace_alerts_like_a_gap() {
        let raw = RawStatus::Unobservable { reason: "forge call failed".into() };
        let (_, st1) = step(0, raw.clone(), 0, HysteresisState::default());
        let (v2, _) = step(10, raw, 0, st1);
        assert!(v2.is_gap);
        let (fire, _) = should_alert(&v2, None);
        assert!(fire);
    }

    #[test]
    fn compose_alert_carries_all_four_fields() {
        let (_, st1) = gap_verdict(0, 0, HysteresisState::default());
        let (v2, _) = gap_verdict(100, 0, st1);
        let body = compose_alert("fleet-size", 100, &v2, Some("scaled aeons to 8"));
        assert!(body.contains("invariant: fleet-size"));
        assert!(body.contains("desired:   8 aeons"));
        assert!(body.contains("observed:  3 aeons"));
        assert!(body.contains("since:     100s ago"));
        assert!(body.contains("last remedy tried: scaled aeons to 8"));
    }

    #[test]
    fn compose_alert_names_a_failed_remedy_as_failed() {
        let raw = RawStatus::Gap { desired: "d".into(), observed: "o".into(), since_hint: None };
        let (_, mut st1) = step(0, raw.clone(), 0, HysteresisState::default());
        crate::core::record_remedy(&mut st1, 0, "restarted the unit");
        let (v2, _) = step(10, raw, 0, st1);
        assert!(v2.remedy_failed);
        let body = compose_alert("loop-stalled", 10, &v2, Some("restarted the unit"));
        assert!(body.contains("restarted the unit — did not close the gap"));
    }

    #[test]
    fn compose_alert_with_no_remedy_says_so() {
        let (_, st1) = gap_verdict(0, 0, HysteresisState::default());
        let (v2, _) = gap_verdict(10, 0, st1);
        let body = compose_alert("x", 10, &v2, None);
        assert!(body.contains("last remedy tried: none attempted"));
    }

    #[test]
    fn compose_alert_reports_unobservable_reason_as_observed() {
        let raw = RawStatus::Unobservable { reason: "queue-watch: no reading".into() };
        let (_, st1) = step(0, raw.clone(), 0, HysteresisState::default());
        let (v2, _) = step(10, raw, 0, st1);
        let body = compose_alert("queue-watch", 10, &v2, None);
        assert!(body.contains("desired:   (unobservable)"));
        assert!(body.contains("observed:  queue-watch: no reading"));
    }

    #[test]
    fn classify_escalation_accepts_exactly_the_three_named_classes() {
        assert_eq!(classify_escalation("permissions"), Some(EscalationClass::Permissions));
        assert_eq!(classify_escalation("policy"), Some(EscalationClass::Policy));
        assert_eq!(classify_escalation("destructive"), Some(EscalationClass::Destructive));
    }

    #[test]
    fn classify_escalation_is_case_and_whitespace_insensitive() {
        assert_eq!(classify_escalation(" Permissions \n"), Some(EscalationClass::Permissions));
        assert_eq!(classify_escalation("POLICY"), Some(EscalationClass::Policy));
    }

    // law-a-control-that-cannot-check-must-refuse, applied to the operator path itself:
    // anything not one of the three named classes is refused by construction, not passed
    // through on a best-effort guess.
    #[test]
    fn classify_escalation_refuses_everything_else() {
        for label in ["urgent", "queue-stall", "", "permission", "destructive action", "tech-debt"] {
            assert_eq!(classify_escalation(label), None, "must refuse: {label}");
        }
    }
}
