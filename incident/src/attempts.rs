//! Reading the alarm-attempt family: which alarms are still undelivered, and the day's counts.
//! An alarm is its reference; its latest attempt says whether it reached anyone.

use std::collections::BTreeMap;

use tsd::alarm::{Attempt, Outcome};

use crate::run::FileOutcome;

/// How far back an undelivered alarm is still owed: past the intents' TTL nothing retries it.
pub const LOOKBACK_S: u64 = 48 * 3600;
pub const DAY_S: u64 = 86_400;

/// An `incident file` filing as an attempt. A filing that names no bead is a failure, whatever
/// the intake said; activation skew files nothing by design and is no attempt.
pub fn of_file(outcome: &FileOutcome) -> Option<Outcome> {
    Some(match outcome {
        FileOutcome::Filed(id) if !id.trim().is_empty() => Outcome::Delivered { id: id.clone() },
        FileOutcome::Filed(_) => Outcome::Failed { reason: "the filing returned no bead id".into() },
        FileOutcome::Unreachable => Outcome::Failed { reason: "the beads store could not be reached; the event stays spooled".into() },
        FileOutcome::Refused => Outcome::Failed { reason: "the filing was refused by its guards; the event stays spooled".into() },
        FileOutcome::Skew => return None,
    })
}

/// Each alarm whose latest attempt in the lookback failed, the longest untried first.
pub fn undelivered(attempts: &[Attempt], now: u64) -> Vec<&Attempt> {
    let mut latest: BTreeMap<&str, &Attempt> = BTreeMap::new();
    for a in attempts.iter().filter(|a| now.saturating_sub(a.at) <= LOOKBACK_S) {
        if latest.get(a.reference.as_str()).is_none_or(|l| l.at <= a.at) {
            latest.insert(&a.reference, a);
        }
    }
    let mut out: Vec<&Attempt> = latest.into_values().filter(|a| matches!(a.outcome, Outcome::Failed { .. })).collect();
    out.sort_by_key(|a| a.at);
    out
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub attempts: u64,
    pub delivered: u64,
    pub failed: u64,
    pub pages: u64,
}

/// The last day's attempts, deliveries (either path), failures, and pages that reached someone.
pub fn counts(attempts: &[Attempt], now: u64, page_kind: &str) -> Counts {
    let mut c = Counts::default();
    for a in attempts.iter().filter(|a| now.saturating_sub(a.at) < DAY_S) {
        c.attempts += 1;
        match a.outcome {
            Outcome::Failed { .. } => c.failed += 1,
            _ => {
                c.delivered += 1;
                if a.kind == page_kind {
                    c.pages += 1;
                }
            }
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(at: u64, reference: &str, outcome: Outcome) -> Attempt {
        Attempt { at, caller: "c".into(), kind: "page".into(), reference: reference.into(), outcome, message: None }
    }

    fn failed() -> Outcome {
        Outcome::Failed { reason: "x".into() }
    }

    #[test]
    fn an_alarm_is_undelivered_until_a_later_attempt_delivers_it() {
        let now = 10 * DAY_S;
        let rows = vec![
            at(now - 300, "a", failed()),
            at(now - 200, "b", failed()),
            at(now - 100, "a", Outcome::DeliveredFallback),
            at(now - 50, "c", failed()),
            at(now - LOOKBACK_S - 1, "old", failed()),
        ];
        let open: Vec<&str> = undelivered(&rows, now).iter().map(|a| a.reference.as_str()).collect();
        assert_eq!(open, ["b", "c"], "oldest first; a delivered and old past the lookback");
        let mut retried = rows.clone();
        retried.push(at(now - 10, "b", failed()));
        let open: Vec<&str> = undelivered(&retried, now).iter().map(|a| a.reference.as_str()).collect();
        assert_eq!(open, ["c", "b"], "a failed retry goes to the back, so one stuck alarm never blocks the rest");
        let c = counts(&rows, now, "page");
        assert_eq!(c, Counts { attempts: 4, delivered: 1, failed: 3, pages: 1 });
    }

    #[test]
    fn a_filing_with_no_id_is_a_failure_and_skew_is_no_attempt() {
        assert_eq!(of_file(&FileOutcome::Filed("sp-1".into())), Some(Outcome::Delivered { id: "sp-1".into() }));
        for o in [FileOutcome::Filed(" ".into()), FileOutcome::Unreachable, FileOutcome::Refused] {
            assert!(matches!(of_file(&o), Some(Outcome::Failed { .. })), "{o:?}");
        }
        assert_eq!(of_file(&FileOutcome::Skew), None);
    }
}
