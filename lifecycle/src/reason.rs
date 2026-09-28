//! Typed reasons for the bead machine's `returned`, `gate_red`, `hold` and `drop` events
//! (design reconciler-time-series-2026-09-27 §2a, row 2b under sp-pswer). The reconciler's
//! rework-by-cause query aggregates these, and it must not invent its own list — so the
//! machine's own reasons are closed enums here, not free text.
//!
//! Each is a plain, data-less enum like [`crate::bead::HoldKind`]: a category the machine
//! already knows, never a place for a caller's own prose. `serde`'s default enum
//! representation refuses an unrecognized variant name at deserialization — the boundary
//! where a caller's raw string becomes evidence, not a new field to check inside `apply`.

/// Why a delivery machine returned a bead to REWORK — the same reason a `bead::Returned`
/// event carries, since a delivery exit's reason is exactly what the bead machine sees
/// (design: "returned(reason) -> REWORK. The failure is the bead's own.").
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReturnedReason {
    /// pr mode: the PR was closed without merging.
    PrClosedUnmerged,
    /// pr mode: a reviewer requested changes.
    PrChangesRequested,
    /// push mode: the push was rejected by a moved base.
    PushRejected,
    /// queue mode: the batch settled red and this member was ejected.
    BatchEjected,
}

impl ReturnedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            ReturnedReason::PrClosedUnmerged => "pr-closed-unmerged",
            ReturnedReason::PrChangesRequested => "pr-changes-requested",
            ReturnedReason::PushRejected => "push-rejected",
            ReturnedReason::BatchEjected => "batch-ejected",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "pr-closed-unmerged" => ReturnedReason::PrClosedUnmerged,
            "pr-changes-requested" => ReturnedReason::PrChangesRequested,
            "push-rejected" => ReturnedReason::PushRejected,
            "batch-ejected" => ReturnedReason::BatchEjected,
            _ => return None,
        })
    }
}

/// Why the certifier's verdict was red. The producer table (design §3.2) names `landing.sh,
/// gate-run` — the categories below are the verdict tokens those emit today (`branch-red`,
/// `syntax`, `beads-data`/`foreign-harness`, plus the confinement/rebase/timeout classes),
/// not a set invented for this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GateRedReason {
    /// A suite the gate ran came back red.
    SuitesFailed,
    /// `bash -n` (or an equivalent syntax check) failed.
    Syntax,
    /// A guard refused the branch outright (e.g. beads data in the harness tree, a foreign
    /// harness reference) — the branch never reached its suites.
    PolicyViolation,
    /// The branch does not rebase cleanly onto the current base.
    NoRebase,
    /// The gate ran longer than its budget and was killed.
    Timeout,
    /// A sandbox/confinement violation during the run.
    Confine,
}

impl GateRedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            GateRedReason::SuitesFailed => "suites-failed",
            GateRedReason::Syntax => "syntax",
            GateRedReason::PolicyViolation => "policy-violation",
            GateRedReason::NoRebase => "no-rebase",
            GateRedReason::Timeout => "timeout",
            GateRedReason::Confine => "confine",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "suites-failed" => GateRedReason::SuitesFailed,
            "syntax" => GateRedReason::Syntax,
            "policy-violation" => GateRedReason::PolicyViolation,
            "no-rebase" => GateRedReason::NoRebase,
            "timeout" => GateRedReason::Timeout,
            "confine" => GateRedReason::Confine,
            _ => return None,
        })
    }
}

/// Why a bead was dropped. The producer table (design §3.2) names "groomer, operator" —
/// groomer.sh's own sweep vocabulary (`closed-no-branch`) is the mechanical case, and
/// `unwanted` is the policy call groomer.sh itself refuses to make and escalates instead
/// (law-escalate-decisions-not-problems), so it can only ever arrive here as the operator's
/// own event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DropReason {
    /// Closed with no branch ever committed — nothing to rework.
    ClosedNoBranch,
    /// The operator decided the bead should not be done.
    Unwanted,
}

impl DropReason {
    pub fn as_str(self) -> &'static str {
        match self {
            DropReason::ClosedNoBranch => "closed-no-branch",
            DropReason::Unwanted => "unwanted",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "closed-no-branch" => DropReason::ClosedNoBranch,
            "unwanted" => DropReason::Unwanted,
            _ => return None,
        })
    }
}

