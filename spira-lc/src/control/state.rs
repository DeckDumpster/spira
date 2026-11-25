//! The bead-state verbs on the control store: `show`, `list`, `event` on the bead machine and
//! `create-bead`, answering in the shape their dolt paths answer, which is a SQL row: every
//! number a string, `holds` and `stack` as JSON text. The caller verbs built on them (release,
//! renew, state, holds, list-state and the rest) reach the control store through these.

use lifecycle::bead::{self, BeadEvent, BeadEventKind, BeadState};
use std::collections::BTreeMap;

use serde_json::{Map, Value};
use state_store::{Backend, Error, Request};

use super::claimable;
use super::records::{LifecycleRecord, Readiness};
use super::store::{Outcome, Store};
use crate::{flag, CANNOT_TELL, REFUSED};

/// The verbs this module answers.
pub const VERBS: &[&str] = &["show", "list", "event", "create-bead", "promote-filed"];

/// Whether this module or the round's verbs answer `verb`.
pub fn is_verb(verb: &str) -> bool {
    VERBS.contains(&verb)
        || rounds::VERBS.contains(&verb)
        || cascades::VERBS.contains(&verb)
        || asks::VERBS.contains(&verb)
        || mending::VERBS.contains(&verb)
        || conditions::VERBS.contains(&verb)
        || landed::VERBS.contains(&verb)
}

/// What one call carries besides its argv: the clock it is judged at and the request id its
/// writes commit under. A retry is a new call, so it mints a new id: a request that lost stays
/// lost.
pub struct Call<'a> {
    pub now: i64,
    pub request: &'a str,
    /// What the claim view reads besides the control store.
    pub inputs: &'a dyn claimable::Inputs,
}

const LIVE_WINDOW_SECS: i64 = 24 * 3600;
const LISTED_TERMINAL: [&str; 4] = ["LANDED", "SUPERSEDED", "DROPPED", "DONE"];

pub fn run<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let rest = &args[1..];
    match args.first().map(String::as_str) {
        Some("show") => show(store, rest, call),
        Some("list") => list(store, rest, call),
        Some("event") => event(store, rest, call),
        Some("create-bead") => create(store, rest, call),
        Some(v) if rounds::VERBS.contains(&v) => rounds::run(store, args, call),
        Some(v) if cascades::VERBS.contains(&v) => cascades::run(store, args, call),
        Some(v) if asks::VERBS.contains(&v) => asks::run(store, args, call),
        Some(v) if mending::VERBS.contains(&v) => mending::run(store, args, call),
        Some(v) if conditions::VERBS.contains(&v) => conditions::run(store, args, call),
        Some(v) if landed::VERBS.contains(&v) => landed::run(store, rest, call),
        Some("promote-filed") => promote(store, rest, call),
        other => (
            CANNOT_TELL,
            format!("{other:?} is not a control-store verb"),
        ),
    }
}

fn cannot_tell(e: impl std::fmt::Display) -> (i32, String) {
    (CANNOT_TELL, format!("cannot tell: {e}"))
}

/// A JSON column as the SQL server renders it: `", "` between items and `": "` after keys.
fn sql_json(v: &Value) -> String {
    match v {
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(sql_json).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", Value::String(k.clone()), sql_json(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => other.to_string(),
    }
}

fn text(v: Option<&str>) -> Value {
    v.map_or(Value::Null, |s| Value::String(s.into()))
}

fn number(v: Option<i64>) -> Value {
    v.map_or(Value::Null, |n| Value::String(n.to_string()))
}

