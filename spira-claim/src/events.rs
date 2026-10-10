//! The events-table fold: attempts, requeues and reclaims for one bead (DESIGN.md §3).
//!
//! Pure: rows in, a [`Ledger`] out. No store, no clock.

use serde::{Deserialize, Serialize};

/// One bd `events` row as `bd sql --json` returns it — only the columns selected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRow {
    pub issue_id: String,
    pub event_type: String,
    #[serde(default)]
    pub new_value: Option<String>,
    pub created_at: String,
}

impl EventRow {
    #[cfg(test)]
    pub fn new(issue_id: &str, event_type: &str, new_value: &str, created_at: &str) -> Self {
        EventRow {
            issue_id: issue_id.into(),
            event_type: event_type.into(),
            new_value: Some(new_value.into()),
            created_at: created_at.into(),
        }
    }
}

/// The event kinds the fold reads; every other event_type is ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    /// `claimed`, or `status_changed` whose value mentions in_progress.
    Claim,
    Close,
    Requeued(String),
    /// bd's own `reopened` row (no cause).
    Reopened,
    /// bead_reopen's cause row (`event_type='reopen'`, new_value = cause).
    Reopen(String),
    Reclaimed,
    PoisonCleared,
}

impl EventKind {
    pub fn of(row: &EventRow) -> Option<EventKind> {
        let v = row.new_value.clone().unwrap_or_default();
        Some(match row.event_type.as_str() {
            "claimed" => EventKind::Claim,
            "status_changed" if v.contains("in_progress") => EventKind::Claim,
            "closed" => EventKind::Close,
            "requeued" => EventKind::Requeued(v),
            "reopened" => EventKind::Reopened,
            "reopen" => EventKind::Reopen(v),
            "reclaimed" => EventKind::Reclaimed,
            "poison.cleared" => EventKind::PoisonCleared,
            _ => return None,
        })
    }

    /// Same-second tie-break: a claim precedes the close it ends, bd's `reopened` precedes
    /// bead_reopen's cause row (which is written after it).
    fn precedence(&self) -> u8 {
        match self {
            EventKind::PoisonCleared => 0,
            EventKind::Claim => 1,
            EventKind::Close => 2,
            EventKind::Requeued(_) | EventKind::Reclaimed => 3,
            EventKind::Reopened => 4,
            EventKind::Reopen(_) => 5,
        }
    }
}

/// What a return (a requeued or reopen cause) says about the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReturnClass {
    /// Not a return at all: the submitted conversion, an incident recurrence.
    NotAReturn,
    /// The base moved; the work was not judged (sp-j1q6o).
    RebaseReturn,
    /// The harness returned it without judging it (law-attempts-count-the-harness).
    HarnessReturn,
    /// The work was judged and found wanting: the cause is on the code-fault allow-list.
    Judged,
    /// A cause no one has promoted to the allow-list: never charged, so a new harness
    /// fault cannot poison a correct bead. Surfaced by `audit` for promotion.
    Unlisted,
}

/// Causes that charge outright: a red verdict on the bead's own tip, a fast-tier red, an
/// owned queue ejection, a failed unit run, and a builder resubmit that still conflicts.
const CODE_FAULT_CAUSES: &[&str] = &[
    "batch-eject",
    "queue-eject-local",
    "fast-tier-red",
    "resubmit-conflict",
    "unit-fail",
    "unit-red",
];

/// The legacy exemption set `_attempts_sql_query` subtracted: `thrash` and `unjudged-*`.
pub fn is_legacy_exempt(cause: &str) -> bool {
    cause == "thrash" || cause.starts_with("unjudged")
}

/// The cause the landing pass records for a gate run that reached no verdict.
pub const NO_VERDICT_CAUSE: &str = "gate-no-verdict";

pub fn is_no_verdict(cause: &str) -> bool {
    let c = cause.trim();
    c == NO_VERDICT_CAUSE || c.starts_with("gate-no-verdict:")
}

/// The cause recorded when the gate exits base-red (rc 76): the base itself fails, so the
/// gate judged nothing about the bead and the attempt is never charged.
pub const BASE_RED_CAUSE: &str = "gate-base-red";

pub fn is_base_red(cause: &str) -> bool {
    let c = cause.trim();
    c == BASE_RED_CAUSE || c.starts_with("gate-base-red:")
}

