//! Pure core: legacy bd events and post-cutover lifecycle event
//! rows both become `bead-stage` rows here, through `lifecycle::classify` itself (design
//! reconciler-time-series-2026-09-27 §2a.1: "call the same crate functions once per legacy
//! event, so there is still one mapping, owned by the lifecycle crate"). No I/O: main.rs
//! reads bd, and appends what this module builds. (The landing
//! ledger it once also read is deleted, sp-2c1n0.)

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
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
        is_epic: false,
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
    for (seq, ev) in (next_seq..).zip(events.iter()) {
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

/// Exports `fetch(after_seq)` batches until one is empty, saving the checkpoint after each batch
/// is written: a run cut short (the caller bounds it in seconds) keeps what it exported, so the
/// next run resumes instead of restarting from the start of a log it can never finish in one go.
pub fn drain<F, W, S>(from: LifecycleCheckpoint, mut fetch: F, mut write: W, mut save: S) -> Result<LifecycleCheckpoint, String>
where
    F: FnMut(i64) -> Result<Vec<LifecycleEventRow>, String>,
    W: FnMut(&[StageRow]) -> Result<(), String>,
    S: FnMut(&LifecycleCheckpoint) -> Result<(), String>,
{
    let mut cp = from;
    loop {
        let batch = fetch(cp.last_seq)?;
        let Some(last) = batch.iter().map(|r| r.seq).max() else { return Ok(cp) };
        if last <= cp.last_seq {
            return Err(format!("the source returned seq {last}, not after {}", cp.last_seq));
        }
        let rows: Vec<StageRow> = batch.iter().map(map_lifecycle_row).collect();
        write(&rows)?;
        cp = LifecycleCheckpoint { last_seq: last };
        save(&cp)?;
    }
}

/// The export is behind when the newest `bead-stage` row trails the newest lifecycle event by
/// more than `limit_secs`; no row at all is behind.
pub fn lag_verdict(newest_stage: Option<i64>, newest_event: i64, limit_secs: i64) -> Result<(), String> {
    match newest_stage {
        None => Err(format!("no bead-stage row, and the newest lifecycle event is at {newest_event}")),
        Some(t) if newest_event - t > limit_secs => Err(format!("newest bead-stage row is {}s older than the newest lifecycle event (limit {limit_secs}s)", newest_event - t)),
        Some(_) => Ok(()),
    }
}

/// One row of a post-cutover lifecycle event,
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

pub const STAGE_FAMILY: &str = "bead-stage";

/// The line `tsd-write --family bead-stage --ts .. --field ..` builds for this row.
pub fn stage_line(row: &StageRow, host: &str) -> Result<String, String> {
    let mut fields = vec![
        tsd::parse_field(&format!("seq={}", row.seq))?,
        tsd::parse_field_str(&format!("machine={}", row.machine))?,
        tsd::parse_field_str(&format!("key={}", row.key))?,
        tsd::parse_field_str(&format!("event={}", row.event))?,
        tsd::parse_field_str(&format!("from_state={}", row.from_state))?,
        tsd::parse_field_str(&format!("to_state={}", row.to_state))?,
        tsd::parse_field(&format!("applied={}", row.applied))?,
        tsd::parse_field_str(&format!("actor={}", row.actor))?,
        tsd::parse_field_str(&format!("source={}", row.source))?,
    ];
    if let Some(refusal) = &row.refusal {
        fields.push(tsd::parse_field_str(&format!("refusal={refusal}"))?);
    }
    if let Some(reason) = &row.reason {
        fields.push(tsd::parse_field_str(&format!("reason={reason}"))?);
    }
    tsd::build_row(&row.ts, host, STAGE_FAMILY, &fields)
}

/// Stores `rows` in the metrics store at `endpoint` in this process, starting no program.
pub fn push_stage_rows(endpoint: &str, rows: &[StageRow]) -> Result<(), String> {
    let host = tsd::default_host();
    let lines: Vec<String> = rows.iter().map(|r| stage_line(r, &host)).collect::<Result<_, _>>()?;
    tsd::store::push_rows(Some(endpoint), STAGE_FAMILY, &lines)
}

/// The most audits one `audits` call is asked for.
pub const LIFECYCLE_BATCH: usize = 100;

/// One batch of the control store's audits after revision `after`, as `spira-lc audits` answers them.
pub fn lifecycle_batch(lc: &spira_store::lc_call::LcClient, after: i64) -> Result<Vec<LifecycleEventRow>, String> {
    let limit = LIFECYCLE_BATCH.to_string();
    let out = lc.answer(&["audits", "--after", &after.to_string(), "--limit", &limit]).map_err(|e| e.to_string())?;
    parse_audits(&out)
}

/// The newest audit's epoch seconds in the store's recent window, or none.
pub fn newest_event(lc: &spira_store::lc_call::LcClient) -> Result<Option<i64>, String> {
    let out = lc.answer(&["audits", "--newest"]).map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| format!("audits --newest: {e}"))?;
    Ok(v.get("newest_at").and_then(|t| t.as_i64()))
}

