//! Lifecycle records on the state-store contract. Each applied transition is one txn that
//! compares the record's `mod_rev`; its audit rides into `events/<rev>` as the request's
//! evidence, refusals included. A claim is the `locks/lifecycle/<id>` lock, taken in the same txn
//! that writes the record: the holder named on the record is the one whose claim created the
//! lock, its fencing token is the lock's `create_rev`, and every write by the holder is fenced
//! on it. The record keeps no lease of its own; reads project `lease_until` from the lock.
//! Every txn that carries an audit also writes, on the same branch, a `history/<id>/` entry
//! for each id its audit names, so a bead's history is read without scanning `events/`.
//! A transition that moves a record's gate (`edges::gate`) puts `lifecycle/<id>/gate` in the same
//! txn, and no other transition writes it.

use lifecycle::bead::{self, BeadEvent, BeadEventKind, BeadState};
use serde_json::Value;
use state_store::{
    Backend, Compare, Contract, Deleted, Error, Fence, Item, Object, Request, Result, Rev, Txn,
};

use super::keys;
use super::records::{decode, encode, Audit, LifecycleRecord, Readiness};
use spira_config::edges::Gate;

/// A value read with the revisions it was read at.
#[derive(Clone, Debug, PartialEq)]
pub struct Read<T> {
    pub value: T,
    pub mod_rev: Rev,
    pub create_rev: Rev,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Applied {
        rev: u64,
        record: Box<LifecycleRecord>,
    },
    /// The machine refused the event; the refusal is audited at `rev`.
    Refused {
        rev: u64,
        refusal: lifecycle::Refusal,
    },
    /// The record moved between the read and the write.
    LostRace { rev: u64 },
    /// The holder's lock expired or is held under another token.
    LockLost { rev: u64 },
    /// Another claim's lock is still live.
    Locked { token: Rev },
    /// A blocker's gate is no longer what the claim was judged against.
    BlockerMoved { rev: u64, blocker: String },
    /// The last readiness pass judged the record not claimable; nothing is written.
    JudgedUnclaimable { blocked_by: Vec<String> },
}

/// Every lifecycle record, each id's readiness, and the revision they were read at.
pub type Snapshot = (
    Vec<Read<LifecycleRecord>>,
    std::collections::BTreeMap<String, Readiness>,
    u64,
);

/// One entry of a dispatcher pass.
#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    pub id: String,
    pub verdict: String,
    pub blocker: Option<String>,
    pub runes: Vec<String>,
}

pub struct Store<B: Backend> {
    contract: Contract<B>,
    /// The shortest claim lock taken, whatever lease the claimant asked for.
    claim_floor_ms: u64,
}

/// Ops a pass puts in one txn, leaving room under etcd's limit for the request and events keys.
const PASS_CHUNK: usize = state_store::limits::MAX_TXN_OPS - 2;
/// How often a read-merge-write is re-planned after losing its compare before it gives up.
const MERGE_ATTEMPTS: usize = 5;

fn sub_request(req: &Request, part: &str, evidence: Value) -> Request {
    Request {
        id: format!("{}/{part}", req.id),
        evidence,
        ..req.clone()
    }
}

fn audit(
    id: &str,
    ev: &BeadEvent,
    from: BeadState,
    to: BeadState,
    refusal: Option<&lifecycle::Refusal>,
) -> Audit {
    let evidence = serde_json::to_value(&ev.kind).unwrap_or(Value::Null);
    Audit {
        machine: "bead".into(),
        lc_key: id.into(),
        event: crate::kind_name(&evidence),
        expect: if refusal.is_none() {
            from.as_str().into()
        } else {
            ev.expect.as_str().into()
        },
        from_state: from.as_str().into(),
        to_state: to.as_str().into(),
        applied: refusal.is_none(),
        refusal: refusal.map(crate::refusal_name),
        evidence,
        actor: ev.actor.clone(),
        at: ev.at.unwrap_or(0),
    }
}

/// A txn's audit for each branch: `then` when it applied, `else` when its compares failed, as
/// `events/<rev>`'s `refused` says.
fn evidence(then: &[Audit], els: &[Audit]) -> Value {
    serde_json::json!({ "then": then, "else": els })
}

fn object_like(prev: &Item, data: Value) -> Object {
    Object {
        metadata: prev.value.metadata.clone(),
        data,
    }
}

