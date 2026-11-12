//! The pass's pure checks, for the binary's pass and the scheduler's `sentinel-pass` task:
//! CHECK 2's reaper and consistency sweep, and CHECK 3b's queue-waiter decision.

use std::collections::HashSet;

use crate::model::{Bead, LcRow};
use spira_store::lc_state;

/// CHECK 2 (reaper half): WORKING rows, not wait-held, whose lease expired more than `grace`
/// ago, or has expired at all with no live holder → (id, seconds since expiry).
/// A holder whose session is provably gone (`gone`) is reaped at once, lease or no lease.
pub fn stale_leases(rows: &[LcRow], now: i64, grace: i64, alive: impl Fn(&str) -> bool, gone: impl Fn(&str) -> bool) -> Vec<(String, i64)> {
    rows.iter()
        .filter(|r| r.state == "WORKING" && !r.holds.iter().any(|h| h == "wait"))
        .filter_map(|r| {
            let past = now - r.lease_until.unwrap_or(now);
            let session_gone = r.holder.as_deref().is_some_and(&gone);
            (session_gone || (r.lease_until.is_some() && (past > grace || (past > 0 && !alive(&r.bead_id))))).then(|| (r.bead_id.clone(), past))
        })
        .collect()
}

/// SUBMITTED, CERTIFIED and IN_DELIVERY keep the holder on purpose (branch affinity); only
/// the states the lifecycle machine clears it on are a violation.
fn holder_forbidden(state: &str) -> bool {
    matches!(state, "OPEN" | "READY" | "REWORK" | "LANDED" | "SUPERSEDED" | "DROPPED" | "DONE")
}

/// CHECK 2c: rows whose holder and state disagree.
pub fn inconsistent(rows: &[LcRow]) -> Vec<String> {
    rows.iter()
        .filter_map(|r| {
            if r.state == "WORKING" && r.holder.is_none() {
                Some(format!(
                    "INCONSISTENT\t{}\tWORKING with no holder",
                    r.bead_id
                ))
            } else if holder_forbidden(&r.state) && r.holder.is_some() {
                Some(format!(
                    "INCONSISTENT\t{}\t{} with a holder still set",
                    r.bead_id, r.state
                ))
            } else {
                None
            }
        })
        .collect()
}

/// Active queue blockers: spira-lc rows in CERTIFIED or IN_DELIVERY — the queue pipeline's
/// states. A tipless design/diagnosis bead reaches a terminal state, never these.
pub fn active_blockers_lc(rows: &[LcRow]) -> HashSet<String> {
    rows.iter()
        .filter(|r| matches!(r.state.as_str(), "CERTIFIED" | "IN_DELIVERY"))
        .map(|r| r.bead_id.clone())
        .collect()
}

/// One decision per ready bead (add when it blocks on an active blocker and is not yet
/// labeled, remove when it no longer does but is), plus an unconditional remove for every
/// labeled id absent from the ready set (lib.sh: "ready dep data is authoritative; a
/// labeled-only bead gets no dep check — removing the label is safe since the bead is not
/// claimable"). `true` = add, `false` = remove.
pub fn decide(active: &HashSet<String>, labeled: &HashSet<String>, ready: &[Bead]) -> Vec<(bool, String)> {
    let mut by_id: std::collections::BTreeMap<&str, &Bead> = std::collections::BTreeMap::new();
    for b in ready {
        by_id.insert(b.id.as_str(), b);
    }
    let mut out = Vec::new();
    for (id, b) in &by_id {
        let currently = labeled.contains(*id);
        let want = !active.is_empty()
            && b.dependencies.iter().any(|d| {
                d.r#type.as_deref() == Some("blocks")
                    && d.target().map(|t| active.contains(t)).unwrap_or(false)
            });
        if want && !currently {
            out.push((true, id.to_string()));
        } else if !want && currently {
            out.push((false, id.to_string()));
        }
    }
    let mut gone: Vec<&String> = labeled.iter().filter(|id| !by_id.contains_key(id.as_str())).collect();
    gone.sort();
    out.extend(gone.into_iter().map(|id| (false, id.clone())));
    out
}

/// The labeled beads whose lifecycle row is claimable (READY/REWORK).
pub fn labeled_claimable(rows: &[LcRow], labeled: Vec<Bead>) -> HashSet<String> {
    let mut out = HashSet::new();
    for b in labeled {
        if rows.iter().any(|r| r.bead_id == b.id && lc_state::is_claimable(&r.state)) {
            out.insert(b.id);
        }
    }
    out
}

