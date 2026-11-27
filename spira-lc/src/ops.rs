//! The ops read model: `ops-view <view>` returns one view as a JSON array, `ops-graph` the
//! machine's legal edges, and the other `ops-*` verbs the day's events. Every row is read from the
//! control store (the records, the rounds, the audit log) and printed as the strings the panes
//! have always read; a bead's title and priority come from the planning plane, where they live.

use std::collections::BTreeMap;

use lifecycle::bead::BeadState;
use serde_json::{json, Map, Value};
use state_store::Backend;

use crate::control::audits::Move;
use crate::control::edges::EdgeTally;
#[cfg(test)]
use crate::control::edges::HOUR_S;
use crate::control::landed::Landed as LandedList;
use crate::control::refusals::{Class, Group as TallyGroup, RefusalTally};
use crate::control::stays::Tally as StayTally;
use crate::control::store::Store;

/// The only views `ops-view` will name; the argument is matched, never interpolated.
pub const VIEWS: [&str; 6] = ["ops_live", "ops_round", "ops_recent", "ops_edges", "ops_dwell", "ops_dwell_p95"];

/// The machines `ops-transitions` reads events of, and the machines `ops-graph` draws.
pub const TRANSITION_MACHINES: [&str; 2] = ["bead", "batch"];
pub const GRAPH_MACHINES: [&str; 4] = ["bead", "batch", "ask", "mending"];

use crate::control::stays::DAY_S;
pub(crate) const LIVE: [BeadState; 7] = [BeadState::Open, BeadState::Ready, BeadState::Working, BeadState::Submitted, BeadState::Certified, BeadState::InDelivery, BeadState::Rework];
const ROUND_STATES: [&str; 5] = ["OPEN", "CI_RUNNING", "GREEN", "ATTRIBUTING", "REBUILDING"];

fn text(v: impl ToString) -> Value {
    Value::String(v.to_string())
}

fn opt(v: Option<impl ToString>) -> Value {
    v.map_or(Value::Null, text)
}

fn object(cols: Vec<(&str, Value)>) -> Value {
    Value::Object(cols.into_iter().map(|(k, v)| (k.to_string(), v)).collect::<Map<_, _>>())
}

/// The machines `ops-transitions` reads events of, matched against `allowed`; the first when the flag is absent.
fn machine_arg(verb: &str, args: &[String], allowed: &[&'static str]) -> Result<&'static str, (i32, String)> {
    let Some(at) = args.iter().position(|a| a == "--machine") else {
        return Ok(allowed[0]);
    };
    args.get(at + 1)
        .and_then(|m| allowed.iter().find(|a| **a == m.as_str()))
        .copied()
        .ok_or_else(|| (2, format!("{verb}: --machine takes one of {}", allowed.join(" | "))))
}

/// The ids' titles and priorities from the planning plane, as strings; a plane that cannot be
/// read leaves them null, as a listing does.
fn planned(ids: &[String]) -> BTreeMap<String, (Value, Value)> {
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    match crate::plan::titled(&ids) {
        Ok(found) => found.into_iter().map(|(id, (t, p))| (id, (t, if p.is_null() { p } else { text(p) }))).collect(),
        Err(e) => {
            eprintln!("spira-lc ops: titles and priorities unread from bd: {e}");
            BTreeMap::new()
        }
    }
}

fn title_cols(plan: &BTreeMap<String, (Value, Value)>, id: &str) -> (Value, Value) {
    plan.get(id).cloned().unwrap_or((Value::Null, Value::Null))
}

struct Live {
    rows: Vec<crate::control::records::LifecycleRecord>,
}

fn live<B: Backend>(store: &Store<B>) -> Result<Live, String> {
    let rev = store.contract().head().map_err(|e| e.to_string())?;
    let rows = store.records_page(rev, Some(&LIVE), None, None, None, |_| true).map_err(|e| e.to_string())?;
    Ok(Live { rows: rows.into_iter().map(|r| r.value).collect() })
}

fn is_live(state: BeadState) -> bool {
    LIVE.contains(&state)
}

/// `ops_live`: every live bead, with the blocker that keeps it from being claimable.
fn ops_live<B: Backend>(store: &Store<B>) -> Result<Vec<Value>, String> {
    let live = live(store)?;
    let states: BTreeMap<&str, BeadState> = live.rows.iter().map(|r| (r.row.bead_id.as_str(), r.row.state)).collect();
    let plan = planned(&live.rows.iter().map(|r| r.row.bead_id.clone()).collect::<Vec<_>>());
    let mut out: Vec<Value> = live
        .rows
        .iter()
        .map(|r| {
            let row = &r.row;
            let blocker = r.claim.as_ref().and_then(|c| c.blocks.iter().filter(|b| states.get(b.as_str()).is_some_and(|s| is_live(*s))).min());
            let holds: Vec<&str> = row.holds.iter().map(|h| h.as_str()).collect();
            let claimable = row.state == BeadState::Ready && holds.is_empty() && blocker.is_none();
            let (title, priority) = title_cols(&plan, &row.bead_id);
            object(vec![
                ("bead_id", text(&row.bead_id)),
                ("state", text(row.state.as_str())),
                ("holds", text(json!(holds))),
                ("holder", opt(row.holder.as_ref())),
                ("persona", opt(row.persona.as_ref())),
                ("rework", text(u8::from(row.state == BeadState::Rework))),
                ("lease_until", opt(row.lease_until)),
                ("since", opt(row.since)),
                ("updated_at", text(r.updated_at)),
                ("priority", priority),
                ("title", title),
                ("claimable", text(u8::from(claimable))),
                ("blocker", opt(blocker)),
                ("repo", opt(r.repo.as_ref())),
            ])
        })
        .collect();
    out.sort_by(|a, b| a["bead_id"].as_str().cmp(&b["bead_id"].as_str()));
    Ok(out)
}

/// `ops_round`: each member of each round still being worked.
fn ops_round<B: Backend>(store: &Store<B>) -> Result<Vec<Value>, String> {
    let rounds = store.open_batches().map_err(|e| e.to_string())?;
    let members: Vec<String> = rounds.iter().flat_map(|(_, r)| r.value.members.iter().map(|m| m.bead_id.clone())).collect();
    let plan = planned(&members);
    let mut out = Vec::new();
    for (_, round) in &rounds {
        let (row, round) = (&round.value.row, &round.value);
        if !ROUND_STATES.contains(&row.state.as_str()) {
            continue;
        }
        for m in &round.members {
            let (title, priority) = title_cols(&plan, &m.bead_id);
            out.push(object(vec![
                ("batch_id", text(&row.batch_id)),
                ("repo", text(&row.repo)),
                ("batch_state", text(row.state.as_str())),
                ("head", opt(row.head.as_ref())),
                ("base", opt(row.base.as_ref())),
                ("opened_at", text(round.opened_at)),
                ("bead_id", text(&m.bead_id)),
                ("tip", text(&m.tip)),
                ("outcome", Value::Null),
                ("title", title),
                ("priority", priority),
            ]));
        }
    }
    out.sort_by(|a, b| (a["batch_id"].as_str(), a["bead_id"].as_str()).cmp(&(b["batch_id"].as_str(), b["bead_id"].as_str())));
    Ok(out)
}

/// `ops_recent`: the beads that landed in the last day, named by the kept landed list and read by
/// key, so the call's work follows the day's landings and not the audit log.
fn ops_recent<B: Backend>(store: &Store<B>, now: i64) -> Result<Vec<Value>, String> {
    let ids = match store.landed_ids(now).map_err(|e| e.to_string())? {
        LandedList::Filling { covered_from } => return Err(format!("the landed list is filling: covered from {}", covered_from.map_or("none".to_string(), |at| at.to_string()))),
        LandedList::Ready(ids) => ids,
    };
    recent_of(store, now, &ids)
}

#[cfg(test)]
fn recent_by_fold<B: Backend>(store: &Store<B>, now: i64) -> Result<Vec<Value>, String> {
    let mut ids = std::collections::BTreeSet::new();
    folded(store, now, DAY_S, |m| {
        if m.machine == "bead" && m.applied && m.to_state == BeadState::Landed.as_str() && m.from_state != m.to_state {
            ids.insert(m.lc_key.to_string());
        }
    })?;
    recent_of(store, now, &ids.into_iter().collect::<Vec<_>>())
}

fn recent_of<B: Backend>(store: &Store<B>, now: i64, ids: &[String]) -> Result<Vec<Value>, String> {
    let mut landed = Vec::new();
    for id in ids {
        let rec = store.record(id).map_err(|e| e.to_string())?;
        if let Some(r) = rec.map(|r| r.value).filter(|r| r.row.state == BeadState::Landed && r.row.since.is_some_and(|s| s >= now - DAY_S)) {
            landed.push(r);
        }
    }
    let plan = planned(&landed.iter().map(|r| r.row.bead_id.clone()).collect::<Vec<_>>());
    let mut out: Vec<Value> = landed
        .iter()
        .map(|r| {
            let (title, priority) = title_cols(&plan, &r.row.bead_id);
            object(vec![
                ("bead_id", text(&r.row.bead_id)),
                ("state", text(r.row.state.as_str())),
                ("since", opt(r.row.since)),
                ("updated_at", text(r.updated_at)),
                ("priority", priority),
                ("title", title),
                ("repo", opt(r.repo.as_ref())),
            ])
        })
        .collect();
    out.sort_by(|a, b| a["bead_id"].as_str().cmp(&b["bead_id"].as_str()));
    Ok(out)
}

/// How often each transition applied and each refusal happened in the last hour and day.
#[derive(Default)]
struct Edges {
    applied: BTreeMap<(String, String, String), (u64, u64, i64)>,
    refused: BTreeMap<(String, String, String), (u64, u64, i64)>,
}

impl Edges {
    /// The reference fold of one audit, newest first, that the tally is tested against.
    #[cfg(test)]
    fn take(&mut self, now: i64, m: &Move) {
        fn tally(map: &mut BTreeMap<(String, String, String), (u64, u64, i64)>, key: (&str, &str, &str), at: i64, now: i64) {
            let e = map.entry((key.0.to_string(), key.1.to_string(), key.2.to_string())).or_insert((0, 0, i64::MIN));
            e.0 += u64::from(at >= now - HOUR_S);
            e.1 += 1;
            e.2 = e.2.max(at);
        }
        if m.machine != "bead" || m.at < now - DAY_S {
            return;
        }
        if !m.applied {
            tally(&mut self.refused, (m.from_state, m.event, m.refusal.unwrap_or_default()), m.at, now);
        } else if m.from_state != m.to_state {
            tally(&mut self.applied, (m.from_state, m.to_state, m.event), m.at, now);
        }
    }

    /// `ops_edges` rows.
    fn rows(self) -> Vec<Value> {
        let mut out: Vec<Value> = self
            .applied
            .into_iter()
            .map(|((from, to, event), (h, d, last))| object(vec![("kind", text("applied")), ("from_state", text(from)), ("to_state", text(to)), ("event", text(event)), ("refusal", Value::Null), ("n_1h", text(h)), ("n_24h", text(d)), ("last_at", text(last))]))
            .collect();
        out.extend(self.refused.into_iter().map(|((from, event, refusal), (h, d, last))| {
            object(vec![("kind", text("refused")), ("from_state", text(from)), ("to_state", Value::Null), ("event", text(event)), ("refusal", text(refusal)), ("n_1h", text(h)), ("n_24h", text(d)), ("last_at", text(last))])
        }));
        out
    }
}

/// The p95 and sample count per state from the stay tally, or why there are none yet.
fn stay_tally<B: Backend>(store: &Store<B>, now: i64) -> Result<StayTally, String> {
    store.stay_tally(now).map_err(|e| e.to_string())
}

/// `ops_edges`: the edge tally's counts as rows, or why there are none yet.
fn ops_edges<B: Backend>(store: &Store<B>, now: i64) -> Result<Vec<Value>, String> {
    match store.edge_tally(now).map_err(|e| e.to_string())? {
        EdgeTally::Filling { covered_from } => Err(format!("the edge tally is filling: covered from {}", covered_from.map_or("none".to_string(), |at| at.to_string()))),
        EdgeTally::Ready(c) => Ok(Edges { applied: c.applied, refused: c.refused }.rows()),
    }
}

fn filling(covered_from: Option<i64>) -> String {
    format!("the stay tally is filling: covered from {}", covered_from.map_or("none".to_string(), |at| at.to_string()))
}

fn ops_dwell_p95<B: Backend>(store: &Store<B>, now: i64) -> Result<Vec<Value>, String> {
    match stay_tally(store, now)? {
        StayTally::Filling { covered_from } => Err(filling(covered_from)),
        StayTally::Ready(p95) => Ok(p95.into_iter().map(|(state, (p95, n))| object(vec![("state", text(state)), ("p95_s", text(p95)), ("samples", text(n))])).collect()),
    }
}

/// `ops_dwell`: each live bead with when it entered its state and what is usual for that state.
fn ops_dwell<B: Backend>(store: &Store<B>, now: i64) -> Result<Vec<Value>, String> {
    let live = live(store)?;
    let p95 = match stay_tally(store, now)? {
        StayTally::Ready(p95) => p95,
        StayTally::Filling { .. } => BTreeMap::new(),
    };
    let plan = planned(&live.rows.iter().map(|r| r.row.bead_id.clone()).collect::<Vec<_>>());
    let mut out = Vec::new();
    for r in &live.rows {
        let row = &r.row;
        let (title, priority) = title_cols(&plan, &row.bead_id);
        let usual = p95.get(row.state.as_str());
        let holds: Vec<&str> = row.holds.iter().map(|h| h.as_str()).collect();
        out.push(object(vec![
            ("bead_id", text(&row.bead_id)),
            ("state", text(row.state.as_str())),
            ("holds", text(json!(holds))),
            ("priority", priority),
            ("title", title),
            ("entered_at", text(row.since.unwrap_or(r.updated_at))),
            ("p95_s", opt(usual.map(|u| u.0))),
            ("samples", opt(usual.map(|u| u.1))),
            ("repo", opt(r.repo.as_ref())),
        ]));
    }
    out.sort_by(|a, b| a["bead_id"].as_str().cmp(&b["bead_id"].as_str()));
    Ok(out)
}

/// The day's applied state changes of `machine`, oldest first.
fn transitions<B: Backend>(store: &Store<B>, machine: &str, now: i64) -> Result<Vec<Value>, String> {
    let mut rows = Vec::new();
    folded(store, now, DAY_S, |m| {
        if m.machine == machine && m.applied && m.from_state != m.to_state {
            rows.push(object(vec![("seq", text(m.rev)), ("bead_id", text(m.lc_key)), ("event", text(m.event)), ("from_state", text(m.from_state)), ("to_state", text(m.to_state)), ("at", text(m.at))]));
        }
    })?;
    rows.reverse();
    Ok(rows)
}

/// Refused bead events inside the window, one row per (actor, event, from_state, refusal) with
/// the count, the span and the newest five `bead@epoch` examples.
#[cfg(test)]
struct Refusals {
    floor: i64,
    groups: BTreeMap<(String, String, String, String), Group>,
}

#[cfg(test)]
#[derive(Default)]
struct Group {
    n: usize,
    first: i64,
    last: i64,
    newest: Vec<(i64, String)>,
}

#[cfg(test)]
impl Refusals {
    fn new(floor: i64) -> Self {
        Refusals { floor, groups: BTreeMap::new() }
    }

    fn take(&mut self, m: &Move) {
        if m.machine != "bead" || m.applied || m.at < self.floor {
            return;
        }
        let g = self.groups.entry((m.actor.into(), m.event.into(), m.from_state.into(), m.refusal.unwrap_or_default().into())).or_default();
        (g.first, g.last) = if g.n == 0 { (m.at, m.at) } else { (g.first.min(m.at), g.last.max(m.at)) };
        g.n += 1;
        g.newest.push((m.at, m.lc_key.into()));
        g.newest.sort_by(|a, b| b.cmp(a));
        g.newest.truncate(5);
    }

    fn rows(self) -> Vec<Value> {
        self.groups
            .into_iter()
            .map(|((actor, event, from, refusal), g)| {
                let examples: Vec<String> = g.newest.iter().map(|(at, key)| format!("{key}@{at}")).collect();
                object(vec![
                    ("actor", text(actor)),
                    ("event", text(event)),
                    ("from_state", text(from)),
                    ("refusal", text(refusal)),
                    ("n", text(g.n)),
                    ("first_at", text(g.first)),
                    ("last_at", text(g.last)),
                    ("examples", text(examples.join(","))),
                ])
            })
            .collect()
    }
}

fn cannot(e: impl std::fmt::Display) -> (i32, String) {
    (2, format!("cannot tell: {e}"))
}

fn with_store(f: impl FnOnce(&Store<crate::control::served::Served>) -> (i32, String)) -> (i32, String) {
    match crate::control::served::store() {
        Ok(s) => f(&s),
        Err(e) => cannot(e),
    }
}

/// Folds the audits of the last `span` seconds into `visit`, newest first, holding none of them.
fn folded<B: Backend>(store: &Store<B>, now: i64, span: i64, visit: impl FnMut(Move)) -> Result<(), String> {
    match store.fold_audits_since(now - span, visit) {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!("the audit log is too long to read back {span}s in one call")),
        Err(e) => Err(e.to_string()),
    }
}

fn view_arg(args: &[String]) -> Result<&'static str, (i32, String)> {
    args.first().and_then(|a| VIEWS.iter().find(|v| **v == a.as_str())).copied().ok_or_else(|| (2, format!("ops-view: takes one of {}", VIEWS.join(" | "))))
}

