//! The batch and delivery machines on the control store. A round is `batches/<repo>/<id>` with
//! its members inside its value; a member's delivery is `deliveries/<id>`. A cascade (cut,
//! land) is one txn: every key it writes is compared at the revision it was read at, so a
//! record that moved between the plan and the commit writes nothing, and the else branch audits
//! each step as a lost race, as the dolt cascade's refusal rows record it.

use lifecycle::batch::{self, BatchEvent, BatchEventKind, BatchRow};
use lifecycle::bead::{self, BeadEvent, BeadEventKind, BeadState};
use lifecycle::delivery::{self, DeliveryEvent, DeliveryEventKind, DeliveryRow, DeliveryState};
use lifecycle::Refusal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use state_store::{Backend, Compare, Error, Item, Object, Request, Result, Rev, Selector, Txn};

use super::{Read, Store};
use crate::control::keys;
use crate::control::records::{decode, encode, Audit, BatchRecord, LifecycleRecord, Member};

/// A machine event's audit, in the dolt `event` row's terms.
#[allow(clippy::too_many_arguments)]
pub(crate) fn machine_audit(
    machine: &str,
    key: &str,
    expect: &str,
    from: &str,
    to: &str,
    refusal: Option<&Refusal>,
    evidence: Value,
    actor: &str,
    at: i64,
) -> Audit {
    Audit {
        machine: machine.into(),
        lc_key: key.into(),
        event: crate::kind_name(&evidence),
        expect: expect.into(),
        from_state: from.into(),
        to_state: to.into(),
        applied: refusal.is_none(),
        refusal: refusal.map(crate::refusal_name),
        evidence,
        actor: actor.into(),
        at,
    }
}

fn evidence_of(kind: &impl Serialize) -> Value {
    serde_json::to_value(kind).unwrap_or(Value::Null)
}

/// Why a verb wrote nothing before it reached a machine: the caller's view was stale (a
/// refusal) or the store could not say.
#[derive(Clone, Debug, PartialEq)]
pub enum Turned {
    Refused(String),
    CannotTell(String),
}

/// What a machine step or cascade did.
#[derive(Clone, Debug, PartialEq)]
pub enum Ran {
    /// Committed at `rev`, or, when `applied` is all false, lost its compares and wrote only
    /// the audit of the loss. One flag per machine step.
    Done { rev: u64, applied: Vec<bool> },
    /// The machine refused the event; the refusal is audited at `rev`.
    Refused { rev: u64, refusal: Refusal },
    /// Nothing was written.
    Turned(Turned),
}

struct Slot {
    key: String,
    read_rev: Option<Rev>,
    value: Option<Object>,
    dirty: bool,
}

/// A cascade being planned: each key is read once, and a later step reads what an earlier one
/// planned for it, so two steps on one record chain instead of both comparing the stored one.
#[derive(Default)]
pub struct Cascade {
    slots: Vec<Slot>,
    then: Vec<Audit>,
    els: Vec<Audit>,
    steps: usize,
}

impl Cascade {
    pub fn new() -> Self {
        Self::default()
    }

    fn slot<B: Backend>(&mut self, store: &Store<B>, key: &str) -> Result<&mut Slot> {
        if let Some(i) = self.slots.iter().position(|s| s.key == key) {
            return Ok(&mut self.slots[i]);
        }
        let item = store.contract().get(key)?;
        self.slots.push(Slot {
            key: key.into(),
            read_rev: item.as_ref().map(|i| i.mod_rev),
            value: item.map(|i| i.value),
            dirty: false,
        });
        Ok(self.slots.last_mut().expect("just pushed"))
    }

    fn get<B: Backend, T: for<'de> Deserialize<'de>>(
        &mut self,
        store: &Store<B>,
        key: &str,
    ) -> Result<Option<T>> {
        let slot = self.slot(store, key)?;
        slot.value.as_ref().map(|o| decode(key, o)).transpose()
    }

