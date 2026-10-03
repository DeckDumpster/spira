//! Replay: rebuild a row from nothing but its event log. The materialized `bead`/`delivery`/
//! `batch` rows are a cache of the log (design §3.3); this module is the fold that proves the
//! cache and the log never diverge (design §7: "replay-equals-state").
//!
//! Because a refusal never mutates its row (every machine's `apply` returns the input row
//! unchanged when `applied` is false), folding the *entire* log — refusals included — through
//! `apply` in order reproduces the live row exactly. There is nothing to filter and nothing
//! to special-case; that is the property this module exists to demonstrate.

use crate::batch::{self, BatchEvent, BatchRow};
use crate::bead::{self, BeadEvent, BeadRow};
use crate::delivery::{self, DeliveryEvent, DeliveryRow};

pub fn fold_bead(bead_id: &str, events: &[BeadEvent]) -> BeadRow {
    let mut row = BeadRow::filed(bead_id);
    for ev in events {
        row = bead::apply(&row, ev).row;
    }
    row
}

pub fn fold_delivery(initial: DeliveryRow, events: &[DeliveryEvent]) -> DeliveryRow {
    let mut row = initial;
    for ev in events {
        row = delivery::apply(&row, ev).row;
    }
    row
}

pub fn fold_batch(initial: BatchRow, events: &[BatchEvent]) -> BatchRow {
    let mut row = initial;
    for ev in events {
        row = batch::apply(&row, ev).row;
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead::{BeadEventKind, BeadState};
    use crate::batch::{BatchEventKind, BatchState};
    use crate::delivery::{DeliveryEventKind, DeliveryState};
    use crate::reason::GateRedReason;

    fn bead_ev(expect: BeadState, version: u64, kind: BeadEventKind) -> BeadEvent {
        BeadEvent { expect, version, kind, actor: "test".into(), at: None }
    }

    /// Byte-for-byte, per the acceptance criteria: replaying the whole log from scratch
    /// must equal the row built incrementally, one event at a time, as it happened live.
    #[test]
    fn bead_replay_matches_incremental_application_byte_for_byte() {
        let log = vec![
            bead_ev(BeadState::Ready, 0, BeadEventKind::Claim { holder: "aeon-1".into(), lease_until: 10, stack: bead::Stack::new(), stack_depth: 0, stack_max_depth: 4 }),
            bead_ev(BeadState::Working, 1, BeadEventKind::Submit { tip: "sha-a".into() }),
            // A stale gate_pass for a tip that no longer applies: refused, must be a no-op.
            bead_ev(BeadState::Submitted, 2, BeadEventKind::GatePass { tip: "stale".into(), gate_key: "k0".into() }),
            bead_ev(BeadState::Submitted, 2, BeadEventKind::GatePass { tip: "sha-a".into(), gate_key: "k1".into() }),
            bead_ev(BeadState::Certified, 3, BeadEventKind::Deliver),
            bead_ev(BeadState::InDelivery, 4, BeadEventKind::Delivered { merge_sha: "m1".into(), proof: "ancestry".into() }),
        ];

        // Incremental: apply one at a time, as a live system would, keeping the running row.
        let mut incremental = BeadRow::filed("sp-replay");
        for ev in &log {
            incremental = bead::apply(&incremental, ev).row;
        }

        // Cold replay: fold the entire log from scratch, as a rebuild would.
        let replayed = fold_bead("sp-replay", &log);

        assert_eq!(incremental, replayed);
        assert_eq!(replayed.state, BeadState::Landed);
        // The refused stale gate_pass must not have left any trace (e.g. gate_key k0).
        assert_eq!(replayed.gate_key.as_deref(), Some("k1"));
    }

    #[test]
    fn bead_replay_is_stable_under_re_chunking() {
        let log = vec![
            bead_ev(BeadState::Ready, 0, BeadEventKind::Claim { holder: "aeon-1".into(), lease_until: 10, stack: bead::Stack::new(), stack_depth: 0, stack_max_depth: 4 }),
            bead_ev(BeadState::Working, 1, BeadEventKind::Submit { tip: "sha-a".into() }),
            bead_ev(BeadState::Submitted, 2, BeadEventKind::GateRed { tip: "sha-a".into(), reason: GateRedReason::SuitesFailed }),
            bead_ev(BeadState::Rework, 3, BeadEventKind::Claim { holder: "aeon-2".into(), lease_until: 20, stack: bead::Stack::new(), stack_depth: 0, stack_max_depth: 4 }),
        ];

        let whole = fold_bead("sp-x", &log);

        // Split the same log into two chunks and fold them independently through the same
        // row, mirroring a rebuild that processes the log in pages.
        let (first, second) = log.split_at(2);
        let partial = fold_bead("sp-x", first);
        let mut resumed = partial;
        for ev in second {
            resumed = bead::apply(&resumed, ev).row;
        }

        assert_eq!(whole, resumed);
    }

    #[test]
    fn delivery_replay_matches_incremental_application() {
        let log = vec![
            DeliveryEvent { expect: DeliveryState::Queued, version: 0, kind: DeliveryEventKind::Cut { batch_id: "b1".into() }, actor: "batcher".into() },
            DeliveryEvent { expect: DeliveryState::Batched, version: 1, kind: DeliveryEventKind::Delivered { merge_sha: "m1".into(), proof: "ancestry".into() }, actor: "batch".into() },
        ];
        let initial = DeliveryRow::start_queue("sp-x");

        let mut incremental = initial.clone();
        for ev in &log {
            incremental = delivery::apply(&incremental, ev).row;
        }
        let replayed = fold_delivery(initial, &log);
        assert_eq!(incremental, replayed);
    }

    #[test]
    fn batch_replay_matches_incremental_application() {
        let log = vec![
            BatchEvent { expect: BatchState::Open, version: 0, kind: BatchEventKind::CiStarted { run: "r1".into() }, actor: "batcher".into() },
            BatchEvent { expect: BatchState::CiRunning, version: 1, kind: BatchEventKind::Green, actor: "verdict".into() },
            BatchEvent { expect: BatchState::Green, version: 2, kind: BatchEventKind::FastForward { sha: "sha1".into() }, actor: "batcher".into() },
        ];
        let initial = BatchRow::cut("b1", "spira", "head1", "base1");

        let mut incremental = initial.clone();
        for ev in &log {
            incremental = batch::apply(&incremental, ev).row;
        }
        let replayed = fold_batch(initial, &log);
        assert_eq!(incremental, replayed);
        assert_eq!(replayed.state, BatchState::Landed);
    }
}