/// Every column a `show` or `list` row may carry, by its dolt name.
fn column(rec: &LifecycleRecord, r: Option<&Readiness>, view: &Value, name: &str) -> Value {
    let row = &rec.row;
    match name {
        "bead_id" => text(Some(&row.bead_id)),
        "state" => text(Some(row.state.as_str())),
        "tip" => text(row.tip.as_deref()),
        "gate_key" => text(row.gate_key.as_deref()),
        "holder" => text(row.holder.as_deref()),
        "persona" => text(row.persona.as_deref()),
        "lease_until" => number(row.lease_until),
        "holds" => Value::String(sql_json(&Value::Array(
            row.holds
                .iter()
                .map(|h| Value::String(h.as_str().into()))
                .collect(),
        ))),
        "reason" => text(row.reason.as_deref()),
        "version" => number(Some(row.version as i64)),
        "stack" => Value::String(sql_json(&Value::Object(
            row.stack
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect(),
        ))),
        "stack_depth" => number(Some(i64::from(row.stack_depth))),
        "since" => number(row.since),
        "worked_s" => number(Some(row.worked_s)),
        "sessions" => number(Some(i64::from(row.sessions))),
        "builder_claims" => number(Some(i64::from(row.builder_claims))),
        "session_ends" => serde_json::to_value(&row.session_ends).unwrap_or(Value::Null),
        "express" => number(Some(i64::from(row.express))),
        "dwarf_phase" => text(row.phase.map(|p| p.as_str())),
        "disposition" => text(row.disposition.map(|d| d.as_str())),
        "disposition_note" => text(row.disposition_note.as_deref()),
        "ejected_red_tip" => text(row.ejected_red_tip.as_deref()),
        "known_red" => number(Some(i64::from(lifecycle::bead::known_red(row)))),
        "sifted_tip" => text(row.sifted_tip.as_deref()),
        "updated_at" => number(Some(rec.updated_at)),
        "repo" => text(rec.repo.as_deref()),
        "claim" => rec.claim.as_ref().map_or(Value::Null, |c| serde_json::to_value(c).unwrap_or(Value::Null)),
        "carried_by" => text(rec.carried_by.as_deref()),
        "manual_hold" => row.manual_hold.as_ref().map_or(Value::Null, |m| serde_json::to_value(m).unwrap_or(Value::Null)),
        "takeover" => row.takeover.as_ref().map_or(Value::Null, |t| serde_json::to_value(t).unwrap_or(Value::Null)),
        "claim_view" => view.clone(),
        "blocked_by" => Value::Array(
            r.map(|r| r.blocked_by.iter().cloned().map(Value::String).collect())
                .unwrap_or_default(),
        ),
        other => unreachable!("no column {other}"),
    }
}

const SHOWN: &[&str] = &[
    "bead_id",
    "state",
    "tip",
    "gate_key",
    "holder",
    "persona",
    "lease_until",
    "holds",
    "reason",
    "version",
    "stack",
    "stack_depth",
    "worked_s",
    "sessions",
    "builder_claims",
    "session_ends",
    "express",
    "dwarf_phase",
    "disposition",
    "disposition_note",
    "ejected_red_tip",
    "known_red",
    "sifted_tip",
    "updated_at",
    "blocked_by",
    "claim_view",
    "claim",
    "carried_by",
    "manual_hold",
    "takeover",
];

const LISTED: &[&str] = &[
    "bead_id",
    "state",
    "tip",
    "holder",
    "persona",
    "lease_until",
    "holds",
    "reason",
    "updated_at",
    "version",
    "stack",
    "stack_depth",
    "since",
    "worked_s",
    "sessions",
    "builder_claims",
    "session_ends",
    "express",
    "dwarf_phase",
    "disposition",
    "disposition_note",
    "ejected_red_tip",
    "known_red",
    "sifted_tip",
    "repo",
    "blocked_by",
    "claim_view",
    "claim",
    "carried_by",
    "manual_hold",
    "takeover",
];

/// A record as `list` prints it.
pub(crate) fn listed(rec: &LifecycleRecord, r: Option<&Readiness>) -> Value {
    row(rec, r, &Value::Null, LISTED)
}

fn row(rec: &LifecycleRecord, r: Option<&Readiness>, view: &Value, cols: &[&str]) -> Value {
    Value::Object(
        cols.iter()
            .map(|c| (c.to_string(), column(rec, r, view, c)))
            .collect::<Map<_, _>>(),
    )
}

