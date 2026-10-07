//! `audit` — a pure fold over rows a caller already fetched. No store.

use super::*;
use crate::events::EventRow;
use std::collections::HashMap;

fn row(id: &str, labels: &[&str]) -> ReadyRow {
    ReadyRow {
        id: id.into(),
        labels: labels.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

#[test]
fn a_bead_with_nothing_charged_is_not_printed() {
    let out = run(
        &[row("sp-a", &[])],
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(!out.contains("sp-a"), "{out}");
    assert!(
        out.contains("--- 0 bead(s) carrying counters, of 1 claimable"),
        "{out}"
    );
}

#[test]
fn zero_is_a_claim_and_the_denominator_proves_it() {
    let out = run(
        &[row("sp-a", &[]), row("sp-b", &[])],
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(
        out.contains("--- 0 bead(s) carrying counters, of 2 claimable"),
        "{out}"
    );
}

#[test]
fn duplicate_candidates_are_counted_once() {
    let out = run(
        &[row("sp-a", &[]), row("sp-a", &[])],
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(out.contains("of 1 claimable"), "{out}");
}

#[test]
fn attempts_and_requeue_causes_come_from_the_fold_not_a_label() {
    let mut ev: HashMap<String, Vec<EventRow>> = HashMap::new();
    ev.insert(
        "sp-c".into(),
        vec![
            EventRow::new("sp-c", "claimed", "", "2026-09-01T00:00:00Z"),
            EventRow::new("sp-c", "reopen", "gate-red", "2026-09-01T01:00:00Z"),
        ],
    );
    let out = run(&[row("sp-c", &[])], &ev, &HashMap::new());
    assert!(out.contains("sp-c"), "{out}");
    assert!(out.contains("attempts=1"), "{out}");
    assert!(out.contains("attempt charged: still open"), "{out}");
    assert!(out.contains("requeues=1"), "{out}");
    assert!(out.contains("requeue gate-red"), "{out}");
}

#[test]
fn reclaim_causes_come_from_the_raw_reclaimed_rows() {
    let mut ev: HashMap<String, Vec<EventRow>> = HashMap::new();
    ev.insert(
        "sp-r".into(),
        vec![
            EventRow::new("sp-r", "claimed", "", "2026-09-01T00:00:00Z"),
            EventRow::new("sp-r", "reclaimed", "ghost", "2026-09-01T00:05:00Z"),
        ],
    );
    let out = run(&[row("sp-r", &[])], &ev, &HashMap::new());
    assert!(out.contains("reclaims=1"), "{out}");
    assert!(out.contains("reclaim ghost"), "{out}");
}

#[test]
fn poisoned_reads_the_lifecycle_snapshot_never_the_label() {
    use lifecycle::bead::{BeadState, HoldKind};
    let mut ev: HashMap<String, Vec<EventRow>> = HashMap::new();
    ev.insert(
        "sp-p".into(),
        vec![EventRow::new("sp-p", "claimed", "", "2026-09-01T00:00:00Z")],
    );
    // Carries the legacy label: the label must not be read.
    let mut lc = HashMap::new();
    lc.insert(
        "sp-p".to_string(),
        LifecycleRow {
            bead_id: "sp-p".into(),
            state: BeadState::Ready,
            holds: [HoldKind::Poison].into(),
            stack_depth: 0,
            tip: None,
            snoozed_until: None,
        },
    );
    let out = run(
        &[row("sp-p", &["spira-poison"])],
        &ev,
        &lc,
    );
    assert!(out.contains("POISONED"), "{out}");
    let mut lc_clear = HashMap::new();
    lc_clear.insert(
        "sp-p".to_string(),
        LifecycleRow {
            bead_id: "sp-p".into(),
            state: BeadState::Ready,
            holds: Default::default(),
            stack_depth: 0,
            tip: None,
            snoozed_until: None,
        },
    );
    let out2 = run(
        &[row("sp-p", &["spira-poison"])],
        &ev,
        &lc_clear,
    );
    assert!(
        !out2.contains("POISONED"),
        "the label is not the poison: {out2}"
    );
}
