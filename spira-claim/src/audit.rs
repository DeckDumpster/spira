//! `spira-claim audit` — what every claimable bead is carrying on the retry ladder, and
//! why. Replaces `spira/attempts.sh audit` (DESIGN.md §9).
//!
//! WHY THE CAUSES ARE DIFFERENT FROM attempts.sh's. The old audit read `sp-attempt-N-cause`
//! / `sp-reclaim-N-cause` bd LABELS through `counter_causes` — a representation `bump_counter`
//! stopped writing at sp-lzt ("counter labels are no longer written"). Every bead claimed
//! since then carries real attempts, requeues and reclaims with no such label, so the old
//! audit's per-cause breakdown has been silently empty for every bead this matters for. This
//! reads the same events-table fold `attempts`/`requeues`/`counts` already answer from
//! (events::fold), which is where the count itself has come from since sp-j1q6o — the
//! causes it prints are the ones actually charging the bead today, not a representation
//! nothing has written in months.

use std::collections::HashMap;

use crate::events::{self, EventRow};
use crate::rank::{LifecycleRow, ReadyRow};

fn poisoned(id: &str, lc: &HashMap<String, LifecycleRow>) -> bool {
    lc.get(id).is_some_and(|r| r.holds.contains(&lifecycle::bead::HoldKind::Poison))
}

/// `candidates`: every bead a persona's partition could claim (attempts.sh's own
/// `uniq_candidates` — exclusions dropped on purpose, so a poisoned or asked-about bead,
/// exactly the one whose count most needs reading, is never filtered out before this runs).
/// `events_by`: every folded row for those ids. `lc`: the lifecycle snapshot — the poison is
/// the machine's `poison` hold, never a bd label.
pub fn run(
    candidates: &[ReadyRow],
    events_by: &HashMap<String, Vec<EventRow>>,
    lc: &HashMap<String, LifecycleRow>,
) -> String {
    let mut out = String::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut n = 0u32;
    let mut total = 0u32;
    for row in candidates {
        if !seen.insert(row.id.clone()) {
            continue;
        }
        total += 1;
        let rows: &[EventRow] = events_by.get(&row.id).map(Vec::as_slice).unwrap_or(&[]);
        let l = events::fold(&row.id, rows);
        if l.attempts == 0 && l.requeues == 0 && l.reclaims == 0 {
            continue;
        }
        n += 1;
        let is_poisoned = poisoned(&row.id, lc);
        out.push_str(&format!(
            "{:<20} attempts={:<3} reclaims={:<3} requeues={:<3} {}\n",
            row.id,
            l.attempts,
            l.reclaims,
            l.requeues,
            if is_poisoned { "POISONED" } else { "" }
        ));
        for a in &l.attempt_log {
            let cause = match &a.outcome {
                events::AttemptOutcome::Charged(c) => format!("charged: {c}"),
                events::AttemptOutcome::Exempt(c) => format!("exempt: {c}"),
                events::AttemptOutcome::Succeeded => continue,
            };
            out.push_str(&format!("    attempt {cause}\n"));
        }
        for r in l.returns.iter().filter(|r| r.counts) {
            out.push_str(&format!("    requeue {}\n", r.cause));
        }
        for e in rows.iter().filter(|e| e.event_type == "reclaimed") {
            out.push_str(&format!(
                "    reclaim {}\n",
                e.new_value.as_deref().unwrap_or("unrecorded")
            ));
        }
    }
    out.push_str(&format!(
        "--- {n} bead(s) carrying counters, of {total} claimable\n"
    ));
    out
}

#[cfg(test)]
#[path = "audit_tests.rs"]
mod tests;