    /// Plans `value` at `key`, keeping the metadata it already had, or giving a new key `owners`.
    fn put<B: Backend, T: Serialize>(
        &mut self,
        store: &Store<B>,
        key: &str,
        value: &T,
        owners: &[&str],
    ) -> Result<()> {
        let data = encode(value)?;
        let slot = self.slot(store, key)?;
        slot.value = Some(match slot.value.take() {
            Some(prior) => Object {
                metadata: prior.metadata,
                data,
            },
            None => Object::new(data).with_owners(owners),
        });
        slot.dirty = true;
        Ok(())
    }

    /// A machine step: its audit when the cascade commits, and its loss when it does not.
    fn step(&mut self, applied: Audit) {
        let mut lost = applied.clone();
        lost.to_state = lost.from_state.clone();
        lost.applied = false;
        lost.refusal = Some("StaleVersion".into());
        self.then.push(applied);
        self.els.push(lost);
        self.steps += 1;
    }

    /// An audit that is not a step of its own: a delivery's birth inside the cut that made it.
    fn note(&mut self, audit: Audit) {
        self.then.push(audit);
    }

    pub fn steps(&self) -> usize {
        self.steps
    }

    /// The txn: each written key compared at the revision it was read at, then written.
    fn txn(&self) -> Txn {
        let written = || self.slots.iter().filter(|s| s.dirty);
        let mut t = Txn::new();
        for s in written() {
            t = t.when(Compare::ModRev(s.key.clone(), s.read_rev));
        }
        for s in written() {
            if let Some(v) = &s.value {
                t = t.put(&s.key, v.clone());
            }
        }
        t
    }
}

/// A planned cascade, or what stopped it before anything was planned.
pub enum Planned {
    Cascade(Cascade),
    Ran(Ran),
}

/// A cut's or a stack's members: each admitted at its tip, refused otherwise.
pub struct Cut<'a> {
    pub batch_id: &'a str,
    pub repo: &'a str,
    pub head: &'a str,
    pub base: &'a str,
    pub parent: Option<&'a str>,
    pub members: &'a [(String, String)],
    pub actor: &'a str,
    pub at: i64,
}

fn refused(msg: String) -> Planned {
    Planned::Ran(Ran::Turned(Turned::Refused(msg)))
}

fn cannot_tell(msg: String) -> Planned {
    Planned::Ran(Ran::Turned(Turned::CannotTell(msg)))
}

/// A delivery still in `batch_id`: an ejected member keeps its place in the member list, but
/// its delivery has left the batch.
fn still_in_batch(row: &DeliveryRow, batch_id: &str) -> bool {
    row.state == DeliveryState::Batched && row.batch_id.as_deref() == Some(batch_id)
}

fn next_record(cur: &LifecycleRecord, row: bead::BeadRow, at: i64) -> LifecycleRecord {
    LifecycleRecord {
        row,
        updated_at: at,
        ..cur.clone()
    }
}

impl<B: Backend> Store<B> {
    pub fn delivery(&self, id: &str) -> Result<Option<Read<DeliveryRow>>> {
        Ok(self.read(&keys::delivery(id), None)?.map(|(r, _)| r))
    }

    /// Every delivery, from one snapshot, in id order.
    pub fn deliveries(&self) -> Result<Vec<Read<DeliveryRow>>> {
        let (items, _) = self
            .contract()
            .list(&[keys::DELIVERIES], &Selector::Everything)?;
        items
            .iter()
            .filter(|i| keys::id_of(keys::DELIVERIES, &i.key).is_some())
            .map(read_of)
            .collect()
    }

    /// The round `id` and its key, whichever repo it was cut in.
    pub fn batch(&self, id: &str) -> Result<Option<(String, Read<BatchRecord>)>> {
        Ok(self
            .batches()?
            .into_iter()
            .find(|(_, r)| r.value.row.batch_id == id))
    }

    /// Every round, from one snapshot, by key.
    pub fn batches(&self) -> Result<Vec<(String, Read<BatchRecord>)>> {
        let (items, _) = self
            .contract()
            .list(&[keys::BATCHES], &Selector::Everything)?;
        items
            .iter()
            .filter(|i| batch_key(&i.key).is_some())
            .map(|i| Ok((i.key.clone(), read_of(i)?)))
            .collect()
    }

