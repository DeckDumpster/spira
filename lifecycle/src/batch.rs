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

/// Where inside a CI_RUNNING pass the round is. The test cap's clock starts at `Suites`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BatchPhase {
    Build,
    Suites,
}

impl BatchPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            BatchPhase::Build => "build",
            BatchPhase::Suites => "suites",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "build" => Some(BatchPhase::Build),
            "suites" => Some(BatchPhase::Suites),
            _ => None,
        }
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
    pub pass: u32,
    pub phase: Option<BatchPhase>,
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
            pass: 0,
            phase: None,
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
    /// OPEN or REBUILDING -> CI_RUNNING, phase build. `n` must be the next pass number.
    PassStarted { n: u32, head: String },
    /// Phase build -> suites: the test cap's clock starts here.
    SuitesStarted { n: u32 },
    /// Phase suites -> GREEN.
    PassGreen { n: u32, suites_s: u64, build_s: u64 },
    /// Any phase -> ATTRIBUTING: the pass ran and suites (or the build) failed.
    PassRed { n: u32, red_suites: Vec<String>, suites_s: u64, build_s: u64 },
    /// Any phase -> ATTRIBUTING: the pass produced no verdict (over the cap, VM lost).
    PassIncomplete { n: u32, reason: String },
    /// Any phase -> ATTRIBUTING: the pass was stopped on purpose, with `done` of `total`
    /// suites finished and `red_suites` already red.
    PassPreempted { n: u32, done: u32, total: u32, red_suites: Vec<String> },
    /// ATTRIBUTING -> OPEN for the next pass, at the head the survivors were rebuilt to.
    PassRebuilt { head: String },
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

fn where_is(row: &BatchRow) -> String {
    match row.phase {
        Some(p) => format!("{} (pass {}, phase {})", row.state.as_str(), row.pass, p.as_str()),
        None if row.pass > 0 => format!("{} (pass {})", row.state.as_str(), row.pass),
        None => row.state.as_str().to_string(),
    }
}

