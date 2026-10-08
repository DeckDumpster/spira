//! Pure core: legacy bd events and post-cutover `spira_lifecycle.event`
//! rows both become `bead-stage` rows here, through `lifecycle::classify` itself (design
//! reconciler-time-series-2026-09-27 §2a.1: "call the same crate functions once per legacy
//! event, so there is still one mapping, owned by the lifecycle crate"). No I/O: main.rs
//! reads bd and `spira_lifecycle`, and appends what this module builds. (The landing
//! ledger it once also read is deleted, sp-2c1n0.)

use std::collections::BTreeMap;

use lifecycle::bead::BeadState;
use lifecycle::classify::{self, BdStatus, BeadFacts};

/// One `bead-stage` row, minus the envelope (`ts`/`host`/`family`) `tsd-write` adds — `ts`
/// is carried here anyway because it is the *source* event's own clock, not "now"
/// (law-producers-stamp-their-own-clock: the producer is this exporter, but what it is
/// stamping is a fact that already happened at a specific time, so `--ts` is passed
/// explicitly rather than left to `tsd-write`'s "now" default).
#[derive(Debug, Clone, PartialEq)]
pub struct StageRow {
    pub ts: String,
    pub seq: i64,
    pub machine: String,
    pub key: String,
    pub event: String,
    pub from_state: String,
    pub to_state: String,
    pub applied: bool,
    pub refusal: Option<String>,
    pub reason: Option<String>,
    pub actor: String,
    pub source: String,
}

/// One row of bd's audit `events` table: `id, issue_id, event_type, actor, new_value,
/// created_at` — the columns `spira/lib.sh`'s own writers and readers already use. Derives
/// `Deserialize` so main.rs can parse `bd sql --json`'s output straight into this shape.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct BdAuditEvent {
    pub id: String,
    pub issue_id: String,
    pub event_type: String,
    pub actor: String,
    pub new_value: Option<String>,
    pub created_at: String,
}

fn default_facts() -> BeadFacts {
    BeadFacts {
        bd_status: BdStatus::Open,
        holder_alive: false,
        labels: Default::default(),
        has_ask_hold: false,
        supersedes: None,
        content_on_base: false,
        batch_open_member: false,
        branch_ahead: false,
        landing_commit: None,
    }
}

/// Applies one legacy event's effect to a running snapshot, in place. Only the facts bd's
/// audit trail actually attests to are ever touched — batch membership, git ancestry and
/// content-on-base are never derivable from this table, so they stay at whatever the caller
/// seeded (`default_facts`'s own defaults), same as `classify`'s contract for a fact nobody
/// supplied evidence for. Every other `event_type` (recurred, lapsed, poison.cleared, a bare
/// `updated`, ...) leaves the snapshot unchanged — the row this event still produces simply
/// carries `from_state == to_state`, an honest "no distinguishable transition" rather than a
/// fabricated one.
fn apply_legacy_event(facts: &mut BeadFacts, ev: &BdAuditEvent) {
    let new_value = ev.new_value.as_deref().unwrap_or("");
    match ev.event_type.as_str() {
        "claimed" => {
            facts.bd_status = BdStatus::InProgress;
            facts.holder_alive = true;
        }
        "status_changed" if new_value.contains("in_progress") => {
            facts.bd_status = BdStatus::InProgress;
            facts.holder_alive = true;
        }
        "closed" => facts.bd_status = BdStatus::Closed,
        "reopened" | "reclaimed" => {
            facts.bd_status = BdStatus::Open;
            facts.holder_alive = false;
        }
        _ => {}
    }
}