    /// The running pass's counts on `id`, if any were recorded.
    pub fn progress(&self, repo: &str, id: &str) -> Result<Option<Value>> {
        Ok(self
            .contract()
            .get(&keys::batch_progress(repo, id))?
            .map(|i| i.value.data))
    }

    fn commit(&self, c: Cascade, req: &Request, part: &str) -> Result<Ran> {
        let r = self.audited_txn(c.txn(), &c.then, &c.els, req, part)?;
        Ok(Ran::Done {
            rev: r.rev,
            applied: vec![r.succeeded; c.steps],
        })
    }

    /// Commits a planned cascade, or answers what stopped it.
    pub fn run_planned(&self, p: Planned, req: &Request, part: &str) -> Result<Ran> {
        match p {
            Planned::Cascade(c) => self.commit(c, req, part),
            Planned::Ran(r) => Ok(r),
        }
    }

    fn refuse_audit(&self, audit: Audit, refusal: Refusal, req: &Request) -> Result<Ran> {
        let r = self.audited_txn(Txn::new(), &[audit], &[], req, "refuse")?;
        Ok(Ran::Refused {
            rev: r.rev,
            refusal,
        })
    }

    /// One batch-machine event on `id`, CAS'd on the round's own key. A batch event that opens
    /// a mending row (`Failed`) belongs to the mending machine, which this store does not serve.
    pub fn batch_event(&self, id: &str, ev: &BatchEvent, at: i64, req: &Request) -> Result<Ran> {
        if matches!(ev.kind, BatchEventKind::Failed { .. }) {
            return Ok(Ran::Turned(Turned::CannotTell(
                "a Failed batch event opens a mending row, and the control store does not serve the mending machine yet".into(),
            )));
        }
        let Some((key, rec)) = self.batch(id)? else {
            return Ok(Ran::Turned(Turned::CannotTell(format!(
                "event: no batch row for {id}"
            ))));
        };
        let mut c = Cascade::new();
        match self.batch_step(&mut c, &key, &rec.value, ev, at)? {
            Ok(_) => self.commit(c, req, "apply"),
            Err((audit, refusal)) => self.refuse_audit(audit, refusal, req),
        }
    }

    /// Plans `ev` on the round at `key`: the next row, or the audited refusal.
    fn batch_step(
        &self,
        c: &mut Cascade,
        key: &str,
        stored: &BatchRecord,
        ev: &BatchEvent,
        at: i64,
    ) -> Result<std::result::Result<BatchRecord, (Audit, Refusal)>> {
        let cur: BatchRecord = c.get(self, key)?.unwrap_or_else(|| stored.clone());
        let out = batch::apply(&cur.row, ev);
        let from = cur.row.state.as_str();
        let audit = |to: &str, r: Option<&Refusal>| {
            machine_audit(
                "batch",
                &cur.row.batch_id,
                ev.expect.as_str(),
                from,
                to,
                r,
                evidence_of(&ev.kind),
                &ev.actor,
                at,
            )
        };
        if !out.applied {
            let refusal = out.refusal.unwrap_or(Refusal::IllegalTransition {
                state: from.into(),
                event: format!("{:?}", ev.kind),
            });
            return Ok(Err((audit(from, Some(&refusal)), refusal)));
        }
        c.step(audit(out.row.state.as_str(), None));
        let next = BatchRecord {
            row: out.row,
            ..cur.clone()
        };
        c.put(self, key, &next, &[])?;
        Ok(Ok(next))
    }

    /// One delivery-machine event on `id`, CAS'd on its own key.
    pub fn delivery_event(
        &self,
        id: &str,
        ev: &DeliveryEvent,
        at: i64,
        req: &Request,
    ) -> Result<Ran> {
        let key = keys::delivery(id);
        let mut c = Cascade::new();
        let Some(cur) = c.get::<B, DeliveryRow>(self, &key)? else {
            return Ok(Ran::Turned(Turned::CannotTell(format!(
                "event: no delivery row for {id}"
            ))));
        };
        let out = delivery::apply(&cur, ev);
        let from = cur.state.as_str();
        let audit = |to: &str, r: Option<&Refusal>| {
            machine_audit(
                "delivery",
                id,
                ev.expect.as_str(),
                from,
                to,
                r,
                evidence_of(&ev.kind),
                &ev.actor,
                at,
            )
        };
        if !out.applied {
            let refusal = out.refusal.unwrap_or(Refusal::IllegalTransition {
                state: from.into(),
                event: format!("{:?}", ev.kind),
            });
            return self.refuse_audit(audit(from, Some(&refusal)), refusal, req);
        }
        c.step(audit(out.row.state.as_str(), None));
        c.put(self, &key, &out.row, &[keys::lifecycle(id).as_str()])?;
        self.commit(c, req, "apply")
    }