pub(crate) fn gate_object(id: &str, gate: &Gate) -> Result<Object> {
    Ok(Object::new(encode(gate)?).with_owners(&[keys::lifecycle(id).as_str()]))
}

/// The compare that holds when `id`'s gate still reads `gate`.
fn gate_compare(id: &str, gate: Option<&Gate>) -> Result<Compare> {
    Ok(Compare::Value(
        keys::gate(id),
        gate.map(|g| gate_object(id, g)).transpose()?,
    ))
}

/// The blockers a claim compares, each with the gate it must still read: every one its
/// readiness was judged against, and each prerequisite its stack builds on, at the tip stacked.
fn claim_edges(judged: Option<&Readiness>, stack: &bead::Stack) -> Vec<(String, Option<Gate>)> {
    let mut edges: Vec<(String, Option<Gate>)> = judged
        .map(|r| r.gates.clone().into_iter().collect())
        .unwrap_or_default();
    for (prereq, tip) in stack {
        let gate = spira_config::edges::gate(BeadState::Certified, Some(tip));
        if !edges
            .iter()
            .any(|(b, g)| b == prereq && g.as_ref() == Some(&gate))
        {
            edges.push((prereq.clone(), Some(gate)));
        }
    }
    edges
}

/// What a claim was judged against, for its event's evidence.
fn judged_evidence(r: &Readiness) -> Result<Value> {
    Ok(serde_json::json!({
        "edges": encode(&r.gates)?,
        "planning_watermark": r.judged_at_planning_watermark,
        "judged_at_rev": r.judged_at_rev,
    }))
}

fn audited_ids(audits: &[Audit]) -> std::collections::BTreeSet<&str> {
    audits.iter().map(|a| a.lc_key.as_str()).collect()
}

/// A WORKING record's `lease_until` is its claim lock's expiry while the lock is live, in epoch
/// seconds rounded up, so a reader never sees a live lease as lapsed. With no live lock it stays
/// the lease the Claim event asked for: the lock's TTL is that lease raised to the floor, so the
/// value is already past and reads as lapsed, as a reaper needs it to.
fn project_lease(rec: &mut LifecycleRecord, lock: Option<&Item>) {
    if rec.row.state != BeadState::Working {
        return;
    }
    if let Some(ms) = lock.and_then(|l| l.lock_expires_at_ms) {
        rec.row.lease_until = Some(ms.div_ceil(1_000) as i64);
    }
}

impl<B: Backend> Store<B> {
    pub fn new(contract: Contract<B>, claim_floor_ms: u64) -> Self {
        Store {
            contract,
            claim_floor_ms,
        }
    }

    pub fn contract(&self) -> &Contract<B> {
        &self.contract
    }

    /// `t` with `id`'s gate put when the transition from `prev` to `next` moves it, or when the
    /// record has none yet; no other transition writes it.
    fn gated(
        &self,
        t: Txn,
        id: &str,
        prev: Option<&LifecycleRecord>,
        next: &LifecycleRecord,
    ) -> Result<Txn> {
        let gate = next.gate();
        let unmoved = prev.map(LifecycleRecord::gate).as_ref() == Some(&gate);
        if unmoved && self.contract.get(&keys::gate(id))?.is_some() {
            return Ok(t);
        }
        Ok(t.put(&keys::gate(id), gate_object(id, &gate)?))
    }

    fn read<T: for<'de> serde::Deserialize<'de>>(
        &self,
        key: &str,
        at: Option<u64>,
    ) -> Result<Option<(Read<T>, Item)>> {
        let item = match at {
            Some(rev) => self.contract.get_at(key, rev)?,
            None => self.contract.get(key)?,
        };
        item.map(|i| {
            let value = decode(key, &i.value)?;
            Ok((
                Read {
                    value,
                    mod_rev: i.mod_rev,
                    create_rev: i.create_rev,
                },
                i,
            ))
        })
        .transpose()
    }

    pub fn record(&self, id: &str) -> Result<Option<Read<LifecycleRecord>>> {
        let rev = self.contract.head()?;
        self.record_at(id, rev)
    }

