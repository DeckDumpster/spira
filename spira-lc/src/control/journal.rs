//! The runtime's journal on the control store: what it keeps across a restart (task state, open
//! effects, the schedule) and the poison it recorded. A turn never deletes a row: an effect is
//! resolved and a schedule row fired by a marker key of its own, so a re-upserted effect or a
//! re-inserted schedule row reads exactly as its row did. Each row not yet closed also has an
//! index key, deleted when the row closes, so a boot read costs the rows it returns and not every
//! row ever written.

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
const OPEN_EFFECT: &str = "open-effect";
const PENDING: &str = "pending";
const META: &str = "journal-meta";

/// Ops one txn may carry, with room for the request's own bookkeeping.
const CHUNK: usize = 100;
const ATTEMPTS: usize = 8;
/// Rows one sweep step reads: the store's page.
pub const SWEEP_STEP: usize = 1_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SweepPhase {
    Schedule,
    Legacy,
    Effect,
}

/// Tables removed by sp-neicv8, written last by the 11-03 import; the sweep deletes their rows.
const LEGACY: &[&str] = &["event", "applied", "cursor"];

/// Where the next sweep step resumes: the table and the last key read, as one argv token.
#[derive(Clone, Debug, PartialEq)]
pub struct SweepCursor {
    pub phase: SweepPhase,
    pub after: String,
}

impl std::fmt::Display for SweepCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let phase = match self.phase {
            SweepPhase::Schedule => "schedule",
            SweepPhase::Legacy => "legacy",
            SweepPhase::Effect => "effect",
        };
        write!(f, "{phase}:{}", self.after)
    }
}

impl std::str::FromStr for SweepCursor {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        let phase = match s.split_once(':') {
            Some(("schedule", _)) => SweepPhase::Schedule,
            Some(("legacy", _)) => SweepPhase::Legacy,
            Some(("effect", _)) => SweepPhase::Effect,
            _ => return Err(format!("journal: sweep cursor {s:?} is not schedule:<key>, legacy:<key> or effect:<key>")),
        };
        Ok(SweepCursor { phase, after: s.split_once(':').map_or(String::new(), |(_, k)| k.to_string()) })
    }
}

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

fn open_effect(task: &str, ekey: &str) -> Object {
    Object::new(json!({ "task": task, "ekey": ekey }))
}

fn pending(id: &str) -> Object {
    Object::new(json!({ "id": id }))
}

/// What a sweep step removed, or with `dry_run` would have: id-less schedule rows, and the index
/// keys it wrote for rows stored before the index.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Swept {
    pub idless_schedules: usize,
    pub legacy_deleted: usize,
    pub indexed_schedules: usize,
    pub indexed_effects: usize,
}

fn effect_of(v: &Value) -> EffectRow {
    EffectRow { task: text(v, "task"), ekey: text(v, "ekey"), request_id: text(v, "request_id"), stream: text(v, "stream"), spec: v["spec"].clone(), requested_at: num(v, "requested_at"), deadline_at: num(v, "deadline_at") }
}