    /// Plans a bead-machine event inside a cascade: applied, it is a step; refused, it is left
    /// out, as the dolt cascade leaves out a step its machine would not take.
    fn bead_step(
        &self,
        c: &mut Cascade,
        id: &str,
        kind: BeadEventKind,
        actor: &str,
        at: i64,
    ) -> Result<std::result::Result<(), Option<Refusal>>> {
        let key = keys::lifecycle(id);
        let Some(cur) = c.get::<B, LifecycleRecord>(self, &key)? else {
            return Ok(Err(None));
        };
        let ev = BeadEvent {
            expect: cur.row.state,
            version: cur.row.version,
            kind,
            actor: actor.into(),
            at: Some(at),
        };
        let out = bead::apply(&cur.row, &ev);
        if !out.applied {
            return Ok(Err(out.refusal));
        }
        let from = cur.row.state.as_str();
        c.step(machine_audit(
            "bead",
            id,
            from,
            from,
            out.row.state.as_str(),
            None,
            evidence_of(&ev.kind),
            actor,
            at,
        ));
        c.put(self, &key, &next_record(&cur, out.row, at), &[])?;
        Ok(Ok(()))
    }

    /// Plans a delivery-machine event from wherever the row stands, as a cascade's step.
    fn delivery_step(
        &self,
        c: &mut Cascade,
        id: &str,
        row: &DeliveryRow,
        kind: DeliveryEventKind,
        actor: &str,
        at: i64,
    ) -> Result<Option<DeliveryRow>> {
        let ev = DeliveryEvent {
            expect: row.state,
            version: row.version,
            kind,
            actor: actor.into(),
        };
        let out = delivery::apply(row, &ev);
        if !out.applied {
            return Ok(None);
        }
        let from = row.state.as_str();
        c.step(machine_audit(
            "delivery",
            id,
            from,
            from,
            out.row.state.as_str(),
            None,
            evidence_of(&ev.kind),
            actor,
            at,
        ));
        c.put(
            self,
            &keys::delivery(id),
            &out.row,
            &[keys::lifecycle(id).as_str()],
        )?;
        Ok(Some(out.row))
    }

    /// Every member admitted at its tip, before anything is planned: a round takes SUBMITTED
    /// beads as well as CERTIFIED ones (`allow_submitted`), a stack only CERTIFIED ones; a
    /// delivery still BATCHED elsewhere, or past cutting, refuses the whole verb.
    fn admitted(
        &self,
        c: &mut Cascade,
        members: &[(String, String)],
        allow_submitted: bool,
    ) -> Result<std::result::Result<Vec<Option<DeliveryRow>>, Planned>> {
        let mut deliveries = Vec::new();
        for (n, (id, tip)) in members.iter().enumerate() {
            if members[..n].iter().any(|(other, _)| other == id) {
                return Ok(Err(cannot_tell(format!(
                    "{id} is named twice in --members"
                ))));
            }
            let Some(rec) = c.get::<B, LifecycleRecord>(self, &keys::lifecycle(id))? else {
                return Ok(Err(refused(format!("refused: no bead row for {id}"))));
            };
            let row = rec.row;
            let ok = row.state == BeadState::Certified
                || (allow_submitted && row.state == BeadState::Submitted);
            if !ok {
                let want = if allow_submitted {
                    "SUBMITTED or CERTIFIED"
                } else {
                    "CERTIFIED"
                };
                return Ok(Err(refused(format!(
                    "refused: {id} is {} not {want}",
                    row.state.as_str()
                ))));
            }
            if row.tip.as_deref() != Some(tip.as_str()) {
                return Ok(Err(refused(format!(
                    "refused: {id}'s certified tip does not match {tip}"
                ))));
            }
            deliveries.push(c.get::<B, DeliveryRow>(self, &keys::delivery(id))?);
        }
        for ((id, _), d) in members.iter().zip(&deliveries) {
            match d.as_ref().map(|d| d.state) {
                None | Some(DeliveryState::Queued | DeliveryState::Exited) => {}
                Some(DeliveryState::Batched) => {
                    let other = d.as_ref().and_then(|d| d.batch_id.as_deref());
                    return Ok(Err(refused(format!(
                        "refused: {id} is still BATCHED in {}",
                        other.unwrap_or("another batch")
                    ))));
                }
                Some(s) => {
                    return Ok(Err(refused(format!(
                        "refused: {id}'s delivery is {} and cannot be cut",
                        s.as_str()
                    ))))
                }
            }
        }
        Ok(Ok(deliveries))
    }

