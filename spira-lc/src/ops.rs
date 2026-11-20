//! The ops read model: `ops-view <view>` returns one view as a JSON array, `ops-graph` the
//! machine's legal edges, and the other `ops-*` verbs the day's events. Every row is read from the
//! control store (the records, the rounds, the audit log) and printed as the strings the panes
//! have always read; a bead's title and priority come from the planning plane, where they live.

use std::collections::{BTreeMap, BTreeSet};

use lifecycle::bead::BeadState;
use serde_json::{json, Map, Value};
use state_store::Backend;

use crate::control::audits::Move;
use crate::control::store::Store;

/// The only views `ops-view` will name; the argument is matched, never interpolated.
pub const VIEWS: [&str; 6] = ["ops_live", "ops_round", "ops_recent", "ops_edges", "ops_dwell", "ops_dwell_p95"];

/// The machines `ops-transitions` reads events of, and the machines `ops-graph` draws.
pub const TRANSITION_MACHINES: [&str; 2] = ["bead", "batch"];
pub const GRAPH_MACHINES: [&str; 4] = ["bead", "batch", "ask", "mending"];

const DAY_S: i64 = 86_400;
const HOUR_S: i64 = 3_600;
const DWELL_WINDOW_S: i64 = 14 * DAY_S;
const LIVE: [BeadState; 7] = [BeadState::Open, BeadState::Ready, BeadState::Working, BeadState::Submitted, BeadState::Certified, BeadState::InDelivery, BeadState::Rework];
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