/// The claim view of each READY or REWORK record in `recs`, asked once for all of them; every
/// other record has none. When the view's inputs cannot be read, each of those records says it
/// cannot tell and why, so a reader of `list` still gets its rows.
fn claim_views<B: Backend>(store: &Store<B>, rev: u64, recs: &[&LifecycleRecord], call: &Call) -> BTreeMap<String, Value> {
    let cands: Vec<LifecycleRecord> = recs
        .iter()
        .filter(|r| matches!(r.row.state, BeadState::Ready | BeadState::Rework))
        .map(|r| (*r).clone())
        .collect();
    let ids: Vec<String> = cands.iter().map(|r| r.row.bead_id.clone()).collect();
    match claimable::view(store, rev, cands, call.inputs, call.now) {
        Ok((_, views)) => views
            .into_iter()
            .map(|(id, v)| (id, serde_json::json!({ "verdict": v.verdict, "blocker": v.blocker, "runes": v.runes, "missing": v.missing })))
            .collect(),
        Err(why) => ids
            .into_iter()
            .map(|id| (id, serde_json::json!({ "verdict": claimable::Verdict::CannotTell, "blocker": null, "runes": [], "missing": [], "error": why })))
            .collect(),
    }
}

/// `r` with only the prerequisites that still block, by `state_of` each one's record.
fn blocking(
    r: Option<Readiness>,
    state_of: impl Fn(&str) -> state_store::Result<Option<BeadState>>,
) -> state_store::Result<Option<Readiness>> {
    let Some(mut r) = r else { return Ok(None) };
    let mut kept = Vec::new();
    for b in r.blocked_by.drain(..) {
        if rounds::still_blocking(state_of(&b)?) {
            kept.push(b);
        }
    }
    r.blocked_by = kept;
    Ok(Some(r))
}

/// `show <id>`: the record and its delivery, or exit 1 with `{}`.
fn show<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let Some(id) = args.first() else {
        return (CANNOT_TELL, "show: missing <bead-id>".into());
    };
    let read = || -> Result<Option<Value>, String> {
        let e = |e: state_store::Error| e.to_string();
        let Some(rec) = store.record(id).map_err(e)? else {
            return Ok(None);
        };
        let rev = store.contract().head().map_err(e)?;
        let view = claim_views(store, rev, &[&rec.value], call).remove(id).unwrap_or(Value::Null);
        let readiness = blocking(store.readiness(id).map_err(e)?.map(|r| r.value), |b| {
            Ok(store.record(b)?.map(|r| r.value.row.state))
        })
        .map_err(e)?;
        let delivery = store.delivery(id).map_err(e)?.map(|d| rounds::delivery_view(&d.value));
        Ok(Some(serde_json::json!({
            "bead": row(&rec.value, readiness.as_ref(), &view, SHOWN),
            "delivery": delivery,
        })))
    };
    match read() {
        Ok(Some(v)) => (0, serde_json::to_string_pretty(&v).unwrap_or_default()),
        Ok(None) => (1, "{}".into()),
        Err(e) => cannot_tell(e),
    }
}

fn csv(v: Option<String>) -> Option<Vec<String>> {
    v.map(|s| {
        s.split(',')
            .filter(|x| !x.is_empty())
            .map(String::from)
            .collect()
    })
}