/// `ops-view <view>`: one view as a JSON array.
pub fn cmd_ops_view(args: &[String]) -> (i32, String) {
    let view = match view_arg(args) {
        Ok(v) => v,
        Err(refusal) => return refusal,
    };
    with_store(|store| view_rows(store, view, crate::db::now_epoch()))
}

fn view_rows<B: Backend>(store: &Store<B>, view: &str, now: i64) -> (i32, String) {
    let rows = match view {
        "ops_live" => ops_live(store),
        "ops_round" => ops_round(store),
        "ops_recent" => ops_recent(store, now),
        "ops_edges" => ops_edges(store, now),
        "ops_dwell_p95" => ops_dwell_p95(store, now),
        _ => ops_dwell(store, now),
    };
    match rows {
        Ok(rows) => (0, Value::Array(rows).to_string()),
        Err(e) => cannot(e),
    }
}

/// The stuck page's bars: the day's transitions plus the clock they are measured against.
pub fn cmd_ops_transitions(args: &[String]) -> (i32, String) {
    let machine = match machine_arg("ops-transitions", args, &TRANSITION_MACHINES) {
        Ok(m) => m,
        Err(refusal) => return refusal,
    };
    with_store(|store| {
        let now = crate::db::now_epoch();
        match transitions(store, machine, now) {
            Ok(events) => (0, json!({ "now": now, "events": events }).to_string()),
            Err(e) => cannot(e),
        }
    })
}

/// `ops-refusals <window-secs>`: the refused bead events of the window, grouped by who asked.
pub fn cmd_ops_refusals(args: &[String]) -> (i32, String) {
    let Some(window) = args.first().and_then(|a| a.parse::<u64>().ok()).filter(|w| *w > 0) else {
        return (2, "ops-refusals: takes one positive <window-secs>".into());
    };
    with_store(|store| refusals_answer(store, crate::db::now_epoch(), window))
}

fn refusals_answer<B: Backend>(store: &Store<B>, now: i64, window: u64) -> (i32, String) {
    if window > DAY_S as u64 {
        return cannot(format!("ops-refusals reads at most {DAY_S} s from the refusal tally; asked {window}"));
    }
    match store.refusal_tally(now, window as i64) {
        Ok(RefusalTally::Filling { covered_from }) => cannot(format!("the refusal tally is filling: covered from {}", covered_from.map_or("none".to_string(), |at| at.to_string()))),
        Ok(RefusalTally::Ready(groups)) => (0, json!({ "now": now, "classes": refusal_rows(groups) }).to_string()),
        Err(e) => cannot(e),
    }
}