/// Folds a chronologically-ordered (`created_at`, `id`) slice of legacy events for every bead
/// they touch into `bead-stage` rows, in the same order. Every event classifies from bd's
/// own facts alone (`classify`'s no-ledger case).
///
/// `next_seq` is the exporter's own running counter for this source (carried in the
/// checkpoint across runs) — bd's audit `events` table has no numeric ordering column of its
/// own (`id` is a UUID), so this is what gives `bead-stage` rows from this source a `seq` at
/// all.
pub fn fold_legacy(
    events: &[BdAuditEvent],
    source: &str,
    next_seq: i64,
) -> Vec<StageRow> {
    let mut facts_by_bead: BTreeMap<String, BeadFacts> = BTreeMap::new();
    let mut state_by_bead: BTreeMap<String, BeadState> = BTreeMap::new();
    let mut rows = Vec::with_capacity(events.len());
    let mut seq = next_seq;

    for ev in events.iter() {
        let facts = facts_by_bead.entry(ev.issue_id.clone()).or_insert_with(default_facts);
        apply_legacy_event(facts, ev);
        let outcome = classify::classify(facts);
        let from_state = state_by_bead
            .get(&ev.issue_id)
            .copied()
            .unwrap_or_else(|| classify::classify(&default_facts()).state);

        rows.push(StageRow {
            ts: ev.created_at.clone(),
            seq,
            machine: "bead".to_string(),
            key: ev.issue_id.clone(),
            event: ev.event_type.clone(),
            from_state: from_state.as_str().to_string(),
            to_state: outcome.state.as_str().to_string(),
            applied: true,
            refusal: None,
            reason: ev.new_value.clone(),
            actor: ev.actor.clone(),
            source: source.to_string(),
        });
        state_by_bead.insert(ev.issue_id.clone(), outcome.state);
        seq += 1;
    }
    rows
}

/// The high-water mark for the legacy/backfill sources: bd's audit `events` table orders by
/// `created_at`, but ties on that column are common (many rows land in the same second on a
/// busy harness) and `id` is a UUID, not a sequence — so the boundary instant needs its own
/// set of already-seen ids, not just a scalar cursor.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LegacyCheckpoint {
    pub last_created_at: String,
    pub last_ids: Vec<String>,
    pub next_seq: i64,
}

impl LegacyCheckpoint {
    pub fn starting_at(since: &str) -> Self {
        LegacyCheckpoint { last_created_at: since.to_string(), last_ids: Vec::new(), next_seq: 1 }
    }
}

/// Splits `events` (already sorted by `created_at`, `id`) into the ones not yet exported per
/// `cp`, and returns the checkpoint advanced past them. Passing the *entire* result back
/// through this on every run, unconditionally, is what makes re-running export nothing new:
/// every row at or before the checkpoint is filtered out before `fold_legacy` ever sees it.
pub fn advance_legacy_checkpoint(events: &[BdAuditEvent], cp: &LegacyCheckpoint) -> (Vec<BdAuditEvent>, LegacyCheckpoint) {
    let mut fresh = Vec::new();
    for ev in events {
        if ev.created_at.as_str() < cp.last_created_at.as_str() {
            continue;
        }
        if ev.created_at == cp.last_created_at && cp.last_ids.contains(&ev.id) {
            continue;
        }
        fresh.push(ev.clone());
    }

    let mut new_cp = cp.clone();
    new_cp.next_seq += fresh.len() as i64;
    if let Some(max_ts) = fresh.iter().map(|e| e.created_at.clone()).max() {
        let mut ids: Vec<String> = fresh.iter().filter(|e| e.created_at == max_ts).map(|e| e.id.clone()).collect();
        if max_ts == cp.last_created_at {
            ids.extend(cp.last_ids.iter().cloned());
        }
        ids.sort();
        ids.dedup();
        new_cp.last_created_at = max_ts;
        new_cp.last_ids = ids;
    }
    (fresh, new_cp)
}

/// The high-water mark for the lifecycle source: the `event` table's own `seq` is a real
/// AUTO_INCREMENT primary key, so unlike the legacy sources this is a single scalar cursor.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LifecycleCheckpoint {
    pub last_seq: i64,
}