    /// The record at `rev`, its `lease_until` projected from its claim's lock at that rev. A
    /// lock's expiry is only known for the lock now held, so a record read at a rev whose lock
    /// has since gone or been retaken keeps its claim's own lease.
    pub fn record_at(&self, id: &str, rev: u64) -> Result<Option<Read<LifecycleRecord>>> {
        let Some((mut r, _)) = self.read::<LifecycleRecord>(&keys::lifecycle(id), Some(rev))?
        else {
            return Ok(None);
        };
        let lock_key = keys::claim_lock(id);
        let then = self.contract.get_at(&lock_key, rev)?;
        let now = self.contract.get(&lock_key)?.map(|l| l.create_rev);
        let lock = then.filter(|l| Some(l.create_rev) == now);
        project_lease(&mut r.value, lock.as_ref());
        Ok(Some(r))
    }

    /// Every lifecycle record, from one snapshot, with the revision it was read at.
    pub fn records(&self) -> Result<(Vec<Read<LifecycleRecord>>, u64)> {
        let (records, _, rev) = self.records_with_readiness()?;
        Ok((records, rev))
    }

    /// Every lifecycle record and every readiness, from one snapshot, with its revision.
    pub fn records_with_readiness(&self) -> Result<Snapshot> {
        let locks = keys::claim_locks();
        let (items, rev) = self.contract.list(
            &[keys::LIFECYCLE, locks.as_str(), keys::READINESS],
            &state_store::Selector::Everything,
        )?;
        let readiness = items
            .iter()
            .filter_map(|i| Some((keys::id_of(keys::READINESS, &i.key)?, i)))
            .map(|(id, i)| Ok((id.to_string(), decode(&i.key, &i.value)?)))
            .collect::<Result<_>>()?;
        let held: std::collections::BTreeMap<&str, &Item> = items
            .iter()
            .filter_map(|i| Some((keys::id_of(&locks, &i.key)?, i)))
            .collect();
        let records = items
            .iter()
            .filter_map(|i| Some((keys::id_of(keys::LIFECYCLE, &i.key)?, i)))
            .map(|(id, i)| {
                let mut value = decode(&i.key, &i.value)?;
                project_lease(&mut value, held.get(id).copied());
                Ok(Read {
                    value,
                    mod_rev: i.mod_rev,
                    create_rev: i.create_rev,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok((records, readiness, rev))
    }

    /// The fence of the claim now holding `id`: its lock, while it is live.
    pub fn holder_fence(&self, id: &str) -> Result<Option<Fence>> {
        Ok(self
            .contract
            .get(&keys::claim_lock(id))?
            .filter(|l| l.lock_expires_at_ms.is_some())
            .map(|l| Fence {
                resource: keys::claim_resource(id),
                token: l.create_rev,
            }))
    }

    pub fn readiness(&self, id: &str) -> Result<Option<Read<Readiness>>> {
        Ok(self.read(&keys::readiness(id), None)?.map(|(r, _)| r))
    }

    /// `true` when an equal record already existed; `Conflict` when a different one does. A
    /// tip promotes an untouched READY birth to SUBMITTED, as `create-bead --submitted-tip` does.
    pub fn create(
        &self,
        id: &str,
        repo: Option<&str>,
        submitted_tip: Option<&str>,
        at: i64,
        req: &Request,
    ) -> Result<bool> {
        let born = LifecycleRecord::born(id, repo, submitted_tip, at);
        self.birth(id, born, submitted_tip, req)
    }

    /// A record for a bead whose filing is still under way: born OPEN, so no claim takes it
    /// before [`Self::promote`]. Answers as [`Self::create`] does.
    pub fn create_open(
        &self,
        id: &str,
        repo: Option<&str>,
        at: i64,
        req: &Request,
    ) -> Result<bool> {
        self.birth(id, LifecycleRecord::born_open(id, repo, at), None, req)
    }

    /// Filing has finished: the OPEN record becomes READY. An already-promoted record is left.
    pub fn promote(&self, id: &str, actor: &str, at: i64, req: &Request) -> Result<Outcome> {
        let Some(cur) = self.record(id)? else {
            return Err(Error::NotFound {
                key: keys::lifecycle(id),
            });
        };
        if cur.value.row.state != BeadState::Open {
            return Ok(Outcome::Applied {
                rev: self.contract.head()?,
                record: Box::new(cur.value),
            });
        }
        let ev = BeadEvent {
            expect: BeadState::Open,
            version: cur.value.row.version,
            kind: BeadEventKind::Promote,
            actor: actor.into(),
            at: Some(at),
        };
        self.apply(id, &ev, req)
    }

    fn birth(
        &self,
        id: &str,
        born: LifecycleRecord,
        submitted_tip: Option<&str>,
        req: &Request,
    ) -> Result<bool> {
        let key = keys::lifecycle(id);
        if let Some(existed) = self.compare_birth(&key, &born, submitted_tip, req)? {
            return Ok(existed);
        }
        let mut audits = Vec::new();
        if let Some(tip) = submitted_tip {
            audits.push(born_submit_audit(id, tip, born.updated_at, &req.actor));
        }
        let value = Object::new(encode(&born)?).with_finalizers(&[keys::PLANNING_CLOSED]);
        // A birth with no audit has no history entry, and `create` writes nothing on a lost race.
        let t = Txn::new()
            .when(Compare::ModRev(key.clone(), None))
            .put(&key, value);
        let t = self.gated(t, id, None, &born)?;
        if self.audited_txn(t, &audits, &[], req, "create")?.succeeded {
            return Ok(false);
        }
        // Another create won between the read and the write, stamped at another second.
        self.compare_birth(&key, &born, submitted_tip, req)?
            .ok_or_else(|| Error::Backend(format!("{key} vanished during its create")))
    }

    /// Commits `t` with each branch's audit as its evidence and, on that branch, a history
    /// entry for every id the audit names.
    fn audited_txn(
        &self,
        mut t: Txn,
        then: &[Audit],
        els: &[Audit],
        req: &Request,
        part: &str,
    ) -> Result<state_store::TxnResult> {
        let sub = sub_request(req, part, evidence(then, els));
        let planned_at = self.contract.head()?;
        let entry = |id: &str| keys::history_entry(id, planned_at, &sub.id);
        for id in audited_ids(then) {
            t = t.put(&entry(id), Object::new(serde_json::json!({})));
        }
        for id in audited_ids(els) {
            t = t.or_put(&entry(id), Object::new(serde_json::json!({})));
        }
        self.contract.txn(&t, &sub)
    }

    /// `None` when there is no record; otherwise the answer `create` gives against it.
    fn compare_birth(
        &self,
        key: &str,
        born: &LifecycleRecord,
        submitted_tip: Option<&str>,
        req: &Request,
    ) -> Result<Option<bool>> {
        let Some((cur, item)) = self.read::<LifecycleRecord>(key, None)? else {
            return Ok(None);
        };
        let conflict = || Error::Conflict {
            key: key.into(),
            existing: Box::new(item.clone()),
        };
        if item.value.metadata.deletion_timestamp.is_some() {
            return Err(conflict());
        }
        if cur.value.same_birth(born) {
            return Ok(Some(true));
        }
        let (id, repo, at) = (&born.row.bead_id, born.repo.as_deref(), born.updated_at);
        let untouched = [
            LifecycleRecord::born(id, repo, None, at),
            LifecycleRecord::born_open(id, repo, at),
        ];
        if submitted_tip.is_some() && untouched.iter().any(|u| cur.value.same_birth(u)) {
            self.promote_born(key, &item, &cur, born.clone(), req)?;
            return Ok(Some(false));
        }
        Err(conflict())
    }

    fn promote_born(
        &self,
        key: &str,
        item: &Item,
        cur: &Read<LifecycleRecord>,
        born: LifecycleRecord,
        req: &Request,
    ) -> Result<()> {
        let tip = born.row.tip.clone().unwrap_or_default();
        let audits = [born_submit_audit(
            &born.row.bead_id,
            &tip,
            born.updated_at,
            &req.actor,
        )];
        let t = Txn::new()
            .when(Compare::ModRev(key.into(), Some(cur.mod_rev)))
            .put(key, object_like(item, encode(&born)?));
        let t = self.gated(t, &born.row.bead_id, Some(&cur.value), &born)?;
        let r = self.audited_txn(t, &audits, &[], req, "promote")?;
        if r.succeeded {
            Ok(())
        } else {
            Err(Error::Conflict {
                key: key.into(),
                existing: Box::new(item.clone()),
            })
        }
    }

    /// An event no lock guards: the operator's, a reaper's, a cascade's. A claim goes through
    /// [`Self::claim`], so no record reaches WORKING without its lock.
    pub fn apply(&self, id: &str, ev: &BeadEvent, req: &Request) -> Result<Outcome> {
        if matches!(ev.kind, BeadEventKind::Claim { .. }) {
            return Err(Error::Backend(format!("a claim of {id} takes its lock")));
        }
        self.transition(id, ev, None, req)
    }

    /// The outcome of a request already applied, read at its own revision.
    fn replayed(&self, id: &str, req: &Request) -> Result<Option<Outcome>> {
        let applied = state_store::request_key(&format!("{}/apply", req.id));
        let Some(r) = self.contract.get(&applied)? else {
            return Ok(None);
        };
        let rev = r.value.data["main"].as_u64().unwrap_or(0);
        if r.value.data["succeeded"].as_bool() != Some(true) {
            return Ok(None);
        }
        let record = self.record_at(id, rev)?.ok_or_else(|| Error::NotFound {
            key: keys::lifecycle(id),
        })?;
        Ok(Some(Outcome::Applied {
            rev,
            record: Box::new(record.value),
        }))
    }

    /// A claim is one txn: it compares the record's `mod_rev`, takes `locks/lifecycle/<id>`, and
    /// writes the record. The holder's fencing token is that lock's `create_rev`, returned as
    /// the txn's first fence. It also compares every blocker's gate as its readiness judged it
    /// and each stacked prerequisite at its stacked tip, so a blocker that moved since conflicts
    /// the claim. A lost compare or a live lock writes nothing, so no lock is left
    /// behind to give back.
    pub fn claim(
        &self,
        id: &str,
        ev: &BeadEvent,
        req: &Request,
    ) -> Result<(Outcome, Option<Fence>)> {
        let BeadEventKind::Claim { lease_until, .. } = &ev.kind else {
            return Err(Error::Backend(format!(
                "claim of {id} with a {:?} event",
                ev.kind
            )));
        };
        let Some(at) = ev.at else {
            return Err(Error::Backend(format!(
                "a claim of {id} carries no at to time its lease from"
            )));
        };
        let ttl_ms = self.lease_ttl_ms(*lease_until, at);
        let resource = keys::claim_resource(id);
        let apply_req = |evidence| sub_request(req, "apply", evidence);
        if let Some(done) = self.replayed(id, req)? {
            // The contract answers the retry from requests/<id>, rebuilding the fence from the
            // original commit's events key; the txn's body is not run again. A fence whose lock
            // has since lapsed or gone is not handed back as if it still held.
            let r = self.contract.txn(
                &Txn::new().lock_acquire(&resource, ttl_ms),
                &apply_req(Value::Null),
            )?;
            let live = self.holder_fence(id)?;
            let fence = r
                .fences
                .into_iter()
                .next()
                .filter(|f| live.as_ref() == Some(f));
            return Ok((done, fence));
        }
        let key = keys::lifecycle(id);
        let Some((cur, item)) = self.read::<LifecycleRecord>(&key, None)? else {
            return Err(Error::NotFound { key });
        };
        let out = bead::apply(&cur.value.row, ev);
        if !out.applied {
            return self
                .refuse(id, ev, &cur.value, out.refusal, req)
                .map(|o| (o, None));
        }
        let next = self.next_record(ev, &cur, out.row);
        let (mut applied, lost) = self.audits(id, ev, &cur.value, &next);
        let judged = self.readiness(id)?.map(|r| r.value);
        if let Some(j) = judged
            .as_ref()
            .filter(|j| j.judged_for_claim && !j.claimable)
        {
            let blocked_by = j.blocked_by.clone();
            return Ok((Outcome::JudgedUnclaimable { blocked_by }, None));
        }
        let edges = claim_edges(judged.as_ref(), &next.row.stack);
        if let Some(j) = &judged {
            if let Some(Value::Object(claim)) = applied[0].evidence.get_mut("Claim") {
                claim.insert("judged".into(), judged_evidence(j)?);
            }
        }
        let mut t = Txn::new().when(Compare::ModRev(key.clone(), Some(cur.mod_rev)));
        for (blocker, gate) in &edges {
            t = t.when(gate_compare(blocker, gate.as_ref())?);
        }
        let t = t
            .lock_acquire(&resource, ttl_ms)
            .put(&key, object_like(&item, encode(&next)?));
        let t = self.gated(t, id, Some(&cur.value), &next)?;
        let r = self.audited_txn(t, &applied, &[lost], req, "apply")?;
        if r.succeeded {
            let fence = r.fences.into_iter().next();
            let record = self.record_at(id, r.rev)?.map_or(next, |r| r.value);
            return Ok((
                Outcome::Applied {
                    rev: r.rev,
                    record: Box::new(record),
                },
                fence,
            ));
        }
        // A request that already lost is spent: its answer is final whatever has moved since,
        // and the claimant tries again under a new request id.
        if r.already_applied || self.contract.get(&key)?.map(|i| i.mod_rev) != Some(cur.mod_rev) {
            return Ok((Outcome::LostRace { rev: r.rev }, None));
        }
        for (blocker, gate) in &edges {
            let now = self.contract.get(&keys::gate(blocker))?.map(|i| i.value);
            if now != gate.as_ref().map(|g| gate_object(blocker, g)).transpose()? {
                let blocker = blocker.clone();
                return Ok((
                    Outcome::BlockerMoved {
                        rev: r.rev,
                        blocker,
                    },
                    None,
                ));
            }
        }
        Ok(match self.holder_fence(id)? {
            Some(live) => (Outcome::Locked { token: live.token }, None),
            // The lock lapsed or went between the txn and this read: retry as on a race.
            None => (Outcome::LostRace { rev: r.rev }, None),
        })
    }

    /// The claim lock's TTL for a lease running to `lease_until` from `at`, raised to the floor.
    fn lease_ttl_ms(&self, lease_until: i64, at: i64) -> u64 {
        (lease_until.saturating_sub(at).max(0) as u64)
            .saturating_mul(1_000)
            .max(self.claim_floor_ms)
    }

    /// The holder's renewal: when the machine would take the Renew, the lock restarts under the
    /// new lease's TTL, then the event is written fenced on it, so a lapsed or superseded
    /// holder extends nothing. A refusal is written as the machine's, the lock untouched.
    pub fn renew_lease(
        &self,
        id: &str,
        fence: &Fence,
        ev: &BeadEvent,
        req: &Request,
    ) -> Result<Outcome> {
        let (BeadEventKind::Renew { lease_until }, Some(at)) = (&ev.kind, ev.at) else {
            return Err(Error::Backend(format!(
                "a renewal of {id} takes a Renew event with an at"
            )));
        };
        if self.replayed(id, req)?.is_none() {
            let Some((cur, _)) = self.read::<LifecycleRecord>(&keys::lifecycle(id), None)? else {
                return Err(Error::NotFound {
                    key: keys::lifecycle(id),
                });
            };
            if bead::apply(&cur.value.row, ev).applied {
                let ttl_ms = self.lease_ttl_ms(*lease_until, at);
                match self.contract.renew_for(
                    fence,
                    ttl_ms,
                    &sub_request(req, "renew", Value::Null),
                ) {
                    Err(Error::LockLost { .. }) => {
                        return Ok(Outcome::LockLost {
                            rev: self.contract.head()?,
                        })
                    }
                    other => other?,
                }
            }
        }
        self.holder_apply(id, fence, ev, req)
    }

    /// The machine's refusal of `ev` for a reason only the caller can judge, audited as one.
    pub fn refuse_event(
        &self,
        id: &str,
        ev: &BeadEvent,
        refusal: lifecycle::Refusal,
        req: &Request,
    ) -> Result<Outcome> {
        let Some((cur, _)) = self.read::<LifecycleRecord>(&keys::lifecycle(id), None)? else {
            return Err(Error::NotFound {
                key: keys::lifecycle(id),
            });
        };
        self.refuse(id, ev, &cur.value, Some(refusal), req)
    }

    /// An event from the holder, fenced on its lock.
    pub fn holder_apply(
        &self,
        id: &str,
        fence: &Fence,
        ev: &BeadEvent,
        req: &Request,
    ) -> Result<Outcome> {
        self.transition(id, ev, Some(fence), req)
    }

    /// The holder's lease, extended on the lock alone: the record is not written.
    pub fn renew(&self, fence: &Fence, req: &Request) -> Result<()> {
        self.contract
            .renew(fence, &sub_request(req, "renew", Value::Null))
    }

    fn transition(
        &self,
        id: &str,
        ev: &BeadEvent,
        fence: Option<&Fence>,
        req: &Request,
    ) -> Result<Outcome> {
        if let Some(done) = self.replayed(id, req)? {
            return Ok(done);
        }
        let Some((cur, item)) = self.read::<LifecycleRecord>(&keys::lifecycle(id), None)? else {
            return Err(Error::NotFound {
                key: keys::lifecycle(id),
            });
        };
        // The claim's lock as this read saw it, so a release below never takes a later claim's.
        let held = self.contract.get(&keys::claim_lock(id))?.map(|l| Fence {
            resource: keys::claim_resource(id),
            token: l.create_rev,
        });
        if let Some(f) = fence {
            let live = self.holder_fence(id)?;
            if live.as_ref() != Some(f) {
                return Ok(Outcome::LockLost {
                    rev: self.contract.head()?,
                });
            }
        }
        let out = bead::apply(&cur.value.row, ev);
        if !out.applied {
            return self.refuse(id, ev, &cur.value, out.refusal, req);
        }
        let leaves = out.row.state != BeadState::Working;
        let outcome = self.write(id, ev, &cur, &item, out.row, fence, req)?;
        // Whoever moved the record out of WORKING, the claim's lock goes with it. A lock
        // already gone needs no release, and one left behind expires on its TTL.
        if let (Outcome::Applied { .. }, true, Some(held)) = (&outcome, leaves, held) {
            let _ = self
                .contract
                .release(&held, &sub_request(req, "release", Value::Null));
        }
        Ok(outcome)
    }

    fn next_record(
        &self,
        ev: &BeadEvent,
        cur: &Read<LifecycleRecord>,
        row: lifecycle::bead::BeadRow,
    ) -> LifecycleRecord {
        LifecycleRecord {
            row,
            updated_at: ev.at.unwrap_or(cur.value.updated_at),
            ..cur.value.clone()
        }
    }

    /// The audit of each branch: the transition applied, or its compare lost as a stale read.
    fn audits(
        &self,
        id: &str,
        ev: &BeadEvent,
        cur: &LifecycleRecord,
        next: &LifecycleRecord,
    ) -> ([Audit; 1], Audit) {
        let from = cur.row.state;
        let applied = [audit(id, ev, from, next.row.state, None)];
        let mut lost = audit(id, ev, from, from, None);
        lost.applied = false;
        lost.expect = ev.expect.as_str().into();
        lost.refusal = Some("StaleVersion".into());
        (applied, lost)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn write(
        &self,
        id: &str,
        ev: &BeadEvent,
        cur: &Read<LifecycleRecord>,
        item: &Item,
        row: lifecycle::bead::BeadRow,
        fence: Option<&Fence>,
        req: &Request,
    ) -> Result<Outcome> {
        let key = keys::lifecycle(id);
        let next = self.next_record(ev, cur, row);
        let (applied, lost) = self.audits(id, ev, &cur.value, &next);
        let t = Txn::new()
            .when(Compare::ModRev(key.clone(), Some(cur.mod_rev)))
            .put(&key, object_like(item, encode(&next)?));
        let mut t = self.gated(t, id, Some(&cur.value), &next)?;
        if let Some(f) = fence {
            t = t.lock_check(f);
        }
        let r = self.audited_txn(t, &applied, &[lost], req, "apply")?;
        if r.succeeded {
            let record = self.record_at(id, r.rev)?.map_or(next, |r| r.value);
            return Ok(Outcome::Applied {
                rev: r.rev,
                record: Box::new(record),
            });
        }
        let moved = self.contract.get(&key)?.map(|i| i.mod_rev) != Some(cur.mod_rev);
        Ok(if fence.is_some() && !moved {
            Outcome::LockLost { rev: r.rev }
        } else {
            Outcome::LostRace { rev: r.rev }
        })
    }

    fn refuse(
        &self,
        id: &str,
        ev: &BeadEvent,
        cur: &LifecycleRecord,
        refusal: Option<lifecycle::Refusal>,
        req: &Request,
    ) -> Result<Outcome> {
        let refusal = refusal.unwrap_or(lifecycle::Refusal::IllegalTransition {
            state: cur.row.state.as_str().into(),
            event: format!("{:?}", ev.kind),
        });
        let audits = [audit(id, ev, cur.row.state, cur.row.state, Some(&refusal))];
        let r = self.audited_txn(Txn::new(), &audits, &[], req, "refuse")?;
        Ok(Outcome::Refused {
            rev: r.rev,
            refusal,
        })
    }

    /// One dispatcher pass onto `readiness/<id>`, keeping what other publishers wrote there.
    pub fn publish_verdicts(
        &self,
        verdicts: &[Verdict],
        judged_at: &str,
        req: &Request,
    ) -> Result<u64> {
        self.merge_readiness(
            verdicts,
            |v| &v.id,
            "verdicts",
            req,
            |r, v, head| {
                r.verdict = Some(v.verdict.clone());
                r.blocker = v.blocker.clone();
                r.runes = v.runes.clone();
                r.judged_at = Some(judged_at.to_string());
                if !r.judged_for_claim {
                    r.judged_at_rev = head;
                }
            },
        )
    }

    /// The running pass's counts, under the batch's own progress key, which goes with the batch.
    pub fn set_progress(
        &self,
        repo: &str,
        batch_id: &str,
        progress: Value,
        req: &Request,
    ) -> Result<u64> {
        if !progress.is_object() {
            return Err(Error::Backend(format!(
                "progress of {batch_id} must be one JSON object"
            )));
        }
        let batch = keys::batch(repo, batch_id);
        let Some(b) = self.contract.get(&batch)? else {
            return Err(Error::NotFound { key: batch });
        };
        let key = keys::batch_progress(repo, batch_id);
        let value = match self.contract.get(&key)? {
            Some(prior) => object_like(&prior, progress),
            None => Object::new(progress).with_owners(&[batch.as_str()]),
        };
        let t = Txn::new()
            .when(Compare::CreateRev(batch.clone(), Some(b.create_rev)))
            .put(&key, value);
        let r = self
            .contract
            .txn(&t, &sub_request(req, "progress", Value::Null))?;
        if !r.succeeded {
            return Err(Error::NotFound { key: batch });
        }
        Ok(r.rev)
    }

    /// Removes the record, or, while the planning plane still has its bead open, only marks it:
    /// the GC controller removes it once [`Self::planning_closed`] clears the finalizer. A
    /// record already gone is removed, as today's `DELETE` of no row is.
    pub fn delete(&self, id: &str, bead_open: bool, req: &Request) -> Result<Deleted> {
        let key = keys::lifecycle(id);
        if self.contract.get(&key)?.is_none() {
            return Ok(Deleted::Removed {
                rev: self.contract.head()?,
            });
        }
        if !bead_open {
            self.planning_closed(id, req)?;
        }
        match self
            .contract
            .delete(&key, None, &sub_request(req, "delete", Value::Null))
        {
            Err(Error::NotFound { .. }) => Ok(Deleted::Removed {
                rev: self.contract.head()?,
            }),
            other => other,
        }
    }

    /// The planning plane has closed the bead: its record no longer waits on it. `false` when
    /// the record carries no such finalizer.
    pub fn planning_closed(&self, id: &str, req: &Request) -> Result<bool> {
        let key = keys::lifecycle(id);
        for attempt in 0..MERGE_ATTEMPTS {
            let Some(item) = self.contract.get(&key)? else {
                return Err(Error::NotFound { key });
            };
            let mut value = item.value.clone();
            value
                .metadata
                .finalizers
                .retain(|f| f != keys::PLANNING_CLOSED);
            if value.metadata.finalizers.len() == item.value.metadata.finalizers.len() {
                return Ok(false);
            }
            let t = Txn::new()
                .when(Compare::ModRev(key.clone(), Some(item.mod_rev)))
                .put(&key, value);
            let sub = sub_request(req, &format!("planning-closed/{attempt}"), Value::Null);
            if self.contract.txn(&t, &sub)?.succeeded {
                return Ok(true);
            }
        }
        Err(Error::Backend(format!(
            "{key}: clearing {} lost {MERGE_ATTEMPTS} races in a row",
            keys::PLANNING_CLOSED
        )))
    }
}

fn born_submit_audit(id: &str, tip: &str, at: i64, actor: &str) -> Audit {
    Audit {
        machine: "bead".into(),
        lc_key: id.into(),
        event: "Submit".into(),
        expect: "READY".into(),
        from_state: "READY".into(),
        to_state: "SUBMITTED".into(),
        applied: true,
        refusal: None,
        evidence: serde_json::json!({ "tip": tip }),
        actor: actor.into(),
        at,
    }
}

mod asks;
mod direct;
mod mending;
mod readiness;
mod rounds;
mod sweeps;

pub use asks::{AskClose, AskRecord};
pub use direct::{Edge, RepoSet};
pub use mending::{MendRecord, MendingRecord, Move, Stepped};
pub use readiness::{Judged, Pass};
pub use rounds::{Cut, Ran, Turned, Withdrawn};
pub use sweeps::Filing;

#[cfg(test)]
mod tests;