fn refusal_rows(groups: BTreeMap<Class, TallyGroup>) -> Vec<Value> {
    groups
        .into_iter()
        .map(|((actor, event, from, refusal), g)| {
            let examples: Vec<String> = g.newest.iter().map(|(at, key)| format!("{key}@{at}")).collect();
            object(vec![
                ("actor", text(actor)),
                ("event", text(event)),
                ("from_state", text(from)),
                ("refusal", text(refusal)),
                ("n", text(g.n)),
                ("first_at", text(g.first_at)),
                ("last_at", text(g.last_at)),
                ("examples", text(examples.join(","))),
            ])
        })
        .collect()
}

/// One bead's whole timeline, refused events included, with the row that says who holds it.
pub fn cmd_ops_bead(args: &[String]) -> (i32, String) {
    let Some(id) = args.first() else {
        return (2, "ops-bead: missing <bead-id>".into());
    };
    with_store(|store| bead_timeline(store, id))
}

fn bead_timeline<B: Backend>(store: &Store<B>, id: &str) -> (i32, String) {
    let (record, history) = match (store.record(id), store.history(id, Some("bead"))) {
        (Ok(r), Ok(h)) => (r, h),
        (Err(e), _) | (_, Err(e)) => return cannot(e),
    };
    if record.is_none() && history.is_empty() {
        return (1, "{}".into());
    }
    let bead = record.map(|r| {
        let (row, holds): (_, Vec<&str>) = (&r.value.row, r.value.row.holds.iter().map(|h| h.as_str()).collect());
        object(vec![
            ("bead_id", text(&row.bead_id)),
            ("state", text(row.state.as_str())),
            ("holder", opt(row.holder.as_ref())),
            ("persona", opt(row.persona.as_ref())),
            ("lease_until", opt(row.lease_until)),
            ("holds", text(json!(holds))),
            ("reason", opt(row.reason.as_ref())),
            ("gate_key", opt(row.gate_key.as_ref())),
            ("updated_at", text(r.value.updated_at)),
        ])
    });
    let events: Vec<Value> = history
        .iter()
        .map(|c| {
            let a = &c.audit;
            object(vec![
                ("seq", text(c.rev)),
                ("event", text(&a.event)),
                ("from_state", text(&a.from_state)),
                ("to_state", text(&a.to_state)),
                ("applied", text(u8::from(a.applied))),
                ("refusal", opt(a.refusal.as_ref())),
                ("evidence", text(&a.evidence)),
                ("actor", text(&a.actor)),
                ("at", text(a.at)),
            ])
        })
        .collect();
    (0, json!({ "now": crate::db::now_epoch(), "bead": bead, "events": events }).to_string())
}

/// A machine's legal moves, read from its own transition table; the bead machine's with no flag.
pub fn cmd_ops_graph(args: &[String]) -> (i32, String) {
    match machine_arg("ops-graph", args, &GRAPH_MACHINES) {
        Ok(machine) => (0, graph_json(machine)),
        Err(refusal) => refusal,
    }
}