/// `ops_recent`: the beads that landed in the last day, found from the day's audits and read by
/// key, so the call's work follows the day's events and not the LANDED records the store holds.
fn ops_recent<B: Backend>(store: &Store<B>, now: i64) -> Result<Vec<Value>, String> {
    let mut ids = std::collections::BTreeSet::new();
    folded(store, now, DAY_S, |m| {
        if m.machine == "bead" && m.applied && m.to_state == BeadState::Landed.as_str() && m.from_state != m.to_state {
            ids.insert(m.lc_key.to_string());
        }
    })?;
    let mut landed = Vec::new();
    for id in &ids {
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

type Tally = (u64, u64, i64);

fn tally(map: &mut BTreeMap<(String, String, String), Tally>, key: (&str, &str, &str), at: i64, now: i64) {
    let e = map.entry((key.0.to_string(), key.1.to_string(), key.2.to_string())).or_insert((0, 0, i64::MIN));
    e.0 += u64::from(at >= now - HOUR_S);
    e.1 += 1;
    e.2 = e.2.max(at);
}

/// How often each transition applied and each refusal happened in the last hour and day, folded
/// one audit at a time.
struct Edges {
    now: i64,
    applied: BTreeMap<(String, String, String), Tally>,
    refused: BTreeMap<(String, String, String), Tally>,
}

impl Edges {
    fn new(now: i64) -> Self {
        Edges { now, applied: BTreeMap::new(), refused: BTreeMap::new() }
    }

    fn take(&mut self, m: &Move) {
        if m.machine != "bead" || m.at < self.now - DAY_S {
            return;
        }
        if !m.applied {
            tally(&mut self.refused, (m.from_state, m.event, m.refusal.unwrap_or_default()), m.at, self.now);
        } else if m.from_state != m.to_state {
            tally(&mut self.applied, (m.from_state, m.to_state, m.event), m.at, self.now);
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

/// Per from-state, the stays that ended in it over the window, folded newest audit first: a
/// bead holds one pending move, never its history.
struct Stays {
    now: i64,
    later: BTreeMap<String, (i64, String)>,
    secs: BTreeMap<String, Vec<i64>>,
}

impl Stays {
    fn new(now: i64) -> Self {
        Stays { now, later: BTreeMap::new(), secs: BTreeMap::new() }
    }

    fn take(&mut self, m: &Move) {
        if m.machine != "bead" || !m.applied || m.from_state == m.to_state || m.at < self.now - DWELL_WINDOW_S {
            return;
        }
        if let Some((at, from)) = self.later.get_mut(m.lc_key) {
            self.secs.entry(from.clone()).or_default().push(*at - m.at);
            (*at, *from) = (m.at, m.from_state.to_string());
        } else {
            self.later.insert(m.lc_key.to_string(), (m.at, m.from_state.to_string()));
        }
    }

    /// The 95th-percentile stay and how many stays it rests on, per state.
    fn p95(self) -> BTreeMap<String, (i64, usize)> {
        self.secs
            .into_iter()
            .map(|(state, mut secs)| {
                secs.sort_unstable();
                let n = secs.len();
                let rank = (n * 95).div_ceil(100).max(1);
                (state, (secs[rank - 1], n))
            })
            .collect()
    }
}

fn ops_dwell_p95_rows(p95: BTreeMap<String, (i64, usize)>) -> Vec<Value> {
    p95.into_iter().map(|(state, (p95, n))| object(vec![("state", text(state)), ("p95_s", text(p95)), ("samples", text(n))])).collect()
}

/// `ops_dwell`: each live bead with when it entered its state and what is usual for that state.
fn ops_dwell<B: Backend>(store: &Store<B>, now: i64) -> Result<Vec<Value>, String> {
    let live = live(store)?;
    let ids: BTreeSet<&str> = live.rows.iter().map(|r| r.row.bead_id.as_str()).collect();
    let mut stays = Stays::new(now);
    let mut newest: BTreeMap<(String, String), i64> = BTreeMap::new();
    folded(store, now, DWELL_WINDOW_S, |m| {
        stays.take(&m);
        if m.machine == "bead" && m.applied && m.from_state != m.to_state && ids.contains(m.lc_key) {
            let e = newest.entry((m.lc_key.to_string(), m.to_state.to_string())).or_insert(i64::MIN);
            *e = (*e).max(m.at);
        }
    })?;
    let p95 = stays.p95();
    let plan = planned(&live.rows.iter().map(|r| r.row.bead_id.clone()).collect::<Vec<_>>());
    let mut out = Vec::new();
    for r in &live.rows {
        let row = &r.row;
        let entered = match newest.get(&(row.bead_id.clone(), row.state.as_str().to_string())) {
            Some(at) => *at,
            None => store
                .history(&row.bead_id, Some("bead"))
                .map_err(|e| e.to_string())?
                .iter()
                .filter(|c| c.audit.applied && c.audit.from_state != c.audit.to_state && c.audit.to_state == row.state.as_str())
                .map(|c| c.audit.at)
                .max()
                .unwrap_or(r.updated_at),
        };
        let (title, priority) = title_cols(&plan, &row.bead_id);
        let usual = p95.get(row.state.as_str());
        let holds: Vec<&str> = row.holds.iter().map(|h| h.as_str()).collect();
        out.push(object(vec![
            ("bead_id", text(&row.bead_id)),
            ("state", text(row.state.as_str())),
            ("holds", text(json!(holds))),
            ("priority", priority),
            ("title", title),
            ("entered_at", text(entered)),
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
struct Refusals {
    floor: i64,
    groups: BTreeMap<(String, String, String, String), Group>,
}

#[derive(Default)]
struct Group {
    n: usize,
    first: i64,
    last: i64,
    newest: Vec<(i64, String)>,
}

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
        "ops_edges" => {
            let mut edges = Edges::new(now);
            folded(store, now, DAY_S, |m| edges.take(&m)).map(|()| edges.rows())
        }
        "ops_dwell_p95" => {
            let mut stays = Stays::new(now);
            folded(store, now, DWELL_WINDOW_S, |m| stays.take(&m)).map(|()| ops_dwell_p95_rows(stays.p95()))
        }
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
    with_store(|store| {
        let now = crate::db::now_epoch();
        let mut refused = Refusals::new(now - window as i64);
        match folded(store, now, window as i64, |m| refused.take(&m)) {
            Ok(()) => (0, json!({ "now": now, "classes": refused.rows() }).to_string()),
            Err(e) => cannot(e),
        }
    })
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
        let b = SqliteBackend::open(&dir.path().join("control.db"), Options::default()).unwrap();
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

    #[test]
    fn the_transitions_machine_is_one_the_log_carries_bead_facts_for() {
        assert_eq!(machine_arg("ops-transitions", &[], &TRANSITION_MACHINES), Ok("bead"));
        assert_eq!(machine_arg("ops-transitions", &["--machine".into(), "batch".into()], &TRANSITION_MACHINES), Ok("batch"));
        assert_eq!(machine_arg("ops-transitions", &["--machine".into(), "bead' OR 1=1 --".into()], &TRANSITION_MACHINES).unwrap_err().0, 2);
        let refused = machine_arg("ops-transitions", &["--machine".into(), "ask".into()], &TRANSITION_MACHINES).unwrap_err();
        assert_eq!(refused.0, 2);
        assert!(refused.1.starts_with("ops-transitions:"), "{}", refused.1);
    }

    #[test]
    fn ops_view_refuses_anything_but_its_views() {
        for v in VIEWS {
            assert_eq!(view_arg(&[v.to_string()]), Ok(v));
        }
        assert_eq!(view_arg(&["bead".to_string()]).unwrap_err().0, 2);
        assert_eq!(view_arg(&[]).unwrap_err().0, 2);
    }

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

    #[test]
    fn the_edges_count_the_hour_and_the_day_and_keep_refusals_apart() {
        let now = 1_000_000;
        let log = [
            audit("sp-a", "Claim", "READY", "WORKING", true, "d", now - 60),
            audit("sp-b", "Claim", "READY", "WORKING", true, "d", now - 2 * HOUR_S),
            audit("sp-c", "Submit", "READY", "READY", false, "d", now - 30),
        ];
        let mut edges = Edges::new(now);
        log.iter().rev().for_each(|l| edges.take(&move_of(l)));
        let rows = edges.rows();
        let applied = rows.iter().find(|r| r["kind"] == "applied").unwrap();
        assert_eq!((applied["n_1h"].as_str(), applied["n_24h"].as_str(), applied["last_at"].as_str()), (Some("1"), Some("2"), Some((now - 60).to_string().as_str())));
        let refused = rows.iter().find(|r| r["kind"] == "refused").unwrap();
        assert_eq!((refused["to_state"].clone(), refused["refusal"].as_str()), (Value::Null, Some("IllegalTransition")));
    }

    #[test]
    fn the_p95_of_a_state_is_the_stay_at_the_95th_percentile_rank_of_the_stays_that_ended_in_it() {
        let now = 1_000_000;
        let mut log = Vec::new();
        for n in 0..20i64 {
            let bead = format!("sp-{n}");
            log.push(audit(&bead, "Claim", "READY", "WORKING", true, "d", now - 5_000 + n));
            log.push(audit(&bead, "Submit", "WORKING", "SUBMITTED", true, "d", now - 5_000 + n + 10 * (n + 1)));
        }
        let mut stays = Stays::new(now);
        log.iter().rev().for_each(|l| stays.take(&move_of(l)));
        let p = stays.p95();
        assert_eq!(p["WORKING"], (190, 20), "stays are 10, 20 .. 200 s; rank ceil(0.95 * 20) = 19");
    }

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

    /// A store shaped like production: many terminal beads with bulky audits across a fortnight,
    /// a few live ones.
    fn fortnight(audits: u64) -> (Store<state_store_sqlite::SqliteBackend>, i64, testkit::TempDir) {
        use state_store::{events_key, Backend, Object, Rev, Write};
        use state_store_sqlite::{Options, SqliteBackend};
        let dir = testkit::TempDir::new("spira-lc-ops-memory");
        let b = SqliteBackend::open(&dir.path().join("control.db"), Options::default()).unwrap();
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
        let c = Contract::open(b, Ports::virtual_clock(&VirtualClock::new(0)), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        (Store::new(c, 1_000), now, dir)
    }

    /// `fortnight`, plus a quarter as many LANDED records (ten landed in the last day) and a few
    /// live ones: the records grow with the history, the answers to the day's questions do not.
    fn with_records(audits: u64) -> (Store<state_store_sqlite::SqliteBackend>, i64, testkit::TempDir) {
        use crate::control::{keys, records::{encode, LifecycleRecord}};
        use state_store::{Backend, Object, Rev, Write};
        let (store, now, dir) = fortnight(audits);
        let b = store.contract().backend();
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
        for n in 0..audits / 8 {
            let (request, row) = (format!("r{n}"), format!("s{n}"));
            let effect = json!({ "task": "t", "ekey": format!("k{n}/x"), "request_id": request, "stream": "s", "spec": { "name": "n", "input": "i", "deadline_ms": 5, "key": format!("k{n}") }, "requested_at": n, "deadline_at": n + 5 });
            let schedule = json!({ "id": row, "at": n, "stream": "s", "kind": { "tag": "tick", "body": "b" }, "cause": null, "correlation": "c" });
            let (resolved, fired) = if n < 5 { (json!([]), json!([])) } else { (json!([request]), json!([row])) };
            let turn = crate::journal::turn_of(&json!({ "effects": [effect], "schedules": [schedule], "resolved": resolved, "fired": fired })).unwrap();
            store.journal_turn(&turn, "runtime").unwrap();
        }
        let (rc, out) = crate::journal::sweep_until_done(&mut |argv| crate::journal::answer(crate::journal::run(&argv[1..], &store)), false);
        assert_eq!(rc, 0, "{out}");
        (store, now, dir)
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
            let mut refused = Refusals::new(now - 14 * DAY_S);
            match folded(s, now, 14 * DAY_S, |m| refused.take(&m)) {
                Ok(()) => (0, json!(refused.rows()).to_string()),
                Err(e) => (2, e),
            }
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

    #[test]
    fn ops_recent_lists_the_beads_landed_in_the_last_day_and_no_others() {
        let (store, now, _dir) = with_records(400);
        let rows = ops_recent(&store, now).unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r["bead_id"].as_str().unwrap()).collect();
        assert_eq!(ids, (20..30).map(|n| format!("sp-{n:05}")).collect::<Vec<_>>());
    }

    #[test]
    fn every_read_verb_holds_no_more_memory_for_a_longer_history() {
        let mut table = Vec::new();
        let mut grown = Vec::new();
        let (short_fixture, long_fixture) = (with_records(2_000), with_records(8_000));
        for (name, call) in verbs() {
            let held = |f: &(Store<state_store_sqlite::SqliteBackend>, i64, testkit::TempDir)| {
                let ((rc, out), peak) = crate::peak_alloc::peak_of(|| call(&f.0, f.1));
                assert!(rc == 0 || rc == 1, "{name}: {rc} {out}");
                peak
            };
            let (short, long) = (held(&short_fixture), held(&long_fixture));
            table.push(format!("{name}: {short} B at 2000, {long} B at 8000"));
            if long >= short * 3 / 2 {
                grown.push(name);
            }
        }
        assert!(grown.is_empty(), "peak grows with history for {grown:?}:\n{}", table.join("\n"));
        eprintln!("{}", table.join("\n"));
    }

    #[test]
    fn the_dwell_views_hold_no_more_memory_for_a_longer_history() {
        for view in ["ops_dwell_p95", "ops_edges", "ops_dwell"] {
            let held = |audits| {
                let (store, now, _dir) = fortnight(audits);
                let ((rc, out), peak) = crate::peak_alloc::peak_of(|| view_rows(&store, view, now));
                assert_eq!(rc, 0, "{view}: {out}");
                peak
            };
            let (short, long) = (held(2_000), held(8_000));
            assert!(long < short * 3 / 2, "{view} held {short} B for 2000 audits and {long} B for 8000: the window is held, not folded");
        }
    }
}