    /// Each member's `MemberAdded` on the round, `Deliver` on its record, and its delivery
    /// (re)queued and `Cut` into the round, folded onto `round` in order.
    #[allow(clippy::too_many_arguments)]
    fn plan_members(
        &self,
        c: &mut Cascade,
        round: &mut BatchRow,
        members: &[(String, String)],
        deliveries: &[Option<DeliveryRow>],
        actor: &str,
        at: i64,
        verb: &str,
    ) -> Result<std::result::Result<(), Planned>> {
        for ((id, tip), delivery) in members.iter().zip(deliveries) {
            let added = BatchEvent {
                expect: round.state,
                version: round.version,
                kind: BatchEventKind::MemberAdded {
                    bead_id: id.clone(),
                    tip: tip.clone(),
                },
                actor: actor.into(),
            };
            let out = batch::apply(round, &added);
            if !out.applied {
                return Ok(Err(cannot_tell(format!(
                    "{verb}: MemberAdded refused unexpectedly for {id}: {:?}",
                    out.refusal
                ))));
            }
            let state = round.state.as_str();
            c.step(machine_audit(
                "batch",
                &round.batch_id,
                state,
                state,
                state,
                None,
                evidence_of(&added.kind),
                actor,
                at,
            ));
            *round = out.row;
            if let Err(r) = self.bead_step(c, id, BeadEventKind::Deliver, actor, at)? {
                return Ok(Err(cannot_tell(format!(
                    "{verb}: Deliver refused unexpectedly for {id}: {r:?}"
                ))));
            }
            let queued = match delivery {
                None => {
                    c.note(machine_audit(
                        "delivery",
                        id,
                        "NONE",
                        "NONE",
                        DeliveryState::Queued.as_str(),
                        None,
                        Value::Null,
                        actor,
                        at,
                    ));
                    DeliveryRow::start_queue(id.as_str())
                }
                Some(row) if row.state == DeliveryState::Exited => {
                    let from = row.state.as_str();
                    c.step(machine_audit(
                        "delivery",
                        id,
                        from,
                        from,
                        DeliveryState::Queued.as_str(),
                        None,
                        Value::Null,
                        actor,
                        at,
                    ));
                    DeliveryRow {
                        version: row.version + 1,
                        ..DeliveryRow::start_queue(id.as_str())
                    }
                }
                Some(row) => row.clone(),
            };
            let cut = DeliveryEventKind::Cut {
                batch_id: round.batch_id.clone(),
            };
            if self
                .delivery_step(c, id, &queued, cut, actor, at)?
                .is_none()
            {
                return Ok(Err(cannot_tell(format!(
                    "{verb}: Cut refused unexpectedly for {id}"
                ))));
            }
        }
        Ok(Ok(()))
    }