/// One row of the post-cutover `spira_lifecycle.event` table (`lifecycle/schema.sql`),
/// already in the machine's own vocabulary — this source needs no classifier at all, only a
/// column rename and a best-effort `reason` pulled from its JSON `evidence`.
#[derive(Debug, Clone, PartialEq)]
pub struct LifecycleEventRow {
    pub seq: i64,
    pub machine: String,
    pub lc_key: String,
    pub event: String,
    pub from_state: String,
    pub to_state: String,
    pub applied: bool,
    pub refusal: Option<String>,
    pub evidence: serde_json::Value,
    pub actor: String,
    pub at: i64,
}

/// `evidence` is an arbitrary `BeadEventKind`/`DeliveryEventKind`/`BatchEventKind` payload
/// (`serde_json::to_value` of the Rust enum) — every free-text cause the lifecycle crate
/// carries today happens to live under one of these three keys (`Returned{reason}`,
/// `GateRed{reason}`, `Drop{reason}`, `Hold{cause}`, `Supersede{by}`); anything else has no
/// single free-text cause to surface, and `reason` is simply absent (never fabricated).
fn reason_from_evidence(evidence: &serde_json::Value) -> Option<String> {
    let obj = evidence.as_object()?;
    for key in ["reason", "cause", "by"] {
        if let Some(v) = obj.get(key).and_then(|v| v.as_str()) {
            return Some(v.to_string());
        }
    }
    None
}

pub fn map_lifecycle_row(ev: &LifecycleEventRow) -> StageRow {
    StageRow {
        ts: epoch_to_iso(ev.at),
        seq: ev.seq,
        machine: ev.machine.clone(),
        key: ev.lc_key.clone(),
        event: ev.event.clone(),
        from_state: ev.from_state.clone(),
        to_state: ev.to_state.clone(),
        applied: ev.applied,
        refusal: ev.refusal.clone(),
        reason: reason_from_evidence(&ev.evidence),
        actor: ev.actor.clone(),
        source: "lifecycle".to_string(),
    }
}