fn illegal(row: &BatchRow, kind: &BatchEventKind) -> Outcome<BatchRow> {
    Outcome::refuse(row.clone(), Refusal::IllegalTransition { state: where_is(row), event: format!("{kind:?}") })
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
            new.phase = None;
            new.reason = Some(reason.clone());
            new.version += 1;
            Outcome::applied(new)
        }

        BatchEventKind::MemberAdded { .. }
        | BatchEventKind::CiStarted { .. }
        | BatchEventKind::PassStarted { .. }
        | BatchEventKind::SuitesStarted { .. }
        | BatchEventKind::PassGreen { .. }
        | BatchEventKind::PassRed { .. }
        | BatchEventKind::PassIncomplete { .. }
        | BatchEventKind::PassPreempted { .. }
        | BatchEventKind::PassRebuilt { .. }
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

    let moved = |state: BatchState, phase: Option<BatchPhase>| {
        let mut new = row.clone();
        new.state = state;
        new.phase = phase;
        new.version += 1;
        new
    };
    let stays = || {
        let mut new = row.clone();
        new.version += 1;
        new
    };
    let started = |n: &u32, head: Option<&String>| {
        if *n != row.pass + 1 {
            return illegal(row, kind);
        }
        let mut new = moved(BatchState::CiRunning, Some(BatchPhase::Build));
        new.pass = *n;
        if let Some(h) = head {
            new.head = Some(h.clone());
        }
        Outcome::applied(new)
    };

    match row.state {
        BatchState::Open => match kind {
            MemberAdded { .. } | Eject { .. } => Outcome::applied(stays()),
            CiStarted { run } => {
                let mut new = moved(BatchState::CiRunning, Some(BatchPhase::Build));
                new.run = Some(run.clone());
                new.pass += 1;
                Outcome::applied(new)
            }
            PassStarted { n, head } => started(n, Some(head)),
            Green | Red | BaseMoved | Rebuilt | FastForward { .. } | Attributed { .. } | Settle | Abandon { .. } | SuitesStarted { .. }
            | PassGreen { .. } | PassRed { .. } | PassIncomplete { .. } | PassPreempted { .. } | PassRebuilt { .. } => illegal(row, kind),
        },

        BatchState::CiRunning => match kind {
            Green => Outcome::applied(moved(BatchState::Green, None)),
            Red => Outcome::applied(moved(BatchState::Attributing, None)),
            Eject { .. } => Outcome::applied(stays()),
            SuitesStarted { n } if *n == row.pass && row.phase == Some(BatchPhase::Build) => {
                Outcome::applied(moved(BatchState::CiRunning, Some(BatchPhase::Suites)))
            }
            PassGreen { n, .. } if *n == row.pass && row.phase == Some(BatchPhase::Suites) => Outcome::applied(moved(BatchState::Green, None)),
            PassRed { n, .. } | PassIncomplete { n, .. } | PassPreempted { n, .. } if *n == row.pass => Outcome::applied(moved(BatchState::Attributing, None)),
            MemberAdded { .. } | CiStarted { .. } | PassStarted { .. } | SuitesStarted { .. } | PassGreen { .. } | PassRed { .. }
            | PassIncomplete { .. } | PassPreempted { .. } | PassRebuilt { .. } | BaseMoved | Rebuilt | FastForward { .. } | Attributed { .. } | Settle
            | Abandon { .. } => illegal(row, kind),
        },

        BatchState::Green => match kind {
            FastForward { sha } => {
                let mut new = moved(BatchState::Landed, None);
                new.reason = Some(sha.clone());
                Outcome::applied(new)
            }
            BaseMoved => Outcome::applied(moved(BatchState::Rebuilding, None)),
            Eject { .. } => Outcome::applied(moved(BatchState::Open, None)),
            MemberAdded { .. } | CiStarted { .. } | PassStarted { .. } | SuitesStarted { .. } | PassGreen { .. } | PassRed { .. }
            | PassIncomplete { .. } | PassPreempted { .. } | PassRebuilt { .. } | Green | Red | Rebuilt | Attributed { .. } | Settle | Abandon { .. } => {
                illegal(row, kind)
            }
        },

        BatchState::Rebuilding => match kind {
            Rebuilt => {
                let mut new = moved(BatchState::CiRunning, Some(BatchPhase::Build));
                new.pass += 1;
                Outcome::applied(new)
            }
            PassStarted { n, head } => started(n, Some(head)),
            MemberAdded { .. } | CiStarted { .. } | SuitesStarted { .. } | PassGreen { .. } | PassRed { .. } | PassIncomplete { .. } | PassPreempted { .. }
            | PassRebuilt { .. } | Green | Red | BaseMoved | FastForward { .. } | Attributed { .. } | Settle | Abandon { .. }
            | Eject { .. } => illegal(row, kind),
        },

        BatchState::Attributing => match kind {
            Attributed { .. } | Eject { .. } => Outcome::applied(stays()),
            Settle => Outcome::applied(moved(BatchState::Settled, None)),
            PassRebuilt { head } => {
                let mut new = moved(BatchState::Open, None);
                new.head = Some(head.clone());
                Outcome::applied(new)
            }
            MemberAdded { .. } | CiStarted { .. } | PassStarted { .. } | SuitesStarted { .. } | PassGreen { .. } | PassRed { .. }
            | PassIncomplete { .. } | PassPreempted { .. } | Green | Red | BaseMoved | Rebuilt | FastForward { .. } | Abandon { .. } => illegal(row, kind),
        },

        BatchState::Landed | BatchState::Settled | BatchState::Abandoned => match kind {
            MemberAdded { .. } | CiStarted { .. } | PassStarted { .. } | SuitesStarted { .. } | PassGreen { .. } | PassRed { .. }
            | PassIncomplete { .. } | PassPreempted { .. } | PassRebuilt { .. } | Green | Red | BaseMoved | Rebuilt | FastForward { .. } | Attributed { .. }
            | Settle | Abandon { .. } | Eject { .. } => terminal(row),
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
            BatchEventKind::PassStarted { n: 1, head: "h".into() },
            BatchEventKind::SuitesStarted { n: 1 },
            BatchEventKind::PassGreen { n: 1, suites_s: 1, build_s: 1 },
            BatchEventKind::PassRed { n: 1, red_suites: vec!["test-x.sh".into()], suites_s: 1, build_s: 1 },
            BatchEventKind::PassIncomplete { n: 1, reason: "cap".into() },
            BatchEventKind::PassPreempted { n: 1, done: 3, total: 9, red_suites: vec![] },
            BatchEventKind::PassRebuilt { head: "h2".into() },
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
    fn eject_refused_outside_open_ci_running_green_and_attributing() {
        for &state in &[BatchState::Rebuilding, BatchState::Landed, BatchState::Settled, BatchState::Abandoned] {
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

    fn step(r: &BatchRow, kind: BatchEventKind) -> BatchRow {
        let out = apply(r, &ev(r.state, r.version, kind.clone()));
        assert!(out.applied, "{kind:?} refused at {:?}: {:?}", r.state, out.refusal);
        out.row
    }

    fn refused(r: &BatchRow, kind: BatchEventKind) -> String {
        let out = apply(r, &ev(r.state, r.version, kind));
        assert!(!out.applied);
        assert_eq!(out.row, *r);
        match out.refusal {
            Some(Refusal::IllegalTransition { state, .. }) => state,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_round_through_a_red_pass_an_eject_and_a_green_second_pass() {
        let r = row(BatchState::Open);
        let r = step(&r, BatchEventKind::PassStarted { n: 1, head: "h1".into() });
        assert_eq!((r.state, r.pass, r.phase), (BatchState::CiRunning, 1, Some(BatchPhase::Build)));
        let r = step(&r, BatchEventKind::SuitesStarted { n: 1 });
        assert_eq!(r.phase, Some(BatchPhase::Suites));
        let r = step(&r, BatchEventKind::PassRed { n: 1, red_suites: vec!["test-x.sh".into()], suites_s: 90, build_s: 30 });
        assert_eq!((r.state, r.phase), (BatchState::Attributing, None));
        let r = step(&r, BatchEventKind::Eject { bead_id: "sp-1".into(), reason: "red".into() });
        assert_eq!(r.state, BatchState::Attributing);
        let r = step(&r, BatchEventKind::PassRebuilt { head: "h2".into() });
        assert_eq!((r.state, r.head.as_deref(), r.pass), (BatchState::Open, Some("h2"), 1));
        let r = step(&r, BatchEventKind::PassStarted { n: 2, head: "h2".into() });
        let r = step(&r, BatchEventKind::SuitesStarted { n: 2 });
        let r = step(&r, BatchEventKind::PassGreen { n: 2, suites_s: 80, build_s: 20 });
        assert_eq!((r.state, r.pass, r.phase), (BatchState::Green, 2, None));
    }

    #[test]
    fn a_pass_killed_at_the_cap_is_incomplete_not_green() {
        let r = step(&row(BatchState::Open), BatchEventKind::PassStarted { n: 1, head: "h".into() });
        let r = step(&r, BatchEventKind::SuitesStarted { n: 1 });
        let incomplete = step(&r, BatchEventKind::PassIncomplete { n: 1, reason: "over the 900s cap".into() });
        assert_eq!(incomplete.state, BatchState::Attributing);
        let green = apply(&row(BatchState::Attributing), &ev(BatchState::Attributing, 0, BatchEventKind::PassGreen { n: 1, suites_s: 1, build_s: 1 }));
        assert!(!green.applied);
    }

    #[test]
    fn a_pass_preempted_in_any_phase_goes_to_attributing_and_only_for_the_current_pass() {
        let preempt = |n| BatchEventKind::PassPreempted { n, done: 3, total: 9, red_suites: vec!["test-x.sh".into()] };
        let building = step(&row(BatchState::Open), BatchEventKind::PassStarted { n: 1, head: "h".into() });
        assert_eq!(step(&building, preempt(1)).state, BatchState::Attributing);
        let suites = step(&building, BatchEventKind::SuitesStarted { n: 1 });
        assert_eq!(step(&suites, preempt(1)).state, BatchState::Attributing);
        refused(&suites, preempt(2));
        refused(&row(BatchState::Open), preempt(1));
    }

    #[test]
    fn pass_events_must_name_the_current_pass_and_phase() {
        let r = step(&row(BatchState::Open), BatchEventKind::PassStarted { n: 1, head: "h".into() });
        let at = refused(&r, BatchEventKind::PassGreen { n: 1, suites_s: 1, build_s: 1 });
        assert_eq!(at, "CI_RUNNING (pass 1, phase build)", "a green before the suites started is refused, naming the phase");
        refused(&r, BatchEventKind::SuitesStarted { n: 2 });
        refused(&row(BatchState::Open), BatchEventKind::PassStarted { n: 2, head: "h".into() });
        let s = step(&r, BatchEventKind::SuitesStarted { n: 1 });
        refused(&s, BatchEventKind::SuitesStarted { n: 1 });
        refused(&s, BatchEventKind::PassGreen { n: 3, suites_s: 1, build_s: 1 });
    }

    #[test]
    fn a_build_failure_is_red_from_the_build_phase() {
        let r = step(&row(BatchState::Open), BatchEventKind::PassStarted { n: 1, head: "h".into() });
        let r = step(&r, BatchEventKind::PassRed { n: 1, red_suites: vec!["workspace-build".into()], suites_s: 0, build_s: 12 });
        assert_eq!(r.state, BatchState::Attributing);
    }

    #[test]
    fn a_rebuilt_base_moved_round_starts_its_next_pass_from_rebuilding() {
        let mut r = row(BatchState::Rebuilding);
        r.pass = 1;
        let r = step(&r, BatchEventKind::PassStarted { n: 2, head: "h2".into() });
        assert_eq!((r.state, r.pass), (BatchState::CiRunning, 2));
    }
}
