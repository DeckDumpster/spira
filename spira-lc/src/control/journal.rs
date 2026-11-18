//! The runtime's journal on the control store: what it keeps across a restart (task state, open
//! effects, the schedule) and the poison it recorded. Never a delete of a row: an effect is
//! resolved and a schedule row fired by a marker key of its own, so a re-upserted effect or a
//! re-inserted schedule row reads exactly as its row did.

use serde_json::{json, Value};
use state_store::{Backend, Compare, Object, Request, Result, Selector, Txn};

use super::keys;
use super::store::Store;

const STATE: &str = "state";
const EFFECT: &str = "effect";
const RESOLVED: &str = "resolved";
const SCHEDULE: &str = "schedule";
const FIRED: &str = "fired";
const POISON: &str = "poison";

/// Ops one txn may carry, with room for the request's own bookkeeping.
const CHUNK: usize = 100;
const ATTEMPTS: usize = 8;

#[derive(Clone, Debug, PartialEq)]
pub struct StateRow {
    pub task: String,
    pub skey: String,
    pub value: String,
    pub at: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EffectRow {
    pub task: String,
    pub ekey: String,
    pub request_id: String,
    pub stream: String,
    pub spec: Value,
    pub requested_at: u64,
    pub deadline_at: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScheduleRow {
    pub id: String,
    pub at: u64,
    pub stream: String,
    pub tag: String,
    pub body: String,
    pub cause: Option<String>,
    pub correlation: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PoisonRow {
    pub stream: String,
    pub event: String,
    pub handler: String,
    pub message: String,
}

/// The journal's rows as another store held them, each effect and schedule row with whether it
/// had been resolved or fired.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Carried {
    pub state: Vec<StateRow>,
    pub effects: Vec<(EffectRow, bool)>,
    pub schedules: Vec<(ScheduleRow, bool)>,
    pub poison: Vec<PoisonRow>,
}

/// One turn's writes, in the order they apply.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Turn {
    pub state: Vec<StateRow>,
    pub effects: Vec<EffectRow>,
    pub schedules: Vec<ScheduleRow>,
    pub resolved: Vec<String>,
    pub fired: Vec<String>,
}

fn text(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").to_string()
}

fn num(v: &Value, k: &str) -> u64 {
    v[k].as_u64().unwrap_or(0)
}

impl<B: Backend> Store<B> {
    /// The turn's writes as txns of at most [`CHUNK`] ops each, in order. A schedule row that
    /// already exists is kept as it is; one that does not is created only if it still does not
    /// when the txn commits, else the turn is planned again.
    pub fn journal_turn(&self, turn: &Turn, actor: &str) -> Result<()> {
        'again: for _ in 0..ATTEMPTS {
            let mut ops: Vec<(bool, Txn)> = Vec::new();
            let mut one = |create: Option<String>, put: Vec<(String, Option<Object>)>| {
                let mut t = Txn::new();
                if let Some(key) = &create {
                    t = t.when(Compare::ModRev(key.clone(), None));
                }
                for (k, v) in put {
                    t = match v {
                        Some(v) => t.put(&k, v),
                        None => t.delete(&k),
                    };
                }
                ops.push((create.is_some(), t));
            };
            for r in &turn.state {
                one(None, vec![(keys::runtime(STATE, &[&r.task, &r.skey]), Some(Object::new(json!({ "task": r.task, "skey": r.skey, "value": r.value, "at": r.at }))))]);
            }
            for e in &turn.effects {
                let row = json!({ "task": e.task, "ekey": e.ekey, "request_id": e.request_id, "stream": e.stream, "spec": e.spec, "requested_at": e.requested_at, "deadline_at": e.deadline_at });
                one(None, vec![(keys::runtime(EFFECT, &[&e.task, &e.ekey]), Some(Object::new(row))), (keys::runtime(RESOLVED, &[&e.request_id]), None)]);
            }
            for s in &turn.schedules {
                let key = keys::runtime(SCHEDULE, &[&s.id]);
                if self.contract().get(&key)?.is_some() {
                    continue;
                }
                let row = json!({ "id": s.id, "at": s.at, "stream": s.stream, "tag": s.tag, "body": s.body, "cause": s.cause, "correlation": s.correlation });
                one(Some(key.clone()), vec![(key, Some(Object::new(row))), (keys::runtime(FIRED, &[&s.id]), None)]);
            }
            for id in &turn.resolved {
                one(None, vec![(keys::runtime(RESOLVED, &[id]), Some(Object::new(json!({}))))]);
            }
            for id in &turn.fired {
                one(None, vec![(keys::runtime(FIRED, &[id]), Some(Object::new(json!({}))))]);
            }
            for group in merged(ops) {
                let req = Request::new(&super::served::request_id(), actor, "journal-turn");
                if !self.contract().txn(&group, &req)?.succeeded {
                    continue 'again;
                }
            }
            return Ok(());
        }
        Err(state_store::Error::Backend(format!("a journal turn lost its race {ATTEMPTS} times")))
    }

