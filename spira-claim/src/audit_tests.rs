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
