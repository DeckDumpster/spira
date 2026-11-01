//! Lifecycle records on the state-store contract. Each applied transition is one txn that
//! compares the record's `mod_rev`; its audit rides into `events/<rev>` as the request's
//! evidence, refusals included. A claim is the `locks/bead/<id>` lock, whose fencing token the
//! record carries, and every write by the holder is fenced on it.

use lifecycle::bead::{self, BeadEvent, BeadEventKind, BeadState};
use serde_json::Value;
use state_store::{
    Backend, Compare, Contract, Deleted, Error, Fence, Item, Object, Request, Result, Rev, Txn,
};

use super::keys;
use super::records::{decode, encode, Audit, LifecycleRecord, Readiness};

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
    LostRace {
        rev: u64,
    },
    /// The holder's lock expired or is held under another token.
    LockLost {
        rev: u64,
    },
    /// Another claim's lock is still live.
    Locked {
        token: Rev,
    },
}

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
        Ok(self.read(&keys::lifecycle(id), None)?.map(|(r, _)| r))
    }

    pub fn record_at(&self, id: &str, rev: u64) -> Result<Option<Read<LifecycleRecord>>> {
        Ok(self.read(&keys::lifecycle(id), Some(rev))?.map(|(r, _)| r))
    }

    /// Every lifecycle record, from one snapshot, with the revision it was read at.
    pub fn records(&self) -> Result<(Vec<Read<LifecycleRecord>>, u64)> {
        let (items, rev) = self
            .contract
            .list(&[keys::LIFECYCLE], &state_store::Selector::Everything)?;
        let records = items
            .iter()
            .filter(|i| keys::id_of(keys::LIFECYCLE, &i.key).is_some())
            .map(|i| {
                Ok(Read {
                    value: decode(&i.key, &i.value)?,
                    mod_rev: i.mod_rev,
                    create_rev: i.create_rev,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok((records, rev))
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
        let key = keys::lifecycle(id);
        let born = LifecycleRecord::born(id, repo, submitted_tip, at);
        if let Some(existed) = self.compare_birth(&key, &born, submitted_tip, req)? {
            return Ok(existed);
        }
        let mut audits = Vec::new();
        if let Some(tip) = submitted_tip {
            audits.push(born_submit_audit(id, tip, at, &req.actor));
        }
        let value = Object::new(encode(&born)?).with_finalizers(&[keys::PLANNING_CLOSED]);
        let sub = sub_request(req, "create", evidence(&audits, &[]));
        match self.contract.create(&key, value, &sub) {
            Ok(c) => Ok(c.existed),
            // Another create won between the read and the write, stamped at another second.
            Err(Error::Conflict { .. }) => self
                .compare_birth(&key, &born, submitted_tip, req)?
                .ok_or_else(|| Error::Backend(format!("{key} vanished during its create"))),
            Err(e) => Err(e),
        }
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
        let untouched = LifecycleRecord::born(
            &born.row.bead_id,
            born.repo.as_deref(),
            None,
            born.updated_at,
        );
        if submitted_tip.is_some() && cur.value.same_birth(&untouched) {
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
        let r = self
            .contract
            .txn(&t, &sub_request(req, "promote", evidence(&audits, &[])))?;
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

    /// A claim takes the lock and then writes the record, fenced on the lock, with its token: two
    /// commits, because the contract takes a lock only in a call of its own. A refused or lost
    /// claim gives the lock back; a crash between the two leaves a lock that expires on its TTL.
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
        let ttl_ms = (lease_until.saturating_sub(at).max(0) as u64)
            .saturating_mul(1_000)
            .max(self.claim_floor_ms);
        let lock_req = sub_request(req, "lock", Value::Null);
        if let Some(done) = self.replayed(id, req)? {
            let fence = self
                .contract
                .lock(&keys::claim_resource(id), ttl_ms, &lock_req)?;
            return Ok((done, Some(fence)));
        }
        let Some((cur, item)) = self.read::<LifecycleRecord>(&keys::lifecycle(id), None)? else {
            return Err(Error::NotFound {
                key: keys::lifecycle(id),
            });
        };
        let out = bead::apply(&cur.value.row, ev);
        if !out.applied {
            return self
                .refuse(id, ev, &cur.value, out.refusal, req)
                .map(|o| (o, None));
        }
        let fence = match self
            .contract
            .lock(&keys::claim_resource(id), ttl_ms, &lock_req)
        {
            Ok(f) => f,
            Err(Error::Locked { token, .. }) => return Ok((Outcome::Locked { token }, None)),
            Err(e) => return Err(e),
        };
        let outcome = self.write(
            id,
            ev,
            &cur,
            &item,
            out.row,
            Some(&fence),
            Some(fence.token),
            req,
        )?;
        if !matches!(outcome, Outcome::Applied { .. }) {
            self.give_back(&fence, req);
            return Ok((outcome, None));
        }
        Ok((outcome, Some(fence)))
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

    fn give_back(&self, fence: &Fence, req: &Request) {
        // A lock already gone needs no release, and one left behind expires on its TTL.
        let _ = self
            .contract
            .release(fence, &sub_request(req, "release", Value::Null));
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
        if let Some(f) = fence {
            if cur.value.claim_token != Some(f.token) {
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
        let token = if leaves { None } else { cur.value.claim_token };
        let outcome = self.write(id, ev, &cur, &item, out.row, fence, token, req)?;
        // Whoever moved the record out of WORKING, the claim's lock goes with it.
        if let (Outcome::Applied { .. }, true, Some(token)) =
            (&outcome, leaves, cur.value.claim_token)
        {
            let held = Fence {
                resource: keys::claim_resource(id),
                token,
            };
            self.give_back(&held, req);
        }
        Ok(outcome)
    }

    #[allow(clippy::too_many_arguments)]
    fn write(
        &self,
        id: &str,
        ev: &BeadEvent,
        cur: &Read<LifecycleRecord>,
        item: &Item,
        row: lifecycle::bead::BeadRow,
        fence: Option<&Fence>,
        token: Option<Rev>,
        req: &Request,
    ) -> Result<Outcome> {
        let key = keys::lifecycle(id);
        let next = LifecycleRecord {
            row,
            updated_at: ev.at.unwrap_or(cur.value.updated_at),
            claim_token: token,
            ..cur.value.clone()
        };
        let from = cur.value.row.state;
        let applied = [audit(id, ev, from, next.row.state, None)];
        let mut lost = audit(id, ev, from, from, None);
        lost.applied = false;
        lost.expect = ev.expect.as_str().into();
        lost.refusal = Some("StaleVersion".into());
        let mut t = Txn::new()
            .when(Compare::ModRev(key.clone(), Some(cur.mod_rev)))
            .put(&key, object_like(item, encode(&next)?));
        if let Some(f) = fence {
            t = t.fenced(f);
        }
        let r = self
            .contract
            .txn(&t, &sub_request(req, "apply", evidence(&applied, &[lost])))?;
        if r.succeeded {
            return Ok(Outcome::Applied {
                rev: r.rev,
                record: Box::new(next),
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
        let r = self.contract.txn(
            &Txn::new(),
            &sub_request(req, "refuse", evidence(&audits, &[])),
        )?;
        Ok(Outcome::Refused {
            rev: r.rev,
            refusal,
        })
    }

    /// One dispatcher pass onto `readiness/<id>`, keeping what other publishers wrote there:
    /// each put compares the readiness it merged into, and a chunk that lost a race is merged
    /// again. A record that does not exist gets no readiness.
    pub fn publish_verdicts(
        &self,
        verdicts: &[Verdict],
        judged_at: &str,
        req: &Request,
    ) -> Result<u64> {
        let mut rev = self.contract.head()?;
        for (n, chunk) in verdicts.chunks(PASS_CHUNK).enumerate() {
            rev = self.publish_chunk(chunk, judged_at, &format!("verdicts/{n}"), req)?;
        }
        Ok(rev)
    }

    fn publish_chunk(
        &self,
        chunk: &[Verdict],
        judged_at: &str,
        part: &str,
        req: &Request,
    ) -> Result<u64> {
        for attempt in 0..MERGE_ATTEMPTS {
            let head = self.contract.head()?;
            let mut t = Txn::new();
            for v in chunk {
                let owner = keys::lifecycle(&v.id);
                if self.contract.get_at(&owner, head)?.is_none() {
                    continue;
                }
                let key = keys::readiness(&v.id);
                let prior = self.read::<Readiness>(&key, Some(head))?;
                t = t.when(Compare::ModRev(
                    key.clone(),
                    prior.as_ref().map(|(r, _)| r.mod_rev),
                ));
                let (mut r, value) = match prior {
                    Some((r, item)) => (r.value, item.value),
                    None => (
                        Readiness::default(),
                        Object::default().with_owners(&[owner.as_str()]),
                    ),
                };
                r.verdict = Some(v.verdict.clone());
                r.blocker = v.blocker.clone();
                r.runes = v.runes.clone();
                r.judged_at = Some(judged_at.to_string());
                r.judged_at_rev = head;
                t = t.put(
                    &key,
                    Object {
                        data: encode(&r)?,
                        ..value
                    },
                );
            }
            if t.ops.is_empty() {
                return Ok(head);
            }
            let sub = sub_request(req, &format!("{part}/{attempt}"), Value::Null);
            let r = self.contract.txn(&t, &sub)?;
            if r.succeeded {
                return Ok(r.rev);
            }
        }
        Err(Error::Backend(format!(
            "readiness pass {part} lost {MERGE_ATTEMPTS} races in a row"
        )))
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

#[cfg(test)]
mod tests;
