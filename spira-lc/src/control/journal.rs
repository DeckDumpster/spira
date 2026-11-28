//! The runtime's journal on the control store: what it keeps across a restart (task state, open
//! effects, the schedule) and the poison it recorded. An effect's row exists only while it is
//! open: the turn that resolves it deletes it, and the runtime's log keeps its record. A schedule
//! row closes by a marker key of its own, so a re-inserted row reads as it did. Each row not yet
//! closed also has an index key, so a boot read costs the rows it returns.

use serde_json::{json, Value};
use spira_lc::effect_ledger::{self, DONE, OPEN_EFFECT};
use state_store::{Backend, Compare, Object, Request, Result, Selector, Txn};

use super::keys;
use super::store::Store;

const STATE: &str = "state";
const EFFECT: &str = "effect";
const RESOLVED: &str = "resolved";
const SCHEDULE: &str = "schedule";
const FIRED: &str = "fired";
const POISON: &str = "poison";
const PENDING: &str = "pending";
const META: &str = "journal-meta";

/// Ops one txn may carry, with room for the request's own bookkeeping.
const CHUNK: usize = 100;
const ATTEMPTS: usize = 8;
/// Rows one sweep step reads at most: the store's page.
pub const SWEEP_STEP: usize = 1_000;
pub const STATE_PAGE: usize = 1_000;
/// Rows one sweep page asks for at most.
pub const SWEEP_PAGE: usize = 25;
/// Rows a step's first page asks for, before it has seen how large they are.
pub const SWEEP_FIRST_PAGE: usize = 4;
/// Value bytes a later page aims to read, sized by the largest value the step has seen.
pub const SWEEP_PAGE_BYTES: usize = 32 * 1024;
/// Milliseconds a sweep step spends before it stops after a page: half the store's 100 ms target.
pub const SWEEP_BUDGET_MS: u64 = 50;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SweepPhase {
    Schedule,
    Legacy,
    Effect,
    /// Every resolved-effect marker: no writer makes one.
    Resolved,
}

/// Tables no reader or writer uses any more, whose rows an old import carried over; the sweep deletes them.
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
            SweepPhase::Resolved => "resolved",
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
            Some(("resolved", _)) => SweepPhase::Resolved,
            _ => return Err(format!("journal: sweep cursor {s:?} is not schedule:<key>, legacy:<key>, effect:<key> or resolved:<key>")),
        };
        let after = s.split_once(':').map_or(String::new(), |(_, k)| k.to_string());
        if phase == SweepPhase::Legacy && !after.is_empty() && !LEGACY.iter().any(|t| after.starts_with(&keys::runtime_table(t))) {
            return Err(format!("journal: sweep cursor {s:?} is in no legacy table"));
        }
        Ok(SweepCursor { phase, after })
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
    /// (task, skey) of state rows to delete.
    pub cleared: Vec<(String, String)>,
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

/// What a sweep step removed, or with `dry_run` would have: id-less schedule rows, legacy rows,
/// closed effect rows and resolved markers, and the index keys it wrote for rows stored before
/// the index.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Swept {
    pub idless_schedules: usize,
    pub legacy_deleted: usize,
    pub indexed_schedules: usize,
    pub indexed_effects: usize,
    pub deleted_effects: usize,
    pub deleted_markers: usize,
}