/// `list [--live] [--bare] [--state S,...] [--ids a,...] [--hold K] [--express] [--repo R] [--after ID]
/// [--limit N]`, in id order.
fn list<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    if args.iter().any(|a| a == "--delivery") {
        return rounds::list_delivery(store, args);
    }
    if args.iter().any(|a| a == "--batches") {
        return rounds::list_batches(store, args, call);
    }
    let states = csv(flag(args, "--state"));
    let ids = csv(flag(args, "--ids"));
    let hold = flag(args, "--hold");
    let repo = flag(args, "--repo");
    let express = args.iter().any(|a| a == "--express");
    let live = args.iter().any(|a| a == "--live");
    let bare = args.iter().any(|a| a == "--bare");
    let (after, limit) = match spira_lc::page(args) {
        Ok(p) => p.unwrap_or_default(),
        Err(e) => return (CANNOT_TELL, format!("cannot tell: list: {e}")),
    };
    if ids.is_none() && limit.is_none() {
        if let Some(refusal) = whole_read_refusal(states.as_deref(), live, args) {
            return (REFUSED, refusal);
        }
    }
    let kept = |rec: &LifecycleRecord| {
        let row = &rec.row;
        let state = row.state.as_str();
        states.as_ref().is_none_or(|s| s.iter().any(|x| x == state))
            && ids.as_ref().is_none_or(|i| i.contains(&row.bead_id))
            && after.as_ref().is_none_or(|a| row.bead_id > *a)
            && hold
                .as_deref()
                .is_none_or(|k| row.holds.iter().any(|h| h.as_str() == k))
            && (!express || row.express)
            && repo
                .as_deref()
                .is_none_or(|r| rec.repo.as_deref() == Some(r))
            && (!live
                || (!row.state.is_terminal()
                    && (!LISTED_TERMINAL.contains(&state)
                        || rec.updated_at >= call.now - LIVE_WINDOW_SECS)))
    };
    // The states the state index is read for: those named, less the terminal ones under
    // --live; a name no state has selects nothing.
    let indexed: Option<Vec<BeadState>> = match (&states, live) {
        (None, false) => None,
        (named, _) => Some(
            BEAD_STATES
                .into_iter()
                .filter(|s| named.as_ref().is_none_or(|n| n.iter().any(|x| x == s.as_str())))
                .filter(|s| !live || !s.is_terminal())
                .collect(),
        ),
    };
    let read = || -> Result<Vec<Value>, String> {
        let e = |e: state_store::Error| e.to_string();
        let rev = store.contract().head().map_err(e)?;
        let records = store
            .records_page(rev, indexed.as_deref(), ids.as_deref(), after.as_deref(), limit, kept)
            .map_err(e)?;
        let recs: Vec<&LifecycleRecord> = records.iter().map(|r| &r.value).collect();
        if bare {
            return Ok(recs.iter().map(|r| row(r, None, &Value::Null, LISTED)).collect());
        }
        let mut views = claim_views(store, rev, &recs, call);
        let ids: Vec<&str> = recs.iter().map(|r| r.row.bead_id.as_str()).collect();
        let mut readiness = store.readiness_of(&ids, rev).map_err(e)?;
        let mut states: std::collections::BTreeMap<String, BeadState> =
            recs.iter().map(|r| (r.row.bead_id.clone(), r.row.state)).collect();
        let off_page: std::collections::BTreeSet<&str> = readiness
            .values()
            .flat_map(|r| r.blocked_by.iter().map(String::as_str))
            .filter(|b| !states.contains_key(*b))
            .collect();
        let off_page: Vec<&str> = off_page.into_iter().collect();
        states.extend(store.states_at(&off_page, rev).map_err(e)?);
        let mut rows = Vec::with_capacity(records.len());
        for r in &recs {
            let id = r.row.bead_id.as_str();
            let ready = blocking(readiness.remove(id), |b| Ok(states.get(b).copied())).map_err(e)?;
            rows.push(row(r, ready.as_ref(), &views.remove(id).unwrap_or(Value::Null), LISTED));
        }
        Ok(rows)
    };
    match read() {
        Ok(rows) => (0, Value::Array(rows).to_string()),
        Err(e) => cannot_tell(e),
    }
}

/// Why an unpaged, unnamed list is refused, or `None` when it reads only live states.
fn whole_read_refusal(states: Option<&[String]>, live: bool, args: &[String]) -> Option<String> {
    let terminal: Vec<&str> = match states {
        Some(named) => BEAD_STATES
            .iter()
            .filter(|s| s.is_terminal() && named.iter().any(|n| n == s.as_str()))
            .map(|s| s.as_str())
            .collect(),
        None => Vec::new(),
    };
    if live || (states.is_some() && terminal.is_empty()) {
        return None;
    }
    let would = match (states, terminal.is_empty()) {
        (Some(_), false) => format!("every {} row", terminal.join("/")),
        _ => "every lifecycle row".to_string(),
    };
    Some(format!(
        "refused: `list {}` would read {would}; read the live set with `list --live` (or `--state <live states>`), named beads with `list --ids a,b`, or history by cursor with `list --after ID --limit N`",
        args.join(" ")
    ))
}