    /// `cut`: the round born OPEN with its members inside it, and each member delivered into
    /// it, in one txn. A round of that id in any repo refuses the cut.
    pub fn plan_cut(&self, cut: &Cut) -> Result<Planned> {
        if self.batch(cut.batch_id)?.is_some() {
            return Ok(cannot_tell(format!(
                "cut: a batch row for {} already exists",
                cut.batch_id
            )));
        }
        // The round's own key is read first, so the txn compares it absent: a same-id cut that
        // commits after this read makes this one lose rather than overwrite it.
        let mut c = Cascade::new();
        let key = keys::batch(cut.repo, cut.batch_id);
        if c.get::<B, BatchRecord>(self, &key)?.is_some() {
            return Ok(cannot_tell(format!(
                "cut: a batch row for {} already exists",
                cut.batch_id
            )));
        }
        // Admission reads every member through the cascade, so what it judged is what is compared.
        let deliveries = match self.admitted(&mut c, cut.members, true)? {
            Ok(d) => d,
            Err(early) => return Ok(early),
        };
        let mut round = BatchRow::cut(cut.batch_id, cut.repo, cut.head, cut.base);
        round.parent = cut.parent.map(str::to_string);
        if let Err(early) = self.plan_members(
            &mut c,
            &mut round,
            cut.members,
            &deliveries,
            cut.actor,
            cut.at,
            "cut",
        )? {
            return Ok(early);
        }
        let rec = BatchRecord {
            row: round,
            members: cut
                .members
                .iter()
                .map(|(id, tip)| Member {
                    bead_id: id.clone(),
                    tip: tip.clone(),
                })
                .collect(),
            opened_at: cut.at,
        };
        c.put(self, &key, &rec, &[])?;
        Ok(Planned::Cascade(c))
    }

    /// `land`: `FastForward` on the round, and every member still in it `Delivered` on its
    /// delivery and its record, in one txn.
    pub fn plan_land(&self, id: &str, ev: &BatchEvent, at: i64, req: &Request) -> Result<Planned> {
        let BatchEventKind::FastForward { sha } = &ev.kind else {
            return Err(Error::Backend(format!(
                "a landing of {id} is a FastForward"
            )));
        };
        let Some((key, rec)) = self.batch(id)? else {
            return Ok(cannot_tell(format!("land: no batch row for {id}")));
        };
        let mut c = Cascade::new();
        if let Err((audit, refusal)) = self.batch_step(&mut c, &key, &rec.value, ev, at)? {
            return Ok(Planned::Ran(self.refuse_audit(audit, refusal, req)?));
        }
        for m in &rec.value.members {
            let Some(d) = c.get::<B, DeliveryRow>(self, &keys::delivery(&m.bead_id))? else {
                continue;
            };
            if !still_in_batch(&d, id) {
                continue;
            }
            let proof = format!("queue-fast-forward:{}", m.tip);
            let delivered = DeliveryEventKind::Delivered {
                merge_sha: sha.clone(),
                proof: proof.clone(),
            };
            self.delivery_step(&mut c, &m.bead_id, &d, delivered, &ev.actor, at)?;
            let landed = BeadEventKind::Delivered {
                merge_sha: sha.clone(),
                proof,
            };
            let _ = self.bead_step(&mut c, &m.bead_id, landed, &ev.actor, at)?;
        }
        Ok(Planned::Cascade(c))
    }

    /// The rounds not yet finished that name `bead` as a member, by id.
    pub fn batches_of(&self, bead: &str) -> Result<Vec<BatchRow>> {
        let mut out: Vec<BatchRow> = self
            .batches()?
            .into_iter()
            .map(|(_, r)| r.value)
            .filter(|r| {
                !matches!(r.row.state.as_str(), "LANDED" | "SETTLED" | "ABANDONED")
                    && r.members.iter().any(|m| m.bead_id == bead)
            })
            .map(|r| r.row)
            .collect();
        out.sort_by(|a, b| a.batch_id.cmp(&b.batch_id));
        Ok(out)
    }
}

/// `(repo, id)` of a round's own key; `None` for its progress key or anything else under it.
pub(crate) fn batch_key(key: &str) -> Option<(&str, &str)> {
    let rest = key.strip_prefix(keys::BATCHES)?;
    let (repo, id) = rest.split_once('/')?;
    (!repo.is_empty() && !id.is_empty() && !id.contains('/')).then_some((repo, id))
}

fn read_of<T: for<'de> Deserialize<'de>>(i: &Item) -> Result<Read<T>> {
    Ok(Read {
        value: decode(&i.key, &i.value)?,
        mod_rev: i.mod_rev,
        create_rev: i.create_rev,
    })
}

#[cfg(test)]
mod tests;