/// Classify a cause string. Order matters: harness and rebase tables first, then the
/// code-fault allow-list; any other cause is `Unlisted` and uncharged.
pub fn classify(cause: &str) -> ReturnClass {
    let c = cause.trim();
    if is_no_verdict(c) || is_base_red(c) {
        return ReturnClass::HarnessReturn;
    }
    match c {
        "work-close-converted" | "recurrence" | "closed-while-live" | "alert-recur" => {
            return ReturnClass::NotAReturn
        }
        "eject" | "queue-eject" | "queue-eject-collateral" | "ejected" | "eviction-race" | "slain" | "fast-tier-harness" | "closed-never-landed-batch-ready" => {
            return ReturnClass::HarnessReturn
        }
        _ => {}
    }
    if is_legacy_exempt(c) {
        return ReturnClass::HarnessReturn;
    }
    if CODE_FAULT_CAUSES.contains(&c) {
        return ReturnClass::Judged;
    }
    let lc = c.to_ascii_lowercase();
    // gate-red, cert-gate-red, rebase-gate-red, stale-red, eject-red… — by TOKEN, so
    // "delivered" or "cleared" never reads as a red.
    if lc.split(|ch: char| !ch.is_ascii_alphanumeric()).any(|t| t == "red") {
        return ReturnClass::Judged;
    }
    if lc.contains("conflict") || lc.starts_with("rebase") || lc.contains("stale") || lc.starts_with("base_withdrawn") {
        return ReturnClass::RebaseReturn;
    }
    ReturnClass::Unlisted
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "outcome", content = "why")]
pub enum AttemptOutcome {
    Succeeded,
    Exempt(String),
    Charged(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Attempt {
    pub claimed_at: String,
    #[serde(flatten)]
    pub outcome: AttemptOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Return {
    pub at: String,
    pub cause: String,
    pub class: ReturnClass,
    /// Whether this return counts toward `requeues`.
    pub counts: bool,
}

/// The per-bead answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ledger {
    pub bead: String,
    /// The last poison.cleared timestamp; attempts count only events strictly after it.
    pub floor: Option<String>,
    pub attempts: u32,
    pub requeues: u32,
    pub reclaims: u32,
    pub legacy_credits: u32,
    pub attempt_log: Vec<Attempt>,
    pub returns: Vec<Return>,
}

/// "YYYY-MM-DDTHH:MM:SS" from either `2026-09-27T14:14:38Z` or
/// `2026-09-27 14:14:38 +0000 UTC`. Anything shorter is returned trimmed, as-is.
pub fn norm_ts(s: &str) -> String {
    let s = s.trim();
    let mut out: String = s.chars().take(19).collect();
    if out.len() == 19 && out.as_bytes()[10] == b' ' {
        out.replace_range(10..11, "T");
    }
    out
}

/// Seconds since the epoch for a normalized timestamp, or None when it does not parse.
pub fn epoch_s(ts: &str) -> Option<i64> {
    let b = ts.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |r: std::ops::Range<usize>| ts.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hh, mm, ss) = (num(11..13)?, num(14..16)?, num(17..19)?);
    // Howard Hinnant's days_from_civil.
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}

/// A `reopen` cause row within this many seconds after a bd `reopened` row is the cause of
/// that reopen (bead_reopen writes both, bd's first).
pub const PAIR_WINDOW_S: i64 = 120;

/// A second claim this close behind an open one is one claim episode seen twice (a claim
/// race), not a session that ended without a close.
pub const DOUBLE_CLAIM_WINDOW_S: i64 = 10;

/// Order one bead's rows deterministically: timestamp, then kind precedence, then input order.
fn ordered(rows: &[EventRow]) -> Vec<(String, EventKind)> {
    let mut v: Vec<(String, u8, usize, EventKind)> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, r)| EventKind::of(r).map(|k| (norm_ts(&r.created_at), k.precedence(), i, k)))
        .collect();
    v.sort_by(|a, b| (&a.0, a.1, a.2).cmp(&(&b.0, b.1, b.2)));
    v.into_iter().map(|(t, _, _, k)| (t, k)).collect()
}