/// Every bead state, for the state index's reads.
const BEAD_STATES: [BeadState; 11] = [
    BeadState::Open,
    BeadState::Ready,
    BeadState::Working,
    BeadState::Submitted,
    BeadState::Certified,
    BeadState::InDelivery,
    BeadState::Rework,
    BeadState::Landed,
    BeadState::Superseded,
    BeadState::Dropped,
    BeadState::Done,
];

fn answer(outcome: state_store::Result<Outcome>) -> (i32, String) {
    match outcome {
        Ok(Outcome::Applied { .. }) => (0, String::new()),
        Ok(Outcome::Refused { refusal, .. }) => match refusal {
            lifecycle::Refusal::BreakGlassIncomplete { missing } => (
                REFUSED,
                format!("refused: BreakGlassIncomplete: missing {missing}"),
            ),
            other => (REFUSED, format!("refused: {:?}", Some(other))),
        },
        Ok(Outcome::LostRace { .. }) => {
            (REFUSED, "refused: lost the race to another writer".into())
        }
        Ok(Outcome::LockLost { .. }) => (
            REFUSED,
            "refused: the holder's claim lock has lapsed or been retaken".into(),
        ),
        Ok(Outcome::BlockerMoved { blocker, .. }) => (
            REFUSED,
            format!("refused: blocker {blocker} moved since the claim was judged"),
        ),
        Ok(Outcome::JudgedUnclaimable { blocked_by, verdict }) => (
            REFUSED,
            match (blocked_by.is_empty(), verdict) {
                (false, _) => format!(
                    "refused: judged not claimable (blocked by {})",
                    blocked_by.join(",")
                ),
                (true, Some(v)) => format!("refused: judged not claimable ({v})"),
                (true, None) => "refused: judged not claimable (no reason recorded)".into(),
            },
        ),
        Ok(Outcome::Locked { token }) => (
            REFUSED,
            format!(
                "refused: another claim's lock is live (token {}.{})",
                token.main, token.sub
            ),
        ),
        Err(e) => cannot_tell(e),
    }
}

