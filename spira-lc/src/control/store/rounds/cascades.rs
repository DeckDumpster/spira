//! The round's cascades past the cut: settle, abandon, eject, stack, stage and promote, and the
//! orphan sweep built on abandon. Each is one txn planned on a [`Cascade`], so a member that
//! moved since its read writes nothing anywhere.

use std::collections::{BTreeMap, BTreeSet};

use lifecycle::batch::{BatchEvent, BatchEventKind, BatchRow, BatchState};
use lifecycle::bead::{BeadEventKind, BeadState};
use lifecycle::delivery::{DeliveryEventKind, DeliveryRow, Mode};
use lifecycle::reason::ReturnedReason;
use state_store::{Backend, Request, Result};

use super::{cannot_tell, over_cap, refused, Cascade, Cut, Planned, Ran, Store};
use crate::control::keys;
use crate::control::records::{BatchRecord, LifecycleRecord, Member};

/// The members a cascade carried out of the round as collateral: `(dependent, the member it
/// was stacked on)`.
pub type Withdrawn = Vec<(String, String)>;

/// What the orphan sweep found, and with `apply`, what it did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Orphans {
    pub orphans: Vec<String>,
    pub phantom_batches: Vec<String>,
    pub abandoned: Vec<String>,
    pub requeued: Vec<String>,
    pub failed: Vec<String>,
    /// Rounds that never began but whose holder's lease is live: theirs, not the sweep's.
    pub leased: Vec<String>,
}

const FINISHED: [BatchState; 3] = [
    BatchState::Landed,
    BatchState::Settled,
    BatchState::Abandoned,
];

fn pairs(members: &[Member]) -> Vec<(String, String)> {
    members
        .iter()
        .map(|m| (m.bead_id.clone(), m.tip.clone()))
        .collect()
}

impl<B: Backend> Store<B> {
    /// The round `id` read into `c`, so the cascade compares it; `None` when there is none.
    fn round(&self, c: &mut Cascade, id: &str) -> Result<Option<(String, BatchRecord)>> {
        let Some((key, _)) = self.batch(id)? else {
            return Ok(None);
        };
        Ok(c.get::<B, BatchRecord>(self, &key)?.map(|r| (key, r)))
    }

    /// `ev` on the round, or the audited refusal that ends the verb.
    fn round_step(
        &self,
        c: &mut Cascade,
        key: &str,
        rec: &BatchRecord,
        ev: &BatchEvent,
        at: i64,
        req: &Request,
    ) -> Result<std::result::Result<BatchRecord, Planned>> {
        match self.batch_step(c, key, rec, ev, at)? {
            Ok(next) => Ok(Ok(next)),
            Err((audit, refusal)) => Ok(Err(Planned::Ran(self.refuse_audit(audit, refusal, req)?))),
        }
    }

    /// A member leaving its delivery: the delivery's exit and the record's, each taken only
    /// where its machine takes it, so a member already out moves nothing.
    fn exit(
        &self,
        c: &mut Cascade,
        id: &str,
        kind: DeliveryEventKind,
        actor: &str,
        at: i64,
    ) -> Result<()> {
        let bead_kind = match &kind {
            DeliveryEventKind::Returned { reason } => BeadEventKind::Returned { reason: *reason },
            DeliveryEventKind::Requeued { tip } => BeadEventKind::Requeued { tip: tip.clone() },
            other => {
                return Err(state_store::Error::Backend(format!(
                    "{id}: {other:?} is not an exit a round's cascade takes"
                )))
            }
        };
        if let Some(d) = c.get::<B, DeliveryRow>(self, &keys::delivery(id))? {
            self.delivery_step(c, id, &d, kind, actor, at)?;
        }
        let _ = self.bead_step(c, id, bead_kind, actor, at)?;
        Ok(())
    }

