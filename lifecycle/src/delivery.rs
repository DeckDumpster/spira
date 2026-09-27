//! The delivery machine (design §3.1.2): one machine per land mode, run as the sub-state of
//! the bead's IN_DELIVERY. A `DeliveryRow` is created by exactly one mode's constructor, so
//! its `state` values are mode-exclusive in practice (`Queued`/`Batched` only ever appear on
//! a queue-mode row, `PrOpen` only on a pr-mode row, `Pushing` only on a push-mode row); the
//! transition table below still gives every (state, event) pair an explicit outcome, so a
//! mismatched pairing — which cannot arise through the constructors, only through a
//! hand-built row — is refused rather than unreachable.
//!
//! Every delivery machine ends in exactly one of three exit events back to the bead:
//! `Delivered`, `Returned`, or `Requeued`. Once one fires, the delivery row is `Exited` and
//! has no further transitions — a new delivery is a new row, started fresh by the next
//! `deliver` event on the bead.

use crate::{Outcome, Refusal, Version};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Mode {
    Queue,
    Pr,
    Push,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DeliveryState {
    Queued,
    Batched,
    PrOpen,
    Pushing,
    Exited,
}

impl DeliveryState {
    pub fn is_terminal(self) -> bool {
        matches!(self, DeliveryState::Exited)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            DeliveryState::Queued => "QUEUED",
            DeliveryState::Batched => "BATCHED",
            DeliveryState::PrOpen => "PR_OPEN",
            DeliveryState::Pushing => "PUSHING",
            DeliveryState::Exited => "EXITED",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "QUEUED" => DeliveryState::Queued,
            "BATCHED" => DeliveryState::Batched,
            "PR_OPEN" => DeliveryState::PrOpen,
            "PUSHING" => DeliveryState::Pushing,
            "EXITED" => DeliveryState::Exited,
            _ => return None,
        })
    }
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Queue => "queue",
            Mode::Pr => "pr",
            Mode::Push => "push",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "queue" => Mode::Queue,
            "pr" => Mode::Pr,
            "push" => Mode::Push,
            _ => return None,
        })
    }
}

/// The exit this delivery row concluded with, once `Exited`. Read by the cascade that
/// applies the matching event to the bead row in the same transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Exit {
    Delivered,
    Returned,
    Requeued,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeliveryRow {
    pub bead_id: String,
    pub mode: Mode,
    pub state: DeliveryState,
    pub batch_id: Option<String>,
    pub pr: Option<u64>,
    pub merge_sha: Option<String>,
    pub exit: Option<Exit>,
    pub version: Version,
}

impl DeliveryRow {
    pub fn start_queue(bead_id: impl Into<String>) -> Self {
        DeliveryRow {
            bead_id: bead_id.into(),
            mode: Mode::Queue,
            state: DeliveryState::Queued,
            batch_id: None,
            pr: None,
            merge_sha: None,
            exit: None,
            version: 0,
        }
    }

    pub fn start_pr(bead_id: impl Into<String>, pr: u64) -> Self {
        DeliveryRow {
            bead_id: bead_id.into(),
            mode: Mode::Pr,
            state: DeliveryState::PrOpen,
            batch_id: None,
            pr: Some(pr),
            merge_sha: None,
            exit: None,
            version: 0,
        }
    }