/// States spelled as the machine's state column holds them, so a pane joins edges to view rows.
fn graph_json(machine: &str) -> String {
    macro_rules! edges_of {
        ($m:ident) => {
            lifecycle::$m::legal_edges().iter().map(|e| serde_json::json!({"from": e.from.as_str(), "event": e.event, "to": e.to.as_str()})).collect::<Vec<Value>>()
        };
    }
    let edges = match machine {
        "batch" => edges_of!(batch),
        "ask" => edges_of!(ask),
        "mending" => edges_of!(mending),
        _ => edges_of!(bead),
    };
    Value::Array(edges).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::audits::Logged;
    use crate::control::records::Audit;
    use lifecycle::bead::{BeadEvent, BeadEventKind};
    use runtime::ports::VirtualClock;
    use state_store::{Backend, Config, Contract, MemBackend, Ports, Request};

    fn store() -> Store<MemBackend> {
        let c = Contract::open(MemBackend::new(), Ports::virtual_clock(&VirtualClock::new(0)), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        Store::new(c, 1_000)
    }

    fn audit(bead: &str, event: &str, from: &str, to: &str, applied: bool, actor: &str, at: i64) -> Logged {
        Logged {
            rev: at as u64,
            audit: Audit { machine: "bead".into(), lc_key: bead.into(), event: event.into(), expect: from.into(), from_state: from.into(), to_state: to.into(), applied, refusal: (!applied).then(|| "IllegalTransition".to_string()), evidence: json!({}), actor: actor.into(), at },
        }
    }

    fn move_of(l: &Logged) -> Move<'_> {
        let a = &l.audit;
        Move { rev: l.rev, machine: &a.machine, lc_key: &a.lc_key, event: &a.event, from_state: &a.from_state, to_state: &a.to_state, applied: a.applied, refusal: a.refusal.as_deref(), actor: &a.actor, at: a.at }
    }

    /// A sqlite store whose log holds `log`, one revision per audit.
    fn logged_store(log: &[Logged]) -> Store<state_store_sqlite::SqliteBackend> {
        use state_store::{events_key, Object, Rev, Write};
        use state_store_sqlite::{Options, SqliteBackend};
        let dir = Box::leak(Box::new(testkit::TempDir::new("spira-lc-ops-log")));
        let b = SqliteBackend::open(&dir.path().join("control.db"), Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
        let mut head = b.head().unwrap();
        for l in log {
            let a = serde_json::to_value(&l.audit).unwrap();
            let events = json!({ "refused": !l.audit.applied, "evidence": { if l.audit.applied { "then" } else { "else" }: [a] } });
            let write = Write::Put { key: events_key(head + 1), value: Object::new(events).encode(), create_rev: Rev::new(head + 1, 0), prev: None };
            head = b.commit(head, &[write]).unwrap();
        }
        let c = Contract::open(b, Ports::virtual_clock(&VirtualClock::new(0)), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        Store::new(c, 1_000)
    }

    #[test_id(tqd2qnu)]
    #[test]
    fn the_graph_is_json_edges_with_state_names_and_events() {
        let out = graph_json("bead");
        let edges: Vec<Value> = serde_json::from_str(&out).unwrap();
        let has = |f: &str, ev: &str, t: &str| edges.iter().any(|e| e["from"] == f && e["event"] == ev && e["to"] == t);
        assert!(has("READY", "Claim", "WORKING"), "{out}");
        assert!(has("SUBMITTED", "GatePass", "CERTIFIED"), "{out}");
        assert!(has("CERTIFIED", "GateRed", "REWORK"), "{out}");
        let terminal = ["LANDED", "DROPPED", "SUPERSEDED", "DONE"];
        assert!(!edges.iter().any(|e| terminal.iter().any(|t| e["from"] == *t)), "no exit from a terminal state: {out}");
    }

    #[test_id(twtwfip)]
    #[test]
    fn ops_graph_machine_answers_that_machines_edges() {
        let edges = |args: &[&str]| -> Vec<Value> {
            let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            let (rc, out) = cmd_ops_graph(&args);
            assert_eq!(rc, 0, "{out}");
            serde_json::from_str(&out).unwrap()
        };
        let has = |edges: &[Value], f: &str, ev: &str, t: &str| edges.iter().any(|e| e["from"] == f && e["event"] == ev && e["to"] == t);
        let batch = edges(&["--machine", "batch"]);
        assert!(has(&batch, "OPEN", "Abandon", "ABANDONED") && has(&batch, "GREEN", "FastForward", "LANDED"));
        assert!(has(&edges(&["--machine", "ask"]), "OPEN", "Answer", "ANSWERED"));
        assert!(has(&edges(&["--machine", "mending"]), "WAITING", "Pickup", "MENDING"));
        assert_eq!(edges(&[]), edges(&["--machine", "bead"]), "no flag answers as the bead machine");
        let (rc, out) = cmd_ops_graph(&["--machine".into(), "delivery".into()]);
        assert_eq!(rc, 2, "{out}");
    }

    #[test_id(t6nvk34)]
    #[test]
    fn the_transitions_machine_is_one_the_log_carries_bead_facts_for() {
        assert_eq!(machine_arg("ops-transitions", &[], &TRANSITION_MACHINES), Ok("bead"));
        assert_eq!(machine_arg("ops-transitions", &["--machine".into(), "batch".into()], &TRANSITION_MACHINES), Ok("batch"));
        assert_eq!(machine_arg("ops-transitions", &["--machine".into(), "bead' OR 1=1 --".into()], &TRANSITION_MACHINES).unwrap_err().0, 2);
        let refused = machine_arg("ops-transitions", &["--machine".into(), "ask".into()], &TRANSITION_MACHINES).unwrap_err();
        assert_eq!(refused.0, 2);
        assert!(refused.1.starts_with("ops-transitions:"), "{}", refused.1);
    }

    #[test_id(tqg6j0i)]
    #[test]
    fn ops_view_refuses_anything_but_its_views() {
        for v in VIEWS {
            assert_eq!(view_arg(&[v.to_string()]), Ok(v));
        }
        assert_eq!(view_arg(&["bead".to_string()]).unwrap_err().0, 2);
        assert_eq!(view_arg(&[]).unwrap_err().0, 2);
    }

    #[test_id(tifobli)]
    #[test]
    fn the_transitions_list_the_days_applied_state_changes_of_the_machine_asked_for() {
        let now = 1_000_000;
        let log = [
            audit("sp-a", "Claim", "READY", "WORKING", true, "d", now - 10),
            audit("sp-a", "Hold", "WORKING", "WORKING", true, "d", now - 9),
            audit("sp-a", "Submit", "WORKING", "SUBMITTED", false, "d", now - 8),
            audit("sp-b", "Claim", "READY", "WORKING", true, "d", now - DAY_S - 1),
        ];
        let s = logged_store(&log);
        let rows = transitions(&s, "bead", now).unwrap();
        assert_eq!(rows, [json!({"seq": "1", "bead_id": "sp-a", "event": "Claim", "from_state": "READY", "to_state": "WORKING", "at": (now - 10).to_string()})]);
        assert!(transitions(&s, "batch", now).unwrap().is_empty());
    }

    #[test_id(tx2pkjy)]
    #[test]
    fn refusals_group_by_who_asked_and_name_the_newest_five_examples() {
        let now = 1_000_000;
        let log: Vec<Logged> = (0..7).map(|n| audit(&format!("sp-{n}"), "Submit", "READY", "READY", false, "dwarf", now - 100 + n)).chain([audit("sp-x", "Submit", "READY", "SUBMITTED", true, "dwarf", now - 5), audit("sp-y", "Submit", "READY", "READY", false, "other", now - 2 * DAY_S)]).collect();
        let mut refused = Refusals::new(now - 3_600);
        log.iter().rev().for_each(|l| refused.take(&move_of(l)));
        let rows = refused.rows();
        assert_eq!(rows.len(), 1, "an applied event and one outside the window are no refusal: {rows:?}");
        assert_eq!(rows[0]["n"], "7");
        assert_eq!(rows[0]["first_at"], (now - 100).to_string());
        assert_eq!(rows[0]["last_at"], (now - 94).to_string());
        assert_eq!(rows[0]["examples"].as_str().unwrap().split(',').count(), 5);
        assert!(rows[0]["examples"].as_str().unwrap().starts_with(&format!("sp-6@{}", now - 94)));
    }

    fn edge_rows<B: Backend>(store: &Store<B>, now: i64) -> Value {
        let (rc, out) = view_rows(store, "ops_edges", now);
        assert_eq!(rc, 0, "{out}");
        serde_json::from_str(&out).unwrap()
    }

    fn reference_rows<B: Backend>(store: &Store<B>, now: i64) -> Value {
        let mut edges = Edges::default();
        folded(store, now, DAY_S, |m| edges.take(now, &m)).unwrap();
        Value::Array(edges.rows())
    }

    fn refused(bead: &str, from: &str, why: &str, at: i64) -> Logged {
        let mut l = audit(bead, "Submit", from, from, false, "d", at);
        l.audit.refusal = Some(why.into());
        l
    }


    #[test_id(t3uo1aw)]
    #[test]
    fn ops_refusals_reads_neither_the_audit_log_nor_the_history() {
        use crate::counting::Counting;
        let (store, now, _dir) = with_records_on(Counting::new, 400);
        the_log_is_there_to_read(&store);
        let (rc, out) = refusals_answer(&store, now, DAY_S as u64);
        assert_eq!(rc, 0, "{out}");
        let log = of_the_log(store.contract().backend().take());
        assert!(log.is_empty(), "ops-refusals read the log: {log:?}");
    }

    fn refusal_reference<B: Backend>(store: &Store<B>, now: i64, window: i64) -> Value {
        let mut refused = Refusals::new(now - window);
        folded(store, now, window, |m| refused.take(&m)).unwrap();
        Value::Array(refused.rows())
    }

    fn refusal_rows<B: Backend>(store: &Store<B>, now: i64, window: u64) -> Value {
        let (rc, out) = refusals_answer(store, now, window);
        assert_eq!(rc, 0, "{out}");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["now"], now);
        v["classes"].clone()
    }

    #[test_id(t6ojl1q)]
    #[test]
    fn the_refusal_tally_answers_what_a_fold_of_the_days_audits_answers() {
        let now = 1_000_020;
        let mut other = refused("sp-o", "READY", "IllegalTransition", now - 10);
        other.audit.machine = "batch".into();
        let mut log = vec![
            refused("sp-old", "READY", "IllegalTransition", now - DAY_S - 60),
            audit("sp-n", "Submit", "READY", "SUBMITTED", true, "d", now - 20),
            other,
        ];
        for n in 0..8 {
            log.push(refused(&format!("sp-{n}"), "READY", "IllegalTransition", now - 7_000 + n * 40));
        }
        for n in 0..3 {
            let mut l = refused(&format!("sp-e{n}"), "READY", "IllegalTransition", now - 100 + n);
            l.audit.actor = "e".into();
            log.push(l);
        }
        log.push(refused("sp-s", "WORKING", "StaleVersion", now - 90_000 + 3_600 * 24));
        log.sort_by_key(|l| l.audit.at);
        let store = logged_store(&log);
        store.fold_refusals(now, 100_000).unwrap();
        for window in [DAY_S, 3_600] {
            let rows = refusal_rows(&store, now, window as u64);
            assert_eq!(rows, refusal_reference(&store, now, window), "window {window}");
        }
        let rows = refusal_rows(&store, now, DAY_S as u64);
        assert!(rows.as_array().unwrap().iter().any(|r| r["actor"] == "d" && r["n"] == "8" && r["examples"].as_str().unwrap().split(',').count() == 5), "{rows}");
        assert!(rows.as_array().unwrap().iter().any(|r| r["actor"] == "e" && r["n"] == "3"), "{rows}");
    }

    fn refusal_keys<B: Backend>(s: &Store<B>) -> Vec<(String, Value)> {
        let (items, _) = s.contract().list(&[crate::control::keys::REFUSALS], &state_store::Selector::Everything).unwrap();
        items.into_iter().filter(|i| i.key != crate::control::keys::REFUSALS_CURSOR).map(|i| (i.key, i.value.data)).collect()
    }

    #[test_id(ton0krc)]
    #[test]
    fn while_the_refusal_tally_fills_ops_refusals_cannot_tell() {
        let now = 1_000_020;
        let log = [refused("sp-a", "READY", "IllegalTransition", now - 60), refused("sp-b", "READY", "IllegalTransition", now - 50), refused("sp-c", "READY", "IllegalTransition", now - 40)];
        let store = logged_store(&log);
        let filling = |store: &_| {
            let (rc, out) = refusals_answer(store, now, 3_600);
            (rc, out.starts_with("cannot tell: the refusal tally is filling: covered from "))
        };
        assert_eq!(filling(&store), (2, true));
        store.fold_refusals(now, 1).unwrap();
        assert_eq!(filling(&store), (2, true));
        store.fold_refusals(now, 100).unwrap();
        assert_eq!(refusal_rows(&store, now, 3_600).as_array().unwrap().len(), 1);
        let empty = logged_store(&[]);
        empty.fold_refusals(now, 100).unwrap();
        assert_eq!(refusal_rows(&empty, now, 3_600), json!([]));
    }

    #[test_id(txv59hq)]
    #[test]
    fn a_window_over_a_day_is_not_answered_from_the_refusal_tally() {
        let store = logged_store(&[]);
        store.fold_refusals(1_000_020, 100).unwrap();
        let (rc, out) = refusals_answer(&store, 1_000_020, 14 * DAY_S as u64);
        assert_eq!((rc, out.as_str()), (2, "cannot tell: ops-refusals reads at most 86400 s from the refusal tally; asked 1209600"));
    }

    #[test_id(tpk7q5k)]
    #[test]
    fn folding_the_refusals_twice_or_in_small_steps_gives_the_tally_of_one_fold() {
        let now = 1_000_000_000;
        let log: Vec<Logged> = (0..40).map(|n| refused(&format!("sp-{}", n % 7), "READY", if n % 3 == 0 { "StaleVersion" } else { "IllegalTransition" }, now - 50_000 + n * 600)).collect();
        let (whole, stepped) = (logged_store(&log), logged_store(&log));
        whole.fold_refusals(now, 10_000).unwrap();
        let again = whole.fold_refusals(now, 10_000).unwrap();
        assert!(!again.committed, "a fold with nothing new writes nothing: {again:?}");
        let mut steps = 0;
        loop {
            steps += 1;
            if stepped.fold_refusals(now, 3).unwrap().revs < 3 {
                break;
            }
        }
        assert!(steps > 5, "{steps}");
        assert_eq!(refusal_keys(&stepped), refusal_keys(&whole));
        assert!(!refusal_keys(&whole).is_empty());
        stepped.fold_refusals(now, 3).unwrap();
        assert_eq!(refusal_keys(&stepped), refusal_keys(&whole), "a repeated step changes nothing");
        assert_eq!(refusal_rows(&stepped, now, DAY_S as u64), refusal_reference(&stepped, now, DAY_S));
    }

    fn edge_keys<B: Backend>(s: &Store<B>) -> Vec<(String, Value)> {
        let (items, _) = s.contract().list(&[crate::control::keys::EDGES], &state_store::Selector::Everything).unwrap();
        items.into_iter().filter(|i| i.key != crate::control::keys::EDGES_CURSOR).map(|i| (i.key, i.value.data)).collect()
    }

    #[test_id(tuspb85)]
    #[test]
    fn the_edge_tally_answers_what_a_fold_of_the_days_audits_answers() {
        let now = 1_000_020;
        let mut other = audit("sp-o", "Claim", "READY", "WORKING", true, "d", now - 10);
        other.audit.machine = "batch".into();
        let log = [
            audit("sp-old", "Claim", "READY", "WORKING", true, "d", now - DAY_S - 120),
            refused("sp-old", "READY", "IllegalTransition", now - DAY_S - 60),
            audit("sp-b", "Claim", "READY", "WORKING", true, "d", now - 2 * HOUR_S),
            audit("sp-a", "Claim", "READY", "WORKING", true, "d", now - 3 * HOUR_S),
            audit("sp-a", "Release", "WORKING", "READY", true, "d", now - 125),
            audit("sp-a", "Claim", "READY", "WORKING", true, "d", now - 60),
            audit("sp-c", "Submit", "READY", "READY", false, "d", now - 30),
            refused("sp-c", "READY", "StaleVersion", now - 31),
            refused("sp-c", "READY", "StaleVersion", now - 3 * HOUR_S),
            audit("sp-n", "Touch", "READY", "READY", true, "d", now - 20),
            other,
        ];
        let store = logged_store(&log);
        store.fold_edges(now, 100_000).unwrap();
        let rows = edge_rows(&store, now);
        assert_eq!(rows, reference_rows(&store, now));
        assert_eq!(rows.as_array().unwrap().len(), 4, "{rows}");
        let refusals: Vec<_> = rows.as_array().unwrap().iter().filter(|r| r["kind"] == "refused").map(|r| r["refusal"].as_str().unwrap()).collect();
        assert_eq!(refusals.len(), 2, "{refusals:?}");
    }

    #[test_id(t36vbi3)]
    #[test]
    fn the_edge_tally_counts_the_hour_and_the_day_and_keeps_refusals_apart() {
        let now = 1_000_020;
        let log = [
            audit("sp-b", "Claim", "READY", "WORKING", true, "d", now - 2 * HOUR_S),
            audit("sp-a", "Claim", "READY", "WORKING", true, "d", now - 60),
            audit("sp-c", "Submit", "READY", "READY", false, "d", now - 30),
        ];
        let store = logged_store(&log);
        store.fold_edges(now, 100_000).unwrap();
        let rows = edge_rows(&store, now);
        let applied = rows.as_array().unwrap().iter().find(|r| r["kind"] == "applied").unwrap();
        assert_eq!((applied["n_1h"].as_str(), applied["n_24h"].as_str(), applied["last_at"].as_str()), (Some("1"), Some("2"), Some((now - 60).to_string().as_str())));
        let refused = rows.as_array().unwrap().iter().find(|r| r["kind"] == "refused").unwrap();
        assert_eq!((refused["to_state"].clone(), refused["refusal"].as_str()), (Value::Null, Some("IllegalTransition")));
    }

    #[test_id(tacolss)]
    #[test]
    fn folding_the_edges_twice_or_in_small_steps_gives_the_tally_of_one_fold() {
        let now = 1_000_000_000;
        let (whole, _) = worked(25, now);
        let (stepped, _) = worked(25, now);
        let at = now - 5 * DAY_S + 30_000;
        whole.fold_edges(at, 10_000).unwrap();
        let again = whole.fold_edges(at, 10_000).unwrap();
        assert!(!again.committed, "a fold with nothing new writes nothing: {again:?}");
        let mut steps = 0;
        loop {
            steps += 1;
            if stepped.fold_edges(at, 3).unwrap().revs < 3 {
                break;
            }
        }
        assert!(steps > 5, "{steps}");
        assert_eq!(edge_keys(&stepped), edge_keys(&whole));
        assert!(!edge_keys(&whole).is_empty());
        stepped.fold_edges(at, 3).unwrap();
        assert_eq!(edge_keys(&stepped), edge_keys(&whole), "a repeated step changes nothing");
    }

    #[test_id(tfqeoox)]
    #[test]
    fn while_the_edge_tally_fills_ops_edges_cannot_tell() {
        let now = 1_000_020;
        let log = [audit("sp-a", "Claim", "READY", "WORKING", true, "d", now - 60), audit("sp-b", "Claim", "READY", "WORKING", true, "d", now - 50), audit("sp-c", "Claim", "READY", "WORKING", true, "d", now - 40)];
        let store = logged_store(&log);
        let (rc, out) = view_rows(&store, "ops_edges", now);
        assert_eq!((rc, out.starts_with("cannot tell: the edge tally is filling: ")), (2, true), "{out}");
        store.fold_edges(now, 1).unwrap();
        let (rc, out) = view_rows(&store, "ops_edges", now);
        assert_eq!((rc, out.starts_with("cannot tell: the edge tally is filling: ")), (2, true), "{out}");
        store.fold_edges(now, 100).unwrap();
        assert_eq!(edge_rows(&store, now).as_array().unwrap().len(), 1);
        let empty = logged_store(&[]);
        empty.fold_edges(now, 100).unwrap();
        assert_eq!(edge_rows(&empty, now), json!([]));
    }

    #[test_id(tobhjcv)]
    #[test]
    fn a_minute_older_than_a_day_is_not_counted_and_the_next_fold_deletes_it() {
        use state_store::{Object, Txn};
        let now = 1_000_020;
        let store = logged_store(&[audit("sp-old", "Claim", "READY", "WORKING", true, "d", now - DAY_S - 120), audit("sp-a", "Claim", "READY", "WORKING", true, "d", now - 60)]);
        store.fold_edges(now, 10_000).unwrap();
        let stale = crate::control::keys::edge_minute((now - DAY_S).div_euclid(60) - 1);
        let plant = json!({ "e": [[false, "READY", "WORKING", "Claim", 5, now - DAY_S - 60]] });
        store.contract().txn(&Txn::new().put(&stale, Object::new(plant)), &Request::new("plant", "t", "t")).unwrap();
        let rows = edge_rows(&store, now);
        assert_eq!((rows[0]["n_24h"].as_str(), rows.as_array().unwrap().len()), (Some("1"), 1), "a minute outside the day is not counted");
        store.fold_edges(now, 10_000).unwrap();
        assert!(edge_keys(&store).iter().all(|(k, _)| *k != stale), "the stale minute is deleted by the next fold");
        assert_eq!(edge_keys(&store).len(), 1);
    }

    /// `n` beads each worked for a known time, committed through the store: the WORKING stays.
    fn worked(n: i64, now: i64) -> (Store<MemBackend>, Vec<i64>) {
        let s = store();
        let mut stays = Vec::new();
        for i in 0..n {
            let (id, at) = (format!("sp-{i}"), now - 5 * DAY_S + i * 60);
            let stay = 10 + i * i * 37;
            s.create(&id, Some("harness"), None, at - 1, &Request::new(&format!("c{i}"), "t", "t")).unwrap();
            let claim = BeadEvent { expect: BeadState::Ready, version: 0, kind: BeadEventKind::Claim { holder: "d".into(), lease_until: at + 3_600, stack: Default::default(), stack_depth: 0, stack_max_depth: 0, persona: None, directed: None }, actor: "d".into(), at: Some(at) };
            s.claim(&id, &claim, &Request::new(&format!("k{i}"), "t", "t")).unwrap();
            let version = s.record(&id).unwrap().unwrap().value.row.version;
            let release = BeadEvent { expect: BeadState::Working, version, kind: BeadEventKind::Release, actor: "d".into(), at: Some(at + stay) };
            s.apply(&id, &release, &Request::new(&format!("r{i}"), "t", "t")).unwrap();
            stays.push(stay);
        }
        (s, stays)
    }

    fn exact_p95(mut stays: Vec<i64>) -> i64 {
        stays.sort_unstable();
        stays[(stays.len() * 95).div_ceil(100).max(1) - 1]
    }

    fn day_keys<B: Backend>(s: &Store<B>) -> Vec<(String, Value)> {
        let (items, _) = s.contract().list(&[crate::control::keys::STAYS], &state_store::Selector::Everything).unwrap();
        items.into_iter().filter(|i| i.key != crate::control::keys::STAYS_CURSOR).map(|i| (i.key, i.value.data)).collect()
    }

    #[test_id(tw7j1yn)]
    #[test]
    fn the_p95_of_a_state_is_the_stay_at_the_95th_percentile_rank_of_the_stays_that_ended_in_it() {
        let now = 1_000_000_000;
        for n in [20, 41, 3] {
            let (s, stays) = worked(n, now);
            s.fold_stays(now, 1_000).unwrap();
            let StayTally::Ready(p) = s.stay_tally(now).unwrap() else { panic!("filling") };
            let (p95, samples) = p["WORKING"];
            let exact = exact_p95(stays);
            assert_eq!(samples, n as usize);
            assert!(p95 >= exact && p95 as f64 <= exact as f64 * 1.091, "tally {p95} vs exact {exact}");
        }
    }

    #[test_id(tgh4r3p)]
    #[test]
    fn folding_twice_or_in_small_steps_gives_the_tally_of_one_uninterrupted_fold() {
        let now = 1_000_000_000;
        let (whole, _) = worked(25, now);
        let (stepped, _) = worked(25, now);
        whole.fold_stays(now, 10_000).unwrap();
        let again = whole.fold_stays(now, 10_000).unwrap();
        assert!(!again.committed, "a fold with nothing new writes nothing: {again:?}");
        let mut steps = 0;
        loop {
            steps += 1;
            let done = stepped.fold_stays(now, 3).unwrap();
            if done.revs < 3 {
                break;
            }
        }
        assert!(steps > 5, "{steps}");
        assert_eq!(day_keys(&stepped), day_keys(&whole));
        assert!(!day_keys(&whole).is_empty());
        stepped.fold_stays(now, 3).unwrap();
        assert_eq!(day_keys(&stepped), day_keys(&whole), "a repeated step changes nothing");
    }

    #[test_id(tvjj7ho)]
    #[test]
    fn a_first_fill_starts_inside_the_window_and_counts_a_stay_that_began_before_it() {
        let now = 2_000_000_000;
        let build = || {
            let s = store();
            let stay = |id: &str, from: i64, to: i64| {
                s.create(id, Some("harness"), None, from - 1, &Request::new(&format!("c-{id}"), "t", "t")).unwrap();
                let claim = BeadEvent { expect: BeadState::Ready, version: 0, kind: BeadEventKind::Claim { holder: "d".into(), lease_until: from + 3_600, stack: Default::default(), stack_depth: 0, stack_max_depth: 0, persona: None, directed: None }, actor: "d".into(), at: Some(from) };
                s.claim(id, &claim, &Request::new(&format!("k-{id}"), "t", "t")).unwrap();
                let version = s.record(id).unwrap().unwrap().value.row.version;
                let release = BeadEvent { expect: BeadState::Working, version, kind: BeadEventKind::Release, actor: "d".into(), at: Some(to) };
                s.apply(id, &release, &Request::new(&format!("r-{id}"), "t", "t")).unwrap();
            };
            stay("sp-before", now - 30 * DAY_S, now - 29 * DAY_S);
            stay("sp-across", now - 20 * DAY_S, now - 10 * DAY_S);
            s
        };
        let (whole, stepped) = (build(), build());
        whole.fold_stays(now, 10_000).unwrap();
        while stepped.fold_stays(now, 2).unwrap().revs == 2 {}
        assert_eq!(day_keys(&stepped), day_keys(&whole));
        let cursor = whole.contract().get(crate::control::keys::STAYS_CURSOR).unwrap().unwrap().value.data;
        assert_eq!(cursor["filled_from"], now - 14 * DAY_S, "the fill started from the window's start");
        let StayTally::Ready(p) = whole.stay_tally(now).unwrap() else { panic!("filling: {cursor}") };
        assert_eq!(p["WORKING"].1, 1, "only the stay that ended inside the window, which began before it");
        assert!(p["WORKING"].0 >= 10 * DAY_S);
    }

    #[test_id(tkv9bxd)]
    #[test]
    fn a_day_before_the_window_is_not_counted_and_the_next_fold_deletes_it() {
        use state_store::{Object, Txn};
        let now = 2_000_000_000;
        let s = store();
        let at = now - DAY_S;
        s.create("sp-a", Some("harness"), None, at - 1, &Request::new("c-a", "t", "t")).unwrap();
        let claim = BeadEvent { expect: BeadState::Ready, version: 0, kind: BeadEventKind::Claim { holder: "d".into(), lease_until: at + 3_600, stack: Default::default(), stack_depth: 0, stack_max_depth: 0, persona: None, directed: None }, actor: "d".into(), at: Some(at) };
        s.claim("sp-a", &claim, &Request::new("k-a", "t", "t")).unwrap();
        s.fold_stays(now, 10_000).unwrap();
        let stale = crate::control::keys::stay_day("WORKING", (now - 14 * DAY_S).div_euclid(DAY_S) - 1);
        s.contract().txn(&Txn::new().put(&stale, Object::new(json!({"n": 5, "b": {"200": 5}}))), &Request::new("plant", "t", "t")).unwrap();
        let samples = |s: &Store<MemBackend>| match s.stay_tally(now).unwrap() {
            StayTally::Ready(p) => p.get("WORKING").map_or(0, |w| w.1),
            _ => panic!("filling"),
        };
        assert_eq!(samples(&s), 0, "a day outside the window is not counted");
        let version = s.record("sp-a").unwrap().unwrap().value.row.version;
        let release = BeadEvent { expect: BeadState::Working, version, kind: BeadEventKind::Release, actor: "d".into(), at: Some(at + 60) };
        s.apply("sp-a", &release, &Request::new("r-a", "t", "t")).unwrap();
        s.fold_stays(now, 10_000).unwrap();
        assert_eq!(samples(&s), 1);
        assert!(day_keys(&s).iter().all(|(k, _)| *k != stale), "the stale day key is deleted by the next fold");
    }

    fn landed_keys<B: Backend>(s: &Store<B>) -> Vec<(String, Value)> {
        let (items, _) = s.contract().list(&["landed/"], &state_store::Selector::Everything).unwrap();
        items.into_iter().map(|i| (i.key, i.value.data)).collect()
    }

    fn rows_of<B: Backend>(s: &Store<B>, view: &str, now: i64) -> Vec<Value> {
        let (rc, out) = view_rows(s, view, now);
        assert_eq!(rc, 0, "{view}: {out}");
        serde_json::from_str(&out).unwrap()
    }

    fn keep_record<B: Backend>(s: &Store<B>, id: &str, state: BeadState, since: i64) {
        use crate::control::{keys, records::{encode, LifecycleRecord}};
        let mut rec = LifecycleRecord::born(id, Some("harness"), None, since);
        rec.row.state = state;
        rec.row.since = Some(since);
        s.contract().create(&keys::lifecycle(id), state_store::Object::new(encode(&rec).unwrap()), &Request::new(&format!("rec-{id}"), "t", "t")).unwrap();
    }

    #[test_id(tkbb0yt)]
    #[test]
    fn the_recent_view_reads_neither_the_audit_log_nor_the_history() {
        use crate::counting::Counting;
        let (store, now, _dir) = with_records_on(Counting::new, 400);
        store.fold_landed(now, 100_000).unwrap();
        the_log_is_there_to_read(&store);
        let rows = rows_of(&store, "ops_recent", now);
        let ids: Vec<&str> = rows.iter().map(|r| r["bead_id"].as_str().unwrap()).collect();
        assert_eq!(ids, (20..30).map(|n| format!("sp-{n:05}")).collect::<Vec<_>>());
        let log = of_the_log(store.contract().backend().take());
        assert!(log.is_empty(), "ops_recent read the log: {log:?}");
    }

    #[test_id(ts79064)]
    #[test]
    fn the_landed_list_answers_what_a_fold_of_the_days_audits_answers() {
        let now = 1_000_000;
        let mut other = audit("sp-f", "FastForward", "CERTIFIED", "LANDED", true, "d", now - 50);
        other.audit.machine = "batch".into();
        let log = [
            audit("sp-a", "FastForward", "CERTIFIED", "LANDED", true, "d", now - 100),
            audit("sp-b", "FastForward", "CERTIFIED", "LANDED", true, "d", now - DAY_S - 100),
            audit("sp-c", "FastForward", "CERTIFIED", "LANDED", true, "d", now - 90),
            audit("sp-c", "Reopen", "LANDED", "REWORK", true, "d", now - 80),
            audit("sp-d", "FastForward", "CERTIFIED", "LANDED", false, "d", now - 70),
            audit("sp-e", "Touch", "LANDED", "LANDED", true, "d", now - 60),
            other,
        ];
        let s = logged_store(&log);
        keep_record(&s, "sp-a", BeadState::Landed, now - 100);
        keep_record(&s, "sp-b", BeadState::Landed, now - DAY_S - 100);
        keep_record(&s, "sp-c", BeadState::Rework, now - 80);
        keep_record(&s, "sp-d", BeadState::Certified, now - 70);
        keep_record(&s, "sp-e", BeadState::Landed, now - 60);
        keep_record(&s, "sp-f", BeadState::Landed, now - 50);
        s.fold_landed(now, 10_000).unwrap();
        let listed = rows_of(&s, "ops_recent", now);
        assert_eq!(listed.iter().map(|r| r["bead_id"].as_str().unwrap()).collect::<Vec<_>>(), ["sp-a"]);
        assert_eq!(listed, recent_by_fold(&s, now).unwrap());
    }

    #[test_id(t6kpzmo)]
    #[test]
    fn folding_the_landed_list_twice_or_in_small_steps_gives_the_list_of_one_fold() {
        let now = 1_000_000;
        let log: Vec<Logged> = (0..10).map(|n| audit(&format!("sp-{n}"), "FastForward", "CERTIFIED", "LANDED", true, "d", now - 100 + n)).collect();
        let (whole, stepped) = (logged_store(&log), logged_store(&log));
        whole.fold_landed(now, 10_000).unwrap();
        let again = whole.fold_landed(now, 10_000).unwrap();
        assert!(!again.committed, "a fold with nothing new writes nothing: {again:?}");
        let mut steps = 0;
        loop {
            steps += 1;
            if stepped.fold_landed(now, 3).unwrap().revs < 3 {
                break;
            }
        }
        assert!(steps > 2, "{steps}");
        let keys = |s: &Sqlite| landed_keys(s).into_iter().filter(|(k, _)| k != crate::control::keys::LANDED_CURSOR).collect::<Vec<_>>();
        assert_eq!(keys(&stepped), keys(&whole));
        assert_eq!(keys(&whole).len(), 10);
        stepped.fold_landed(now, 3).unwrap();
        assert_eq!(keys(&stepped), keys(&whole), "a repeated step changes nothing");
    }

    #[test_id(tmocnpa)]
    #[test]
    fn while_the_landed_list_fills_ops_recent_cannot_tell() {
        let now = 1_000_000;
        let s = logged_store(&[audit("sp-a", "FastForward", "CERTIFIED", "LANDED", true, "d", now - 100)]);
        let (rc, out) = view_rows(&s, "ops_recent", now);
        assert_eq!(rc, 2, "{out}");
        assert!(out.contains("the landed list is filling: "), "{out}");
        let head = s.contract().head().unwrap();
        s.contract().create(crate::control::keys::LANDED_CURSOR, state_store::Object::new(json!({ "rev": 0, "filled_from": 0, "fill_to": head })), &Request::new("cur", "t", "t")).unwrap();
        let (rc, out) = view_rows(&s, "ops_recent", now);
        assert_eq!(rc, 2, "{out}");
        assert!(out.contains("the landed list is filling: "), "{out}");
        let empty = logged_store(&[]);
        empty.fold_landed(now, 10_000).unwrap();
        assert_eq!(view_rows(&empty, "ops_recent", now), (0, "[]".to_string()));
    }

    #[test_id(tnd2iqs)]
    #[test]
    fn a_landing_older_than_a_day_is_not_listed_and_the_next_fold_deletes_it() {
        use state_store::{Object, Txn};
        let now = 2_000_000_000;
        let s = logged_store(&[audit("sp-a", "FastForward", "CERTIFIED", "LANDED", true, "d", now - 100)]);
        s.fold_landed(now, 10_000).unwrap();
        let stale = crate::control::keys::landed_entry(now - DAY_S - 5, "sp-old");
        s.contract().txn(&Txn::new().put(&stale, Object::new(json!({ "at": now - DAY_S - 5 }))), &Request::new("plant", "t", "t")).unwrap();
        let listed = || match s.landed_ids(now).unwrap() {
            crate::control::landed::Landed::Ready(ids) => ids,
            other => panic!("{other:?}"),
        };
        assert_eq!(listed(), ["sp-a"], "an entry older than a day is not listed");
        s.fold_landed(now, 10_000).unwrap();
        assert!(landed_keys(&s).iter().all(|(k, _)| *k != stale), "the stale entry is deleted by the next fold");
        assert_eq!(listed(), ["sp-a"]);
    }

    #[test_id(t1kac28)]
    #[test]
    fn the_recent_view_reads_as_many_keys_for_a_longer_history() {
        use crate::counting::Counting;
        let reads = |audits| {
            let (store, now, _dir) = with_records_on(Counting::new, audits);
            store.contract().backend().take();
            rows_of(&store, "ops_recent", now);
            store.contract().backend().take().len()
        };
        assert_eq!(reads(200), reads(800));
    }

    #[test_id(tdrvk8j)]
    #[test]
    fn a_bare_list_page_reads_only_lifecycle_rows() {
        use crate::control::{claimable, keys, state};
        use crate::counting::Counting;
        let (store, now, _dir) = with_records_on(Counting::new, 400);
        store.contract().backend().take();
        let page = |argv: &[&str]| -> (i32, String, Vec<(&'static str, String, usize)>) {
            store.contract().backend().take();
            let argv: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
            let (rc, out) = state::run(&store, &argv, &state::Call { now, request: "r", inputs: &claimable::FIXED });
            let reads = store.contract().backend().take();
            (rc, out, reads)
        };
        let (rc, out, reads) = page(&["list", "--bare", "--limit", "100"]);
        assert_eq!(rc, 0, "{out}");
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap().as_array().map(|a| a.len()), Some(100), "{out:.200}");
        let off: Vec<_> = reads.iter().filter(|(_, key, _)| !key.trim_start_matches("locks/").starts_with(keys::LIFECYCLE)).collect();
        assert!(off.is_empty(), "a bare page read beyond the lifecycle rows: {off:?}");

        let (rc, out, reads) = page(&["list", "--limit", "100"]);
        assert_eq!(rc, 0, "{out:.200}");
        assert!(reads.iter().any(|(_, key, _)| key.starts_with(keys::READINESS)), "the full page did not read readiness, so the bare page proves nothing");
    }

    #[test_id(tq6yvp2)]
    #[test]
    fn the_edges_view_reads_neither_the_audit_log_nor_the_history() {
        use crate::counting::Counting;
        let (store, now, _dir) = with_records_on(Counting::new, 400);
        the_log_is_there_to_read(&store);
        let (rc, out) = view_rows(&store, "ops_edges", now);
        assert_eq!(rc, 0, "{out}");
        let log = of_the_log(store.contract().backend().take());
        assert!(log.is_empty(), "ops_edges read the log: {log:?}");
    }

    #[test_id(tvrk235)]
    #[test]
    fn the_edges_view_reads_as_many_keys_for_a_longer_history() {
        use crate::counting::Counting;
        let reads = |audits| {
            let (store, now, _dir) = with_records_on(Counting::new, audits);
            store.contract().backend().take();
            let (rc, out) = view_rows(&store, "ops_edges", now);
            assert_eq!(rc, 0, "{out}");
            store.contract().backend().take().len()
        };
        let a_day = (DAY_S / 60) as usize / crate::control::edges::MINUTES_PAGE + 1;
        let (short, long) = (reads(200), reads(800));
        assert!(short == long && long <= a_day + 4, "{short} reads for 200 audits and {long} for 800, a day is {a_day} pages");
    }

    #[test_id(tslxytf)]
    #[test]
    fn the_dwell_views_read_neither_the_audit_log_nor_the_history() {
        use crate::counting::Counting;
        let (store, now, _dir) = with_records_on(Counting::new, 400);
        store.fold_stays(now, 100_000).unwrap();
        the_log_is_there_to_read(&store);
        for view in ["ops_dwell", "ops_dwell_p95"] {
            let (rc, out) = view_rows(&store, view, now);
            assert_eq!(rc, 0, "{view}: {out}");
            let log = of_the_log(store.contract().backend().take());
            assert!(log.is_empty(), "{view} read the log: {log:?}");
        }
    }

    #[test_id(to0yu7m)]
    #[test]
    fn a_live_beads_entry_older_than_the_window_is_its_row_since_and_reads_no_history() {
        use crate::counting::Counting;
        let now = 2_000_000_000;
        let s = open_on(Counting::<MemBackend>::default());
        let entered = now - 20 * DAY_S;
        s.create("sp-old", Some("harness"), None, entered - 5, &Request::new("c", "t", "t")).unwrap();
        let claim = BeadEvent { expect: BeadState::Ready, version: 0, kind: BeadEventKind::Claim { holder: "d".into(), lease_until: entered + 3_600, stack: Default::default(), stack_depth: 0, stack_max_depth: 0, persona: None, directed: None }, actor: "d".into(), at: Some(entered) };
        s.claim("sp-old", &claim, &Request::new("k", "t", "t")).unwrap();
        s.contract().backend().take();
        let (rc, out) = view_rows(&s, "ops_dwell", now);
        assert_eq!(rc, 0, "{out}");
        let rows: Vec<Value> = serde_json::from_str(&out).unwrap();
        assert_eq!(rows[0]["entered_at"], entered.to_string());
        assert!(s.contract().backend().take().iter().all(|(_, key, _)| !key.starts_with("history/") && !key.starts_with("events/")));
    }

    #[test_id(tjipoph)]
    #[test]
    fn while_the_fill_is_short_of_the_window_p95_cannot_tell_and_dwell_has_null_p95() {
        let now = 2_000_000_000;
        let (s, _) = worked(3, now);
        let (rc, out) = view_rows(&s, "ops_dwell_p95", now);
        assert_eq!(rc, 2, "{out}");
        assert!(out.contains("filling") && out.contains("covered from"), "{out}");
        let (rc, out) = view_rows(&s, "ops_dwell", now);
        assert_eq!(rc, 0, "{out}");
        let head = s.contract().head().unwrap();
        let from = now - 3 * DAY_S;
        s.contract().create(crate::control::keys::STAYS_CURSOR, state_store::Object::new(json!({ "rev": head, "filled_from": from, "fill_to": head })), &Request::new("cur", "t", "t")).unwrap();
        let (rc, out) = view_rows(&s, "ops_dwell_p95", now);
        assert_eq!(rc, 2, "{out}");
        assert!(out.contains(&from.to_string()), "{out}");
        let (rc, out) = view_rows(&s, "ops_dwell", now);
        assert_eq!(rc, 0, "{out}");
        let rows: Vec<Value> = serde_json::from_str(&out).unwrap();
        assert!(rows.iter().all(|r| r["p95_s"].is_null() && r["samples"].is_null()), "{out}");
    }

    #[test_id(t7n34g6)]
    #[test]
    fn a_beads_timeline_carries_its_row_and_the_refused_event_beside_the_applied_one() {
        let s = store();
        assert_eq!(bead_timeline(&s, "sp-none").0, 1);
        s.create("sp-a", Some("harness"), None, 100, &Request::new("c1", "t", "t")).unwrap();
        let bad = BeadEvent { expect: lifecycle::bead::BeadState::Working, version: 0, kind: BeadEventKind::Release, actor: "d".into(), at: Some(110) };
        s.apply("sp-a", &bad, &Request::new("e1", "t", "t")).unwrap();
        let (rc, out) = bead_timeline(&s, "sp-a");
        assert_eq!(rc, 0, "{out}");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["bead"]["state"], "READY");
        assert_eq!(v["bead"]["holds"], "[]");
        let events = v["events"].as_array().unwrap();
        assert!(events.iter().any(|e| e["applied"] == "0" && e["event"] == "Release" && e["refusal"].is_string()), "{out}");
        assert!(events.iter().all(|e| e["seq"].as_str().unwrap().parse::<u64>().is_ok()));
    }

    #[test_id(tt9xewe)]
    #[test]
    fn the_live_view_names_what_keeps_a_ready_bead_from_being_claimed() {
        let s = store();
        s.create("sp-a", Some("harness"), None, 100, &Request::new("c1", "t", "t")).unwrap();
        s.create("sp-b", Some("harness"), None, 100, &Request::new("c2", "t", "t")).unwrap();
        let rows = ops_live(&s).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["claimable"], "1");
        assert_eq!(rows[0]["rework"], "0");
        assert_eq!(rows[0]["repo"], "harness");
        assert_eq!(rows[0]["holder"], Value::Null);
        assert_eq!(rows[0]["holds"], "[]");
    }

    /// The backend of a store shaped like production: many terminal beads with bulky audits
    /// across a fortnight, a few live ones.
    fn fortnight_backend(audits: u64) -> (state_store_sqlite::SqliteBackend, i64, testkit::TempDir) {
        use state_store::{events_key, Backend, Object, Rev, Write};
        use state_store_sqlite::{Options, SqliteBackend};
        let dir = testkit::TempDir::new("spira-lc-ops-memory");
        let b = SqliteBackend::open(&dir.path().join("control.db"), Options { clock: std::sync::Arc::new(VirtualClock::new(0)), ..Options::default() }).unwrap();
        let now: i64 = 2_000_000_000;
        let bulk = "x".repeat(2_000);
        let states = ["READY", "WORKING", "SUBMITTED", "CERTIFIED"];
        let mut head = b.head().unwrap();
        let imported: Vec<Write> = (0..audits / 2)
            .map(|n| {
                let audit = json!({ "machine": "bead", "lc_key": format!("sp-{:04}", n % 300), "event": "Claim", "expect": "READY", "from_state": "READY", "to_state": "WORKING", "applied": true, "refusal": null, "evidence": { "bulk": bulk }, "actor": "a", "at": now - 20 * DAY_S });
                Write::Put { key: crate::control::keys::history_entry(&format!("sp-{:04}", n % 300), 0, &format!("import-{n}")), value: Object::new(json!({ "seq": n + 1, "audit": audit })).encode(), create_rev: Rev::new(head + 1 + n, 0), prev: None }
            })
            .collect();
        for chunk in imported.chunks(1) {
            head = b.commit(head, chunk).unwrap();
        }
        for n in 0..audits {
            let (from, to) = (states[(n % 4) as usize], states[((n + 1) % 4) as usize]);
            let at = now - 13 * DAY_S + (n as i64) * (13 * DAY_S) / audits as i64;
            let a = json!({ "machine": "bead", "lc_key": format!("sp-{:04}", n % 300), "event": "Claim", "expect": from, "from_state": from, "to_state": to, "applied": true, "refusal": null, "evidence": { "bulk": bulk }, "actor": "a", "at": at });
            let events = json!({ "refused": false, "evidence": { "then": [a] } });
            let write = Write::Put { key: events_key(head + 1), value: Object::new(events).encode(), create_rev: Rev::new(head + 1, 0), prev: None };
            head = b.commit(head, &[write]).unwrap();
        }
        (b, now, dir)
    }

    #[test_id(twebfct)]
    #[test]
    fn a_fixture_store_ends_no_read_for_the_time_it_took() {
        let (b, now, _dir) = fortnight_backend(2_000);
        b.set_max_read_ms(0);
        open_on(b).fold_stays(now, 100_000).unwrap();
    }

    fn open_on<B: Backend>(b: B) -> Store<B> {
        Store::new(Contract::open(b, Ports::virtual_clock(&VirtualClock::new(0)), Config { min_lock_ttl_ms: 1_000 }).unwrap(), 1_000)
    }

    fn fortnight(audits: u64) -> (Store<state_store_sqlite::SqliteBackend>, i64, testkit::TempDir) {
        let (b, now, dir) = fortnight_backend(audits);
        let store = open_on(b);
        store.fold_stays(now, 100_000).unwrap();
        store.fold_edges(now, 100_000).unwrap();
        store.fold_landed(now, 100_000).unwrap();
        (store, now, dir)
    }

    /// `fortnight`, plus a quarter as many LANDED records (ten landed in the last day) and a few
    /// live ones: the records grow with the history, the answers to the day's questions do not.
    fn with_records_on<B: Backend>(wrap: impl FnOnce(state_store_sqlite::SqliteBackend) -> B, audits: u64) -> (Store<B>, i64, testkit::TempDir) {
        use crate::control::{keys, records::{encode, LifecycleRecord}};
        use state_store::{Backend, Object, Rev, Write};
        let (b, now, dir) = fortnight_backend(audits);
        let mut head = b.head().unwrap();
        for n in (20..audits / 4 + 20).rev() {
            let at = if n < 30 { now - 100 } else { now - 5 * DAY_S };
            let a = json!({ "machine": "bead", "lc_key": format!("sp-{n:05}"), "event": "FastForward", "expect": "GREEN", "from_state": "CERTIFIED", "to_state": "LANDED", "applied": true, "refusal": null, "evidence": {}, "actor": "a", "at": at });
            let events = json!({ "refused": false, "evidence": { "then": [a] } });
            head += 1;
            b.commit(head - 1, &[Write::Put { key: state_store::events_key(head), value: Object::new(events).encode(), create_rev: Rev::new(head, 0), prev: None }]).unwrap();
        }
        let live = [BeadState::Ready, BeadState::Working, BeadState::Submitted, BeadState::Certified];
        let mut writes = Vec::new();
        for n in 0..audits / 4 + 20 {
            let mut rec = LifecycleRecord::born(&format!("sp-{n:05}"), Some("harness"), None, now - 5 * DAY_S);
            if n < 20 {
                rec.row.state = live[n as usize % 4];
            } else {
                rec.row.state = BeadState::Landed;
                rec.row.since = Some(if n < 30 { now - 100 } else { now - 5 * DAY_S });
                rec.updated_at = rec.row.since.unwrap();
            }
            head += 1;
            writes.push(Write::Put { key: keys::lifecycle(&rec.row.bead_id), value: Object::new(encode(&rec).unwrap()).encode(), create_rev: Rev::new(head, 0), prev: None });
        }
        b.commit(b.head().unwrap(), &writes).unwrap();
        let store = open_on(wrap(b));
        store.fold_landed(now, 100_000).unwrap();
        for n in 0..audits / 8 {
            let (request, row) = (format!("r{n}"), format!("s{n}"));
            let effect = json!({ "task": "t", "ekey": format!("k{n}/x"), "request_id": request, "stream": "s", "spec": { "name": "n", "input": "i", "deadline_ms": 5, "key": format!("k{n}") }, "requested_at": n, "deadline_at": n + 5 });
            let schedule = json!({ "id": row, "at": n, "stream": "s", "kind": { "tag": "tick", "body": "b" }, "cause": null, "correlation": "c" });
            let (resolved, fired) = if n < 5 { (json!([]), json!([])) } else { (json!([request]), json!([row])) };
            let turn = crate::journal::turn_of(&json!({ "effects": [effect], "schedules": [schedule], "resolved": resolved, "fired": fired })).unwrap();
            store.journal_turn(&turn, "runtime").unwrap();
        }
        let (rc, out) = crate::journal::sweep_until_done(&mut |argv| crate::journal::answer(crate::journal::run(&argv[1..], &store)), false, None);
        assert_eq!(rc, 0, "{out}");
        store.fold_stays(now, 100_000).unwrap();
        store.fold_edges(now, 100_000).unwrap();
        store.fold_refusals(now, 100_000).unwrap();
        (store, now, dir)
    }

    fn of_the_log(reads: Vec<(&'static str, String, usize)>) -> Vec<(&'static str, String, usize)> {
        reads.into_iter().filter(|(k, key, _)| (*k == "get" || *k == "range") && (key.starts_with("events/") || key.starts_with("history/"))).collect()
    }

    /// Leaves the read counter empty, having proved the fixture has a log and `of_the_log` sees a read of it.
    fn the_log_is_there_to_read(store: &Store<crate::counting::Counting<state_store_sqlite::SqliteBackend>>) {
        let head = store.contract().head().unwrap();
        store.contract().backend().take();
        assert!(!store.contract().backend().range("events/", None, 1, head).unwrap().is_empty(), "the fixture has no log to read");
        assert!(!store.contract().backend().range("history/", None, 1, head).unwrap().is_empty(), "the fixture has no history to read");
        assert_eq!(of_the_log(store.contract().backend().take()).len(), 2, "the filter cannot see a read of the log");
    }

    fn with_records(audits: u64) -> (Store<state_store_sqlite::SqliteBackend>, i64, testkit::TempDir) {
        with_records_on(|b| b, audits)
    }

    type Sqlite = Store<state_store_sqlite::SqliteBackend>;
    type Call = Box<dyn Fn(&Sqlite, i64) -> (i32, String)>;

    /// Every read verb of the production mix that this store answers, as (name, call).
    fn verbs() -> Vec<(&'static str, Call)> {
        use crate::control::{claimable, state, verbs as control_verbs};
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        let mut out: Vec<(&'static str, Call)> = Vec::new();
        for v in VIEWS {
            out.push((v, Box::new(move |s, now| view_rows(s, v, now))));
        }
        out.push(("ops-transitions", Box::new(|s, now| match transitions(s, "bead", now) {
            Ok(e) => (0, json!(e).to_string()),
            Err(e) => (2, e),
        })));
        out.push(("ops-refusals", Box::new(|s, now| {
            refusals_answer(s, now, DAY_S as u64)
        })));
        for (name, argv) in [
            ("list --live", args(&["list", "--live"])),
            ("list --ids", args(&["list", "--ids", "sp-00001,sp-00002,sp-00030"])),
            ("list --batches", args(&["list", "--batches"])),
            ("show", args(&["show", "sp-00001"])),
        ] {
            out.push((name, Box::new(move |s, now| state::run(s, &argv, &state::Call { now, request: "r", inputs: &claimable::FIXED }))));
        }
        out.push(("claimable", Box::new(|s, now| claimable::run(s, &["claimable".to_string()], &claimable::FIXED, &state::Call { now, request: "r", inputs: &claimable::FIXED }))));
        for (name, argv) in [
            ("dwell", args(&["dwell", "--window", "86400"])),
            ("watch", args(&["watch", "life/", "--from-rev", "0", "--wait-ms", "0"])),
            ("audits", args(&["audits", "--limit", "50"])),
            ("audits --newest", args(&["audits", "--newest"])),
        ] {
            out.push((name, Box::new(move |s, now| {
                let mut argv = argv.clone();
                if argv[0] == "dwell" {
                    argv.extend(["--now".to_string(), now.to_string()]);
                }
                control_verbs::run(s, &argv)
            })));
        }
        for op in ["state", "open-effects", "due"] {
            let argv = args(&[op]);
            out.push((Box::leak(format!("journal {op}").into_boxed_str()), Box::new(move |s, _| crate::journal::answer(crate::journal::run(&argv, s)))));
        }
        out
    }

    #[test_id(tod0922)]
    #[test]
    fn ops_recent_lists_the_beads_landed_in_the_last_day_and_no_others() {
        let (store, now, _dir) = with_records(400);
        let rows = ops_recent(&store, now).unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r["bead_id"].as_str().unwrap()).collect();
        assert_eq!(ids, (20..30).map(|n| format!("sp-{n:05}")).collect::<Vec<_>>());
    }

    /// Verbs whose answer is a page of 1_000 audits or more grow with the history below that size; `watch_holds_one_page_of_the_log_however_long_it_is` and `audits_newest_walking_the_whole_window_holds_one_chunk` bound them above it.
    const PAGED_ABOVE_THE_FIXTURE: [&str; 2] = ["watch", "audits --newest"];

    #[test_id(t4fqhcn)]
    #[test]
    fn every_read_verb_holds_no_more_memory_for_a_longer_history() {
        let mut table = Vec::new();
        let mut grown = Vec::new();
        let (short_fixture, long_fixture) = (with_records(200), with_records(400));
        for (name, call) in verbs() {
            let held = |f: &(Store<state_store_sqlite::SqliteBackend>, i64, testkit::TempDir)| {
                let ((rc, out), peak) = crate::peak_alloc::peak_of(|| call(&f.0, f.1));
                assert!(rc == 0 || rc == 1, "{name}: {rc} {out}");
                peak
            };
            let (short, long) = (held(&short_fixture), held(&long_fixture));
            table.push(format!("{name}: {short} B at 200, {long} B at 400"));
            if long >= short * 3 / 2 && !PAGED_ABOVE_THE_FIXTURE.contains(&name) {
                grown.push(name);
            }
        }
        assert!(grown.is_empty(), "peak grows with history for {grown:?}:\n{}", table.join("\n"));
        eprintln!("{}", table.join("\n"));
    }

    #[test_id(tl43oaa)]
    #[test]
    fn the_dwell_views_hold_no_more_memory_for_a_longer_history() {
        let (short_fixture, long_fixture) = (fortnight(200), fortnight(400));
        for view in ["ops_dwell_p95", "ops_edges", "ops_dwell"] {
            let held = |f: &(Store<state_store_sqlite::SqliteBackend>, i64, testkit::TempDir)| {
                let (store, now, _dir) = f;
                let ((rc, out), peak) = crate::peak_alloc::peak_of(|| view_rows(store, view, *now));
                assert_eq!(rc, 0, "{view}: {out}");
                peak
            };
            let (short, long) = (held(&short_fixture), held(&long_fixture));
            assert!(long < short * 3 / 2, "{view} held {short} B for 200 audits and {long} B for 400: the window is held, not folded");
        }
    }
}