    /// Whether the record's last applied `Returned` follows its last `Deliver`: the queue
    /// ejected it from this delivery, whatever its record and delivery say now.
    fn returned_since_delivery(&self, id: &str) -> Result<bool> {
        let applied: Vec<String> = self
            .history(id, Some("bead"))?
            .into_iter()
            .filter(|c| c.audit.applied)
            .map(|c| c.audit.event)
            .collect();
        let last = |e: &str| applied.iter().rposition(|x| x == e);
        Ok(match (last("Returned"), last("Deliver")) {
            (Some(r), Some(d)) => r > d,
            (Some(_), None) => true,
            (None, _) => false,
        })
    }

    /// A released member's exit: an ejected one keeps its verdict, any other goes back to where
    /// it entered delivery.
    fn release_kind(&self, id: &str, tip: String) -> Result<DeliveryEventKind> {
        Ok(if self.returned_since_delivery(id)? {
            DeliveryEventKind::Returned {
                reason: ReturnedReason::BatchEjectedRed,
            }
        } else {
            DeliveryEventKind::Requeued { tip }
        })
    }

    /// The round's members stacked on any of `roots`, transitively, by declared stack or by
    /// branch containment; the roots themselves are never among them.
    fn stacked_dependents(
        &self,
        c: &mut Cascade,
        rec: &BatchRecord,
        roots: &[String],
    ) -> Result<Withdrawn> {
        let members = pairs(&rec.members);
        let tips: BTreeMap<String, String> = members.iter().cloned().collect();
        let mut stacks = Vec::new();
        for (id, _) in &members {
            if let Some(r) = c.get::<B, LifecycleRecord>(self, &keys::lifecycle(id))? {
                stacks.push((id.clone(), r.row));
            }
        }
        let contained = crate::cutover::branch_containment(&rec.row, &members);
        Ok(crate::cutover::stacked_dependents_from(
            &stacks, &tips, &contained, roots,
        ))
    }

    /// Each dependent returned as collateral, its audits naming the member it followed out.
    fn withdraw(&self, c: &mut Cascade, withdrawn: &Withdrawn, actor: &str, at: i64) -> Result<()> {
        for (id, prereq) in withdrawn {
            let kind = DeliveryEventKind::Returned {
                reason: ReturnedReason::BaseWithdrawn,
            };
            self.exit(c, id, kind, actor, at)?;
            c.name_prerequisite(id, prereq);
        }
        Ok(())
    }

    /// `settle`: `Settle` on the round; each member in `eject` returned red and each member
    /// stacked on one returned as collateral; each other member in `requeue` requeued.
    pub fn plan_settle(
        &self,
        id: &str,
        ev: &BatchEvent,
        eject: &[String],
        requeue: &[String],
        at: i64,
        req: &Request,
    ) -> Result<(Planned, Withdrawn)> {
        let mut c = Cascade::new();
        let Some((key, rec)) = self.round(&mut c, id)? else {
            return Ok((
                cannot_tell(format!("settle: no batch row for {id}")),
                vec![],
            ));
        };
        if let Err(early) = self.round_step(&mut c, &key, &rec, ev, at, req)? {
            return Ok((early, vec![]));
        }
        // Only a member is ejected, and once: an id the round never held has no delivery in it.
        let mut named: Vec<String> = Vec::new();
        for e in eject {
            if rec.members.iter().any(|m| &m.bead_id == e) && !named.contains(e) {
                named.push(e.clone());
            }
        }
        let eject = named;
        let eject = eject.as_slice();
        let withdrawn: Withdrawn = self
            .stacked_dependents(&mut c, &rec, eject)?
            .into_iter()
            .filter(|(d, _)| !eject.contains(d))
            .collect();
        for e in eject {
            let red = DeliveryEventKind::Returned {
                reason: ReturnedReason::BatchEjectedRed,
            };
            self.exit(&mut c, e, red, &ev.actor, at)?;
        }
        self.withdraw(&mut c, &withdrawn, &ev.actor, at)?;
        let carried = |r: &String| eject.contains(r) || withdrawn.iter().any(|(d, _)| d == r);
        for r in requeue.iter().filter(|r| !carried(r)) {
            let Some(m) = rec.members.iter().find(|m| &m.bead_id == r) else {
                continue;
            };
            let kind = DeliveryEventKind::Requeued { tip: m.tip.clone() };
            self.exit(&mut c, r, kind, &ev.actor, at)?;
        }
        Ok((Planned::Cascade(c), withdrawn))
    }