fn schedule_of(v: &Value) -> ScheduleRow {
    ScheduleRow { id: text(v, "id"), at: num(v, "at"), stream: text(v, "stream"), tag: text(v, "tag"), body: text(v, "body"), cause: v["cause"].as_str().map(str::to_string), correlation: text(v, "correlation") }
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
                one(None, vec![(keys::runtime(EFFECT, &[&e.task, &e.ekey]), Some(Object::new(row))), (keys::runtime(OPEN_EFFECT, &[&e.request_id]), Some(open_effect(&e.task, &e.ekey))), (keys::runtime(RESOLVED, &[&e.request_id]), None)]);
            }
            for s in &turn.schedules {
                let key = keys::runtime(SCHEDULE, &[&s.id]);
                if self.contract().get(&key)?.is_some() {
                    continue;
                }
                let row = json!({ "id": s.id, "at": s.at, "stream": s.stream, "tag": s.tag, "body": s.body, "cause": s.cause, "correlation": s.correlation });
                one(Some(key.clone()), vec![(key, Some(Object::new(row))), (keys::runtime(PENDING, &[&s.id]), Some(pending(&s.id))), (keys::runtime(FIRED, &[&s.id]), None)]);
            }
            for id in &turn.resolved {
                one(None, vec![(keys::runtime(RESOLVED, &[id]), Some(Object::new(json!({})))), (keys::runtime(OPEN_EFFECT, &[id]), None)]);
            }
            for id in &turn.fired {
                one(None, vec![(keys::runtime(FIRED, &[id]), Some(Object::new(json!({})))), (keys::runtime(PENDING, &[id]), None)]);
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
    /// what the runtime has written here since is never overwritten. A schedule row with an empty
    /// id refuses the whole carry before any write. Returns how many rows were written.
    pub fn journal_carry(&self, rows: &Carried, actor: &str) -> Result<usize> {
        if let Some((s, _)) = rows.schedules.iter().find(|(s, _)| s.id.is_empty()) {
            return Err(state_store::Error::Backend(format!("journal carry refused: a schedule row has an empty id: stream={} kind={} at={}", s.stream, s.tag, s.at)));
        }
        let mut written = 0;
        let mut one = |key: String, row: Value, extra: Vec<(String, Object)>| -> Result<()> {
            let mut t = Txn::new().when(Compare::ModRev(key.clone(), None)).put(&key, Object::new(row));
            for (k, o) in extra {
                t = t.put(&k, o);
            }
            let req = Request::new(&super::served::request_id(), actor, "carry-journal");
            written += usize::from(self.contract().txn(&t, &req)?.succeeded);
            Ok(())
        };
        for r in &rows.state {
            one(keys::runtime(STATE, &[&r.task, &r.skey]), json!({ "task": r.task, "skey": r.skey, "value": r.value, "at": r.at }), vec![])?;
        }
        for (e, resolved) in &rows.effects {
            let row = json!({ "task": e.task, "ekey": e.ekey, "request_id": e.request_id, "stream": e.stream, "spec": e.spec, "requested_at": e.requested_at, "deadline_at": e.deadline_at });
            let extra = match resolved {
                true => (keys::runtime(RESOLVED, &[&e.request_id]), Object::new(json!({}))),
                false => (keys::runtime(OPEN_EFFECT, &[&e.request_id]), open_effect(&e.task, &e.ekey)),
            };
            one(keys::runtime(EFFECT, &[&e.task, &e.ekey]), row, vec![extra])?;
        }
        for (s, fired) in &rows.schedules {
            let row = json!({ "id": s.id, "at": s.at, "stream": s.stream, "tag": s.tag, "body": s.body, "cause": s.cause, "correlation": s.correlation });
            let extra = match fired {
                true => (keys::runtime(FIRED, &[&s.id]), Object::new(json!({}))),
                false => (keys::runtime(PENDING, &[&s.id]), pending(&s.id)),
            };
            one(keys::runtime(SCHEDULE, &[&s.id]), row, vec![extra])?;
        }
        for p in &rows.poison {
            one(keys::runtime(POISON, &[&p.stream]), json!({ "stream": p.stream, "event": p.event, "handler": p.handler, "message": p.message }), vec![])?;
        }
        Ok(written)
    }

    pub fn journal_state(&self) -> Result<Vec<StateRow>> {
        let mut rows: Vec<StateRow> = self
            .table(STATE)?
            .iter()
            .map(|(_, v)| StateRow { task: text(v, "task"), skey: text(v, "skey"), value: text(v, "value"), at: num(v, "at") })
            .collect();
        rows.sort_by(|a, b| (&a.task, &a.skey).cmp(&(&b.task, &b.skey)));
        Ok(rows)
    }

    /// Whether a sweep has indexed every row stored before the index: until then the readers
    /// list the whole tables, as a store written by an older release needs.
    fn indexed(&self) -> Result<bool> {
        Ok(self.contract().get(&keys::runtime(META, &["indexed"]))?.is_some())
    }

    pub fn journal_open_effects(&self) -> Result<Vec<EffectRow>> {
        let mut rows: Vec<EffectRow> = if self.indexed()? {
            let mut rows = Vec::new();
            for open in self.table(OPEN_EFFECT)? {
                let request = open.0;
                if let Some(i) = self.contract().get(&keys::runtime(EFFECT, &[&text(&open.1, "task"), &text(&open.1, "ekey")]))? {
                    let row = effect_of(&i.value.data);
                    if keys::component(&row.request_id) == request && self.contract().get(&keys::runtime(RESOLVED, &[&row.request_id]))?.is_none() {
                        rows.push(row);
                    }
                }
            }
            rows
        } else {
            let resolved = self.markers(RESOLVED)?;
            self.table(EFFECT)?.iter().map(|(_, v)| effect_of(v)).filter(|r| !resolved.contains(&keys::component(&r.request_id))).collect()
        };
        rows.sort_by(|a, b| (a.requested_at, &a.request_id).cmp(&(b.requested_at, &b.request_id)));
        Ok(rows)
    }

    /// The schedule rows not yet fired, due at or before `now` when it is given, by row id. A row
    /// with no id is never returned: nothing could fire it.
    pub fn journal_due(&self, now: Option<u64>) -> Result<Vec<ScheduleRow>> {
        let mut rows: Vec<ScheduleRow> = if self.indexed()? {
            let mut rows = Vec::new();
            for (_, p) in self.table(PENDING)? {
                let id = text(&p, "id");
                if self.contract().get(&keys::runtime(FIRED, &[&id]))?.is_some() {
                    continue;
                }
                if let Some(i) = self.contract().get(&keys::runtime(SCHEDULE, &[&id]))? {
                    rows.push(schedule_of(&i.value.data));
                }
            }
            rows
        } else {
            let fired = self.markers(FIRED)?;
            self.table(SCHEDULE)?.iter().map(|(_, v)| schedule_of(v)).filter(|r| !fired.contains(&keys::component(&r.id))).collect()
        };
        rows.retain(|r| !r.id.is_empty() && now.is_none_or(|n| r.at <= n));
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(rows)
    }

    /// One bounded step of the sweep: reads at most [`SWEEP_STEP`] rows after `from`, deletes the
    /// schedule rows stored without an id, the [`LEGACY`] rows, and indexes the open rows an older release stored
    /// without one. The step that exhausts the effects marks the store indexed and answers no
    /// cursor. Every write is idempotent, so a step that fails is run again from the same cursor.
    /// With `dry_run` it counts and writes nothing.
    pub fn journal_sweep_step(&self, from: Option<&SweepCursor>, dry_run: bool, actor: &str) -> Result<(Swept, Option<SweepCursor>)> {
        let mut out = Swept::default();
        let mut ops: Vec<state_store::Op> = Vec::new();
        let contract = self.contract();
        let head = contract.head()?;
        let (phase, after) = from.map_or((SweepPhase::Schedule, None), |c| (c.phase, Some(c.after.as_str()).filter(|a| !a.is_empty())));
        let legacy_at = after.and_then(|a| LEGACY.iter().position(|t| a.starts_with(&keys::runtime_table(t)))).unwrap_or(0);
        let table = match phase {
            SweepPhase::Schedule => SCHEDULE,
            SweepPhase::Legacy => LEGACY[legacy_at],
            SweepPhase::Effect => EFFECT,
        };
        let page = contract.range_at(&keys::runtime_table(table), after, SWEEP_STEP, head)?;
        for item in &page {
            let v = &item.value.data;
            match phase {
                SweepPhase::Schedule => {
                    let id = text(v, "id");
                    if id.is_empty() {
                        out.idless_schedules += 1;
                        ops.push(state_store::Op::Delete(item.key.clone()));
                    } else if contract.get(&keys::runtime(FIRED, &[&id]))?.is_none() {
                        out.indexed_schedules += 1;
                        ops.push(state_store::Op::Put(keys::runtime(PENDING, &[&id]), pending(&id)));
                    }
                }
                SweepPhase::Legacy => {
                    out.legacy_deleted += 1;
                    ops.push(state_store::Op::Delete(item.key.clone()));
                }
                SweepPhase::Effect => {
                    let e = effect_of(v);
                    if contract.get(&keys::runtime(RESOLVED, &[&e.request_id]))?.is_none() {
                        out.indexed_effects += 1;
                        ops.push(state_store::Op::Put(keys::runtime(OPEN_EFFECT, &[&e.request_id]), open_effect(&e.task, &e.ekey)));
                    }
                }
            }
        }
        let last = page.last().map(|i| i.key.clone());
        let next = match (page.len() < SWEEP_STEP, phase) {
            (false, _) => last.map(|after| SweepCursor { phase, after }),
            (true, SweepPhase::Schedule) => Some(SweepCursor { phase: SweepPhase::Legacy, after: String::new() }),
            (true, SweepPhase::Legacy) => Some(match LEGACY.get(legacy_at + 1) {
                Some(t) => SweepCursor { phase, after: keys::runtime_table(t) },
                None => SweepCursor { phase: SweepPhase::Effect, after: String::new() },
            }),
            (true, SweepPhase::Effect) => None,
        };
        if next.is_none() {
            ops.push(state_store::Op::Put(keys::runtime(META, &["indexed"]), Object::new(json!({}))));
        }
        if !dry_run {
            for chunk in ops.chunks(CHUNK) {
                let t = Txn { ops: chunk.to_vec(), ..Txn::default() };
                contract.txn(&t, &Request::new(&super::served::request_id(), actor, "journal-sweep"))?;
            }
        }
        Ok((out, next))
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

    /// Every key under the table with the row it holds.
    fn table(&self, table: &str) -> Result<Vec<(String, Value)>> {
        let prefix = keys::runtime_table(table);
        let (items, _) = self.contract().list(&[prefix.as_str()], &Selector::Everything)?;
        Ok(items.into_iter().map(|i| (i.key.strip_prefix(&prefix).map_or(i.key.clone(), str::to_string), i.value.data)).collect())
    }

    fn markers(&self, table: &str) -> Result<std::collections::BTreeSet<String>> {
        Ok(self.table(table)?.into_iter().map(|(k, _)| k).collect())
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
