//! The ask machine on the control store. `asks/<id>` holds an ask's state; its question is a
//! decision bead on the planning plane. Closing an ask lifts the ask hold on the work bead it
//! names in the same txn, so the two never disagree.

use lifecycle::ask::{self, AskEvent, AskEventKind, AskRow, AskState};
use lifecycle::bead::BeadEventKind;
use lifecycle::Refusal;
use serde::{Deserialize, Serialize};
use state_store::{Backend, Error, Object, Request, Result, Selector};

use super::rounds::{machine_audit, Cascade, Ran};
use super::{sub_request, Store};
use crate::control::keys;
use crate::control::records::{decode, encode};

/// An ask's state, as its machine keeps it, and when it opened and closed (display only).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AskRecord {
    #[serde(flatten)]
    pub row: AskRow,
    pub opened_at: i64,
    #[serde(default)]
    pub closed_at: Option<i64>,
}

/// What a close did.
#[derive(Clone, Debug, PartialEq)]
pub enum AskClose {
    NoAsk,
    /// The machine refused the exit; the refusal is audited.
    Refused(Refusal),
    Closed,
    /// The ask or its work bead moved between the read and the write.
    LostRace,
}

impl<B: Backend> Store<B> {
    /// Opens ask `id`. An ask that already exists is left as it is.
    pub fn create_ask(
        &self,
        id: &str,
        work_bead: Option<&str>,
        at: i64,
        req: &Request,
    ) -> Result<()> {
        let rec = AskRecord {
            row: AskRow::open(id, work_bead.map(str::to_string)),
            opened_at: at,
            closed_at: None,
        };
        let sub = sub_request(req, "create-ask", serde_json::Value::Null);
        match self
            .contract()
            .create(&keys::ask(id), Object::new(encode(&rec)?), &sub)
        {
            Ok(_) | Err(Error::Conflict { .. }) => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub fn ask(&self, id: &str) -> Result<Option<AskRecord>> {
        Ok(self.read(&keys::ask(id), None)?.map(|(r, _)| r.value))
    }

    /// Every ask, from one snapshot, in id order.
    pub fn asks(&self) -> Result<Vec<AskRecord>> {
        let (items, _) = self.contract().list(&[keys::ASKS], &Selector::Everything)?;
        items
            .iter()
            .filter(|i| keys::id_of(keys::ASKS, &i.key).is_some())
            .map(|i| decode(&i.key, &i.value))
            .collect()
    }

    /// Closes ask `id` by `kind`, and lifts the ask hold on its work bead: a `Reply` carrying
    /// `message_id` for an answer, `AskWithdrawn` for any other exit. A work bead with no
    /// record, or whose machine will not take the lift, is nothing to lift.
    pub fn close_ask(
        &self,
        id: &str,
        kind: AskEventKind,
        actor: &str,
        message_id: &str,
        at: i64,
        req: &Request,
    ) -> Result<AskClose> {
        match self.plan_close_ask(id, kind, actor, message_id, at, req)? {
            Ok(c) => self.commit_close(c, req),
            Err(done) => Ok(done),
        }
    }

    /// Commits a planned close: `LostRace` when the ask or its work bead moved since its read.
    fn commit_close(&self, c: Cascade, req: &Request) -> Result<AskClose> {
        Ok(match self.commit(c, req, "close-ask")? {
            Ran::Done { applied, .. } if applied.iter().all(|a| *a) => AskClose::Closed,
            _ => AskClose::LostRace,
        })
    }

    /// The close as one cascade, or what ended it before anything was planned.
    fn plan_close_ask(
        &self,
        id: &str,
        kind: AskEventKind,
        actor: &str,
        message_id: &str,
        at: i64,
        req: &Request,
    ) -> Result<std::result::Result<Cascade, AskClose>> {
        let key = keys::ask(id);
        let mut c = Cascade::new();
        let Some(mut rec) = c.get::<B, AskRecord>(self, &key)? else {
            return Ok(Err(AskClose::NoAsk));
        };
        let ev = AskEvent {
            expect: rec.row.state,
            version: rec.row.version,
            kind: kind.clone(),
            actor: actor.into(),
        };
        let out = ask::apply(&rec.row, &ev);
        let evidence = serde_json::to_value(&kind).unwrap_or_default();
        let from = rec.row.state.as_str();
        let audit = |to: &str, r: Option<&Refusal>| {
            machine_audit("ask", id, from, from, to, r, evidence.clone(), actor, at)
        };
        if !out.applied {
            let refusal = out
                .refusal
                .unwrap_or(Refusal::Terminal { state: from.into() });
            self.refuse_audit(audit(from, Some(&refusal)), refusal.clone(), req)?;
            return Ok(Err(AskClose::Refused(refusal)));
        }
        c.step(audit(out.row.state.as_str(), None));
        let lift = match out.row.state {
            AskState::Answered => BeadEventKind::Reply {
                message_id: message_id.into(),
            },
            _ => BeadEventKind::AskWithdrawn,
        };
        let work = rec.row.work_bead.clone();
        rec.row = out.row;
        rec.closed_at = Some(at);
        c.put(self, &key, &rec, &[])?;
        if let Some(w) = work {
            let _ = self.bead_step(&mut c, &w, lift, actor, at)?;
        }
        Ok(Ok(c))
    }
}

#[cfg(test)]
mod tests;