/// Epoch seconds (UTC) to `YYYY-MM-DDTHH:MM:SSZ`, the same stamp `tsd-write`'s own `--ts`
/// expects. Hand-rolled (civil-from-days, Howard Hinnant's algorithm) rather than a `chrono`
/// dependency nothing else in this workspace pulls in.
pub fn epoch_to_iso(epoch_s: i64) -> String {
    let days = epoch_s.div_euclid(86400);
    let secs_of_day = epoch_s.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let hh = secs_of_day / 3600;
    let mm = (secs_of_day % 3600) / 60;
    let ss = secs_of_day % 60;
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(id: &str, issue: &str, kind: &str, actor: &str, new_value: Option<&str>, at: &str) -> BdAuditEvent {
        BdAuditEvent {
            id: id.to_string(),
            issue_id: issue.to_string(),
            event_type: kind.to_string(),
            actor: actor.to_string(),
            new_value: new_value.map(str::to_string),
            created_at: at.to_string(),
        }
    }

    // ── legacy fold: every event maps to exactly one row, with lifecycle state names ──────

    #[test]
    fn claim_then_close_maps_ready_to_working_to_dropped() {
        let events = vec![
            ev("1", "sp-a", "claimed", "aeon-1", None, "2026-09-15T00:00:00Z"),
            ev("2", "sp-a", "closed", "aeon-1", None, "2026-09-15T00:05:00Z"),
        ];
        let rows = fold_legacy(&events, "legacy", 1);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].from_state, "READY");
        assert_eq!(rows[0].to_state, "WORKING");
        assert_eq!(rows[0].event, "claimed");
        assert_eq!(rows[0].seq, 1);
        assert_eq!(rows[1].from_state, "WORKING");
        // No landing or content evidence at all: classify()'s own default for a bare bd
        // "closed" is the residue rule, DROPPED — an honest limitation of this source, not
        // a defect, stated in this module's own doc comment.
        assert_eq!(rows[1].to_state, "DROPPED");
        assert_eq!(rows[1].seq, 2);
    }

    #[test]
    fn reopened_returns_to_ready() {
        let events = vec![
            ev("1", "sp-c", "claimed", "aeon-1", None, "2026-09-15T00:00:00Z"),
            ev("2", "sp-c", "reopened", "harness", None, "2026-09-15T00:05:00Z"),
        ];
        let rows = fold_legacy(&events, "legacy", 1);
        assert_eq!(rows[1].to_state, "READY");
    }

    #[test]
    fn an_event_type_with_no_fact_effect_still_emits_one_row_with_from_equal_to_to() {
        let events = vec![ev("1", "sp-d", "recurred", "harness", Some("closed-not-landed"), "2026-09-15T00:00:00Z")];
        let rows = fold_legacy(&events, "legacy", 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].from_state, rows[0].to_state);
        assert_eq!(rows[0].reason.as_deref(), Some("closed-not-landed"));
    }

    #[test]
    fn two_beads_interleaved_do_not_share_state() {
        let events = vec![
            ev("1", "sp-e", "claimed", "aeon-1", None, "2026-09-15T00:00:00Z"),
            ev("2", "sp-f", "claimed", "aeon-2", None, "2026-09-15T00:00:01Z"),
            ev("3", "sp-e", "closed", "aeon-1", None, "2026-09-15T00:00:02Z"),
        ];
        let rows = fold_legacy(&events, "legacy", 1);
        assert_eq!(rows[1].to_state, "WORKING", "sp-f's own claim must classify sp-f, not sp-e");
        assert_eq!(rows[2].from_state, "WORKING", "sp-e's close follows sp-e's own claim");
    }

    #[test]
    fn seq_is_assigned_in_order_starting_from_next_seq() {
        let events = vec![
            ev("1", "sp-g", "claimed", "aeon-1", None, "2026-09-15T00:00:00Z"),
            ev("2", "sp-g", "closed", "aeon-1", None, "2026-09-15T00:00:01Z"),
        ];
        let rows = fold_legacy(&events, "legacy", 41);
        assert_eq!(rows[0].seq, 41);
        assert_eq!(rows[1].seq, 42);
    }

    // ── the checkpoint: re-running exports nothing new ────────────────────────────────────

    #[test]
    fn advancing_past_every_event_leaves_nothing_fresh_on_a_second_call() {
        let events = vec![
            ev("1", "sp-h", "claimed", "aeon-1", None, "2026-09-15T00:00:00Z"),
            ev("2", "sp-h", "closed", "aeon-1", None, "2026-09-15T00:00:01Z"),
        ];
        let cp0 = LegacyCheckpoint::starting_at("2026-09-15T00:00:00Z");
        let (fresh1, cp1) = advance_legacy_checkpoint(&events, &cp0);
        assert_eq!(fresh1.len(), 2);
        let (fresh2, cp2) = advance_legacy_checkpoint(&events, &cp1);
        assert!(fresh2.is_empty(), "re-running the same query must export nothing new");
        assert_eq!(cp1, cp2, "a no-op run must not perturb the checkpoint");
    }

    #[test]
    fn a_tie_at_the_boundary_instant_is_deduped_by_id_not_dropped_wholesale() {
        let cp0 = LegacyCheckpoint::starting_at("2026-09-15T00:00:00Z");
        let first_batch = vec![
            ev("1", "sp-i", "claimed", "aeon-1", None, "2026-09-15T00:00:05Z"),
            ev("2", "sp-j", "claimed", "aeon-2", None, "2026-09-15T00:00:05Z"),
        ];
        let (fresh1, cp1) = advance_legacy_checkpoint(&first_batch, &cp0);
        assert_eq!(fresh1.len(), 2);
        // A third event lands at the exact same instant on a later poll — must still be
        // picked up, not skipped just because the checkpoint's timestamp already equals it.
        let second_batch = vec![
            first_batch[0].clone(),
            first_batch[1].clone(),
            ev("3", "sp-k", "claimed", "aeon-3", None, "2026-09-15T00:00:05Z"),
        ];
        let (fresh2, _cp2) = advance_legacy_checkpoint(&second_batch, &cp1);
        assert_eq!(fresh2.len(), 1);
        assert_eq!(fresh2[0].id, "3");
    }

    #[test]
    fn next_seq_advances_by_exactly_the_number_of_fresh_rows() {
        let cp0 = LegacyCheckpoint::starting_at("2026-09-15T00:00:00Z");
        let events = vec![
            ev("1", "sp-l", "claimed", "aeon-1", None, "2026-09-15T00:00:05Z"),
            ev("2", "sp-l", "closed", "aeon-1", None, "2026-09-15T00:00:06Z"),
        ];
        let (_fresh, cp1) = advance_legacy_checkpoint(&events, &cp0);
        assert_eq!(cp1.next_seq, 3);
        let (fresh2, cp2) = advance_legacy_checkpoint(&events, &cp1);
        assert!(fresh2.is_empty());
        assert_eq!(cp2.next_seq, 3, "next_seq must not advance when nothing fresh was found");
    }

    // ── lifecycle source: a straight column mapping, refusals included ────────────────────

    #[test]
    fn an_applied_lifecycle_event_maps_with_reason_from_evidence() {
        let row = LifecycleEventRow {
            seq: 7,
            machine: "bead".to_string(),
            lc_key: "sp-m".to_string(),
            event: "GateRed".to_string(),
            from_state: "SUBMITTED".to_string(),
            to_state: "REWORK".to_string(),
            applied: true,
            refusal: None,
            evidence: serde_json::json!({"tip": "sha1", "reason": "flaky"}),
            actor: "verdict".to_string(),
            at: 1758000000,
        };
        let out = map_lifecycle_row(&row);
        assert_eq!(out.seq, 7);
        assert_eq!(out.machine, "bead");
        assert_eq!(out.key, "sp-m");
        assert_eq!(out.from_state, "SUBMITTED");
        assert_eq!(out.to_state, "REWORK");
        assert!(out.applied);
        assert_eq!(out.reason.as_deref(), Some("flaky"));
        assert_eq!(out.source, "lifecycle");
    }

    #[test]
    fn an_attempt_history_fact_exports_with_its_cause_as_the_reason() {
        let row = LifecycleEventRow {
            seq: 9,
            machine: "fact".to_string(),
            lc_key: "sp-m".to_string(),
            event: "requeued".to_string(),
            from_state: String::new(),
            to_state: String::new(),
            applied: true,
            refusal: None,
            evidence: serde_json::json!({"cause": "gate-red"}),
            actor: "harness".to_string(),
            at: 1758000000,
        };
        let out = map_lifecycle_row(&row);
        assert_eq!((out.machine.as_str(), out.event.as_str()), ("fact", "requeued"));
        assert_eq!(out.reason.as_deref(), Some("gate-red"));
    }

    #[test]
    fn a_refused_lifecycle_event_still_yields_one_row_with_applied_false() {
        let row = LifecycleEventRow {
            seq: 8,
            machine: "batch".to_string(),
            lc_key: "b1".to_string(),
            event: "FastForward".to_string(),
            from_state: "GREEN".to_string(),
            to_state: "GREEN".to_string(),
            applied: false,
            refusal: Some("StaleVersion".to_string()),
            evidence: serde_json::json!({"sha": "s1"}),
            actor: "batcher".to_string(),
            at: 1758000010,
        };
        let out = map_lifecycle_row(&row);
        assert!(!out.applied);
        assert_eq!(out.refusal.as_deref(), Some("StaleVersion"));
        assert_eq!(out.reason, None, "no reason/cause/by key in this evidence — must not fabricate one");
    }

    #[test]
    fn epoch_to_iso_matches_a_known_instant() {
        assert_eq!(epoch_to_iso(1758758400), "2025-09-25T00:00:00Z");
        assert_eq!(epoch_to_iso(0), "1970-01-01T00:00:00Z");
    }
}