/// Fold one bead's rows (rows for other beads are ignored).
pub fn fold(bead: &str, rows: &[EventRow]) -> Ledger {
    let mine: Vec<EventRow> = rows.iter().filter(|r| r.issue_id == bead).cloned().collect();
    let evs = ordered(&mine);

    let floor = evs
        .iter()
        .filter(|(_, k)| *k == EventKind::PoisonCleared)
        .map(|(t, _)| t.clone())
        .max();

    // ---- attempts: events strictly after the floor -------------------------------------
    let mut attempt_log: Vec<Attempt> = Vec::new();
    let mut open: Option<String> = None;
    let mut credits: u32 = 0;
    // A no-verdict run exempts the open attempt; the close that later ends the same
    // session must not then forgive an earlier, real failure.
    let mut spent_by_no_verdict = false;
    for (t, k) in evs.iter().filter(|(t, _)| floor.as_ref().map_or(true, |f| t > f)) {
        match k {
            EventKind::Claim => {
                if let (Some(at), Some(now)) = (open.as_deref().and_then(epoch_s), epoch_s(t)) {
                    if now - at <= DOUBLE_CLAIM_WINDOW_S {
                        continue;
                    }
                }
                spent_by_no_verdict = false;
                if let Some(at) = open.take() {
                    attempt_log.push(Attempt {
                        claimed_at: at,
                        outcome: AttemptOutcome::Charged("claimed again without a close".into()),
                    });
                }
                open = Some(t.clone());
            }
            EventKind::Close => match open.take() {
                Some(at) => attempt_log.push(Attempt { claimed_at: at, outcome: AttemptOutcome::Succeeded }),
                None if spent_by_no_verdict => spent_by_no_verdict = false,
                None => credits += 1,
            },
            EventKind::Requeued(c) if is_legacy_exempt(c) => match open.take() {
                Some(at) => attempt_log.push(Attempt { claimed_at: at, outcome: AttemptOutcome::Exempt(c.clone()) }),
                None => credits += 1,
            },
            EventKind::Requeued(c) | EventKind::Reopen(c)
                if matches!(classify(c), ReturnClass::RebaseReturn | ReturnClass::HarnessReturn | ReturnClass::Unlisted) =>
            {
                if let Some(at) = open.take() {
                    attempt_log.push(Attempt { claimed_at: at, outcome: AttemptOutcome::Exempt(c.clone()) });
                    spent_by_no_verdict = is_no_verdict(c) || is_base_red(c);
                }
            }
            _ => {}
        }
    }
    if let Some(at) = open.take() {
        attempt_log.push(Attempt { claimed_at: at, outcome: AttemptOutcome::Charged("still open".into()) });
    }
    let charged = attempt_log.iter().filter(|a| matches!(a.outcome, AttemptOutcome::Charged(_))).count() as u32;
    let attempts = charged.saturating_sub(credits);

    // ---- requeues: every reopen cause row, plus bare bd reopens (no floor) --------------
    let mut returns = Vec::new();
    let cause_rows: Vec<(i64, usize)> = evs
        .iter()
        .enumerate()
        .filter(|(_, (_, k))| matches!(k, EventKind::Reopen(_)))
        .map(|(i, (t, _))| (epoch_s(t).unwrap_or(i64::MIN), i))
        .collect();
    let mut paired = vec![false; evs.len()];
    for (i, (t, k)) in evs.iter().enumerate() {
        if *k != EventKind::Reopened {
            continue;
        }
        let at = epoch_s(t);
        let partner = cause_rows.iter().find(|(ct, ci)| {
            *ci > i && !paired[*ci] && at.is_some_and(|a| *ct >= a && *ct - a <= PAIR_WINDOW_S)
        });
        match partner {
            Some((_, ci)) => paired[*ci] = true,
            None => returns.push(Return {
                at: t.clone(),
                cause: "unrecorded".into(),
                class: ReturnClass::Judged,
                counts: true,
            }),
        }
    }
    for (t, k) in &evs {
        if let EventKind::Reopen(c) = k {
            let class = classify(c);
            returns.push(Return { at: t.clone(), cause: c.clone(), class, counts: class == ReturnClass::Judged });
        }
    }
    returns.sort_by(|a, b| a.at.cmp(&b.at));
    let requeues = returns.iter().filter(|r| r.counts).count() as u32;

    let reclaims = evs.iter().filter(|(_, k)| *k == EventKind::Reclaimed).count() as u32;

    Ledger { bead: bead.to_string(), floor, attempts, requeues, reclaims, legacy_credits: credits, attempt_log, returns }
}