fn is_read_ended(e: &state_store::Error) -> bool {
    matches!(e, state_store::Error::Backend(m) if m.starts_with(state_store::READ_ENDED))
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
            let mut one = |compares: Vec<Compare>, put: Vec<(String, Option<Object>)>| {
                let mut t = Txn::new();
                let compared = !compares.is_empty();
                for c in compares {
                    t = t.when(c);
                }
                for (k, v) in put {
                    t = match v {
                        Some(v) => t.put(&k, v),
                        None => t.delete(&k),
                    };
                }
                ops.push((compared, t));
            };
            for r in &turn.state {
                one(vec![], vec![(keys::runtime(STATE, &[&r.task, &r.skey]), Some(Object::new(json!({ "task": r.task, "skey": r.skey, "value": r.value, "at": r.at }))))]);
            }
            for (task, skey) in &turn.cleared {
                one(vec![], vec![(keys::runtime(STATE, &[task, skey]), None)]);
            }
            let closing: std::collections::BTreeSet<&str> = turn.resolved.iter().map(String::as_str).collect();
            let opening: Vec<&EffectRow> = turn.effects.iter().filter(|e| !closing.contains(e.request_id.as_str())).collect();
            let rewritten: std::collections::BTreeSet<String> = opening.iter().map(|e| keys::runtime(EFFECT, &[&e.task, &e.ekey])).collect();
            for e in &opening {
                let row = json!({ "task": e.task, "ekey": e.ekey, "request_id": e.request_id, "stream": e.stream, "spec": e.spec, "requested_at": e.requested_at, "deadline_at": e.deadline_at });
                one(vec![], vec![(keys::runtime(EFFECT, &[&e.task, &e.ekey]), Some(Object::new(row))), (keys::runtime(OPEN_EFFECT, &[&e.request_id]), Some(open_effect(&e.task, &e.ekey))), (keys::runtime(RESOLVED, &[&e.request_id]), None)]);
            }
            let schedule_keys: Vec<String> = turn.schedules.iter().map(|s| keys::runtime(SCHEDULE, &[&s.id])).collect();
            let schedule_refs: Vec<&str> = schedule_keys.iter().map(String::as_str).collect();
            let existing = self.contract().get_many_at(&schedule_refs, self.contract().head()?)?;
            for ((s, key), found) in turn.schedules.iter().zip(schedule_keys).zip(existing.iter().map(Option::is_some)) {
                if found {
                    continue;
                }
                let row = json!({ "id": s.id, "at": s.at, "stream": s.stream, "tag": s.tag, "body": s.body, "cause": s.cause, "correlation": s.correlation });
                one(vec![Compare::ModRev(key.clone(), None)], vec![(key, Some(Object::new(row))), (keys::runtime(PENDING, &[&s.id]), Some(pending(&s.id))), (keys::runtime(FIRED, &[&s.id]), None)]);
            }
            for id in &turn.resolved {
                let open_key = keys::runtime(OPEN_EFFECT, &[id]);
                let mut put = vec![(open_key.clone(), None), (keys::runtime(RESOLVED, &[id]), None), (keys::runtime(DONE, &[id]), None)];
                let mut compares = Vec::new();
                if let Some(open) = self.contract().get(&open_key)? {
                    let key = keys::runtime(EFFECT, &[&text(&open.value.data, "task"), &text(&open.value.data, "ekey")]);
                    let ours = |r: &state_store::Item| keys::component(&text(&r.value.data, "request_id")) == keys::component(id);
                    if let Some(row) = self.contract().get(&key)?.filter(|r| !rewritten.contains(&key) && ours(r)) {
                        compares.push(Compare::ModRev(key.clone(), Some(row.mod_rev)));
                        put.push((key, None));
                    }
                }
                one(compares, put);
            }
            for id in &turn.fired {
                one(vec![], vec![(keys::runtime(FIRED, &[id]), Some(Object::new(json!({})))), (keys::runtime(PENDING, &[id]), None)]);
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
        let mut one = |key: String, row: Object, extra: Vec<(String, Object)>| -> Result<()> {
            let mut t = Txn::new().when(Compare::ModRev(key.clone(), None)).put(&key, row);
            for (k, o) in extra {
                t = t.put(&k, o);
            }
            let req = Request::new(&super::served::request_id(), actor, "carry-journal");
            written += usize::from(self.contract().txn(&t, &req)?.succeeded);
            Ok(())
        };
        for r in &rows.state {
            one(keys::runtime(STATE, &[&r.task, &r.skey]), Object::new(json!({ "task": r.task, "skey": r.skey, "value": r.value, "at": r.at })), vec![])?;
        }
        for (e, _) in rows.effects.iter().filter(|(_, resolved)| !resolved) {
            let row = json!({ "task": e.task, "ekey": e.ekey, "request_id": e.request_id, "stream": e.stream, "spec": e.spec, "requested_at": e.requested_at, "deadline_at": e.deadline_at });
            let index = (keys::runtime(OPEN_EFFECT, &[&e.request_id]), open_effect(&e.task, &e.ekey));
            one(keys::runtime(EFFECT, &[&e.task, &e.ekey]), Object::new(row), vec![index])?;
        }
        for (s, fired) in &rows.schedules {
            let row = json!({ "id": s.id, "at": s.at, "stream": s.stream, "tag": s.tag, "body": s.body, "cause": s.cause, "correlation": s.correlation });
            let extra = match fired {
                true => (keys::runtime(FIRED, &[&s.id]), Object::new(json!({}))),
                false => (keys::runtime(PENDING, &[&s.id]), pending(&s.id)),
            };
            one(keys::runtime(SCHEDULE, &[&s.id]), Object::new(row), vec![extra])?;
        }
        for p in &rows.poison {
            one(keys::runtime(POISON, &[&p.stream]), Object::new(json!({ "stream": p.stream, "event": p.event, "handler": p.handler, "message": p.message })), vec![])?;
        }
        Ok(written)
    }

    /// At most `limit` state rows after the store key `after`, in key order, and the key to resume
    /// from when the page is full.
    pub fn journal_state_page(&self, after: Option<&str>, limit: usize) -> Result<(Vec<StateRow>, Option<String>)> {
        let contract = self.contract();
        let head = contract.head()?;
        let page = contract.range_at(&keys::runtime_table(STATE), after, limit, head)?;
        let next = page.last().map(|i| i.key.clone()).filter(|_| page.len() >= limit);
        let rows = page.iter().map(|i| StateRow { task: text(&i.value.data, "task"), skey: text(&i.value.data, "skey"), value: text(&i.value.data, "value"), at: num(&i.value.data, "at") }).collect();
        Ok((rows, next))
    }

    pub fn journal_state(&self) -> Result<Vec<StateRow>> {
        let (mut rows, mut after) = (Vec::new(), None);
        loop {
            let (page, next) = self.journal_state_page(after.as_deref(), STATE_PAGE)?;
            rows.extend(page);
            match next {
                Some(n) => after = Some(n),
                None => break,
            }
        }
        rows.sort_by(|a, b| (&a.task, &a.skey).cmp(&(&b.task, &b.skey)));
        Ok(rows)
    }

    /// Whether a sweep has indexed every row stored before the index: until then the readers
    /// list the whole tables, as a store written by an older release needs.
    fn indexed(&self) -> Result<bool> {
        Ok(self.contract().get(&keys::runtime(META, &["indexed"]))?.is_some())
    }

    /// Marks an open effect run; see [`effect_ledger::done`].
    pub fn journal_done(&self, id: &str, actor: &str) -> Result<bool> {
        effect_ledger::done(self.contract(), id, &Request::new(&super::served::request_id(), actor, "journal-done"))
    }

    pub fn journal_has_done(&self, id: &str) -> Result<bool> {
        effect_ledger::has_done(self.contract(), id)
    }

    pub fn journal_open_effects(&self) -> Result<Vec<EffectRow>> {
        let mut rows: Vec<EffectRow> = if self.indexed()? {
            let mut rows = Vec::new();
            for open in self.table(OPEN_EFFECT)? {
                let request = open.0;
                if let Some(i) = self.contract().get(&keys::runtime(EFFECT, &[&text(&open.1, "task"), &text(&open.1, "ekey")]))? {
                    let row = effect_of(&i.value.data);
                    if keys::component(&row.request_id) == request {
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

    /// One bounded step of the sweep: reads pages of about [`SWEEP_PAGE_BYTES`], at most
    /// [`SWEEP_PAGE`] rows, after `from` until [`SWEEP_STEP`] rows are read or [`SWEEP_BUDGET_MS`] is spent, and for each page deletes the
    /// schedule rows stored without an id and the [`LEGACY`] rows, indexes the open rows an older release stored
    /// without one, and deletes each closed effect row, with its marker, unless it changed since the step read
    /// it. In an indexed store a row is open only if its index names it; before that, only a marker closes one.
    /// The step that exhausts the effects marks the store indexed; the markers left are then deleted, and the
    /// step that exhausts them answers no cursor. Each page and its lookups are read at the head current
    /// when the page is asked. After an earlier page, an ended read or a write refused for the deadline
    /// ends the step with what the finished pages did. Every write is idempotent, so a step that
    /// fails is run again from the same cursor.
    /// With `dry_run` it counts and writes nothing.
    pub fn journal_sweep_step(&self, from: Option<&SweepCursor>, dry_run: bool, actor: &str) -> Result<(Swept, Option<SweepCursor>)> {
        let contract = self.contract();
        let started = contract.now_ms();
        let head = contract.head()?;
        let (phase, start_after) = from.map_or((SweepPhase::Schedule, None), |c| (c.phase, Some(c.after.clone()).filter(|a| !a.is_empty())));
        let legacy_at = start_after.as_deref().filter(|_| phase == SweepPhase::Legacy).and_then(|a| LEGACY.iter().position(|t| a.starts_with(&keys::runtime_table(t)))).unwrap_or(0);
        let table = match phase {
            SweepPhase::Schedule => SCHEDULE,
            SweepPhase::Legacy => LEGACY[legacy_at],
            SweepPhase::Effect => EFFECT,
            SweepPhase::Resolved => RESOLVED,
        };
        let indexed = phase == SweepPhase::Effect && contract.get_many_at(&[keys::runtime(META, &["indexed"]).as_str()], head)?.iter().any(Option::is_some);
        let mut out = Swept::default();
        let mut after = start_after;
        let mut read = 0;
        let (mut ask, mut largest) = (SWEEP_FIRST_PAGE, 0);
        let exhausted = loop {
            // A read below the head costs the whole range's history, and each page's deletes move the head.
            let head = contract.head()?;
            let page = match contract.range_at(&keys::runtime_table(table), after.as_deref(), ask, head) {
                Err(e) if read > 0 && is_read_ended(&e) => break false,
                page => page?,
            };
            let exhausted = page.len() < ask;
            largest = page.iter().map(|item| serde_json::to_vec(&item.value.data).map_or(0, |v| v.len())).fold(largest, usize::max);
            let mut ops: Vec<state_store::Op> = Vec::new();
            let mut closed: Vec<(Txn, bool)> = Vec::new();
            let mut counts = Swept::default();
            let looked_up: Vec<String> = match phase {
                SweepPhase::Schedule => page.iter().map(|item| text(&item.value.data, "id")).filter(|id| !id.is_empty()).map(|id| keys::runtime(FIRED, &[&id])).collect(),
                SweepPhase::Effect => {
                    let ids: Vec<String> = page.iter().map(|item| effect_of(&item.value.data).request_id).collect();
                    let markers = ids.iter().map(|id| keys::runtime(RESOLVED, &[id]));
                    match indexed {
                        true => markers.chain(ids.iter().map(|id| keys::runtime(OPEN_EFFECT, &[id]))).collect(),
                        false => markers.collect(),
                    }
                }
                SweepPhase::Legacy | SweepPhase::Resolved => Vec::new(),
            };
            let refs: Vec<&str> = looked_up.iter().map(String::as_str).collect();
            let looked = if refs.is_empty() { Ok(Vec::new()) } else { contract.get_many_at(&refs, head) };
            let found = match looked {
                Err(e) if read > 0 && is_read_ended(&e) => break false,
                found => found?,
            };
            let mut fired = found.iter();
            for (n, item) in page.iter().enumerate() {
                let v = &item.value.data;
                match phase {
                    SweepPhase::Schedule => {
                        let id = text(v, "id");
                        if id.is_empty() {
                            counts.idless_schedules += 1;
                            ops.push(state_store::Op::Delete(item.key.clone()));
                        } else if fired.next().and_then(Option::as_ref).is_none() {
                            counts.indexed_schedules += 1;
                            ops.push(state_store::Op::Put(keys::runtime(PENDING, &[&id]), pending(&id)));
                        }
                    }
                    SweepPhase::Legacy => {
                        counts.legacy_deleted += 1;
                        ops.push(state_store::Op::Delete(item.key.clone()));
                    }
                    SweepPhase::Effect => {
                        let e = effect_of(v);
                        let marked = found[n].is_some();
                        let open = match indexed {
                            true => found[page.len() + n].as_ref().is_some_and(|i| text(&i.value.data, "task") == e.task && text(&i.value.data, "ekey") == e.ekey),
                            false => !marked,
                        };
                        if !open {
                            let mut t = Txn::new().when(Compare::ModRev(item.key.clone(), Some(item.mod_rev))).delete(&item.key);
                            if marked {
                                t = t.delete(&keys::runtime(RESOLVED, &[&e.request_id]));
                            }
                            closed.push((t, marked));
                        } else if !indexed {
                            counts.indexed_effects += 1;
                            ops.push(state_store::Op::Put(keys::runtime(OPEN_EFFECT, &[&e.request_id]), open_effect(&e.task, &e.ekey)));
                        }
                    }
                    SweepPhase::Resolved => {
                        counts.deleted_markers += 1;
                        ops.push(state_store::Op::Delete(item.key.clone()));
                    }
                }
            }
            read += page.len();
            ask = (SWEEP_PAGE_BYTES / largest.max(1)).clamp(1, SWEEP_PAGE).min(SWEEP_STEP.saturating_sub(read));
            let last = page.last().map(|i| i.key.clone());
            let finishing = exhausted && phase == SweepPhase::Effect;
            if finishing {
                ops.push(state_store::Op::Put(keys::runtime(META, &["indexed"]), Object::new(json!({}))));
            }
            if dry_run {
                // A dry run leaves every marker for the marker phase to count.
                counts.deleted_effects = closed.len();
            } else {
                let (mut rows, mut markers) = (0, 0);
                let written = (|| -> Result<()> {
                    let req = || Request::new(&super::served::request_id(), actor, "journal-sweep");
                    for chunk in ops.chunks(CHUNK) {
                        contract.txn(&Txn { ops: chunk.to_vec(), ..Txn::default() }, &req())?;
                    }
                    for (t, marked) in &closed {
                        if contract.txn(t, &req())?.succeeded {
                            rows += 1;
                            markers += usize::from(*marked);
                        }
                    }
                    Ok(())
                })();
                counts.deleted_effects = rows;
                counts.deleted_markers += markers;
                match written {
                    Ok(()) => {}
                    Err(state_store::Error::Deadline) if read > page.len() => {
                        out.deleted_effects += rows;
                        out.deleted_markers += markers;
                        break false;
                    }
                    Err(e) => return Err(e),
                }
            }
            out.idless_schedules += counts.idless_schedules;
            out.legacy_deleted += counts.legacy_deleted;
            out.indexed_schedules += counts.indexed_schedules;
            out.indexed_effects += counts.indexed_effects;
            out.deleted_effects += counts.deleted_effects;
            out.deleted_markers += counts.deleted_markers;
            after = last.or(after);
            if exhausted {
                break true;
            }
            if read >= SWEEP_STEP || contract.now_ms().saturating_sub(started) >= SWEEP_BUDGET_MS {
                break false;
            }
        };
        let next = match (exhausted, phase) {
            (false, _) => after.map(|after| SweepCursor { phase, after }),
            (true, SweepPhase::Schedule) => Some(SweepCursor { phase: SweepPhase::Legacy, after: String::new() }),
            (true, SweepPhase::Legacy) => Some(match LEGACY.get(legacy_at + 1) {
                Some(t) => SweepCursor { phase, after: keys::runtime_table(t) },
                None => SweepCursor { phase: SweepPhase::Effect, after: String::new() },
            }),
            (true, SweepPhase::Effect) => Some(SweepCursor { phase: SweepPhase::Resolved, after: String::new() }),
            (true, SweepPhase::Resolved) => None,
        };
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