pub fn parse_audits(text: &str) -> Result<Vec<LifecycleEventRow>, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("audits: not JSON: {e}"))?;
    let rows = v.get("rows").and_then(|r| r.as_array()).ok_or("audits: no rows array")?;
    let text_of = |r: &serde_json::Value, k: &str| r.get(k).and_then(|x| x.as_str()).map(str::to_string).ok_or_else(|| format!("audits: missing {k:?} in {r}"));
    let num_of = |r: &serde_json::Value, k: &str| r.get(k).and_then(|x| x.as_i64()).ok_or_else(|| format!("audits: missing {k:?} in {r}"));
    rows.iter()
        .map(|r| {
            Ok(LifecycleEventRow {
                seq: num_of(r, "seq")?,
                machine: text_of(r, "machine")?,
                lc_key: text_of(r, "lc_key")?,
                event: text_of(r, "event")?,
                from_state: text_of(r, "from_state")?,
                to_state: text_of(r, "to_state")?,
                applied: r.get("applied").and_then(|x| x.as_bool()).ok_or_else(|| format!("audits: missing \"applied\" in {r}"))?,
                refusal: r.get("refusal").and_then(|x| x.as_str()).map(str::to_string),
                evidence: r.get("evidence").cloned().unwrap_or(serde_json::Value::Null),
                actor: text_of(r, "actor")?,
                at: num_of(r, "at")?,
            })
        })
        .collect()
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

    struct Scripted(std::sync::Mutex<Vec<String>>, Vec<(i32, String)>);

    impl spira_store::lc_call::Transport for Scripted {
        fn exchange(&self, argv: &[String], _: Option<&str>, _: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
            let n = self.0.lock().unwrap().len();
            self.0.lock().unwrap().push(argv.join(" "));
            let (code, out) = self.1[n.min(self.1.len() - 1)].clone();
            Ok(spira_store::lc_call::Reply { code, out, err: String::new(), read_ended: false })
        }
    }

    fn scripted(answers: &[&str]) -> (spira_store::lc_call::LcClient, std::sync::Arc<Scripted>) {
        let t = std::sync::Arc::new(Scripted(Default::default(), answers.iter().map(|a| (0, a.to_string())).collect()));
        (spira_store::lc_call::LcClient::over(t.clone(), true), t)
    }

    const ROW: &str = r#"{"seq":7,"machine":"bead","lc_key":"sp-a","event":"Claim","from_state":"READY","to_state":"WORKING","applied":true,"refusal":null,"evidence":{"cause":"ask"},"actor":"d","at":86400}"#;

    #[test_id(t20clfp)]
    #[test]
    fn the_control_store_audits_drain_into_stage_rows_and_resume_past_the_last_revision() {
        let first = format!(r#"{{"rows":[{ROW}]}}"#);
        let (lc, wire) = scripted(&[&first, r#"{"rows":[]}"#]);
        let mut written = Vec::new();
        let mut saved = Vec::new();
        let cp = drain(LifecycleCheckpoint { last_seq: 3 }, |after| lifecycle_batch(&lc, after), |rows| { written.extend_from_slice(rows); Ok(()) }, |cp| { saved.push(*cp); Ok(()) }).unwrap();
        assert_eq!(cp.last_seq, 7);
        assert_eq!(saved, [LifecycleCheckpoint { last_seq: 7 }]);
        assert_eq!(written.len(), 1);
        assert_eq!((written[0].ts.as_str(), written[0].source.as_str(), written[0].reason.as_deref()), ("1970-01-02T00:00:00Z", "lifecycle", Some("ask")));
        assert_eq!(*wire.0.lock().unwrap(), ["audits --after 3 --limit 100", "audits --after 7 --limit 100"]);
    }

    fn event(seq: i64) -> LifecycleEventRow {
        LifecycleEventRow {
            seq,
            machine: "bead".into(),
            lc_key: format!("sp-{seq}"),
            event: "Claim".into(),
            from_state: "READY".into(),
            to_state: "WORKING".into(),
            applied: true,
            refusal: None,
            evidence: serde_json::Value::Null,
            actor: "d".into(),
            at: 1_700_000_000 + seq,
        }
    }

    fn log_of(n: i64) -> impl FnMut(i64) -> Result<Vec<LifecycleEventRow>, String> {
        move |after| Ok(((after + 1)..=n.min(after + LIFECYCLE_BATCH as i64)).map(event).collect())
    }

    #[test_id(tr9b2k1)]
    #[test]
    fn a_drain_of_250_events_is_three_pushes_in_this_process() {
        let store = spira_otel::fixture::FakeStore::start();
        let started = spira_config::bounded::started();
        let (mut pushes, mut saved) = (Vec::new(), Vec::new());
        let cp = drain(
            LifecycleCheckpoint { last_seq: 0 },
            log_of(250),
            |rows| {
                pushes.push(rows.len());
                push_stage_rows(store.addr(), rows)
            },
            |cp| {
                saved.push(cp.last_seq);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(pushes, [100, 100, 50]);
        assert_eq!((cp.last_seq, saved), (250, vec![100, 200, 250]));
        assert_eq!(spira_config::bounded::started(), started);
        let rows = tsd::store::Reader::new(Some(store.addr())).unwrap().rows("bead-stage", 0).unwrap();
        assert_eq!(rows.len(), 250);
    }

    #[test_id(t3m8qv0)]
    #[test]
    fn a_pushed_line_is_the_line_tsd_write_builds_with_and_without_refusal_and_reason() {
        let base = map_lifecycle_row(&event(7));
        let both = StageRow { applied: false, refusal: Some("stale".into()), reason: Some("a b".into()), ..base.clone() };
        for row in [base, both] {
            let mut fields = vec![tsd::parse_field(&format!("seq={}", row.seq)).unwrap()];
            for (k, v) in [("machine", &row.machine), ("key", &row.key), ("event", &row.event), ("from_state", &row.from_state), ("to_state", &row.to_state)] {
                fields.push(tsd::parse_field_str(&format!("{k}={v}")).unwrap());
            }
            fields.push(tsd::parse_field(&format!("applied={}", row.applied)).unwrap());
            for (k, v) in [("actor", &row.actor), ("source", &row.source)] {
                fields.push(tsd::parse_field_str(&format!("{k}={v}")).unwrap());
            }
            for (k, v) in [("refusal", &row.refusal), ("reason", &row.reason)] {
                if let Some(v) = v {
                    fields.push(tsd::parse_field_str(&format!("{k}={v}")).unwrap());
                }
            }
            let want = tsd::build_row(&row.ts, "h", "bead-stage", &fields).unwrap();
            assert_eq!(stage_line(&row, "h").unwrap(), want);
        }
    }

    #[test_id(tn4c7wd)]
    #[test]
    fn a_refused_push_leaves_the_checkpoint_at_the_previous_batch_and_names_the_stores_error() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        drop(l);
        let live = spira_otel::fixture::FakeStore::start();
        let mut saved = Vec::new();
        let mut n = 0;
        let err = drain(
            LifecycleCheckpoint { last_seq: 0 },
            log_of(250),
            |rows| {
                n += 1;
                push_stage_rows(if n == 1 { live.addr() } else { &addr }, rows)
            },
            |cp| {
                saved.push(cp.last_seq);
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(saved, [100]);
        assert!(err.contains("bead-stage: row not stored"), "{err}");
    }

    #[test_id(tvxlv5f)]
    #[test]
    fn a_refused_audit_keeps_its_refusal_and_a_store_that_cannot_answer_is_an_error_not_an_empty_log() {
        let refused = ROW.replace(r#""applied":true,"refusal":null"#, r#""applied":false,"refusal":"stale""#);
        let rows = parse_audits(&format!(r#"{{"rows":[{refused}]}}"#)).unwrap();
        assert!(!rows[0].applied);
        assert_eq!(map_lifecycle_row(&rows[0]).refusal.as_deref(), Some("stale"));
        let down = std::sync::Arc::new(Scripted(Default::default(), vec![(2, "cannot tell: store down".into())]));
        let lc = spira_store::lc_call::LcClient::over(down, true);
        assert!(lifecycle_batch(&lc, 0).is_err());
        assert!(parse_audits("{}").is_err());
    }

    #[test_id(t0eh5e4)]
    #[test]
    fn the_newest_event_comes_from_the_audits_verb() {
        let (lc, wire) = scripted(&[r#"{"newest_at":99}"#, r#"{"newest_at":null}"#]);
        assert_eq!(newest_event(&lc).unwrap(), Some(99));
        assert_eq!(newest_event(&lc).unwrap(), None);
        assert_eq!(wire.0.lock().unwrap()[0], "audits --newest");
    }

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

    #[test_id(twvbd30)]
    #[test]
    fn claim_then_close_maps_ready_to_working_to_dropped() {
        let events = vec![
            ev("1", "sp-a", "claimed", "dwarf-1", None, "2026-09-15T00:00:00Z"),
            ev("2", "sp-a", "closed", "dwarf-1", None, "2026-09-15T00:05:00Z"),
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

    #[test_id(t5ar0fg)]
    #[test]
    fn reopened_returns_to_ready() {
        let events = vec![
            ev("1", "sp-c", "claimed", "dwarf-1", None, "2026-09-15T00:00:00Z"),
            ev("2", "sp-c", "reopened", "harness", None, "2026-09-15T00:05:00Z"),
        ];
        let rows = fold_legacy(&events, "legacy", 1);
        assert_eq!(rows[1].to_state, "READY");
    }

    #[test_id(tg1tcr6)]
    #[test]
    fn an_event_type_with_no_fact_effect_still_emits_one_row_with_from_equal_to_to() {
        let events = vec![ev("1", "sp-d", "recurred", "harness", Some("closed-not-landed"), "2026-09-15T00:00:00Z")];
        let rows = fold_legacy(&events, "legacy", 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].from_state, rows[0].to_state);
        assert_eq!(rows[0].reason.as_deref(), Some("closed-not-landed"));
    }

    #[test_id(tr5qz57)]
    #[test]
    fn two_beads_interleaved_do_not_share_state() {
        let events = vec![
            ev("1", "sp-e", "claimed", "dwarf-1", None, "2026-09-15T00:00:00Z"),
            ev("2", "sp-f", "claimed", "dwarf-2", None, "2026-09-15T00:00:01Z"),
            ev("3", "sp-e", "closed", "dwarf-1", None, "2026-09-15T00:00:02Z"),
        ];
        let rows = fold_legacy(&events, "legacy", 1);
        assert_eq!(rows[1].to_state, "WORKING", "sp-f's own claim must classify sp-f, not sp-e");
        assert_eq!(rows[2].from_state, "WORKING", "sp-e's close follows sp-e's own claim");
    }

    #[test_id(tvr1trl)]
    #[test]
    fn seq_is_assigned_in_order_starting_from_next_seq() {
        let events = vec![
            ev("1", "sp-g", "claimed", "dwarf-1", None, "2026-09-15T00:00:00Z"),
            ev("2", "sp-g", "closed", "dwarf-1", None, "2026-09-15T00:00:01Z"),
        ];
        let rows = fold_legacy(&events, "legacy", 41);
        assert_eq!(rows[0].seq, 41);
        assert_eq!(rows[1].seq, 42);
    }

    // ── the checkpoint: re-running exports nothing new ────────────────────────────────────

    #[test_id(t6seln4)]
    #[test]
    fn advancing_past_every_event_leaves_nothing_fresh_on_a_second_call() {
        let events = vec![
            ev("1", "sp-h", "claimed", "dwarf-1", None, "2026-09-15T00:00:00Z"),
            ev("2", "sp-h", "closed", "dwarf-1", None, "2026-09-15T00:00:01Z"),
        ];
        let cp0 = LegacyCheckpoint::starting_at("2026-09-15T00:00:00Z");
        let (fresh1, cp1) = advance_legacy_checkpoint(&events, &cp0);
        assert_eq!(fresh1.len(), 2);
        let (fresh2, cp2) = advance_legacy_checkpoint(&events, &cp1);
        assert!(fresh2.is_empty(), "re-running the same query must export nothing new");
        assert_eq!(cp1, cp2, "a no-op run must not perturb the checkpoint");
    }

    #[test_id(tjcev4g)]
    #[test]
    fn a_tie_at_the_boundary_instant_is_deduped_by_id_not_dropped_wholesale() {
        let cp0 = LegacyCheckpoint::starting_at("2026-09-15T00:00:00Z");
        let first_batch = vec![
            ev("1", "sp-i", "claimed", "dwarf-1", None, "2026-09-15T00:00:05Z"),
            ev("2", "sp-j", "claimed", "dwarf-2", None, "2026-09-15T00:00:05Z"),
        ];
        let (fresh1, cp1) = advance_legacy_checkpoint(&first_batch, &cp0);
        assert_eq!(fresh1.len(), 2);
        // A third event lands at the exact same instant on a later poll — must still be
        // picked up, not skipped just because the checkpoint's timestamp already equals it.
        let second_batch = vec![
            first_batch[0].clone(),
            first_batch[1].clone(),
            ev("3", "sp-k", "claimed", "dwarf-3", None, "2026-09-15T00:00:05Z"),
        ];
        let (fresh2, _cp2) = advance_legacy_checkpoint(&second_batch, &cp1);
        assert_eq!(fresh2.len(), 1);
        assert_eq!(fresh2[0].id, "3");
    }

    #[test_id(tmz0etb)]
    #[test]
    fn next_seq_advances_by_exactly_the_number_of_fresh_rows() {
        let cp0 = LegacyCheckpoint::starting_at("2026-09-15T00:00:00Z");
        let events = vec![
            ev("1", "sp-l", "claimed", "dwarf-1", None, "2026-09-15T00:00:05Z"),
            ev("2", "sp-l", "closed", "dwarf-1", None, "2026-09-15T00:00:06Z"),
        ];
        let (_fresh, cp1) = advance_legacy_checkpoint(&events, &cp0);
        assert_eq!(cp1.next_seq, 3);
        let (fresh2, cp2) = advance_legacy_checkpoint(&events, &cp1);
        assert!(fresh2.is_empty());
        assert_eq!(cp2.next_seq, 3, "next_seq must not advance when nothing fresh was found");
    }

    // ── lifecycle source: a straight column mapping, refusals included ────────────────────

    #[test_id(t8ruyl9)]
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

    #[test_id(tgx8rhv)]
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

    #[test_id(t4w36h8)]
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

    fn logged(seq: i64) -> LifecycleEventRow {
        LifecycleEventRow { seq, machine: "bead".into(), lc_key: "sp-x".into(), event: "Claim".into(), from_state: "READY".into(), to_state: "WORKING".into(), applied: true, refusal: None, evidence: serde_json::Value::Null, actor: "a".into(), at: 1758000000 + seq }
    }

    #[test_id(twbu0hy)]
    #[test]
    fn a_run_cut_short_keeps_the_checkpoint_of_the_batches_it_finished() {
        let log: Vec<i64> = (1..=9).collect();
        let fetch = |after: i64| Ok(log.iter().copied().filter(|s| *s > after).take(3).map(logged).collect());
        let mut written = 0;
        let write = |rows: &[StageRow]| {
            if written >= 6 {
                return Err("killed".to_string());
            }
            written += rows.len();
            Ok(())
        };
        let mut saved = vec![];
        let r = drain(LifecycleCheckpoint::default(), fetch, write, |cp| {
            saved.push(cp.last_seq);
            Ok(())
        });
        assert!(r.is_err());
        assert_eq!(saved, vec![3, 6], "the batches before the cut are not exported again");
    }

    #[test_id(t1r9a6z)]
    #[test]
    fn a_drained_log_ends_at_its_last_seq_and_a_stuck_source_is_refused() {
        let all: Vec<i64> = (1..=5).collect();
        let fetch = |after: i64| Ok(all.iter().copied().filter(|s| *s > after).take(2).map(logged).collect());
        let end = drain(LifecycleCheckpoint { last_seq: 1 }, fetch, |_| Ok(()), |_| Ok(())).unwrap();
        assert_eq!(end.last_seq, 5);
        assert!(drain(LifecycleCheckpoint { last_seq: 4 }, |_| Ok(vec![logged(2)]), |_| Ok(()), |_| Ok(())).is_err());
    }

    #[test_id(tpwjbqp)]
    #[test]
    fn the_export_is_behind_when_its_newest_row_trails_the_newest_event_by_over_the_limit() {
        assert!(lag_verdict(Some(1000), 1600, 600).is_ok());
        assert!(lag_verdict(Some(1000), 1601, 600).unwrap_err().contains("601s"));
        assert!(lag_verdict(None, 1600, 600).is_err());
    }

    #[test_id(tjh3fm4)]
    #[test]
    fn epoch_to_iso_matches_a_known_instant() {
        assert_eq!(epoch_to_iso(1758758400), "2025-09-25T00:00:00Z");
        assert_eq!(epoch_to_iso(0), "1970-01-01T00:00:00Z");
    }
}
