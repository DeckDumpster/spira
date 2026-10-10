//! The MENDING machine: one row per (batch, pass, suite) failure, from the moment a mender is
//! picked for it to its outcome. The deadline is fixed at pickup from evidence the caller
//! supplies and no event moves it; past it the only transition is `Expire`, which the machine
//! applies as a return carrying whatever diagnosis exists.

use crate::{Outcome, Refusal, Version};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MendingState {
    Waiting,
    Mending,
    Mended,
    Environment,
    Flaky,
    Returned,
    Unsure,
}

impl MendingState {
    pub fn is_terminal(self) -> bool {
        !matches!(self, MendingState::Waiting | MendingState::Mending)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            MendingState::Waiting => "WAITING",
            MendingState::Mending => "MENDING",
            MendingState::Mended => "MENDED",
            MendingState::Environment => "ENVIRONMENT",
            MendingState::Flaky => "FLAKY",
            MendingState::Returned => "RETURNED",
            MendingState::Unsure => "UNSURE",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "WAITING" => MendingState::Waiting,
            "MENDING" => MendingState::Mending,
            "MENDED" => MendingState::Mended,
            "ENVIRONMENT" => MendingState::Environment,
            "FLAKY" => MendingState::Flaky,
            "RETURNED" => MendingState::Returned,
            "UNSURE" => MendingState::Unsure,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MendingRow {
    pub batch_id: String,
    pub pass: u32,
    pub suite: String,
    pub state: MendingState,
    pub picked_at: Option<i64>,
    pub deadline: Option<i64>,
    pub triage_ended_at: Option<i64>,
    pub diagnosis: Option<String>,
    pub version: Version,
}

impl MendingRow {
    /// Opened by the batch's `Failed` event in the same transaction; no prior row to swap.
    pub fn failed(batch_id: impl Into<String>, pass: u32, suite: impl Into<String>) -> Self {
        MendingRow {
            batch_id: batch_id.into(),
            pass,
            suite: suite.into(),
            state: MendingState::Waiting,
            picked_at: None,
            deadline: None,
            triage_ended_at: None,
            diagnosis: None,
            version: 0,
        }
    }

    pub fn key(&self) -> String {
        key(&self.batch_id, self.pass, &self.suite)
    }

    /// Only a MENDING row carries a deadline that can be overdue; WAITING has none.
    pub fn overdue(&self, now: i64) -> bool {
        self.state == MendingState::Mending && self.deadline.is_some_and(|d| now >= d)
    }
}

pub fn key(batch_id: &str, pass: u32, suite: &str) -> String {
    format!("{batch_id}#{pass}#{suite}")
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MendingEventKind {
    /// WAITING -> MENDING: a mender took the failure. The deadline is `at + deadline_s`.
    Pickup { at: i64, deadline_s: u64 },
    /// Stamps when triage ended, once, while MENDING.
    TriageEnded { at: i64 },
    /// Replaces the diagnosis written so far. Moves no state and no deadline.
    Diagnosis { text: String },
    Mended { at: i64 },
    Environment { at: i64 },
    Flaky { at: i64 },
    Unsure { at: i64 },
    /// The mender's own return, carrying its diagnosis.
    Return { at: i64, diagnosis: String },
    /// MENDING -> RETURNED once `at` has reached the deadline, with the diagnosis so far.
    Expire { at: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MendingEvent {
    pub expect: MendingState,
    pub version: Version,
    pub kind: MendingEventKind,
    pub actor: String,
}

fn illegal(row: &MendingRow, kind: &MendingEventKind) -> Outcome<MendingRow> {
    Outcome::refuse(row.clone(), Refusal::IllegalTransition { state: row.state.as_str().to_string(), event: format!("{kind:?}") })
}

pub fn apply(row: &MendingRow, ev: &MendingEvent) -> Outcome<MendingRow> {
    if ev.version != row.version {
        return Outcome::refuse(row.clone(), Refusal::StaleVersion { given: ev.version, current: row.version });
    }
    if ev.expect != row.state {
        return Outcome::refuse(
            row.clone(),
            Refusal::ExpectMismatch { expected: ev.expect.as_str().to_string(), actual: row.state.as_str().to_string() },
        );
    }
    if row.state.is_terminal() {
        return Outcome::refuse(row.clone(), Refusal::Terminal { state: row.state.as_str().to_string() });
    }
    let kind = &ev.kind;
    let mut new = row.clone();
    new.version += 1;
    let past_deadline = |at: i64| row.deadline.filter(|&d| at >= d);
    let settle = |mut new: MendingRow, state: MendingState, at: i64| match past_deadline(at) {
        Some(deadline) => Outcome::refuse(row.clone(), Refusal::DeadlinePassed { deadline, at }),
        None => {
            new.state = state;
            Outcome::applied(new)
        }
    };
    match (row.state, kind) {
        (MendingState::Waiting, MendingEventKind::Pickup { at, deadline_s }) => {
            new.state = MendingState::Mending;
            new.picked_at = Some(*at);
            new.deadline = Some(at.saturating_add(i64::try_from(*deadline_s).unwrap_or(i64::MAX)));
            Outcome::applied(new)
        }
        (MendingState::Mending, MendingEventKind::TriageEnded { at }) if row.triage_ended_at.is_none() => match past_deadline(*at) {
            Some(deadline) => Outcome::refuse(row.clone(), Refusal::DeadlinePassed { deadline, at: *at }),
            None => {
                new.triage_ended_at = Some(*at);
                Outcome::applied(new)
            }
        },
        (MendingState::Mending, MendingEventKind::Diagnosis { text }) => {
            new.diagnosis = Some(text.clone());
            Outcome::applied(new)
        }
        (MendingState::Mending, MendingEventKind::Mended { at }) => settle(new, MendingState::Mended, *at),
        (MendingState::Mending, MendingEventKind::Environment { at }) => settle(new, MendingState::Environment, *at),
        (MendingState::Mending, MendingEventKind::Flaky { at }) => settle(new, MendingState::Flaky, *at),
        (MendingState::Mending, MendingEventKind::Unsure { at }) => settle(new, MendingState::Unsure, *at),
        (MendingState::Mending, MendingEventKind::Return { at, diagnosis }) => {
            new.diagnosis = Some(diagnosis.clone());
            settle(new, MendingState::Returned, *at)
        }
        (MendingState::Mending, MendingEventKind::Expire { at }) if row.overdue(*at) => {
            new.state = MendingState::Returned;
            Outcome::applied(new)
        }
        _ => illegal(row, kind),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picked(at: i64) -> MendingRow {
        let r = MendingRow::failed("b1", 1, "test-x.sh");
        step(&r, MendingEventKind::Pickup { at, deadline_s: 300 })
    }

    fn step(r: &MendingRow, kind: MendingEventKind) -> MendingRow {
        let out = apply(r, &MendingEvent { expect: r.state, version: r.version, kind: kind.clone(), actor: "t".into() });
        assert!(out.applied, "{kind:?} refused at {:?}: {:?}", r.state, out.refusal);
        out.row
    }

    fn refusal(r: &MendingRow, kind: MendingEventKind) -> Refusal {
        let out = apply(r, &MendingEvent { expect: r.state, version: r.version, kind, actor: "t".into() });
        assert!(!out.applied);
        assert_eq!(out.row, *r);
        out.refusal.unwrap()
    }

    #[test]
    fn pickup_stamps_a_deadline_from_the_pickup_not_from_the_failure() {
        let r = picked(1000);
        assert_eq!((r.state, r.picked_at, r.deadline), (MendingState::Mending, Some(1000), Some(1300)));
        assert_eq!(MendingRow::failed("b1", 1, "s").deadline, None, "a waiting failure has no clock");
    }

    #[test]
    fn nothing_the_mender_sends_moves_the_deadline() {
        let r = picked(1000);
        let r = step(&r, MendingEventKind::TriageEnded { at: 1050 });
        let r = step(&r, MendingEventKind::Diagnosis { text: "x".into() });
        assert_eq!((r.deadline, r.triage_ended_at), (Some(1300), Some(1050)));
        assert!(matches!(refusal(&r, MendingEventKind::TriageEnded { at: 1060 }), Refusal::IllegalTransition { .. }), "triage ends once");
        assert!(matches!(refusal(&r, MendingEventKind::Pickup { at: 1200, deadline_s: 300 }), Refusal::IllegalTransition { .. }));
    }

    #[test]
    fn every_outcome_settles_a_row_before_its_deadline() {
        let outcomes = [
            (MendingEventKind::Mended { at: 1299 }, MendingState::Mended),
            (MendingEventKind::Environment { at: 1299 }, MendingState::Environment),
            (MendingEventKind::Flaky { at: 1299 }, MendingState::Flaky),
            (MendingEventKind::Unsure { at: 1299 }, MendingState::Unsure),
            (MendingEventKind::Return { at: 1299, diagnosis: "d".into() }, MendingState::Returned),
        ];
        for (kind, state) in outcomes {
            assert_eq!(step(&picked(1000), kind).state, state);
        }
        let r = step(&picked(1000), MendingEventKind::Return { at: 1299, diagnosis: "d".into() });
        assert_eq!(r.diagnosis.as_deref(), Some("d"));
    }

    #[test]
    fn an_outcome_at_or_past_the_deadline_is_refused() {
        for kind in [
            MendingEventKind::Mended { at: 1300 },
            MendingEventKind::Environment { at: 1300 },
            MendingEventKind::Flaky { at: 1300 },
            MendingEventKind::Unsure { at: 1300 },
            MendingEventKind::Return { at: 1300, diagnosis: "late".into() },
            MendingEventKind::TriageEnded { at: 1300 },
        ] {
            assert_eq!(refusal(&picked(1000), kind), Refusal::DeadlinePassed { deadline: 1300, at: 1300 });
        }
    }

    #[test]
    fn expire_returns_with_the_diagnosis_written_so_far_and_only_when_overdue() {
        let r = step(&picked(1000), MendingEventKind::Diagnosis { text: "half a diagnosis".into() });
        assert!(matches!(refusal(&r, MendingEventKind::Expire { at: 1299 }), Refusal::IllegalTransition { .. }));
        assert!(!r.overdue(1299) && r.overdue(1300));
        let r = step(&r, MendingEventKind::Expire { at: 1300 });
        assert_eq!((r.state, r.diagnosis.as_deref()), (MendingState::Returned, Some("half a diagnosis")));
        assert!(matches!(refusal(&r, MendingEventKind::Expire { at: 9999 }), Refusal::Terminal { .. }));
    }

    #[test]
    fn a_waiting_row_never_expires_and_has_no_outcome() {
        let r = MendingRow::failed("b1", 1, "s");
        assert!(!r.overdue(i64::MAX));
        for kind in [MendingEventKind::Expire { at: i64::MAX }, MendingEventKind::Mended { at: 1 }, MendingEventKind::Diagnosis { text: "x".into() }] {
            assert!(matches!(refusal(&r, kind), Refusal::IllegalTransition { .. }));
        }
    }

    #[test]
    fn stale_version_and_expect_mismatch_never_mutate() {
        let r = picked(1000);
        let out = apply(&r, &MendingEvent { expect: r.state, version: 0, kind: MendingEventKind::Expire { at: 5000 }, actor: "t".into() });
        assert!(matches!(out.refusal, Some(Refusal::StaleVersion { .. })) && out.row == r);
        let out = apply(&r, &MendingEvent { expect: MendingState::Waiting, version: r.version, kind: MendingEventKind::Expire { at: 5000 }, actor: "t".into() });
        assert!(matches!(out.refusal, Some(Refusal::ExpectMismatch { .. })) && out.row == r);
    }
}