/// `event bead <id> --expect S --version N --actor A --kind <json>`. A claim takes its lock in
/// the txn that writes the record; a renewal is fenced on that lock; every other event is
/// unfenced, and one that moves the record out of WORKING gives the lock back.
fn event<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let Some(machine) = args.first() else {
        return (CANNOT_TELL, "event: missing <machine>".into());
    };
    let Some(key) = args.get(1) else {
        return (CANNOT_TELL, "event: missing <key>".into());
    };
    let (Some(expect), Some(version), Some(actor), Some(kind_json)) = (
        flag(args, "--expect"),
        flag(args, "--version"),
        flag(args, "--actor"),
        flag(args, "--kind"),
    ) else {
        return (
            CANNOT_TELL,
            "event: --expect, --version, --actor and --kind are all required".into(),
        );
    };
    let Ok(version) = version.parse::<u64>() else {
        return (
            CANNOT_TELL,
            "event: --version must be a non-negative integer".into(),
        );
    };
    match machine.as_str() {
        "bead" => {}
        "batch" | "delivery" => {
            let round = match rounds::round_fields("event", args) {
                Ok(r) => r,
                Err(e) => return e,
            };
            return rounds::event(
                store,
                machine,
                key,
                &expect,
                version,
                &actor,
                &kind_json,
                round.as_ref(),
                call,
            );
        }
        other => {
            return (
                CANNOT_TELL,
                format!("event: unknown machine {other:?} (want bead, delivery or batch)"),
            )
        }
    }
    let Some(expect) = BeadState::from_str(&expect) else {
        return (CANNOT_TELL, format!("event: unknown bead state {expect:?}"));
    };
    let kind: BeadEventKind = match serde_json::from_str(&kind_json) {
        Ok(k) => k,
        Err(e) => {
            return (
                CANNOT_TELL,
                format!("event: --kind did not parse as a bead event: {e}"),
            )
        }
    };
    match store.record(key) {
        Ok(Some(_)) => {}
        Ok(None) => return (CANNOT_TELL, format!("event: no bead row for {key}")),
        Err(e) => return cannot_tell(e),
    }
    let ev = BeadEvent {
        expect,
        version,
        kind,
        actor: actor.clone(),
        at: Some(call.now),
    };
    let req = Request::new(call.request, &actor, "lc.event");
    let outcome = match &ev.kind {
        BeadEventKind::Claim { .. } => store.claim(key, &ev, &req).map(|(o, _)| o),
        BeadEventKind::Renew { .. } => match store.holder_fence(key) {
            Ok(Some(fence)) => store.renew_lease(key, &fence, &ev, &req),
            Ok(None) => unfenced_renew(store, key, &ev, &req),
            Err(e) => Err(e),
        },
        _ => store.apply(key, &ev, &req),
    };
    if let (BeadEventKind::BaseWithdrawn { prereq, tip }, Ok(Outcome::Applied { record, .. })) = (&ev.kind, &outcome) {
        tell_holder_of_withdrawn_base(key, &record.row, prereq, tip);
    }
    answer(outcome)
}

/// A base_withdrawn that leaves the record WORKING does not itself unblock its holder: the
/// eventual submit is refused until it rebases. Nothing else surfaces that mid-session, so the
/// holder is told once. Best-effort: a note that fails to post does not undo an applied move.
fn tell_holder_of_withdrawn_base(key: &str, row: &lifecycle::bead::LifecycleRow, prereq: &str, tip: &str) {
    if row.state != BeadState::Working {
        return;
    }
    let holder = row.holder.as_deref().unwrap_or("its holder");
    let _ = crate::bd::note(
        key,
        &format!(
            "Base withdrawn: {prereq}'s certified tip {tip} — the one this claim's stack was built on — no longer stands. {holder} is still WORKING, but its eventual submit is refused until it rebases onto the current stack."
        ),
    );
}

/// A renewal with no live claim lock: the machine's refusal when it has one, as the dolt path
/// answers; otherwise the lock has lapsed, and a lapsed holder extends nothing.
fn unfenced_renew<B: Backend>(
    store: &Store<B>,
    key: &str,
    ev: &BeadEvent,
    req: &Request,
) -> state_store::Result<Outcome> {
    let Some(cur) = store.record(key)? else {
        return Err(Error::NotFound {
            key: super::keys::lifecycle(key),
        });
    };
    let out = bead::apply(&cur.value.row, ev);
    match out.refusal {
        Some(refusal) if !out.applied => store.refuse_event(key, ev, refusal, req),
        _ => Ok(Outcome::LockLost {
            rev: store.contract().head()?,
        }),
    }
}

/// `create-bead <id> [--repo R] [--submitted-tip SHA] [--ready]`.
/// The record is born OPEN, unclaimable until `promote-filed` says filing has finished; `--ready` is
/// for a creator whose bead was filed whole before, and a submitted tip is born SUBMITTED. A
/// record that already exists is left as it is. Title and priority are planning-plane fields:
/// `--title` and `--priority` are refused.
fn create<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let Some(id) = args.first() else {
        return (CANNOT_TELL, "create-bead: missing <bead-id>".into());
    };
    if !crate::cutover::is_row_key(id) {
        return (CANNOT_TELL, format!("create-bead: {id:?} is not a bead id"));
    }
    if let Some(planning) = ["--title", "--priority"].into_iter().find(|f| args.iter().any(|a| a == f)) {
        return (
            REFUSED,
            format!("create-bead: {planning} is a planning-plane field; the lifecycle record keeps none"),
        );
    }
    let req = Request::new(call.request, "bead.sh", "lc.create-bead");
    let (repo, tip) = (flag(args, "--repo"), flag(args, "--submitted-tip"));
    let ready = args.iter().any(|a| a == "--ready");
    let made = if ready || tip.is_some() {
        store.create(id, repo.as_deref(), tip.as_deref(), call.now, &req)
    } else {
        store.create_open(id, repo.as_deref(), call.now, &req)
    };
    match made {
        Ok(_) => (0, String::new()),
        Err(Error::Conflict { .. }) => match fill_missing_repo(store, id, repo.as_deref(), &req) {
            Ok(()) => (0, String::new()),
            Err(e) => cannot_tell(e),
        },
        Err(e) => cannot_tell(e),
    }
}