    /// `abandon-batch`: `Abandon` on the round and every member released.
    pub fn plan_abandon(
        &self,
        id: &str,
        ev: &BatchEvent,
        at: i64,
        req: &Request,
    ) -> Result<Planned> {
        let mut c = Cascade::new();
        let Some((key, rec)) = self.round(&mut c, id)? else {
            return Ok(cannot_tell(format!("abandon-batch: no batch row for {id}")));
        };
        if let Err(early) = self.round_step(&mut c, &key, &rec, ev, at, req)? {
            return Ok(early);
        }
        for m in &rec.members {
            let kind = self.release_kind(&m.bead_id, m.tip.clone())?;
            self.exit(&mut c, &m.bead_id, kind, &ev.actor, at)?;
        }
        Ok(Planned::Cascade(c))
    }

    /// `eject-member`: `Eject` on the round, the member requeued, and every member stacked on
    /// it returned as collateral.
    pub fn plan_eject(
        &self,
        id: &str,
        bead: &str,
        ev: &BatchEvent,
        at: i64,
        req: &Request,
    ) -> Result<(Planned, Withdrawn)> {
        let mut c = Cascade::new();
        let Some((key, rec)) = self.round(&mut c, id)? else {
            return Ok((
                cannot_tell(format!("eject-member: no batch row for {id}")),
                vec![],
            ));
        };
        let Some(tip) = rec
            .members
            .iter()
            .find(|m| m.bead_id == bead)
            .map(|m| m.tip.clone())
        else {
            return Ok((
                cannot_tell(format!("eject-member: {bead} is not a member of {id}")),
                vec![],
            ));
        };
        if let Err(early) = self.round_step(&mut c, &key, &rec, ev, at, req)? {
            return Ok((early, vec![]));
        }
        let withdrawn = self.stacked_dependents(&mut c, &rec, &[bead.to_string()])?;
        self.exit(
            &mut c,
            bead,
            DeliveryEventKind::Requeued { tip },
            &ev.actor,
            at,
        )?;
        self.withdraw(&mut c, &withdrawn, &ev.actor, at)?;
        Ok((Planned::Cascade(c), withdrawn))
    }

    /// `stack`: CERTIFIED members added to an OPEN round, each delivered into it as a cut
    /// delivers its members.
    pub fn plan_stack(
        &self,
        id: &str,
        members: &[(String, String)],
        actor: &str,
        at: i64,
    ) -> Result<Planned> {
        let mut c = Cascade::new();
        let Some((key, mut rec)) = self.round(&mut c, id)? else {
            return Ok(cannot_tell(format!("stack: no batch row for {id}")));
        };
        if rec.row.state != BatchState::Open {
            return Ok(refused(format!(
                "refused: {id} is {} not OPEN",
                rec.row.state.as_str()
            )));
        }
        if let Some(m) = members
            .iter()
            .find(|(m, _)| rec.members.iter().any(|x| &x.bead_id == m))
        {
            return Ok(cannot_tell(format!(
                "stack: {} is already a member of {id}",
                m.0
            )));
        }
        if let Some(over) = over_cap("stack", rec.members.len() + members.len()) {
            return Ok(over);
        }
        let deliveries = match self.admitted(&mut c, members, false)? {
            Ok(d) => d,
            Err(early) => return Ok(early),
        };
        if let Err(early) = self.plan_members(
            &mut c,
            &mut rec.row,
            members,
            &deliveries,
            actor,
            at,
            "stack",
        )? {
            return Ok(early);
        }
        rec.members.extend(members.iter().map(|(id, tip)| Member {
            bead_id: id.clone(),
            tip: tip.clone(),
        }));
        c.put(self, &key, &rec, &[])?;
        self.index_members(&mut c, &key, &rec.row, &rec.members[rec.members.len() - members.len()..])?;
        Ok(Planned::Cascade(c))
    }

