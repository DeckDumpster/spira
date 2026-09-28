//! The delivery machine (design §3.1.2): one machine per land mode, run as the sub-state of
//! the bead's IN_DELIVERY. A `DeliveryRow` is created by exactly one mode's constructor, so
//! its `state` values are mode-exclusive in practice (`Queued`/`Batched` only ever appear on
//! a queue-mode or local-mode row, `PrOpen` only on a pr-mode row, `Pushing` only on a
//! push-mode row); the transition table below still gives every (state, event) pair an
//! explicit outcome, so a mismatched pairing — which cannot arise through the constructors,
//! only through a hand-built row — is refused rather than unreachable.
//!
//! Every delivery machine ends in exactly one of three exit events back to the bead:
//! `Delivered`, `Returned`, or `Requeued`. Once one fires, the delivery row is `Exited` and
//! has no further *bead-facing* transitions — a new delivery is a new row, started fresh by
//! the next `deliver` event on the bead. Local mode (design local-main-2026-09-27 §3, row 8)
//! is the one exception: its `Exited` row (reached by a `Delivered` exit whose proof is a
//! local fast-forward — `queue.local`'s "landed_local") still accepts exactly two further
//! events, `Published`/`PublishRed`, recording the publish queue's own outcome for that
//! commit. Neither touches the bead row — the bead is already LANDED and terminal by then —
//! so they carry no bead-side cascade at all, only their own event-log entry. A red publish's
//! fix goes forward on a NEW bead with its own fresh delivery row; this one's story ends at
//! whichever of `Published`/`PublishRed` it receives.