/// Why a hold was placed. One per [`crate::bead::HoldKind`] the design names a producer for
/// (design §3.2: "CHECK 4, semantic layer `work blocked`, CHECK 3b, operator"); the literal
/// question or blocker id a producer knows is carried alongside the event by its own caller
/// (e.g. the mail sent to the operator), never inside this field — the same split as
/// `gate_red`'s `tip` (identifying data) versus its `reason` (category).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HoldCause {
    /// CHECK 4: the bead has failed enough attempts to stop retrying it.
    AttemptsExhausted,
    /// semantic layer `work blocked`: an aeon asked the operator a question.
    OperatorQuestion,
    /// CHECK 3b: blocked on a dependency that has not landed.
    UnlandedBlocker,
    /// an aeon's own request that the bead be superseded, pending confirmation.
    SupersedeRequest,
    /// an operator's own manual hold, for no mechanically-detected cause.
    ManualHold,
}

impl HoldCause {
    pub fn as_str(self) -> &'static str {
        match self {
            HoldCause::AttemptsExhausted => "attempts-exhausted",
            HoldCause::OperatorQuestion => "operator-question",
            HoldCause::UnlandedBlocker => "unlanded-blocker",
            HoldCause::SupersedeRequest => "supersede-request",
            HoldCause::ManualHold => "manual-hold",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "attempts-exhausted" => HoldCause::AttemptsExhausted,
            "operator-question" => HoldCause::OperatorQuestion,
            "unlanded-blocker" => HoldCause::UnlandedBlocker,
            "supersede-request" => HoldCause::SupersedeRequest,
            "manual-hold" => HoldCause::ManualHold,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_RETURNED: [ReturnedReason; 4] = [
        ReturnedReason::PrClosedUnmerged,
        ReturnedReason::PrChangesRequested,
        ReturnedReason::PushRejected,
        ReturnedReason::BatchEjected,
    ];
    const ALL_GATE_RED: [GateRedReason; 6] = [
        GateRedReason::SuitesFailed,
        GateRedReason::Syntax,
        GateRedReason::PolicyViolation,
        GateRedReason::NoRebase,
        GateRedReason::Timeout,
        GateRedReason::Confine,
    ];
    const ALL_DROP: [DropReason; 2] = [DropReason::ClosedNoBranch, DropReason::Unwanted];
    const ALL_HOLD_CAUSE: [HoldCause; 5] = [
        HoldCause::AttemptsExhausted,
        HoldCause::OperatorQuestion,
        HoldCause::UnlandedBlocker,
        HoldCause::SupersedeRequest,
        HoldCause::ManualHold,
    ];

    #[test]
    fn every_returned_reason_round_trips_through_its_own_string() {
        for r in ALL_RETURNED {
            assert_eq!(ReturnedReason::from_str(r.as_str()), Some(r));
        }
    }

    #[test]
    fn every_gate_red_reason_round_trips_through_its_own_string() {
        for r in ALL_GATE_RED {
            assert_eq!(GateRedReason::from_str(r.as_str()), Some(r));
        }
    }

    #[test]
    fn every_drop_reason_round_trips_through_its_own_string() {
        for r in ALL_DROP {
            assert_eq!(DropReason::from_str(r.as_str()), Some(r));
        }
    }

    #[test]
    fn every_hold_cause_round_trips_through_its_own_string() {
        for r in ALL_HOLD_CAUSE {
            assert_eq!(HoldCause::from_str(r.as_str()), Some(r));
        }
    }

    #[test]
    fn an_unrecognized_returned_reason_is_refused_by_from_str() {
        assert_eq!(ReturnedReason::from_str("flaky"), None);
    }

    #[test]
    fn an_unrecognized_reason_is_refused_as_evidence_at_deserialization() {
        // The boundary a producer's raw string crosses to become an event's evidence
        // (see spira-lc's `event bead ... --kind <json>`, which already treats a parse
        // error as "cannot tell", i.e. refused). A tag this crate's enum does not name
        // must fail here, not silently become a new, uncounted reason.
        let bogus = r#"{"GateRed":{"tip":"t1","reason":"flaky"}}"#;
        let parsed: Result<crate::bead::BeadEventKind, _> = serde_json::from_str(bogus);
        assert!(parsed.is_err(), "an unrecognized gate_red reason must be refused, not accepted as {parsed:?}");

        let known = r#"{"GateRed":{"tip":"t1","reason":"suites-failed"}}"#;
        let parsed: crate::bead::BeadEventKind =
            serde_json::from_str(known).expect("a known reason must still parse");
        assert_eq!(parsed, crate::bead::BeadEventKind::GateRed { tip: "t1".into(), reason: GateRedReason::SuitesFailed });
    }
}