    /// `stage`: a round born STAGED behind `cut.parent`, its members recorded at their tips
    /// and not yet delivered. One flag: whether the birth committed.
    pub fn stage(&self, cut: &Cut, req: &Request) -> Result<Ran> {
        if let Some(Planned::Ran(over)) = over_cap("stage", cut.members.len()) {
            return Ok(over);
        }
        let mut c = Cascade::new();
        let Some(key) = self.new_round(&mut c, cut.repo, cut.batch_id)? else {
            return Ok(Ran::Turned(super::Turned::CannotTell(format!(
                "stage: a batch row for {} already exists",
                cut.batch_id
            ))));
        };
        if let Err(Planned::Ran(early)) = self.admit_records(&mut c, cut.members, true)? {
            return Ok(early);
        }
        let parent = cut.parent.unwrap_or_default();
        let rec = BatchRecord {
            row: BatchRow::staged(cut.batch_id, cut.repo, cut.head, cut.base, parent),
            members: cut
                .members
                .iter()
                .map(|(id, tip)| Member {
                    bead_id: id.clone(),
                    tip: tip.clone(),
                })
                .collect(),
            opened_at: cut.at,
            round: Default::default(),
        };
        self.put_round(&mut c, &key, &rec)?;
        let r = self.audited_txn(c.txn(), &[], &[], req, "stage")?;
        Ok(Ran::Done {
            rev: r.rev,
            applied: vec![r.succeeded],
        })
    }

    /// `promote`: STAGED to OPEN at `head` and `base`, and every staged member, still at its
    /// staged tip, delivered into the round as a cut delivers it.
    #[allow(clippy::too_many_arguments)]
    pub fn plan_promote(
        &self,
        id: &str,
        head: &str,
        base: &str,
        actor: &str,
        at: i64,
        req: &Request,
    ) -> Result<Planned> {
        let mut c = Cascade::new();
        let Some((key, rec)) = self.round(&mut c, id)? else {
            return Ok(cannot_tell(format!("promote: no batch row for {id}")));
        };
        if rec.row.state != BatchState::Staged {
            return Ok(refused(format!(
                "refused: {id} is {} not STAGED",
                rec.row.state.as_str()
            )));
        }
        let members = pairs(&rec.members);
        let deliveries = match self.admitted(&mut c, &members, true)? {
            Ok(d) => d,
            Err(early) => return Ok(early),
        };
        let ev = BatchEvent {
            expect: BatchState::Staged,
            version: rec.row.version,
            kind: BatchEventKind::Promote {
                head: head.into(),
                base: base.into(),
            },
            actor: actor.into(),
        };
        let mut next = match self.round_step(&mut c, &key, &rec, &ev, at, req)? {
            Ok(n) => n,
            Err(early) => return Ok(early),
        };
        if let Err(early) = self.plan_members(
            &mut c,
            &mut next.row,
            &members,
            &deliveries,
            actor,
            at,
            "promote",
        )? {
            return Ok(early);
        }
        c.put(self, &key, &next, &[])?;
        Ok(Planned::Cascade(c))
    }

