//! The batch machine (design §3.1.3): queue mode only, keyed by `batch_id`. It replaces the
//! `open`, `attributing-*`, `bisect` and `unreproduced/` files, and emits its members'
//! delivery exits as a side effect of its own state transitions — the cascade a caller
//! assembles is: apply this event to the batch row, then apply the matching exit event to
//! every affected member's delivery row, all in one transaction.

use crate::{Outcome, Refusal, Version};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BatchState {
    Open,
    CiRunning,
    Green,
    Attributing,
    Rebuilding,
    Landed,
    Settled,
    Abandoned,
}

impl BatchState {
    pub fn is_terminal(self) -> bool {
        matches!(self, BatchState::Landed | BatchState::Settled | BatchState::Abandoned)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            BatchState::Open => "OPEN",
            BatchState::CiRunning => "CI_RUNNING",
            BatchState::Green => "GREEN",
            BatchState::Attributing => "ATTRIBUTING",
            BatchState::Rebuilding => "REBUILDING",
            BatchState::Landed => "LANDED",
            BatchState::Settled => "SETTLED",
            BatchState::Abandoned => "ABANDONED",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "OPEN" => BatchState::Open,
            "CI_RUNNING" => BatchState::CiRunning,
            "GREEN" => BatchState::Green,
            "ATTRIBUTING" => BatchState::Attributing,
            "REBUILDING" => BatchState::Rebuilding,
            "LANDED" => BatchState::Landed,
            "SETTLED" => BatchState::Settled,
            "ABANDONED" => BatchState::Abandoned,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BatchRow {
    pub batch_id: String,
    pub repo: String,
    pub state: BatchState,
    pub parent: Option<String>,
    pub head: Option<String>,
    pub base: Option<String>,
    pub run: Option<String>,
    pub reason: Option<String>,
    pub version: Version,
}

impl BatchRow {
    /// `cut` creates the batch; there is no prior row to compare-and-swap against, so this
    /// is a constructor rather than a transition, exactly as `BeadRow::filed` is.
    pub fn cut(batch_id: impl Into<String>, repo: impl Into<String>, head: impl Into<String>, base: impl Into<String>) -> Self {
        BatchRow {
            batch_id: batch_id.into(),
            repo: repo.into(),
            state: BatchState::Open,
            parent: None,
            head: Some(head.into()),
            base: Some(base.into()),
            run: None,
            reason: None,
            version: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BatchEventKind {
    /// Valid only while OPEN. Membership itself lives in the `batch_member` table; this
    /// event only proves the batch was still accepting members when it was added.
    MemberAdded { bead_id: String, tip: String },
    CiStarted { run: String },
    Green,
    Red,
    BaseMoved,
    Rebuilt,
    FastForward { sha: String },
    /// Valid only while ATTRIBUTING. Per-member bookkeeping; does not itself move the batch.
    Attributed { bead_id: String, outcome: String },
    Settle,
    Abandon { reason: String },
    /// A manual, operator/czar-initiated single-member eject, valid while OPEN, CI_RUNNING or
    /// GREEN (which returns to OPEN: the head changed) — before any Red event exists. Distinct from the CI-driven
    /// Red->Attributing->Settle path: that path's per-member exit is logged only once CI
    /// has actually run, so a live-batch eject cannot reuse it without fabricating a Red
    /// that never happened (design: the event log is the record of what happened).
    /// Per-member bookkeeping; does not itself move the batch.
    Eject { bead_id: String, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BatchEvent {
    pub expect: BatchState,
    pub version: Version,
    pub kind: BatchEventKind,
    pub actor: String,
}

fn illegal(row: &BatchRow, kind: &BatchEventKind) -> Outcome<BatchRow> {
    Outcome::refuse(
        row.clone(),
        Refusal::IllegalTransition { state: row.state.as_str().to_string(), event: format!("{kind:?}") },
    )
}

fn terminal(row: &BatchRow) -> Outcome<BatchRow> {
    Outcome::refuse(row.clone(), Refusal::Terminal { state: row.state.as_str().to_string() })
}

/// Apply one event to one batch row. Exhaustive over `(BatchState, BatchEventKind)`; no
/// wildcard arm.
pub fn apply(row: &BatchRow, ev: &BatchEvent) -> Outcome<BatchRow> {
    if ev.version != row.version {
        return Outcome::refuse(row.clone(), Refusal::StaleVersion { given: ev.version, current: row.version });
    }
    if ev.expect != row.state {
        return Outcome::refuse(
            row.clone(),
            Refusal::ExpectMismatch { expected: ev.expect.as_str().to_string(), actual: row.state.as_str().to_string() },
        );
    }

    match &ev.kind {
        // Orthogonal: valid from any non-terminal state.
        BatchEventKind::Abandon { reason } => {
            if row.state.is_terminal() {
                return terminal(row);
            }
            let mut new = row.clone();
            new.state = BatchState::Abandoned;
            new.reason = Some(reason.clone());
            new.version += 1;
            Outcome::applied(new)
        }

        BatchEventKind::MemberAdded { .. }
        | BatchEventKind::CiStarted { .. }
        | BatchEventKind::Green
        | BatchEventKind::Red
        | BatchEventKind::BaseMoved
        | BatchEventKind::Rebuilt
        | BatchEventKind::FastForward { .. }
        | BatchEventKind::Attributed { .. }
        | BatchEventKind::Settle
        | BatchEventKind::Eject { .. } => primary_transition(row, &ev.kind),
    }
}

fn primary_transition(row: &BatchRow, kind: &BatchEventKind) -> Outcome<BatchRow> {
    // Deliberately not `use BatchState::*`: `BatchState::Green` and `BatchEventKind::Green`
    // share a name, and importing both makes a bare `Green` pattern an ambiguous binding
    // instead of a match on the event variant. States are qualified below instead.
    use BatchEventKind::*;

    match row.state {
        BatchState::Open => match kind {
            MemberAdded { .. } => {
                let mut new = row.clone();
                new.version += 1;
                Outcome::applied(new)
            }
            CiStarted { run } => {
                let mut new = row.clone();
                new.state = BatchState::CiRunning;
                new.run = Some(run.clone());
                new.version += 1;
                Outcome::applied(new)
            }
            Eject { .. } => {
                let mut new = row.clone();
                new.version += 1;
                Outcome::applied(new)
            }
            Green | Red | BaseMoved | Rebuilt | FastForward { .. } | Attributed { .. } | Settle | Abandon { .. } => illegal(row, kind),
        },

        BatchState::CiRunning => match kind {
            Green => {
                let mut new = row.clone();
                new.state = BatchState::Green;
                new.version += 1;
                Outcome::applied(new)
            }
            Red => {
                let mut new = row.clone();
                new.state = BatchState::Attributing;
                new.version += 1;
                Outcome::applied(new)
            }
            Eject { .. } => {
                let mut new = row.clone();
                new.version += 1;
                Outcome::applied(new)
            }
            MemberAdded { .. } | CiStarted { .. } | BaseMoved | Rebuilt | FastForward { .. } | Attributed { .. }
            | Settle | Abandon { .. } => illegal(row, kind),
        },

        BatchState::Green => match kind {
            FastForward { sha } => {
                let mut new = row.clone();
                new.state = BatchState::Landed;
                new.reason = Some(sha.clone());
                new.version += 1;
                Outcome::applied(new)
            }
            BaseMoved => {
                let mut new = row.clone();
                new.state = BatchState::Rebuilding;
                new.version += 1;
                Outcome::applied(new)
            }
            Eject { .. } => {
                let mut new = row.clone();
                new.state = BatchState::Open;
                new.version += 1;
                Outcome::applied(new)
            }
            MemberAdded { .. } | CiStarted { .. } | Green | Red | Rebuilt | Attributed { .. } | Settle | Abandon { .. } => illegal(row, kind),
        },

        BatchState::Rebuilding => match kind {
            Rebuilt => {
                let mut new = row.clone();
                new.state = BatchState::CiRunning;
                new.version += 1;
                Outcome::applied(new)
            }
            MemberAdded { .. } | CiStarted { .. } | Green | Red | BaseMoved | FastForward { .. }
            | Attributed { .. } | Settle | Abandon { .. } | Eject { .. } => illegal(row, kind),
        },

        BatchState::Attributing => match kind {
            Attributed { .. } => {
                let mut new = row.clone();
                new.version += 1;
                Outcome::applied(new)
            }
            Settle => {
                let mut new = row.clone();
                new.state = BatchState::Settled;
                new.version += 1;
                Outcome::applied(new)
            }
            MemberAdded { .. } | CiStarted { .. } | Green | Red | BaseMoved | Rebuilt | FastForward { .. } | Abandon { .. }
            | Eject { .. } => illegal(row, kind),
        },

        BatchState::Landed | BatchState::Settled | BatchState::Abandoned => match kind {
            MemberAdded { .. } | CiStarted { .. } | Green | Red | BaseMoved | Rebuilt | FastForward { .. }
            | Attributed { .. } | Settle | Abandon { .. } | Eject { .. } => terminal(row),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_STATES: [BatchState; 8] = [
        BatchState::Open,
        BatchState::CiRunning,
        BatchState::Green,
        BatchState::Attributing,
        BatchState::Rebuilding,
        BatchState::Landed,
        BatchState::Settled,
        BatchState::Abandoned,
    ];

    fn kinds() -> Vec<BatchEventKind> {
        vec![
            BatchEventKind::MemberAdded { bead_id: "sp-1".into(), tip: "t1".into() },
            BatchEventKind::CiStarted { run: "run1".into() },
            BatchEventKind::Green,
            BatchEventKind::Red,
            BatchEventKind::BaseMoved,
            BatchEventKind::Rebuilt,
            BatchEventKind::FastForward { sha: "sha1".into() },
            BatchEventKind::Attributed { bead_id: "sp-1".into(), outcome: "requeue".into() },
            BatchEventKind::Settle,
            BatchEventKind::Abandon { reason: "r".into() },
            BatchEventKind::Eject { bead_id: "sp-1".into(), reason: "manual".into() },
        ]
    }

    fn ev(expect: BatchState, version: Version, kind: BatchEventKind) -> BatchEvent {
        BatchEvent { expect, version, kind, actor: "test".into() }
    }

    fn row(state: BatchState) -> BatchRow {
        let mut r = BatchRow::cut("b1", "spira", "head1", "base1");
        r.state = state;
        r
    }

    #[test]
    fn every_state_event_pair_has_an_explicit_outcome() {
        for &state in &ALL_STATES {
            for kind in kinds() {
                let r = row(state);
                let out = apply(&r, &ev(state, r.version, kind));
                if out.applied {
                    assert_eq!(out.row.version, r.version + 1);
                } else {
                    assert_eq!(out.row, r);
                }
            }
        }
    }

    #[test]
    fn terminal_states_absorb_everything_but_abandon_is_also_refused() {
        for &state in &[BatchState::Landed, BatchState::Settled, BatchState::Abandoned] {
            for kind in kinds() {
                let r = row(state);
                let out = apply(&r, &ev(state, 0, kind));
                assert!(!out.applied, "{state:?} must be terminal");
                assert_eq!(out.row, r);
            }
        }
    }

    #[test]
    fn full_green_path_to_landed() {
        let r = row(BatchState::Open);
        let out = apply(&r, &ev(BatchState::Open, 0, BatchEventKind::CiStarted { run: "r1".into() }));
        assert!(out.applied);
        let r = out.row;
        let out = apply(&r, &ev(BatchState::CiRunning, r.version, BatchEventKind::Green));
        assert!(out.applied);
        let r = out.row;
        let out = apply(&r, &ev(BatchState::Green, r.version, BatchEventKind::FastForward { sha: "sha1".into() }));
        assert!(out.applied);
        assert_eq!(out.row.state, BatchState::Landed);
    }

    #[test]
    fn red_path_to_settled_via_attributing() {
        let r = row(BatchState::CiRunning);
        let out = apply(&r, &ev(BatchState::CiRunning, r.version, BatchEventKind::Red));
        assert!(out.applied);
        assert_eq!(out.row.state, BatchState::Attributing);
        let r = out.row;
        let out = apply(&r, &ev(BatchState::Attributing, r.version, BatchEventKind::Attributed { bead_id: "sp-1".into(), outcome: "eject".into() }));
        assert!(out.applied);
        assert_eq!(out.row.state, BatchState::Attributing);
        let r = out.row;
        let out = apply(&r, &ev(BatchState::Attributing, r.version, BatchEventKind::Settle));
        assert!(out.applied);
        assert_eq!(out.row.state, BatchState::Settled);
    }

    #[test]
    fn base_moved_rebuilds_then_returns_to_ci_running() {
        let r = row(BatchState::Green);
        let out = apply(&r, &ev(BatchState::Green, r.version, BatchEventKind::BaseMoved));
        assert!(out.applied);
        assert_eq!(out.row.state, BatchState::Rebuilding);
        let r = out.row;
        let out = apply(&r, &ev(BatchState::Rebuilding, r.version, BatchEventKind::Rebuilt));
        assert!(out.applied);
        assert_eq!(out.row.state, BatchState::CiRunning);
    }

    #[test]
    fn abandon_available_from_every_non_terminal_state() {
        for &state in &[BatchState::Open, BatchState::CiRunning, BatchState::Green, BatchState::Rebuilding, BatchState::Attributing] {
            let r = row(state);
            let out = apply(&r, &ev(state, 0, BatchEventKind::Abandon { reason: "gave up".into() }));
            assert!(out.applied, "{state:?} should accept abandon");
            assert_eq!(out.row.state, BatchState::Abandoned);
        }
    }

    #[test]
    fn eject_legal_only_from_open_and_ci_running() {
        for &state in &[BatchState::Open, BatchState::CiRunning] {
            let r = row(state);
            let out = apply(&r, &ev(state, 0, BatchEventKind::Eject { bead_id: "sp-1".into(), reason: "manual".into() }));
            assert!(out.applied, "{state:?} should accept eject");
            assert_eq!(out.row.state, state, "eject must not move the batch");
            assert_eq!(out.row.version, r.version + 1);
        }
    }

    #[test]
    fn eject_from_green_voids_the_certification_and_reopens_the_batch() {
        let r = row(BatchState::Green);
        let out = apply(&r, &ev(BatchState::Green, 0, BatchEventKind::Eject { bead_id: "sp-1".into(), reason: "manual".into() }));
        assert!(out.applied);
        assert_eq!(out.row.state, BatchState::Open);
        assert_eq!(out.row.version, r.version + 1);
        let out = apply(&out.row, &ev(BatchState::Open, 1, BatchEventKind::CiStarted { run: "r".into() }));
        let out = apply(&out.row, &ev(BatchState::CiRunning, 2, BatchEventKind::Green));
        let out = apply(&out.row, &ev(BatchState::Green, 3, BatchEventKind::FastForward { sha: "abc".into() }));
        assert!(out.applied);
        assert_eq!(out.row.state, BatchState::Landed);
    }

    #[test]
    fn eject_refused_outside_open_and_ci_running() {
        for &state in &[BatchState::Rebuilding, BatchState::Attributing, BatchState::Landed, BatchState::Settled, BatchState::Abandoned] {
            let r = row(state);
            let out = apply(&r, &ev(state, 0, BatchEventKind::Eject { bead_id: "sp-1".into(), reason: "manual".into() }));
            assert!(!out.applied, "{state:?} must refuse eject");
            assert_eq!(out.row, r);
        }
    }

    #[test]
    fn expect_mismatch_never_mutates() {
        let r = row(BatchState::Open);
        let out = apply(&r, &ev(BatchState::CiRunning, 0, BatchEventKind::Green));
        assert!(!out.applied);
        assert_eq!(out.row, r);
        assert!(matches!(out.refusal, Some(Refusal::ExpectMismatch { .. })));
    }

    #[test]
    fn stale_version_never_mutates() {
        let mut r = row(BatchState::Open);
        r.version = 3;
        let out = apply(&r, &ev(BatchState::Open, 2, BatchEventKind::CiStarted { run: "r".into() }));
        assert!(!out.applied);
        assert_eq!(out.row, r);
        assert!(matches!(out.refusal, Some(Refusal::StaleVersion { .. })));
    }
}