    /// Rows carried over from another store: each is written only where its key is still free, so
    /// what the runtime has written here since is never overwritten. Returns how many rows were written.
    pub fn journal_carry(&self, rows: &Carried, actor: &str) -> Result<usize> {
        let mut written = 0;
        let mut one = |key: String, row: Value, marker: Option<String>| -> Result<()> {
            let mut t = Txn::new().when(Compare::ModRev(key.clone(), None)).put(&key, Object::new(row));
            if let Some(m) = marker {
                t = t.put(&m, Object::new(json!({})));
            }
            let req = Request::new(&super::served::request_id(), actor, "carry-journal");
            written += usize::from(self.contract().txn(&t, &req)?.succeeded);
            Ok(())
        };
        for r in &rows.state {
            one(keys::runtime(STATE, &[&r.task, &r.skey]), json!({ "task": r.task, "skey": r.skey, "value": r.value, "at": r.at }), None)?;
        }
        for (e, resolved) in &rows.effects {
            let row = json!({ "task": e.task, "ekey": e.ekey, "request_id": e.request_id, "stream": e.stream, "spec": e.spec, "requested_at": e.requested_at, "deadline_at": e.deadline_at });
            one(keys::runtime(EFFECT, &[&e.task, &e.ekey]), row, resolved.then(|| keys::runtime(RESOLVED, &[&e.request_id])))?;
        }
        for (s, fired) in &rows.schedules {
            let row = json!({ "id": s.id, "at": s.at, "stream": s.stream, "tag": s.tag, "body": s.body, "cause": s.cause, "correlation": s.correlation });
            one(keys::runtime(SCHEDULE, &[&s.id]), row, fired.then(|| keys::runtime(FIRED, &[&s.id])))?;
        }
        for p in &rows.poison {
            one(keys::runtime(POISON, &[&p.stream]), json!({ "stream": p.stream, "event": p.event, "handler": p.handler, "message": p.message }), None)?;
        }
        Ok(written)
    }

    pub fn journal_state(&self) -> Result<Vec<StateRow>> {
        let mut rows: Vec<StateRow> = self
            .table(STATE)?
            .iter()
            .map(|v| StateRow { task: text(v, "task"), skey: text(v, "skey"), value: text(v, "value"), at: num(v, "at") })
            .collect();
        rows.sort_by(|a, b| (&a.task, &a.skey).cmp(&(&b.task, &b.skey)));
        Ok(rows)
    }

    pub fn journal_open_effects(&self) -> Result<Vec<EffectRow>> {
        let resolved: std::collections::BTreeSet<String> = self.markers(RESOLVED)?;
        let mut rows: Vec<EffectRow> = self
            .table(EFFECT)?
            .iter()
            .filter(|v| !resolved.contains(&keys::component(&text(v, "request_id"))))
            .map(|v| EffectRow { task: text(v, "task"), ekey: text(v, "ekey"), request_id: text(v, "request_id"), stream: text(v, "stream"), spec: v["spec"].clone(), requested_at: num(v, "requested_at"), deadline_at: num(v, "deadline_at") })
            .collect();
        rows.sort_by(|a, b| (a.requested_at, &a.request_id).cmp(&(b.requested_at, &b.request_id)));
        Ok(rows)
    }

    /// The schedule rows not yet fired, due at or before `now` when it is given, by row id.
    pub fn journal_due(&self, now: Option<u64>) -> Result<Vec<ScheduleRow>> {
        let fired = self.markers(FIRED)?;
        let mut rows: Vec<ScheduleRow> = self
            .table(SCHEDULE)?
            .iter()
            .filter(|v| !fired.contains(&keys::component(&text(v, "id"))) && now.is_none_or(|n| num(v, "at") <= n))
            .map(|v| ScheduleRow { id: text(v, "id"), at: num(v, "at"), stream: text(v, "stream"), tag: text(v, "tag"), body: text(v, "body"), cause: v["cause"].as_str().map(str::to_string), correlation: text(v, "correlation") })
            .collect();
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(rows)
    }

    /// Records the first poison of a stream; a stream already poisoned keeps its first.
    pub fn journal_poison(&self, p: &PoisonRow, actor: &str) -> Result<()> {
        let key = keys::runtime(POISON, &[&p.stream]);
        let t = Txn::new()
            .when(Compare::ModRev(key.clone(), None))
            .put(&key, Object::new(json!({ "stream": p.stream, "event": p.event, "handler": p.handler, "message": p.message })));
        self.contract().txn(&t, &Request::new(&super::served::request_id(), actor, "journal-poison")).map(|_| ())
    }

    pub fn journal_poisoned(&self, stream: &str) -> Result<Option<PoisonRow>> {
        Ok(self.contract().get(&keys::runtime(POISON, &[stream]))?.map(|i| {
            let v = &i.value.data;
            PoisonRow { stream: text(v, "stream"), event: text(v, "event"), handler: text(v, "handler"), message: text(v, "message") }
        }))
    }

    fn table(&self, table: &str) -> Result<Vec<Value>> {
        let prefix = keys::runtime_table(table);
        let (items, _) = self.contract().list(&[prefix.as_str()], &Selector::Everything)?;
        Ok(items.into_iter().map(|i| i.value.data).collect())
    }

    fn markers(&self, table: &str) -> Result<std::collections::BTreeSet<String>> {
        let prefix = keys::runtime_table(table);
        let (items, _) = self.contract().list(&[prefix.as_str()], &Selector::Everything)?;
        Ok(items.into_iter().filter_map(|i| i.key.strip_prefix(&prefix).map(str::to_string)).collect())
    }
}

/// The planned txns folded into as few as [`CHUNK`] allows, never folding past one that carries
/// a compare, so a lost compare repeats only its own writes.
fn merged(planned: Vec<(bool, Txn)>) -> Vec<Txn> {
    let mut out: Vec<Txn> = Vec::new();
    let mut open: Option<Txn> = None;
    for (compared, t) in planned {
        if compared {
            out.extend(open.take());
            out.push(t);
            continue;
        }
        let full = open.as_ref().is_some_and(|o| o.ops.len() + t.ops.len() > CHUNK);
        if full {
            out.extend(open.take());
        }
        open = Some(match open.take() {
            None => t,
            Some(mut o) => {
                o.ops.extend(t.ops);
                o
            }
        });
    }
    out.extend(open);
    out
}