    /// `requeue-orphans`: records IN_DELIVERY that no unfinished round names, and the members of
    /// a round that opened and never began (OPEN, no run, pass or phase, cut more than
    /// `stale_secs` before `now`). With `apply`, each such round is abandoned and each other
    /// orphan released in its own txn. A delivery live outside a round is no orphan, and a round
    /// whose lease is live is its holder's: it is listed `leased` and left alone.
    pub fn requeue_orphans(
        &self,
        actor: &str,
        apply: bool,
        stale_secs: i64,
        now: i64,
        req: &Request,
    ) -> Result<Orphans> {
        let (records, _) = self.records()?;
        let rounds = self.batches()?;
        let named: BTreeSet<&str> = rounds
            .iter()
            .filter(|(_, r)| !FINISHED.contains(&r.value.row.state))
            .flat_map(|(_, r)| r.value.members.iter().map(|m| m.bead_id.as_str()))
            .collect();
        let mut out = Orphans::default();
        for r in &records {
            let row = &r.value.row;
            if row.state != BeadState::InDelivery || named.contains(row.bead_id.as_str()) {
                continue;
            }
            let live_elsewhere = self
                .delivery(&row.bead_id)?
                .is_some_and(|d| d.value.mode != Mode::Queue && !d.value.state.is_terminal());
            if !live_elsewhere {
                out.orphans.push(row.bead_id.clone());
            }
        }
        let never_began: Vec<(&String, &BatchRecord)> = rounds
            .iter()
            .map(|(k, r)| (k, &r.value))
            .filter(|(_, r)| {
                let row = &r.row;
                row.state == BatchState::Open
                    && row.run.is_none()
                    && row.pass == 0
                    && row.phase.is_none()
                    && r.opened_at < now - stale_secs
            })
            .collect();
        let mut phantoms: Vec<&BatchRecord> = Vec::new();
        for (key, r) in never_began {
            if self.round_leased(key)? {
                out.leased.push(r.row.batch_id.clone());
            } else {
                phantoms.push(r);
            }
        }
        out.leased.sort();
        phantoms.sort_by(|a, b| a.row.batch_id.cmp(&b.row.batch_id));
        out.phantom_batches = phantoms.iter().map(|r| r.row.batch_id.clone()).collect();
        let phantom_members: BTreeSet<&str> = phantoms
            .iter()
            .flat_map(|r| r.members.iter().map(|m| m.bead_id.as_str()))
            .collect();
        for m in &phantom_members {
            if !out.orphans.iter().any(|o| o == m) {
                out.orphans.push(m.to_string());
            }
        }
        out.orphans.sort();
        if !apply {
            return Ok(out);
        }
        for r in &phantoms {
            let id = &r.row.batch_id;
            let ev = BatchEvent {
                expect: BatchState::Open,
                version: r.row.version,
                kind: BatchEventKind::Abandon {
                    reason: format!(
                        "requeue-orphans: opened and never began (no PR, run, pass or phase in {stale_secs}s)"
                    ),
                },
                actor: actor.into(),
            };
            let p = self.plan_abandon(id, &ev, now, req)?;
            match self.run_planned(p, req, &format!("abandon/{id}"))? {
                Ran::Done { applied, .. } if applied.iter().all(|a| *a) => {
                    out.abandoned.push(id.clone())
                }
                _ => out.failed.push(id.clone()),
            }
        }
        let any_failed = !out.failed.is_empty();
        for id in out.orphans.clone() {
            if phantom_members.contains(id.as_str()) {
                if !any_failed {
                    out.requeued.push(id);
                }
                continue;
            }
            let Some(rec) = self.record(&id)? else {
                continue;
            };
            let tip = rec.value.row.tip.unwrap_or_default();
            let kind = self.release_kind(&id, tip)?;
            let mut c = Cascade::new();
            self.exit(&mut c, &id, kind, actor, now)?;
            let ran = self.run_planned(Planned::Cascade(c), req, &format!("release/{id}"))?;
            match ran {
                Ran::Done { applied, .. } if !applied.is_empty() && applied.iter().all(|a| *a) => {
                    out.requeued.push(id)
                }
                _ => out.failed.push(id),
            }
        }
        Ok(out)
    }

    /// Whether `locks/round/<repo>/<id>` is live for the round at `batches/<repo>/<id>`.
    fn round_leased(&self, batch_key: &str) -> Result<bool> {
        let Some(round) = batch_key.strip_prefix(keys::BATCHES) else {
            return Ok(false);
        };
        let lock = format!("{}round/{round}", state_store::LOCKS_PREFIX);
        Ok(self
            .contract()
            .get(&lock)?
            .is_some_and(|l| l.lock_expires_at_ms.is_some()))
    }
}