/// Parse `bd sql --json` output. Empty or non-JSON is an error (cannot tell), `[]`/`null`
/// is zero rows.
pub fn parse_rows(text: &str) -> Result<Vec<EventRow>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Err("empty output where JSON rows were expected".into());
    }
    let v: serde_json::Value = serde_json::from_str(t).map_err(|e| format!("not JSON: {e}"))?;
    let arr = match v {
        serde_json::Value::Null => return Ok(Vec::new()),
        serde_json::Value::Array(a) => a,
        other => vec![other],
    };
    arr.into_iter()
        .map(|r| {
            let s = |k: &str| match r.get(k) {
                Some(serde_json::Value::String(s)) => Some(s.clone()),
                Some(serde_json::Value::Null) | None => None,
                Some(other) => Some(other.to_string()),
            };
            Ok(EventRow {
                issue_id: s("issue_id").ok_or_else(|| format!("event row without issue_id: {r}"))?,
                event_type: s("event_type").ok_or_else(|| format!("event row without event_type: {r}"))?,
                new_value: s("new_value"),
                created_at: s("created_at").ok_or_else(|| format!("event row without created_at: {r}"))?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: &str = "sp-x";

    /// Build rows from (event_type, new_value) with one-minute spacing.
    fn rows(spec: &[(&str, &str)]) -> Vec<EventRow> {
        spec.iter()
            .enumerate()
            .map(|(i, (t, v))| EventRow::new(B, t, v, &format!("2026-09-28T{:02}:{:02}:00Z", 10 + i / 60, i % 60)))
            .collect()
    }

    /// The legacy SQL, as arithmetic: max(claims - closes - thrash/unjudged, 0) after floor.
    fn legacy_sql(rows: &[EventRow]) -> u32 {
        let floor = rows.iter().filter(|r| r.event_type == "poison.cleared").map(|r| norm_ts(&r.created_at)).max();
        let after: Vec<&EventRow> =
            rows.iter().filter(|r| floor.as_ref().map_or(true, |f| &norm_ts(&r.created_at) > f)).collect();
        let v = |r: &EventRow| r.new_value.clone().unwrap_or_default();
        let claims = after
            .iter()
            .filter(|r| r.event_type == "claimed" || (r.event_type == "status_changed" && v(r).contains("in_progress")))
            .count() as i64;
        let closes = after.iter().filter(|r| r.event_type == "closed").count() as i64;
        let exempt = after.iter().filter(|r| r.event_type == "requeued" && is_legacy_exempt(&v(r))).count() as i64;
        (claims - closes - exempt).max(0) as u32
    }

    fn at(kind: &str, ts: &str) -> EventRow {
        EventRow::new(B, kind, "", &format!("2026-09-28T10:{ts}Z"))
    }

    #[test]
    fn a_stack_conflict_refusal_is_not_an_attempt() {
        assert_eq!(classify("stack-conflict"), ReturnClass::RebaseReturn);
        let r = [
            EventRow::new(B, "claimed", "", "2026-09-01T00:00:00Z"),
            EventRow::new(B, "requeued", "stack-conflict", "2026-09-01T00:00:02Z"),
            EventRow::new(B, "claimed", "", "2026-09-01T00:01:00Z"),
            EventRow::new(B, "requeued", "stack-conflict", "2026-09-01T00:01:02Z"),
        ];
        assert_eq!(fold(B, &r).attempts, 0);
    }

    #[test]
    fn a_double_claim_a_second_apart_is_one_attempt() {
        let r = vec![at("claimed", "00:00"), at("claimed", "00:01")];
        assert_eq!(fold(B, &r).attempts, 1);
    }

    #[test]
    fn three_claims_released_without_submit_are_three_attempts() {
        let r = vec![at("claimed", "00:00"), at("claimed", "10:00"), at("claimed", "20:00")];
        assert_eq!(fold(B, &r).attempts, 3);
    }

    #[test]
    fn the_old_count_charged_a_double_claim_twice() {
        // Planted control: counting every claim row, as the fold used to, reaches 3 here.
        let r = vec![at("claimed", "00:00"), at("claimed", "00:01"), at("claimed", "10:00"), at("claimed", "20:00")];
        let old = r.iter().filter(|x| EventKind::of(x) == Some(EventKind::Claim)).count();
        assert_eq!(old, 4);
        assert_eq!(fold(B, &r).attempts, 3);
        let two = vec![at("claimed", "00:00"), at("claimed", "00:01"), at("claimed", "10:00")];
        assert_eq!(two.len(), 3, "old count: poisoned at 3");
        assert_eq!(fold(B, &two).attempts, 2, "new count: not poisoned");
    }

    #[test]
    fn a_collateral_ejection_is_free_and_an_owners_is_charged() {
        let free = fold(B, &rows(&[("claimed", ""), ("reopen", "queue-eject-collateral")]));
        assert_eq!((free.attempts, free.requeues), (0, 0));
        let owned = fold(B, &rows(&[("claimed", ""), ("reopen", "queue-eject-local")]));
        assert_eq!((owned.attempts, owned.requeues), (1, 1));
    }

    #[test]
    fn the_counts_are_the_same_whichever_log_a_row_lives_in() {
        let all = rows(&[
            ("claimed", ""),
            ("requeued", "gate-red"),
            ("claimed", ""),
            ("closed", ""),
            ("claimed", ""),
            ("poison.cleared", "operator"),
            ("claimed", ""),
            ("reopen", "batch-eject"),
        ]);
        let want = fold(B, &all);
        // The harness-written kinds moved to the lifecycle log; bd keeps what it writes itself.
        let (facts, bd): (Vec<_>, Vec<_>) = all
            .iter()
            .cloned()
            .partition(|r| matches!(r.event_type.as_str(), "requeued" | "reopen" | "poison.cleared"));
        let mut union = bd;
        union.extend(facts);
        let got = fold(B, &union);
        assert_eq!((got.attempts, got.requeues, got.reclaims, got.floor), (want.attempts, want.requeues, want.reclaims, want.floor));
        assert_eq!(got.attempt_log, want.attempt_log);
        assert_eq!(got.returns, want.returns);
    }

    #[test]
    fn null_safe_no_events() {
        let l = fold(B, &[]);
        assert_eq!((l.attempts, l.requeues, l.reclaims), (0, 0, 0));
        assert_eq!(l.floor, None);
    }

    #[test]
    fn null_safe_nothing_after_floor() {
        // sp-r66qd: three failures, then a clear (unpoison.sh, now spira-claim unpoison), then nothing: 0, not <nil>.
        let r = rows(&[("claimed", ""), ("claimed", ""), ("claimed", ""), ("poison.cleared", "operator")]);
        let l = fold(B, &r);
        assert_eq!(l.attempts, 0);
        assert!(l.floor.is_some());
    }

    #[test]
    fn poison_cleared_floor() {
        let r = rows(&[("claimed", ""), ("claimed", ""), ("claimed", ""), ("poison.cleared", "x"), ("claimed", "")]);
        assert_eq!(fold(B, &r).attempts, 1);
    }

    #[test]
    fn fresh_run_after_clear_counts_from_zero() {
        let r = rows(&[
            ("claimed", ""),
            ("claimed", ""),
            ("claimed", ""),
            ("poison.cleared", "x"),
            ("claimed", ""),
            ("claimed", ""),
            ("claimed", ""),
        ]);
        assert_eq!(fold(B, &r).attempts, 3);
    }

    #[test]
    fn same_second_as_floor_is_excluded() {
        let r = vec![
            EventRow::new(B, "claimed", "", "2026-09-28T10:00:00Z"),
            EventRow::new(B, "poison.cleared", "x", "2026-09-28T10:00:00Z"),
        ];
        assert_eq!(fold(B, &r).attempts, 0);
    }

    #[test]
    fn status_changed_in_progress_is_a_claim_other_status_is_not() {
        let r = rows(&[
            ("status_changed", r#"{"status":"in_progress"}"#),
            ("status_changed", r#"{"status":"open"}"#),
            ("label_added", "in_progress"),
        ]);
        assert_eq!(fold(B, &r).attempts, 1);
    }

    #[test]
    fn claim_then_close_is_zero() {
        assert_eq!(fold(B, &rows(&[("claimed", ""), ("closed", "OUTCOME: submitted")])).attempts, 0);
    }

    #[test]
    fn thrash_is_net_zero() {
        let r = rows(&[("claimed", ""), ("requeued", "thrash"), ("claimed", ""), ("requeued", "thrash")]);
        assert_eq!(fold(B, &r).attempts, 0);
    }

    #[test]
    fn unjudged_is_net_zero() {
        let r = rows(&[("claimed", ""), ("requeued", "unjudged-killed"), ("claimed", ""), ("closed", "")]);
        assert_eq!(fold(B, &r).attempts, 0);
    }

    #[test]
    fn backfill_credit_with_no_open_attempt_still_forgives() {
        // An operator's hand-written unjudged-backfill offset row after the fact.
        let r = rows(&[("claimed", ""), ("claimed", ""), ("claimed", ""), ("requeued", "unjudged-backfill")]);
        // claims 3: two charged by re-claim, the third closed Exempt by the backfill row.
        assert_eq!(fold(B, &r).attempts, 2);
        let r = rows(&[("claimed", ""), ("status_changed", r#"{"status":"open"}"#), ("requeued", "unjudged-backfill")]);
        assert_eq!(fold(B, &r).attempts, 0);
    }

    #[test]
    fn hand_close_is_a_credit() {
        let r = rows(&[("claimed", ""), ("claimed", ""), ("closed", ""), ("closed", "")]);
        assert_eq!(fold(B, &r).attempts, 0);
    }

    #[test]
    fn rebase_returns_do_not_count_as_attempts() {
        // A session that claimed, could not rebase, and was sent back: not a failure.
        let r = rows(&[
            ("claimed", ""),
            ("requeued", "merge-conflict"),
            ("reopen", "rebase-conflict"),
            ("claimed", ""),
            ("requeued", "rebase-conflict"),
            ("claimed", ""),
            ("reopen", "round-122-conflict-returned"),
        ]);
        let l = fold(B, &r);
        assert_eq!(l.attempts, 0, "{l:#?}");
    }

    #[test]
    fn rebase_return_of_closed_work_does_not_forgive_an_earlier_failure() {
        // Two charged failures, then a success, then a rebase return of the closed work.
        let r = rows(&[
            ("claimed", ""),
            ("claimed", ""),
            ("claimed", ""),
            ("closed", ""),
            ("reopen", "rebase-conflict"),
        ]);
        assert_eq!(fold(B, &r).attempts, 2);
    }

    #[test]
    fn a_fast_tier_tool_failure_is_the_harnesss_and_a_fast_tier_red_is_judged() {
        assert_eq!(classify("fast-tier-harness"), ReturnClass::HarnessReturn);
        assert_eq!(classify("fast-tier-red"), ReturnClass::Judged);
    }

    #[test]
    fn three_no_verdict_runs_charge_nothing() {
        let nv = NO_VERDICT_CAUSE;
        let r = rows(&[
            ("claimed", ""),
            ("requeued", nv),
            ("claimed", ""),
            ("requeued", nv),
            ("claimed", ""),
            ("requeued", nv),
        ]);
        let l = fold(B, &r);
        assert_eq!(l.attempts, 0, "{l:#?}");
        assert_eq!(l.requeues, 0);
        let one_session = rows(&[("claimed", ""), ("requeued", nv), ("requeued", nv), ("requeued", nv)]);
        assert_eq!(fold(B, &one_session).attempts, 0);
    }

    #[test]
    fn three_base_red_gate_exits_charge_nothing() {
        let br = BASE_RED_CAUSE;
        for cause in [br.to_string(), format!("{br}:base-red")] {
            let r = rows(&[
                ("claimed", ""),
                ("reopen", &cause),
                ("claimed", ""),
                ("requeued", &cause),
                ("claimed", ""),
                ("reopen", &cause),
            ]);
            let l = fold(B, &r);
            assert_eq!(l.attempts, 0, "{l:#?}");
            assert_eq!(l.requeues, 0, "{l:#?}");
        }
        assert_eq!(classify("gate-base-red"), ReturnClass::HarnessReturn);
        assert_eq!(classify("gate-red"), ReturnClass::Judged);
    }

    #[test]
    fn a_base_red_run_then_its_close_forgives_nothing_earlier() {
        let r = rows(&[("claimed", ""), ("claimed", ""), ("requeued", BASE_RED_CAUSE), ("closed", "")]);
        assert_eq!(fold(B, &r).attempts, 1);
    }

    #[test]
    fn three_branch_red_runs_still_charge() {
        let r = rows(&[
            ("claimed", ""),
            ("reopen", "cert-gate-red"),
            ("claimed", ""),
            ("reopen", "cert-gate-red"),
            ("claimed", ""),
            ("reopen", "cert-gate-red"),
        ]);
        assert_eq!(fold(B, &r).attempts, 3);
    }

    #[test]
    fn a_no_verdict_run_then_its_close_forgives_nothing_earlier() {
        let r = rows(&[
            ("claimed", ""),
            ("claimed", ""),
            ("requeued", NO_VERDICT_CAUSE),
            ("closed", ""),
        ]);
        assert_eq!(fold(B, &r).attempts, 1);
    }

    #[test]
    fn no_verdict_causes_are_harness_returns() {
        assert_eq!(classify("gate-no-verdict"), ReturnClass::HarnessReturn);
        assert_eq!(classify("gate-no-verdict:died"), ReturnClass::HarnessReturn);
        assert_eq!(classify("gate-red"), ReturnClass::Judged);
    }

    #[test]
    fn judged_reopen_does_not_forgive() {
        let r = rows(&[("claimed", ""), ("reopen", "gate-red"), ("claimed", ""), ("reopen", "gate-red")]);
        assert_eq!(fold(B, &r).attempts, 2);
    }

    #[test]
    fn legacy_streams_match_the_old_sql_exactly() {
        // Every stream built only from legacy event kinds must equal _attempts_sql_query.
        let alphabet: [(&str, &str); 7] = [
            ("claimed", ""),
            ("status_changed", r#"{"status":"in_progress"}"#),
            ("closed", ""),
            ("requeued", "thrash"),
            ("requeued", "unjudged-killed"),
            ("requeued", "unjudged-sop-silent"),
            ("poison.cleared", "x"),
        ];
        // All sequences up to length 6 over the alphabet (7^6 ≈ 117k, fast).
        let mut idx = vec![0usize; 0];
        let mut checked = 0;
        for len in 0..=6 {
            idx.clear();
            idx.resize(len, 0);
            loop {
                let spec: Vec<(&str, &str)> = idx.iter().map(|&i| alphabet[i]).collect();
                let r = rows(&spec);
                assert_eq!(fold(B, &r).attempts, legacy_sql(&r), "stream {spec:?}");
                checked += 1;
                // odometer
                let mut p = 0;
                loop {
                    if p == len {
                        break;
                    }
                    idx[p] += 1;
                    if idx[p] < alphabet.len() {
                        break;
                    }
                    idx[p] = 0;
                    p += 1;
                }
                if p == len {
                    break;
                }
            }
        }
        assert!(checked > 100_000);
    }

    #[test]
    fn rebase_returns_do_not_count_as_requeues() {
        // sp-j1q6o's regression: returned three times for rebase, then submitted.
        let r = rows(&[
            ("claimed", ""),
            ("closed", "OUTCOME: submitted"),
            ("reopened", ""),
            ("reopen", "work-close-converted"),
            ("requeued", "merge-conflict"),
            ("reopen", "rebase-conflict"),
            ("claimed", ""),
            ("closed", "OUTCOME: submitted"),
            ("reopened", ""),
            ("reopen", "work-close-converted"),
            ("reopen", "eject"),
            ("claimed", ""),
            ("closed", "OUTCOME: submitted"),
            ("reopened", ""),
            ("reopen", "work-close-converted"),
            ("reopen", "round-122-conflict-returned"),
            ("claimed", ""),
            ("closed", "OUTCOME: submitted"),
            ("reopened", ""),
            ("reopen", "work-close-converted"),
        ]);
        let l = fold(B, &r);
        assert_eq!(l.requeues, 0, "{l:#?}");
        assert_eq!(l.attempts, 0);
    }

    #[test]
    fn three_red_returns_count() {
        let r = rows(&[
            ("claimed", ""),
            ("closed", ""),
            ("reopened", ""),
            ("reopen", "work-close-converted"),
            ("reopen", "cert-gate-red"),
            ("claimed", ""),
            ("closed", ""),
            ("reopened", ""),
            ("reopen", "work-close-converted"),
            ("reopen", "batch-eject"),
            ("claimed", ""),
            ("closed", ""),
            ("reopened", ""),
            ("reopen", "unit-fail"),
        ]);
        assert_eq!(fold(B, &r).requeues, 3);
    }

    #[test]
    fn unpaired_reopened_counts() {
        // G13: five bare bd reopens with no cause row (a hand `bd reopen`) still count.
        let r = rows(&[("reopened", ""), ("reopened", ""), ("reopened", ""), ("reopened", ""), ("reopened", "")]);
        assert_eq!(fold(B, &r).requeues, 5);
    }

    #[test]
    fn reopened_paired_only_within_window() {
        let r = vec![
            EventRow::new(B, "reopened", "", "2026-09-28T10:00:00Z"),
            EventRow::new(B, "reopen", "rebase-conflict", "2026-09-28T10:00:02Z"),
            EventRow::new(B, "reopened", "", "2026-09-28T11:00:00Z"),
            EventRow::new(B, "reopen", "rebase-conflict", "2026-09-28T11:05:00Z"),
        ];
        // first pair matches; second reopened is 5 min before its cause row: unpaired, counts.
        assert_eq!(fold(B, &r).requeues, 1);
    }

    #[test]
    fn requeues_ignore_the_poison_floor() {
        let r = rows(&[("reopen", "gate-red"), ("poison.cleared", "x"), ("reopen", "gate-red")]);
        assert_eq!(fold(B, &r).requeues, 2);
    }

    #[test]
    fn sp_vd9dn_real_stream_is_zero() {
        // Verbatim shape of sp-vd9dn's events on 2026-09-27/28 (the false ask).
        let spec = [
            ("claimed", "2026-09-27 09:31:57 +0000 UTC", ""),
            ("closed", "2026-09-27 10:48:56 +0000 UTC", "OUTCOME: submitted"),
            ("reopened", "2026-09-27 10:58:23 +0000 UTC", ""),
            ("reopen", "2026-09-27 10:58:24 +0000 UTC", "work-close-converted"),
            ("requeued", "2026-09-27 13:29:37 +0000 UTC", "merge-conflict"),
            ("reopen", "2026-09-27 13:29:40 +0000 UTC", "rebase-conflict"),
            ("claimed", "2026-09-27 13:56:28 +0000 UTC", ""),
            ("closed", "2026-09-27 14:14:38 +0000 UTC", "OUTCOME: submitted"),
            ("reopened", "2026-09-27 14:29:05 +0000 UTC", ""),
            ("reopen", "2026-09-27 14:29:08 +0000 UTC", "work-close-converted"),
            ("claimed", "2026-09-28 05:23:15 +0000 UTC", ""),
            ("status_changed", "2026-09-28 05:27:40 +0000 UTC", r#"{"assignee":"","status":"open"}"#),
            ("claimed", "2026-09-28 05:31:40 +0000 UTC", ""),
            ("closed", "2026-09-28 05:44:49 +0000 UTC", "OUTCOME: submitted"),
            ("reopened", "2026-09-28 05:48:54 +0000 UTC", ""),
            ("reopen", "2026-09-28 05:48:56 +0000 UTC", "work-close-converted"),
            ("reopen", "2026-09-28 07:36:27 +0000 UTC", "eject"),
            ("claimed", "2026-09-28 07:36:44 +0000 UTC", ""),
            ("closed", "2026-09-28 07:59:07 +0000 UTC", "OUTCOME: submitted"),
            ("reopened", "2026-09-28 07:59:17 +0000 UTC", ""),
            ("reopen", "2026-09-28 07:59:18 +0000 UTC", "work-close-converted"),
        ];
        let r: Vec<EventRow> = spec.iter().map(|(t, at, v)| EventRow::new("sp-vd9dn", t, v, at)).collect();
        let l = fold("sp-vd9dn", &r);
        assert_eq!(l.requeues, 0, "{l:#?}");
        // bahamut's released-without-close session stays charged, exactly as the old SQL did.
        assert_eq!(l.attempts, legacy_sql(&r));
        assert_eq!(l.attempts, 1);
    }

    #[test]
    fn counts_reclaims() {
        let r = rows(&[("reclaimed", ""), ("reclaimed", "lease"), ("reclaimed", "")]);
        assert_eq!(fold(B, &r).reclaims, 3);
    }

    #[test]
    fn other_beads_rows_are_ignored() {
        let mut r = rows(&[("claimed", "")]);
        r.push(EventRow::new("sp-other", "claimed", "", "2026-09-28T10:00:00Z"));
        assert_eq!(fold(B, &r).attempts, 1);
        assert_eq!(fold("sp-other", &r).attempts, 1);
    }

    #[test]
    fn an_unlisted_cause_charges_nothing_and_each_code_fault_cause_charges() {
        for cause in ["sccache-connection-reset", "sift-bounced-deadlocked-parent", "some-future-cause"] {
            let l = fold(B, &rows(&[("claimed", ""), ("reopen", cause), ("claimed", ""), ("requeued", cause), ("claimed", ""), ("reopen", cause)]));
            assert_eq!((l.attempts, l.requeues), (0, 0), "{cause}");
        }
        for cause in ["gate-red", "cert-gate-red", "fast-tier-red", "unit-fail", "resubmit-conflict", "queue-eject-local", "batch-eject"] {
            let l = fold(B, &rows(&[("claimed", ""), ("reopen", cause), ("claimed", ""), ("reopen", cause)]));
            assert_eq!((l.attempts, l.requeues), (2, 2), "{cause}");
        }
    }

    #[test]
    fn classification_table() {
        use ReturnClass::*;
        for (c, want) in [
            ("work-close-converted", NotAReturn),
            ("recurrence", NotAReturn),
            ("closed-while-live", NotAReturn),
            ("alert-recur", NotAReturn),
            ("rebase-conflict", RebaseReturn),
            ("merge-conflict", RebaseReturn),
            ("round-122-conflict-returned", RebaseReturn),
            ("rebase-stale", RebaseReturn),
            ("base_withdrawn: sp-a abc123", RebaseReturn),
            ("eject", HarnessReturn),
            ("queue-eject", HarnessReturn),
            ("queue-eject-collateral", HarnessReturn),
            ("queue-eject-local", Judged),
            ("ejected", HarnessReturn),
            ("eviction-race", HarnessReturn),
            ("slain", HarnessReturn),
            ("thrash", HarnessReturn),
            ("unjudged-killed", HarnessReturn),
            ("closed-never-landed-batch-ready", HarnessReturn),
            ("batch-eject", Judged),
            ("gate-red", Judged),
            ("cert-gate-red", Judged),
            ("rebase-gate-red", Judged),
            ("stale-red", Judged),
            ("eject-red", Judged),
            ("confine-fail", Unlisted),
            ("closed-without-commit", Unlisted),
            ("delivers-mismatch", Unlisted),
            ("prod-dirty", Unlisted),
            ("no-sop", Unlisted),
            ("something-new", Unlisted),
            ("fast-tier-red", Judged),
            ("unit-fail", Judged),
            ("resubmit-conflict", Judged),
            ("sccache-connection-reset", Unlisted),
            ("sift-bounced-deadlocked-parent", Unlisted),
            ("build-fence-shared-target", Unlisted),
            ("lint-shared-target", Unlisted),
            ("", Unlisted),
        ] {
            assert_eq!(classify(c), want, "cause {c:?}");
        }
    }

    #[test]
    fn parse_rows_contract() {
        assert!(parse_rows("").is_err());
        assert!(parse_rows("Error: schema mismatch").is_err());
        assert_eq!(parse_rows("[]").unwrap(), vec![]);
        assert_eq!(parse_rows("null").unwrap(), vec![]);
        let r = parse_rows(r#"[{"issue_id":"a","event_type":"claimed","new_value":null,"created_at":"2026-09-28T10:00:00Z"}]"#)
            .unwrap();
        assert_eq!(r[0].new_value, None);
        assert!(parse_rows(r#"[{"event_type":"claimed","created_at":"x"}]"#).is_err());
    }

    #[test]
    fn timestamps_normalize_and_parse() {
        assert_eq!(norm_ts("2026-09-27 14:14:38 +0000 UTC"), "2026-09-27T14:14:38");
        assert_eq!(norm_ts("2026-09-27T14:14:38Z"), "2026-09-27T14:14:38");
        assert_eq!(epoch_s("1970-01-01T00:00:00"), Some(0));
        assert_eq!(epoch_s("2026-09-28T00:00:00").unwrap() - epoch_s("2026-09-27T23:59:00").unwrap(), 60);
        assert_eq!(epoch_s("bogus"), None);
    }
}