    pub fn start_push(bead_id: impl Into<String>) -> Self {
        DeliveryRow {
            bead_id: bead_id.into(),
            mode: Mode::Push,
            state: DeliveryState::Pushing,
            batch_id: None,
            pr: None,
            merge_sha: None,
            exit: None,
            version: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DeliveryEventKind {
    /// queue mode only: the batcher cuts this member into a batch.
    Cut { batch_id: String },
    Delivered { merge_sha: String, proof: String },
    Returned { reason: String },
    Requeued { tip: String },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeliveryEvent {
    pub expect: DeliveryState,
    pub version: Version,
    pub kind: DeliveryEventKind,
    pub actor: String,
}

fn illegal(row: &DeliveryRow, kind: &DeliveryEventKind) -> Outcome<DeliveryRow> {
    Outcome::refuse(
        row.clone(),
        Refusal::IllegalTransition { state: row.state.as_str().to_string(), event: format!("{kind:?}") },
    )
}

fn terminal(row: &DeliveryRow) -> Outcome<DeliveryRow> {
    Outcome::refuse(row.clone(), Refusal::Terminal { state: row.state.as_str().to_string() })
}

/// Apply one event to one delivery row. Exhaustive over `(DeliveryState, DeliveryEventKind)`;
/// no wildcard arm.
pub fn apply(row: &DeliveryRow, ev: &DeliveryEvent) -> Outcome<DeliveryRow> {
    if ev.version != row.version {
        return Outcome::refuse(row.clone(), Refusal::StaleVersion { given: ev.version, current: row.version });
    }
    if ev.expect != row.state {
        return Outcome::refuse(
            row.clone(),
            Refusal::ExpectMismatch { expected: ev.expect.as_str().to_string(), actual: row.state.as_str().to_string() },
        );
    }

    use DeliveryEventKind::*;
    use DeliveryState::*;

    match row.state {
        Queued => match &ev.kind {
            Cut { batch_id } => {
                let mut new = row.clone();
                new.state = Batched;
                new.batch_id = Some(batch_id.clone());
                new.version += 1;
                Outcome::applied(new)
            }
            Delivered { .. } | Returned { .. } | Requeued { .. } => illegal(row, &ev.kind),
        },

        Batched => match &ev.kind {
            Delivered { merge_sha, proof: _ } => {
                let mut new = row.clone();
                new.state = Exited;
                new.exit = Some(Exit::Delivered);
                new.merge_sha = Some(merge_sha.clone());
                new.version += 1;
                Outcome::applied(new)
            }
            Returned { .. } => {
                let mut new = row.clone();
                new.state = Exited;
                new.exit = Some(Exit::Returned);
                new.version += 1;
                Outcome::applied(new)
            }
            Requeued { .. } => {
                let mut new = row.clone();
                new.state = Exited;
                new.exit = Some(Exit::Requeued);
                new.version += 1;
                Outcome::applied(new)
            }
            Cut { .. } => illegal(row, &ev.kind),
        },

        // pr mode is external custody: the machine observes merged/closed but never
        // requeues a PR, because a human or the forge already decided its fate.
        PrOpen => match &ev.kind {
            Delivered { merge_sha, proof: _ } => {
                let mut new = row.clone();
                new.state = Exited;
                new.exit = Some(Exit::Delivered);
                new.merge_sha = Some(merge_sha.clone());
                new.version += 1;
                Outcome::applied(new)
            }
            Returned { .. } => {
                let mut new = row.clone();
                new.state = Exited;
                new.exit = Some(Exit::Returned);
                new.version += 1;
                Outcome::applied(new)
            }
            Requeued { .. } | Cut { .. } => illegal(row, &ev.kind),
        },

        Pushing => match &ev.kind {
            Delivered { merge_sha, proof: _ } => {
                let mut new = row.clone();
                new.state = Exited;
                new.exit = Some(Exit::Delivered);
                new.merge_sha = Some(merge_sha.clone());
                new.version += 1;
                Outcome::applied(new)
            }
            Returned { .. } => {
                let mut new = row.clone();
                new.state = Exited;
                new.exit = Some(Exit::Returned);
                new.version += 1;
                Outcome::applied(new)
            }
            Requeued { .. } => {
                let mut new = row.clone();
                new.state = Exited;
                new.exit = Some(Exit::Requeued);
                new.version += 1;
                Outcome::applied(new)
            }
            Cut { .. } => illegal(row, &ev.kind),
        },

        Exited => match &ev.kind {
            Cut { .. } | Delivered { .. } | Returned { .. } | Requeued { .. } => terminal(row),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds() -> Vec<DeliveryEventKind> {
        vec![
            DeliveryEventKind::Cut { batch_id: "b1".into() },
            DeliveryEventKind::Delivered { merge_sha: "s".into(), proof: "p".into() },
            DeliveryEventKind::Returned { reason: "r".into() },
            DeliveryEventKind::Requeued { tip: "t".into() },
        ]
    }

    fn ev(expect: DeliveryState, version: Version, kind: DeliveryEventKind) -> DeliveryEvent {
        DeliveryEvent { expect, version, kind, actor: "test".into() }
    }

    #[test]
    fn every_state_event_pair_has_an_explicit_outcome() {
        for &state in &[DeliveryState::Queued, DeliveryState::Batched, DeliveryState::PrOpen, DeliveryState::Pushing, DeliveryState::Exited] {
            for kind in kinds() {
                let mut row = DeliveryRow::start_queue("sp-x");
                row.state = state;
                let out = apply(&row, &ev(state, row.version, kind));
                if out.applied {
                    assert_eq!(out.row.version, row.version + 1);
                } else {
                    assert_eq!(out.row, row);
                }
            }
        }
    }

    #[test]
    fn terminal_absorbs_everything() {
        let mut row = DeliveryRow::start_queue("sp-x");
        row.state = DeliveryState::Exited;
        for kind in kinds() {
            let out = apply(&row, &ev(DeliveryState::Exited, 0, kind));
            assert!(!out.applied);
            assert!(matches!(out.refusal, Some(Refusal::Terminal { .. })));
        }
    }

    #[test]
    fn queue_mode_full_cycle() {
        let row = DeliveryRow::start_queue("sp-x");
        let out = apply(&row, &ev(DeliveryState::Queued, 0, DeliveryEventKind::Cut { batch_id: "b1".into() }));
        assert!(out.applied);
        assert_eq!(out.row.state, DeliveryState::Batched);
        assert_eq!(out.row.batch_id.as_deref(), Some("b1"));

        let out2 = apply(&out.row, &ev(DeliveryState::Batched, out.row.version, DeliveryEventKind::Delivered { merge_sha: "sha1".into(), proof: "ancestry".into() }));
        assert!(out2.applied);
        assert_eq!(out2.row.state, DeliveryState::Exited);
        assert_eq!(out2.row.exit, Some(Exit::Delivered));
    }

    #[test]
    fn pr_mode_never_requeues() {
        let row = DeliveryRow::start_pr("sp-x", 42);
        let out = apply(&row, &ev(DeliveryState::PrOpen, 0, DeliveryEventKind::Requeued { tip: "t".into() }));
        assert!(!out.applied);
        assert!(matches!(out.refusal, Some(Refusal::IllegalTransition { .. })));
    }

    #[test]
    fn push_mode_can_requeue_or_return_on_rejection() {
        let row = DeliveryRow::start_push("sp-x");
        let out = apply(&row, &ev(DeliveryState::Pushing, 0, DeliveryEventKind::Requeued { tip: "t".into() }));
        assert!(out.applied);
        assert_eq!(out.row.exit, Some(Exit::Requeued));

        let row = DeliveryRow::start_push("sp-y");
        let out = apply(&row, &ev(DeliveryState::Pushing, 0, DeliveryEventKind::Returned { reason: "rejected".into() }));
        assert!(out.applied);
        assert_eq!(out.row.exit, Some(Exit::Returned));
    }

    #[test]
    fn cut_is_illegal_outside_queue_mode() {
        let row = DeliveryRow::start_pr("sp-x", 1);
        let out = apply(&row, &ev(DeliveryState::PrOpen, 0, DeliveryEventKind::Cut { batch_id: "b1".into() }));
        assert!(!out.applied);

        let row = DeliveryRow::start_push("sp-x");
        let out = apply(&row, &ev(DeliveryState::Pushing, 0, DeliveryEventKind::Cut { batch_id: "b1".into() }));
        assert!(!out.applied);
    }

    #[test]
    fn expect_mismatch_never_mutates() {
        let row = DeliveryRow::start_queue("sp-x");
        let out = apply(&row, &ev(DeliveryState::Batched, 0, DeliveryEventKind::Delivered { merge_sha: "s".into(), proof: "p".into() }));
        assert!(!out.applied);
        assert_eq!(out.row, row);
    }
}