/// A record born without a repo takes the one a later `create-bead` names; one that has a repo keeps it.
fn fill_missing_repo<B: Backend>(store: &Store<B>, id: &str, repo: Option<&str>, req: &Request) -> state_store::Result<()> {
    let Some(repo) = repo else { return Ok(()) };
    if store.record(id)?.is_some_and(|rec| rec.value.repo.is_none()) {
        store.set_repo(id, Some(repo), None, req)?;
    }
    Ok(())
}

mod asks;
mod cascades;
mod conditions;
pub(crate) mod landed;
mod mending;
mod rounds;

/// `promote-filed <id> [--actor A]`: filing has finished, so the OPEN record becomes READY. A record
/// already past OPEN is left as it is.
fn promote<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let Some(id) = args.first() else {
        return (CANNOT_TELL, "promote-filed: missing <bead-id>".into());
    };
    let actor = flag(args, "--actor").unwrap_or_else(|| "bead.sh".into());
    let req = Request::new(call.request, &actor, "lc.promote");
    match store.record(id) {
        Ok(Some(_)) => answer(store.promote(id, &actor, call.now, &req)),
        Ok(None) => (CANNOT_TELL, format!("promote-filed: no bead row for {id}")),
        Err(e) => cannot_tell(e),
    }
}

#[cfg(test)]
mod parity;

#[cfg(test)]
mod manual_hold_columns {
    use super::*;
    use lifecycle::bead::ManualHold;
    use lifecycle::reason::HoldCause;

    #[test_id(trgh5aa)]
    #[test]
    fn list_and_show_print_the_manual_hold_a_record_carries_and_null_otherwise() {
        let mut rec = LifecycleRecord::born("sp-a", Some("spira"), None, 100);
        assert_eq!(listed(&rec, None)["manual_hold"], Value::Null);
        rec.row.manual_hold = Some(ManualHold { at: Some(7), cause: HoldCause::SupersedeRequest, detail: Some("sp-b".into()) });
        let want = serde_json::json!({ "at": 7, "cause": "supersede-request", "detail": "sp-b" });
        assert_eq!(listed(&rec, None)["manual_hold"], want);
        assert_eq!(row(&rec, None, &Value::Null, SHOWN)["manual_hold"], want);
    }

    #[test_id(tn7w4xc)]
    #[test]
    fn list_and_show_print_builder_claims_and_session_ends_from_the_record() {
        let mut rec = LifecycleRecord::born("sp-a", Some("spira"), None, 100);
        let shown = |rec: &LifecycleRecord| [listed(rec, None), row(rec, None, &Value::Null, SHOWN)];
        for v in shown(&rec) {
            assert_eq!((&v["builder_claims"], &v["session_ends"]), (&Value::String("0".into()), &serde_json::json!([])));
        }
        rec.row.builder_claims = 3;
        rec.row.session_ends = vec![lifecycle::bead::SessionEnd { claim: 3, left: "Submit by d".into(), back: None, at: Some(5) }];
        for v in shown(&rec) {
            assert_eq!(v["builder_claims"], "3");
            assert_eq!(v["session_ends"], serde_json::json!([{ "claim": 3, "left": "Submit by d", "back": null, "at": 5 }]));
        }
    }
}
