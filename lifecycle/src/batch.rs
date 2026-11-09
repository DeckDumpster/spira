//! The batch machine (design §3.1.3): queue mode only, keyed by `batch_id`. It replaces the
//! `open`, `attributing-*`, `bisect` and `unreproduced/` files, and emits its members'
//! delivery exits as a side effect of its own state transitions — the cascade a caller
//! assembles is: apply this event to the batch row, then apply the matching exit event to
//! every affected member's delivery row, all in one transaction.

use crate::{Outcome, Refusal, Version};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum BatchState {
    /// Assembled behind a running round and not yet cut: its members are untouched.
    Staged,
    Open,
    CiRunning,
    Green,
    /// Green and being landed: the ref has not moved yet.
    Landing,
    /// The ref moved and the members are marked; the release is being published.
    Deploying,
    Attributing,
    Rebuilding,
    Landed,
    Settled,
    Abandoned,
    Discarded,
}

impl BatchState {
    pub fn is_terminal(self) -> bool {
        matches!(self, BatchState::Landed | BatchState::Settled | BatchState::Abandoned | BatchState::Discarded)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            BatchState::Staged => "STAGED",
            BatchState::Open => "OPEN",
            BatchState::CiRunning => "CI_RUNNING",
            BatchState::Green => "GREEN",
            BatchState::Landing => "LANDING",
            BatchState::Deploying => "DEPLOYING",
            BatchState::Attributing => "ATTRIBUTING",
            BatchState::Rebuilding => "REBUILDING",
            BatchState::Landed => "LANDED",
            BatchState::Settled => "SETTLED",
            BatchState::Abandoned => "ABANDONED",
            BatchState::Discarded => "DISCARDED",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "STAGED" => BatchState::Staged,
            "OPEN" => BatchState::Open,
            "CI_RUNNING" => BatchState::CiRunning,
            "GREEN" => BatchState::Green,
            "LANDING" => BatchState::Landing,
            "DEPLOYING" => BatchState::Deploying,
            "ATTRIBUTING" => BatchState::Attributing,
            "REBUILDING" => BatchState::Rebuilding,
            "LANDED" => BatchState::Landed,
            "SETTLED" => BatchState::Settled,
            "ABANDONED" => BatchState::Abandoned,
            "DISCARDED" => BatchState::Discarded,
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

impl BatchRow {
    /// A round assembled behind `parent`, the round whose pass is still running.
    pub fn staged(batch_id: impl Into<String>, repo: impl Into<String>, head: impl Into<String>, base: impl Into<String>, parent: impl Into<String>) -> Self {
        let mut row = BatchRow::cut(batch_id, repo, head, base);
        row.state = BatchState::Staged;
        row.parent = Some(parent.into());
        row
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BatchEventKind {
    /// Valid only while OPEN. Membership itself lives in the `batch_member` table; this
    /// event only proves the batch was still accepting members when it was added.
    MemberAdded { bead_id: String, tip: String },
    CiStarted { run: String },
    /// OPEN, REBUILDING or ATTRIBUTING -> CI_RUNNING, phase build. `n` must be the next pass number.
    PassStarted { n: u32, head: String },
    /// Phase build -> suites: the test cap's clock starts here. For a pass that already has its
    /// verdict it is `LateBoundary`: done, and nothing changes.
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
    /// One suite of the running pass went red, reported mid-pass; the pass itself is untouched.
    /// Valid only in phase suites, for the current pass; for a judged pass it is `LateBoundary`.
    /// Per-failure bookkeeping: the batch does not move, and a mender's MENDING row is opened for it.
    Failed { suite: String, n: u32 },
    /// ATTRIBUTING -> OPEN for the next pass, at the head the survivors were rebuilt to.
    PassRebuilt { head: String },
    Green,
    Red,
    BaseMoved,
    Rebuilt,
    FastForward { sha: String },
    /// GREEN -> LANDING: the landing took its lock and has not moved the ref.
    LandStarted,
    /// LANDING -> GREEN: the landing was refused before the ref moved.
    LandAborted { reason: String },
    /// DEPLOYING -> LANDED: the release is in force.
    Deployed,
    /// DEPLOYING -> LANDED: the landing stands and the release was not activated.
    DeployFaulted { why: String },
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
    /// STAGED -> OPEN at the head and base the staged members were re-merged onto.
    Promote { head: String, base: String },
    /// STAGED -> DISCARDED: the round it waited behind went red or was abandoned, or its
    /// members no longer merge. Nothing was delivered, so nothing is returned.
    Discard { reason: String },
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

fn late(row: &BatchRow, kind: &BatchEventKind, n: u32) -> Outcome<BatchRow> {
    Outcome::refuse(row.clone(), Refusal::LateBoundary { state: where_is(row), event: format!("{kind:?}"), pass: n })
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
            if matches!(row.state, BatchState::Staged | BatchState::Landing | BatchState::Deploying) {
                return illegal(row, &ev.kind);
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
        | BatchEventKind::Failed { .. }
        | BatchEventKind::PassRebuilt { .. }
        | BatchEventKind::Green
        | BatchEventKind::Red
        | BatchEventKind::BaseMoved
        | BatchEventKind::Rebuilt
        | BatchEventKind::FastForward { .. }
        | BatchEventKind::LandStarted
        | BatchEventKind::LandAborted { .. }
        | BatchEventKind::Deployed
        | BatchEventKind::DeployFaulted { .. }
        | BatchEventKind::Attributed { .. }
        | BatchEventKind::Settle
        | BatchEventKind::Eject { .. }
        | BatchEventKind::Promote { .. }
        | BatchEventKind::Discard { .. } => primary_transition(row, &ev.kind),
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
            | PassGreen { .. } | PassRed { .. } | PassIncomplete { .. } | PassPreempted { .. } | Failed { .. } | PassRebuilt { .. } | Promote { .. } | Discard { .. } | LandStarted | LandAborted { .. } | Deployed | DeployFaulted { .. } => illegal(row, kind),
        },

        BatchState::CiRunning => match kind {
            SuitesStarted { n } | Failed { n, .. } if *n < row.pass => late(row, kind, *n),
            Green => Outcome::applied(moved(BatchState::Green, None)),
            Red => Outcome::applied(moved(BatchState::Attributing, None)),
            Eject { .. } => Outcome::applied(stays()),
            Failed { n, .. } if *n == row.pass && row.phase == Some(BatchPhase::Suites) => Outcome::applied(stays()),
            SuitesStarted { n } if *n == row.pass && row.phase == Some(BatchPhase::Build) => {
                Outcome::applied(moved(BatchState::CiRunning, Some(BatchPhase::Suites)))
            }
            PassGreen { n, .. } if *n == row.pass && row.phase == Some(BatchPhase::Suites) => Outcome::applied(moved(BatchState::Green, None)),
            PassRed { n, .. } | PassIncomplete { n, .. } | PassPreempted { n, .. } if *n == row.pass => Outcome::applied(moved(BatchState::Attributing, None)),
            MemberAdded { .. } | CiStarted { .. } | PassStarted { .. } | SuitesStarted { .. } | PassGreen { .. } | PassRed { .. }
            | PassIncomplete { .. } | PassPreempted { .. } | Failed { .. } | PassRebuilt { .. } | BaseMoved | Rebuilt | FastForward { .. } | Attributed { .. } | Settle
            | Abandon { .. } | Promote { .. } | Discard { .. } | LandStarted | LandAborted { .. } | Deployed | DeployFaulted { .. } => illegal(row, kind),
        },

        BatchState::Green => match kind {
            SuitesStarted { n } | Failed { n, .. } if *n <= row.pass => late(row, kind, *n),
            FastForward { sha } => {
                let mut new = moved(BatchState::Landed, None);
                new.reason = Some(sha.clone());
                Outcome::applied(new)
            }
            LandStarted => Outcome::applied(moved(BatchState::Landing, None)),
            BaseMoved => Outcome::applied(moved(BatchState::Rebuilding, None)),
            Eject { .. } => Outcome::applied(moved(BatchState::Open, None)),
            MemberAdded { .. } | CiStarted { .. } | PassStarted { .. } | SuitesStarted { .. } | PassGreen { .. } | PassRed { .. }
            | PassIncomplete { .. } | PassPreempted { .. } | Failed { .. } | PassRebuilt { .. } | Green | Red | Rebuilt | Attributed { .. } | Settle | Abandon { .. }
            | Promote { .. } | Discard { .. } | LandAborted { .. } | Deployed | DeployFaulted { .. } => {
                illegal(row, kind)
            }
        },

        BatchState::Landing => match kind {
            SuitesStarted { n } | Failed { n, .. } if *n <= row.pass => late(row, kind, *n),
            FastForward { sha } => {
                let mut new = moved(BatchState::Deploying, None);
                new.reason = Some(sha.clone());
                Outcome::applied(new)
            }
            LandAborted { reason } => {
                let mut new = moved(BatchState::Green, None);
                new.reason = Some(reason.clone());
                Outcome::applied(new)
            }
            MemberAdded { .. } | CiStarted { .. } | PassStarted { .. } | SuitesStarted { .. } | PassGreen { .. } | PassRed { .. }
            | PassIncomplete { .. } | PassPreempted { .. } | Failed { .. } | PassRebuilt { .. } | Green | Red | BaseMoved | Rebuilt | Attributed { .. } | Settle
            | Abandon { .. } | Eject { .. } | Promote { .. } | Discard { .. } | LandStarted | Deployed | DeployFaulted { .. } => illegal(row, kind),
        },

        BatchState::Deploying => match kind {
            SuitesStarted { n } | Failed { n, .. } if *n <= row.pass => late(row, kind, *n),
            Deployed => Outcome::applied(moved(BatchState::Landed, None)),
            DeployFaulted { why } => {
                let mut new = moved(BatchState::Landed, None);
                let sha = row.reason.as_deref().unwrap_or_default();
                new.reason = Some(format!("{sha} deploy-fault: {why}"));
                Outcome::applied(new)
            }
            MemberAdded { .. } | CiStarted { .. } | PassStarted { .. } | SuitesStarted { .. } | PassGreen { .. } | PassRed { .. }
            | PassIncomplete { .. } | PassPreempted { .. } | Failed { .. } | PassRebuilt { .. } | Green | Red | BaseMoved | Rebuilt | FastForward { .. } | Attributed { .. }
            | Settle | Abandon { .. } | Eject { .. } | Promote { .. } | Discard { .. } | LandStarted | LandAborted { .. } => illegal(row, kind),
        },

        BatchState::Rebuilding => match kind {
            SuitesStarted { n } | Failed { n, .. } if *n <= row.pass => late(row, kind, *n),
            Rebuilt => {
                let mut new = moved(BatchState::CiRunning, Some(BatchPhase::Build));
                new.pass += 1;
                Outcome::applied(new)
            }
            PassStarted { n, head } => started(n, Some(head)),
            MemberAdded { .. } | CiStarted { .. } | SuitesStarted { .. } | PassGreen { .. } | PassRed { .. } | PassIncomplete { .. } | PassPreempted { .. } | Failed { .. }
            | PassRebuilt { .. } | Green | Red | BaseMoved | FastForward { .. } | Attributed { .. } | Settle | Abandon { .. }
            | Eject { .. } | Promote { .. } | Discard { .. } | LandStarted | LandAborted { .. } | Deployed | DeployFaulted { .. } => illegal(row, kind),
        },

        BatchState::Attributing => match kind {
            SuitesStarted { n } | Failed { n, .. } if *n <= row.pass => late(row, kind, *n),
            PassStarted { n, head } => started(n, Some(head)),
            Attributed { .. } | Eject { .. } => Outcome::applied(stays()),
            Settle => Outcome::applied(moved(BatchState::Settled, None)),
            PassRebuilt { head } => {
                let mut new = moved(BatchState::Open, None);
                new.head = Some(head.clone());
                Outcome::applied(new)
            }
            MemberAdded { .. } | CiStarted { .. } | SuitesStarted { .. } | PassGreen { .. } | PassRed { .. }
            | PassIncomplete { .. } | PassPreempted { .. } | Failed { .. } | Green | Red | BaseMoved | Rebuilt | FastForward { .. } | Abandon { .. } | Promote { .. } | Discard { .. } | LandStarted | LandAborted { .. } | Deployed | DeployFaulted { .. } => illegal(row, kind),
        },

        BatchState::Staged => match kind {
            Promote { head, base } => {
                let mut new = moved(BatchState::Open, None);
                new.head = Some(head.clone());
                new.base = Some(base.clone());
                Outcome::applied(new)
            }
            Discard { reason } => {
                let mut new = moved(BatchState::Discarded, None);
                new.reason = Some(reason.clone());
                Outcome::applied(new)
            }
            MemberAdded { .. } | CiStarted { .. } | PassStarted { .. } | SuitesStarted { .. } | PassGreen { .. } | PassRed { .. }
            | PassIncomplete { .. } | PassPreempted { .. } | Failed { .. } | PassRebuilt { .. } | Green | Red | BaseMoved | Rebuilt | FastForward { .. } | Attributed { .. }
            | Settle | Abandon { .. } | Eject { .. } | LandStarted | LandAborted { .. } | Deployed | DeployFaulted { .. } => illegal(row, kind),
        },

        BatchState::Landed | BatchState::Settled | BatchState::Abandoned | BatchState::Discarded => match kind {
            MemberAdded { .. } | CiStarted { .. } | PassStarted { .. } | SuitesStarted { .. } | PassGreen { .. } | PassRed { .. }
            | PassIncomplete { .. } | PassPreempted { .. } | Failed { .. } | PassRebuilt { .. } | Green | Red | BaseMoved | Rebuilt | FastForward { .. } | Attributed { .. }
            | Settle | Abandon { .. } | Eject { .. } | Promote { .. } | Discard { .. } | LandStarted | LandAborted { .. } | Deployed | DeployFaulted { .. } => terminal(row),
        },
    }
}

/// Every state the machine has, in declaration order.
pub const STATES: [BatchState; 12] = [
    BatchState::Staged,
    BatchState::Open,
    BatchState::CiRunning,
    BatchState::Green,
    BatchState::Landing,
    BatchState::Deploying,
    BatchState::Attributing,
    BatchState::Rebuilding,
    BatchState::Landed,
    BatchState::Settled,
    BatchState::Abandoned,
    BatchState::Discarded,
];

/// One legal move of the batch machine: `event` takes a row in `from` to `to`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct LegalEdge {
    pub from: BatchState,
    pub event: &'static str,
    pub to: BatchState,
}

/// The event's name as `event.event` logs it (the serde variant tag). Exhaustive, so a new
/// variant does not compile until it is named here and probed in [`event_probes`].
fn kind_name(kind: &BatchEventKind) -> &'static str {
    match kind {
        BatchEventKind::MemberAdded { .. } => "MemberAdded",
        BatchEventKind::CiStarted { .. } => "CiStarted",
        BatchEventKind::PassStarted { .. } => "PassStarted",
        BatchEventKind::SuitesStarted { .. } => "SuitesStarted",
        BatchEventKind::PassGreen { .. } => "PassGreen",
        BatchEventKind::PassRed { .. } => "PassRed",
        BatchEventKind::PassIncomplete { .. } => "PassIncomplete",
        BatchEventKind::PassPreempted { .. } => "PassPreempted",
        BatchEventKind::Failed { .. } => "Failed",
        BatchEventKind::PassRebuilt { .. } => "PassRebuilt",
        BatchEventKind::Green => "Green",
        BatchEventKind::Red => "Red",
        BatchEventKind::BaseMoved => "BaseMoved",
        BatchEventKind::Rebuilt => "Rebuilt",
        BatchEventKind::FastForward { .. } => "FastForward",
        BatchEventKind::LandStarted => "LandStarted",
        BatchEventKind::LandAborted { .. } => "LandAborted",
        BatchEventKind::Deployed => "Deployed",
        BatchEventKind::DeployFaulted { .. } => "DeployFaulted",
        BatchEventKind::Attributed { .. } => "Attributed",
        BatchEventKind::Settle => "Settle",
        BatchEventKind::Abandon { .. } => "Abandon",
        BatchEventKind::Eject { .. } => "Eject",
        BatchEventKind::Promote { .. } => "Promote",
        BatchEventKind::Discard { .. } => "Discard",
    }
}

/// One probe per event variant, plus `PassStarted` for a second pass.
fn event_probes() -> Vec<BatchEventKind> {
    vec![
        BatchEventKind::MemberAdded { bead_id: "sp-1".into(), tip: "t1".into() },
        BatchEventKind::CiStarted { run: "run1".into() },
        BatchEventKind::PassStarted { n: 1, head: "h".into() },
        BatchEventKind::PassStarted { n: 2, head: "h".into() },
        BatchEventKind::SuitesStarted { n: 1 },
        BatchEventKind::PassGreen { n: 1, suites_s: 1, build_s: 1 },
        BatchEventKind::PassRed { n: 1, red_suites: vec!["test-x.sh".into()], suites_s: 1, build_s: 1 },
        BatchEventKind::PassIncomplete { n: 1, reason: "cap".into() },
        BatchEventKind::PassPreempted { n: 1, done: 3, total: 9, red_suites: vec![] },
        BatchEventKind::Failed { suite: "test-x.sh".into(), n: 1 },
        BatchEventKind::PassRebuilt { head: "h2".into() },
        BatchEventKind::Green,
        BatchEventKind::Red,
        BatchEventKind::BaseMoved,
        BatchEventKind::Rebuilt,
        BatchEventKind::FastForward { sha: "sha1".into() },
        BatchEventKind::LandStarted,
        BatchEventKind::LandAborted { reason: "base moved".into() },
        BatchEventKind::Deployed,
        BatchEventKind::DeployFaulted { why: "unit failed".into() },
        BatchEventKind::Attributed { bead_id: "sp-1".into(), outcome: "requeue".into() },
        BatchEventKind::Settle,
        BatchEventKind::Abandon { reason: "r".into() },
        BatchEventKind::Eject { bead_id: "sp-1".into(), reason: "manual".into() },
        BatchEventKind::Promote { head: "h3".into(), base: "b3".into() },
        BatchEventKind::Discard { reason: "n red".into() },
    ]
}

/// Every event name the machine has, in probe order.
pub fn event_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Vec::new();
    for kind in event_probes() {
        let name = kind_name(&kind);
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Every `(state, event, to)` the machine accepts that moves a row to a different state,
/// read by applying each probe to each state through [`apply`]. A move whose target depends on
/// the row's pass or phase is found only for the row shapes probed.
pub fn legal_edges() -> Vec<LegalEdge> {
    let mut edges = std::collections::BTreeSet::new();
    for state in STATES {
        for (pass, phase) in [(0, None), (1, Some(BatchPhase::Build)), (1, Some(BatchPhase::Suites))] {
            let mut row = BatchRow::cut("b-edge", "repo", "head", "base");
            row.state = state;
            row.pass = pass;
            row.phase = phase;
            for kind in event_probes() {
                let ev = BatchEvent { expect: state, version: row.version, kind, actor: "probe".into() };
                let out = apply(&row, &ev);
                if out.applied && out.row.state != state {
                    edges.insert(LegalEdge { from: state, event: kind_name(&ev.kind), to: out.row.state });
                }
            }
        }
    }
    edges.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_STATES: [BatchState; 12] = STATES;

    fn kinds() -> Vec<BatchEventKind> {
        event_probes()
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
    fn legal_edges_are_the_machines_own_moves() {
        let edges = legal_edges();
        let has = |f: BatchState, e: &str, t: BatchState| edges.iter().any(|x| x.from == f && x.event == e && x.to == t);
        assert!(has(BatchState::Open, "Abandon", BatchState::Abandoned));
        assert!(has(BatchState::Green, "FastForward", BatchState::Landed));
        assert!(has(BatchState::CiRunning, "PassGreen", BatchState::Green));
        assert!(has(BatchState::Attributing, "PassRebuilt", BatchState::Open));
        assert!(!has(BatchState::Open, "MemberAdded", BatchState::Open), "a move that stays put is not an edge");
        assert!(edges.iter().all(|e| !e.from.is_terminal() && e.from != e.to));
    }

    #[test]
    fn every_event_name_is_its_serde_tag_and_listed() {
        let names = event_names();
        for kind in event_probes() {
            let tag = serde_json::to_value(&kind).unwrap();
            let tag = tag.as_str().map(str::to_string).or_else(|| tag.as_object().and_then(|o| o.keys().next().cloned())).unwrap();
            assert_eq!(kind_name(&kind), tag);
            assert!(names.contains(&kind_name(&kind)));
        }
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
        for &state in &[BatchState::Landed, BatchState::Settled, BatchState::Abandoned, BatchState::Discarded] {
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

    fn rows_in(state: BatchState) -> BatchRow {
        let mut r = row(state);
        r.reason = Some("sha1".into());
        r
    }

    #[test]
    fn landing_and_deploying_apply_every_legal_move_and_refuse_the_rest_naming_the_state() {
        use BatchEventKind::*;
        let legal: [(BatchState, BatchEventKind, BatchState); 5] = [
            (BatchState::Green, LandStarted, BatchState::Landing),
            (BatchState::Landing, FastForward { sha: "sha1".into() }, BatchState::Deploying),
            (BatchState::Landing, LandAborted { reason: "base moved".into() }, BatchState::Green),
            (BatchState::Deploying, Deployed, BatchState::Landed),
            (BatchState::Deploying, DeployFaulted { why: "unit failed".into() }, BatchState::Landed),
        ];
        for (from, kind, to) in &legal {
            let r = rows_in(*from);
            let out = apply(&r, &ev(*from, r.version, kind.clone()));
            assert!(out.applied, "{from:?} {kind:?}: {:?}", out.refusal);
            assert_eq!(out.row.state, *to);
            assert_eq!(out.row.version, r.version + 1);
        }
        for from in [BatchState::Landing, BatchState::Deploying] {
            for kind in kinds() {
                if legal.iter().any(|(f, k, _)| *f == from && *k == kind) {
                    continue;
                }
                let r = rows_in(from);
                let out = apply(&r, &ev(from, r.version, kind.clone()));
                assert!(!out.applied, "{from:?} must refuse {kind:?}");
                match out.refusal {
                    Some(Refusal::IllegalTransition { state, .. }) => assert!(state.contains(from.as_str()), "{state}"),
                    other => panic!("{other:?}"),
                }
                assert_eq!(out.row, r);
            }
        }
    }

    #[test]
    fn a_deploy_fault_lands_the_batch_and_keeps_the_sha_and_the_fault() {
        let r = row(BatchState::Green);
        let r = step(&r, BatchEventKind::LandStarted);
        let r = step(&r, BatchEventKind::FastForward { sha: "sha1".into() });
        assert_eq!(r.reason.as_deref(), Some("sha1"));
        let r = step(&r, BatchEventKind::DeployFaulted { why: "unit failed".into() });
        assert_eq!((r.state, r.reason.as_deref()), (BatchState::Landed, Some("sha1 deploy-fault: unit failed")));
        assert!(BatchState::from_str("LANDING") == Some(BatchState::Landing) && BatchState::from_str("DEPLOYING") == Some(BatchState::Deploying));
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
    fn a_failed_suite_is_recorded_mid_pass_without_moving_the_batch() {
        let failed = |suite: &str, n| BatchEventKind::Failed { suite: suite.into(), n };
        let building = step(&row(BatchState::Open), BatchEventKind::PassStarted { n: 1, head: "h".into() });
        assert_eq!(refused(&building, failed("test-a.sh", 1)), "CI_RUNNING (pass 1, phase build)", "no suite runs during the build");
        let suites = step(&building, BatchEventKind::SuitesStarted { n: 1 });
        let one = step(&suites, failed("test-a.sh", 1));
        let two = step(&one, failed("test-b.sh", 1));
        assert_eq!((two.state, two.pass, two.phase, two.version), (BatchState::CiRunning, 1, Some(BatchPhase::Suites), suites.version + 2));
        refused(&suites, failed("test-a.sh", 2));
        for state in [BatchState::Open, BatchState::Green, BatchState::Attributing, BatchState::Rebuilding] {
            refused(&row(state), failed("test-a.sh", 1));
        }
        let red = step(&two, BatchEventKind::PassRed { n: 1, red_suites: vec!["test-a.sh".into(), "test-b.sh".into()], suites_s: 1, build_s: 1 });
        assert_eq!(red.state, BatchState::Attributing, "the verdict still ends the pass");
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

    fn at_pass(state: BatchState, pass: u32) -> BatchRow {
        let mut r = rows_in(state);
        r.pass = pass;
        r
    }

    fn boundaries(n: u32) -> [BatchEventKind; 2] {
        [BatchEventKind::SuitesStarted { n }, BatchEventKind::Failed { suite: "spira-lint".into(), n }]
    }

    #[test]
    fn a_boundary_for_a_pass_that_has_its_verdict_is_late_in_every_state_and_changes_nothing() {
        let judged = [BatchState::Green, BatchState::Landing, BatchState::Deploying, BatchState::Rebuilding, BatchState::Attributing];
        for &state in &ALL_STATES {
            let r = at_pass(state, 2);
            for n in [1, 2, 3] {
                for kind in boundaries(n) {
                    let out = apply(&r, &ev(state, r.version, kind.clone()));
                    let late = match state {
                        s if judged.contains(&s) => n <= 2,
                        BatchState::CiRunning => n < 2,
                        _ => false,
                    };
                    if late {
                        assert!(!out.applied, "{state:?} {kind:?}");
                        assert_eq!(out.row, r, "{state:?} {kind:?}: a late boundary never mutates");
                        assert_eq!(out.refusal, Some(Refusal::LateBoundary { state: where_is(&r), event: format!("{kind:?}"), pass: n }), "{state:?} {kind:?}");
                    } else {
                        assert!(!matches!(out.refusal, Some(Refusal::LateBoundary { .. })), "{state:?} {kind:?}: {:?}", out.refusal);
                    }
                }
            }
        }
    }

    #[test]
    fn a_boundary_for_the_pass_running_now_still_applies_and_a_future_pass_is_still_illegal() {
        let building = at_pass(BatchState::CiRunning, 2);
        let mut building = building;
        building.phase = Some(BatchPhase::Build);
        let suites = step(&building, BatchEventKind::SuitesStarted { n: 2 });
        let failed = step(&suites, BatchEventKind::Failed { suite: "test-a.sh".into(), n: 2 });
        assert_eq!((failed.state, failed.phase), (BatchState::CiRunning, Some(BatchPhase::Suites)));
        for kind in boundaries(3) {
            refused(&failed, kind.clone());
            refused(&at_pass(BatchState::Attributing, 2), kind);
        }
    }

    #[test]
    fn attributing_starts_its_next_pass_at_the_head_it_names_and_no_other_pass() {
        let r = at_pass(BatchState::Attributing, 1);
        let next = step(&r, BatchEventKind::PassStarted { n: 2, head: "h2".into() });
        assert_eq!((next.state, next.phase, next.pass, next.head.as_deref()), (BatchState::CiRunning, Some(BatchPhase::Build), 2, Some("h2")));
        for n in [0, 1, 3] {
            assert_eq!(refused(&r, BatchEventKind::PassStarted { n, head: "h2".into() }), "ATTRIBUTING (pass 1)");
        }
    }

    #[test]
    fn a_red_lint_pass_then_its_late_boundaries_then_a_green_second_pass() {
        let r = step(&row(BatchState::Open), BatchEventKind::PassStarted { n: 1, head: "h1".into() });
        let r = step(&r, BatchEventKind::PassRed { n: 1, red_suites: vec!["spira-lint".into()], suites_s: 0, build_s: 9 });
        for kind in boundaries(1) {
            let out = apply(&r, &ev(r.state, r.version, kind));
            assert!(matches!(out.refusal, Some(Refusal::LateBoundary { pass: 1, .. })), "{:?}", out.refusal);
        }
        let r = step(&r, BatchEventKind::PassStarted { n: 2, head: "h1".into() });
        let r = step(&r, BatchEventKind::SuitesStarted { n: 2 });
        let r = step(&r, BatchEventKind::PassGreen { n: 2, suites_s: 80, build_s: 20 });
        assert_eq!((r.state, r.pass), (BatchState::Green, 2));
    }

    fn staged() -> BatchRow {
        BatchRow::staged("r2", "spira", "staged-head", "n-head", "r1")
    }

    #[test]
    fn a_staged_round_promotes_to_open_at_the_head_it_was_rebuilt_to() {
        let r = staged();
        assert_eq!((r.state, r.parent.as_deref()), (BatchState::Staged, Some("r1")));
        let r = step(&r, BatchEventKind::Promote { head: "new-head".into(), base: "landed".into() });
        assert_eq!((r.state, r.head.as_deref(), r.base.as_deref(), r.pass), (BatchState::Open, Some("new-head"), Some("landed"), 0));
        step(&r, BatchEventKind::PassStarted { n: 1, head: "new-head".into() });
    }

    #[test]
    fn a_staged_round_is_discarded_with_its_reason_and_then_absorbs_everything() {
        let r = step(&staged(), BatchEventKind::Discard { reason: "r1 red".into() });
        assert_eq!((r.state, r.reason.as_deref()), (BatchState::Discarded, Some("r1 red")));
        for kind in kinds() {
            assert!(!apply(&r, &ev(r.state, r.version, kind)).applied);
        }
    }

    #[test]
    fn a_staged_round_has_no_pass_and_is_not_abandoned() {
        let r = staged();
        for kind in [
            BatchEventKind::PassStarted { n: 1, head: "h".into() },
            BatchEventKind::CiStarted { run: "x".into() },
            BatchEventKind::Abandon { reason: "r".into() },
            BatchEventKind::Eject { bead_id: "sp-1".into(), reason: "m".into() },
            BatchEventKind::MemberAdded { bead_id: "sp-1".into(), tip: "t".into() },
        ] {
            assert_eq!(refused(&r, kind), "STAGED");
        }
    }

    #[test]
    fn only_a_staged_round_promotes_or_is_discarded() {
        for &state in &[BatchState::Open, BatchState::CiRunning, BatchState::Green, BatchState::Attributing, BatchState::Rebuilding] {
            let r = row(state);
            for kind in [BatchEventKind::Promote { head: "h".into(), base: "b".into() }, BatchEventKind::Discard { reason: "r".into() }] {
                assert!(!apply(&r, &ev(state, 0, kind)).applied, "{state:?}");
            }
        }
    }
}