use crate::reason::ReturnedReason;
use crate::{Outcome, Refusal, Version};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Mode {
    Queue,
    Pr,
    Push,
    Local,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DeliveryState {
    Queued,
    Batched,
    PrOpen,
    Pushing,
    Exited,
    /// Local mode only, reached from `Exited` — the publish queue carried this commit to the
    /// forge and forge CI went green.
    Published,
    /// Local mode only, reached from `Exited` — the publish queue's forge CI came back red.
    /// The bead is not reopened; a fix-forward bead carries the retry.
    PublishRed,
}

impl DeliveryState {
    pub fn is_terminal(self) -> bool {
        matches!(self, DeliveryState::Exited | DeliveryState::Published | DeliveryState::PublishRed)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            DeliveryState::Queued => "QUEUED",
            DeliveryState::Batched => "BATCHED",
            DeliveryState::PrOpen => "PR_OPEN",
            DeliveryState::Pushing => "PUSHING",
            DeliveryState::Exited => "EXITED",
            DeliveryState::Published => "PUBLISHED",
            DeliveryState::PublishRed => "PUBLISH_RED",
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
            "PUBLISHED" => DeliveryState::Published,
            "PUBLISH_RED" => DeliveryState::PublishRed,
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
            Mode::Local => "local",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "queue" => Mode::Queue,
            "pr" => Mode::Pr,
            "push" => Mode::Push,
            "local" => Mode::Local,
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

    /// `queue.local`'s own start: the same `Queued -> Batched -> Delivered("landed_local")`
    /// shape as `start_queue`'s queue.forge round, distinguished only by `mode` — see the
    /// module doc for what that distinction unlocks once the row reaches `Exited`.
    pub fn start_local(bead_id: impl Into<String>) -> Self {
        DeliveryRow {
            bead_id: bead_id.into(),
            mode: Mode::Local,
            state: DeliveryState::Queued,
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
    /// queue and local mode: the batcher (or a local round) cuts this member into a batch.
    Cut { batch_id: String },
    Delivered { merge_sha: String, proof: String },
    Returned { reason: ReturnedReason },
    Requeued { tip: String },
    /// local mode only, legal from `Exited`: the publish queue carried this commit's SHA to
    /// the forge and forge CI went green. Never touches the bead row.
    Published { forge_sha: String },
    /// local mode only, legal from `Exited`: the publish queue's forge CI came back red.
    /// `fix_forward` is the id of the new bead carrying the retry — evidence only, recorded
    /// on this event, never applied as a bead-machine event against either bead.
    PublishRed { fix_forward: String },
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

    // Deliberately not `use DeliveryState::*`: `DeliveryState::Published`/`PublishRed` and
    // `DeliveryEventKind::Published`/`PublishRed` share names (the state a fact lands in
    // and the event that records it), and importing both makes those patterns ambiguous
    // bindings instead of matches on the event variant — the same reason bead.rs's own
    // `primary_transition` qualifies `BeadState` instead of glob-importing it.
    use DeliveryEventKind::*;

    match row.state {
        DeliveryState::Queued => match &ev.kind {
            Cut { batch_id } => {
                let mut new = row.clone();
                new.state = DeliveryState::Batched;
                new.batch_id = Some(batch_id.clone());
                new.version += 1;
                Outcome::applied(new)
            }
            Delivered { .. } | Returned { .. } | Requeued { .. } | Published { .. } | PublishRed { .. } => {
                illegal(row, &ev.kind)
            }
        },

        DeliveryState::Batched => match &ev.kind {
            Delivered { merge_sha, proof: _ } => {
                let mut new = row.clone();
                new.state = DeliveryState::Exited;
                new.exit = Some(Exit::Delivered);
                new.merge_sha = Some(merge_sha.clone());
                new.version += 1;
                Outcome::applied(new)
            }
            Returned { .. } => {
                let mut new = row.clone();
                new.state = DeliveryState::Exited;
                new.exit = Some(Exit::Returned);
                new.version += 1;
                Outcome::applied(new)
            }
            Requeued { .. } => {
                let mut new = row.clone();
                new.state = DeliveryState::Exited;
                new.exit = Some(Exit::Requeued);
                new.version += 1;
                Outcome::applied(new)
            }
            Cut { .. } | Published { .. } | PublishRed { .. } => illegal(row, &ev.kind),
        },

        // pr mode is external custody: the machine observes merged/closed but never
        // requeues a PR, because a human or the forge already decided its fate.
        DeliveryState::PrOpen => match &ev.kind {
            Delivered { merge_sha, proof: _ } => {
                let mut new = row.clone();
                new.state = DeliveryState::Exited;
                new.exit = Some(Exit::Delivered);
                new.merge_sha = Some(merge_sha.clone());
                new.version += 1;
                Outcome::applied(new)
            }
            Returned { .. } => {
                let mut new = row.clone();
                new.state = DeliveryState::Exited;
                new.exit = Some(Exit::Returned);
                new.version += 1;
                Outcome::applied(new)
            }
            Requeued { .. } | Cut { .. } | Published { .. } | PublishRed { .. } => illegal(row, &ev.kind),
        },

        DeliveryState::Pushing => match &ev.kind {
            Delivered { merge_sha, proof: _ } => {
                let mut new = row.clone();
                new.state = DeliveryState::Exited;
                new.exit = Some(Exit::Delivered);
                new.merge_sha = Some(merge_sha.clone());
                new.version += 1;
                Outcome::applied(new)
            }
            Returned { .. } => {
                let mut new = row.clone();
                new.state = DeliveryState::Exited;
                new.exit = Some(Exit::Returned);
                new.version += 1;
                Outcome::applied(new)
            }
            Requeued { .. } => {
                let mut new = row.clone();
                new.state = DeliveryState::Exited;
                new.exit = Some(Exit::Requeued);
                new.version += 1;
                Outcome::applied(new)
            }
            Cut { .. } | Published { .. } | PublishRed { .. } => illegal(row, &ev.kind),
        },

        // The one exception to "Exited has no further transitions" (module doc): local mode's
        // publish facts land here, never on the bead. Any other mode's Exited row refuses
        // them as illegal, not terminal — the row is not inherently closed to them, only this
        // row's own mode is wrong for them.
        DeliveryState::Exited => match &ev.kind {
            Published { forge_sha } => {
                if row.mode != Mode::Local {
                    return illegal(row, &ev.kind);
                }
                let mut new = row.clone();
                new.state = DeliveryState::Published;
                new.merge_sha = Some(forge_sha.clone());
                new.version += 1;
                Outcome::applied(new)
            }
            PublishRed { fix_forward: _ } => {
                if row.mode != Mode::Local {
                    return illegal(row, &ev.kind);
                }
                let mut new = row.clone();
                new.state = DeliveryState::PublishRed;
                new.version += 1;
                Outcome::applied(new)
            }
            Cut { .. } | Delivered { .. } | Returned { .. } | Requeued { .. } => terminal(row),
        },

        DeliveryState::Published | DeliveryState::PublishRed => match &ev.kind {
            Cut { .. } | Delivered { .. } | Returned { .. } | Requeued { .. } | Published { .. } | PublishRed { .. } => {
                terminal(row)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four exits every mode shares.
    fn core_kinds() -> Vec<DeliveryEventKind> {
        vec![
            DeliveryEventKind::Cut { batch_id: "b1".into() },
            DeliveryEventKind::Delivered { merge_sha: "s".into(), proof: "p".into() },
            DeliveryEventKind::Returned { reason: ReturnedReason::PushRejected },
            DeliveryEventKind::Requeued { tip: "t".into() },
        ]
    }

    /// local mode's own two, legal only from an `Exited` row whose `mode` is `Local`.
    fn publish_kinds() -> Vec<DeliveryEventKind> {
        vec![
            DeliveryEventKind::Published { forge_sha: "f1".into() },
            DeliveryEventKind::PublishRed { fix_forward: "sp-fix".into() },
        ]
    }

    fn kinds() -> Vec<DeliveryEventKind> {
        let mut k = core_kinds();
        k.extend(publish_kinds());
        k
    }

    fn ev(expect: DeliveryState, version: Version, kind: DeliveryEventKind) -> DeliveryEvent {
        DeliveryEvent { expect, version, kind, actor: "test".into() }
    }

    #[test]
    fn every_state_event_pair_has_an_explicit_outcome() {
        for &state in &[
            DeliveryState::Queued,
            DeliveryState::Batched,
            DeliveryState::PrOpen,
            DeliveryState::Pushing,
            DeliveryState::Exited,
            DeliveryState::Published,
            DeliveryState::PublishRed,
        ] {
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
        // A queue-mode Exited row: the four shared exits refuse as Terminal, exactly as
        // before local mode existed.
        let mut row = DeliveryRow::start_queue("sp-x");
        row.state = DeliveryState::Exited;
        for kind in core_kinds() {
            let out = apply(&row, &ev(DeliveryState::Exited, 0, kind));
            assert!(!out.applied);
            assert!(matches!(out.refusal, Some(Refusal::Terminal { .. })));
        }
        // The two publish facts are wrong for this row's mode, not "the row is closed" —
        // refused as IllegalTransition, the same distinction `cut_is_illegal_outside_queue_
        // mode` draws for `Cut` in pr/push mode.
        for kind in publish_kinds() {
            let out = apply(&row, &ev(DeliveryState::Exited, 0, kind));
            assert!(!out.applied);
            assert!(matches!(out.refusal, Some(Refusal::IllegalTransition { .. })));
        }

        // PUBLISHED and PUBLISH_RED are fully terminal, for every kind, regardless of mode.
        for &state in &[DeliveryState::Published, DeliveryState::PublishRed] {
            row.state = state;
            for kind in kinds() {
                let kind_dbg = format!("{kind:?}");
                let out = apply(&row, &ev(state, 0, kind));
                assert!(!out.applied, "{state:?} must absorb {kind_dbg}");
                assert!(matches!(out.refusal, Some(Refusal::Terminal { .. })));
            }
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
        let out = apply(&row, &ev(DeliveryState::Pushing, 0, DeliveryEventKind::Returned { reason: ReturnedReason::PushRejected }));
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

    // ── local mode (design local-main-2026-09-27 §3, row 8): submitted -> batched ->
    // landed_local -> published | publish_red ──────────────────────────────────────────

    #[test]
    fn local_mode_lands_then_publishes_green() {
        let row = DeliveryRow::start_local("sp-x");
        assert_eq!(row.mode, Mode::Local);
        assert_eq!(row.state, DeliveryState::Queued); // "submitted"

        let out = apply(&row, &ev(DeliveryState::Queued, 0, DeliveryEventKind::Cut { batch_id: "round-7".into() }));
        assert!(out.applied);
        assert_eq!(out.row.state, DeliveryState::Batched);

        // "landed_local": a local fast-forward is a Delivered exit like any other mode's.
        let out2 = apply(
            &out.row,
            &ev(DeliveryState::Batched, out.row.version, DeliveryEventKind::Delivered { merge_sha: "localsha1".into(), proof: "local-fast-forward".into() }),
        );
        assert!(out2.applied);
        assert_eq!(out2.row.state, DeliveryState::Exited);
        assert_eq!(out2.row.exit, Some(Exit::Delivered));
        assert_eq!(out2.row.merge_sha.as_deref(), Some("localsha1"));

        // The publish queue later carries that same commit to the forge, green.
        let out3 = apply(
            &out2.row,
            &ev(DeliveryState::Exited, out2.row.version, DeliveryEventKind::Published { forge_sha: "forgesha1".into() }),
        );
        assert!(out3.applied);
        assert_eq!(out3.row.state, DeliveryState::Published);
        assert!(out3.row.state.is_terminal());
    }

    #[test]
    fn local_mode_lands_then_publish_red_never_reopens() {
        let mut row = DeliveryRow::start_local("sp-x");
        row.state = DeliveryState::Exited;
        row.exit = Some(Exit::Delivered);
        row.merge_sha = Some("localsha1".into());

        let out = apply(&row, &ev(DeliveryState::Exited, 0, DeliveryEventKind::PublishRed { fix_forward: "sp-fix1".into() }));
        assert!(out.applied);
        assert_eq!(out.row.state, DeliveryState::PublishRed);

        // Terminal: no second publish attempt on this row, green or red — a fix-forward
        // bead carries the retry on a fresh row instead.
        let again = apply(&out.row, &ev(DeliveryState::PublishRed, out.row.version, DeliveryEventKind::Published { forge_sha: "forgesha2".into() }));
        assert!(!again.applied);
        assert!(matches!(again.refusal, Some(Refusal::Terminal { .. })));
    }

    #[test]
    fn publish_facts_are_refused_before_landed_local() {
        // Not yet Exited: neither publish fact is legal, for any mode.
        for (row, state) in [
            (DeliveryRow::start_local("sp-a"), DeliveryState::Queued),
            (
                {
                    let mut r = DeliveryRow::start_local("sp-b");
                    r.state = DeliveryState::Batched;
                    r
                },
                DeliveryState::Batched,
            ),
        ] {
            for kind in publish_kinds() {
                let out = apply(&row, &ev(state, row.version, kind));
                assert!(!out.applied, "{state:?} must refuse a publish fact before landed_local");
                assert!(matches!(out.refusal, Some(Refusal::IllegalTransition { .. })));
            }
        }
    }

    #[test]
    fn publish_facts_are_refused_for_every_non_local_mode() {
        for mut row in [DeliveryRow::start_queue("sp-a"), DeliveryRow::start_pr("sp-b", 1), DeliveryRow::start_push("sp-c")] {
            row.state = DeliveryState::Exited;
            row.exit = Some(Exit::Delivered);
            for kind in publish_kinds() {
                let out = apply(&row, &ev(DeliveryState::Exited, row.version, kind));
                assert!(!out.applied, "{:?} mode must refuse a publish fact", row.mode);
                assert!(matches!(out.refusal, Some(Refusal::IllegalTransition { .. })));
                assert_eq!(out.row, row, "a refusal must not mutate the row");
            }
        }
    }

    #[test]
    fn a_bead_closes_on_landed_local_and_a_later_publish_never_touches_it() {
        // The cross-machine property the acceptance criteria names: landed_local closes the
        // bead (via the ordinary Delivered cascade every mode already uses); published is
        // then recorded on the delivery row alone, with no further bead event at all.
        use crate::bead::{self, BeadEvent, BeadEventKind, BeadRow, BeadState};

        let mut bead_row = BeadRow::filed("sp-x");
        bead_row.tip = Some("localsha1".into());
        bead_row.state = BeadState::InDelivery;

        let delivery_row = {
            let mut r = DeliveryRow::start_local("sp-x");
            r.state = DeliveryState::Batched;
            r
        };
        let d_out = apply(
            &delivery_row,
            &ev(DeliveryState::Batched, delivery_row.version, DeliveryEventKind::Delivered { merge_sha: "localsha1".into(), proof: "local-fast-forward".into() }),
        );
        assert!(d_out.applied);

        // The cascade a caller performs: the delivery exit's own Delivered kind is replayed
        // as the matching bead event (cutover.rs's `land` does exactly this for queue mode).
        let b_out = bead::apply(
            &bead_row,
            &BeadEvent {
                expect: BeadState::InDelivery,
                version: bead_row.version,
                kind: BeadEventKind::Delivered { merge_sha: "localsha1".into(), proof: "local-fast-forward".into() },
                actor: "queue.sh land-local".into(),
            },
        );
        assert!(b_out.applied);
        assert_eq!(b_out.row.state, BeadState::Landed);
        let closed_bead = b_out.row;

        // Now the publish queue reports green. Nothing here ever calls bead::apply again —
        // the closed bead is simply never touched.
        let p_out = apply(&d_out.row, &ev(DeliveryState::Exited, d_out.row.version, DeliveryEventKind::Published { forge_sha: "forgesha1".into() }));
        assert!(p_out.applied);
        assert_eq!(p_out.row.state, DeliveryState::Published);
        assert_eq!(closed_bead.state, BeadState::Landed, "the bead row is untouched by the publish fact");
    }
}
