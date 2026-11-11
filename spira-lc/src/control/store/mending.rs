//! The mending machine on the control store. A failure's row is `mendings/<batch#pass#suite>`,
//! born WAITING in the txn of its round's `Failed`; each member branch a mend moved is a
//! `mends/<bead>/` entry, written in the txn of the row's `Mended`.

use lifecycle::mending::{self, HandFix, MendingEvent, MendingEventKind, MendingRow, MendingState};
use lifecycle::Refusal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use state_store::{Backend, Request, Result, Selector};

use super::rounds::{machine_audit, Cascade, Ran};
use super::Store;
use crate::control::keys;
use crate::control::records::{decode, Audit};

/// A failure's row, as its machine keeps it, and when the round failed (display only).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MendingRecord {
    #[serde(flatten)]
    pub row: MendingRow,
    pub failed_at: i64,
}

impl MendingRecord {
    pub fn failed(batch_id: &str, pass: u32, suite: &str, at: i64) -> Self {
        MendingRecord {
            row: MendingRow::failed(batch_id, pass, suite),
            failed_at: at,
        }
    }
}

/// One member branch a mend moved, from the tip it was built on to the tip it left.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MendRecord {
    pub batch_id: String,
    pub pass: u32,
    pub suite: String,
    pub bead_id: String,
    pub from_tip: String,
    pub tip: String,
    pub ask_id: Option<String>,
    pub checked: Vec<String>,
    pub diagnosis: Option<String>,
    pub at: i64,
}

/// A member a mend moves: `(bead, from tip, to tip)`.
pub type Move = (String, String, String);

/// What one step on a failure's row did.
#[derive(Clone, Debug, PartialEq)]
pub enum Stepped {
    NoRow,
    /// Applied, leaving the row in this state.
    Applied(MendingState),
    /// The machine refused the event; the refusal is audited.
    Refused(Refusal),
    LostRace,
}

impl<B: Backend> Store<B> {
    pub fn mending(&self, batch: &str, pass: u32, suite: &str) -> Result<Option<MendingRecord>> {
        let key = keys::mending(&mending::key(batch, pass, suite));
        Ok(self.read(&key, None)?.map(|(r, _)| r.value))
    }

    /// Every failure's row, from one snapshot.
    pub fn mendings(&self) -> Result<Vec<MendingRecord>> {
        let (items, _) = self
            .contract()
            .list(&[keys::MENDINGS], &Selector::Everything)?;
        items.iter().map(|i| decode(&i.key, &i.value)).collect()
    }

    /// Every mend of `bead`'s branch, in the order they landed.
    pub fn mends_of(&self, bead: &str) -> Result<Vec<MendRecord>> {
        let (items, _) = self
            .contract()
            .list(&[keys::mends(bead).as_str()], &Selector::Everything)?;
        items.iter().map(|i| decode(&i.key, &i.value)).collect()
    }

    /// Plans `kind` on the row read into `c`: the step and the row's next value, or the
    /// machine's refusal with its audit.
    fn plan_mending(
        &self,
        c: &mut Cascade,
        row: &MendingRecord,
        kind: &MendingEventKind,
        evidence: Value,
        actor: &str,
        at: i64,
    ) -> Result<std::result::Result<MendingRecord, (Audit, Refusal)>> {
        let ev = MendingEvent {
            expect: row.row.state,
            version: row.row.version,
            kind: kind.clone(),
            actor: actor.into(),
        };
        let out = mending::apply(&row.row, &ev);
        let from = row.row.state.as_str();
        let audit = |to: &str, r: Option<&Refusal>| Audit {
            event: kind_name(kind),
            ..machine_audit(
                "mending",
                &row.row.key(),
                from,
                from,
                to,
                r,
                evidence.clone(),
                actor,
                at,
            )
        };
        if !out.applied {
            let refusal = out.refusal.unwrap_or(Refusal::IllegalTransition {
                state: from.into(),
                event: kind_name(kind),
            });
            return Ok(Err((audit(from, Some(&refusal)), refusal)));
        }
        c.step(audit(out.row.state.as_str(), None));
        let next = MendingRecord {
            row: out.row,
            ..row.clone()
        };
        c.put(self, &keys::mending(&row.row.key()), &next, &[])?;
        Ok(Ok(next))
    }

    /// Commits one step on the row at `key` under a part naming the row and the event, so two
    /// steps one call takes (close-ended's TriageEnded then Return, a sweep over many rows) are
    /// two requests, never a replay of the first.
    fn commit_step(
        &self,
        c: Cascade,
        state: MendingState,
        key: &str,
        event: &str,
        req: &Request,
    ) -> Result<Stepped> {
        let part = format!("mending/{key}/{event}");
        Ok(match self.commit(c, req, &part)? {
            Ran::Done { applied, .. } if applied.iter().all(|a| *a) => Stepped::Applied(state),
            _ => Stepped::LostRace,
        })
    }

    /// One event on the failure's row, CAS'd on it.
    pub fn mending_event(
        &self,
        row: &MendingRow,
        kind: MendingEventKind,
        actor: &str,
        at: i64,
        req: &Request,
    ) -> Result<Stepped> {
        let mut c = Cascade::new();
        let key = keys::mending(&row.key());
        let Some(cur) = c.get::<B, MendingRecord>(self, &key)? else {
            return Ok(Stepped::NoRow);
        };
        let evidence = serde_json::to_value(&kind).unwrap_or(Value::Null);
        match self.plan_mending(&mut c, &cur, &kind, evidence, actor, at)? {
            Ok(next) => self.commit_step(c, next.row.state, &row.key(), &kind_name(&kind), req),
            Err((audit, refusal)) => {
                self.refuse_audit(audit, refusal.clone(), req)?;
                Ok(Stepped::Refused(refusal))
            }
        }
    }

    /// A mend refused before its machine saw it, audited on the row's log; the row stays.
    pub fn refuse_mend(
        &self,
        row: &MendingRow,
        refusal: Refusal,
        actor: &str,
        at: i64,
        req: &Request,
    ) -> Result<()> {
        let from = row.state.as_str();
        let audit = Audit {
            event: "Mended".into(),
            ..machine_audit(
                "mending",
                &row.key(),
                from,
                from,
                from,
                Some(&refusal),
                Value::String(format!("{refusal:?}")),
                actor,
                at,
            )
        };
        self.refuse_audit(audit, refusal, req).map(|_| ())
    }

    /// `Mended` on the row, and each member's move recorded on its branch, in one txn.
    #[allow(clippy::too_many_arguments)]
    pub fn record_mend(
        &self,
        row: &MendingRow,
        moves: &[Move],
        ask: Option<&str>,
        checked: &[String],
        hand: Option<&HandFix>,
        actor: &str,
        at: i64,
        req: &Request,
    ) -> Result<Stepped> {
        let mut c = Cascade::new();
        let key = keys::mending(&row.key());
        let Some(cur) = c.get::<B, MendingRecord>(self, &key)? else {
            return Ok(Stepped::NoRow);
        };
        let evidence = serde_json::json!({ "Mended": {
            "hand": hand,
            "at": at,
            "members": moves.iter().map(|(b, f, t)| format!("{b}:{f}:{t}")).collect::<Vec<_>>(),
            "ask": ask,
            "checked": checked,
        }});
        let kind = MendingEventKind::Mended { at };
        let next = match self.plan_mending(&mut c, &cur, &kind, evidence, actor, at)? {
            Ok(next) => next,
            Err((audit, refusal)) => {
                self.refuse_audit(audit, refusal.clone(), req)?;
                return Ok(Stepped::Refused(refusal));
            }
        };
        let planned_at = self.contract().head()?;
        for (n, (bead, from, to)) in moves.iter().enumerate() {
            let m = MendRecord {
                batch_id: cur.row.batch_id.clone(),
                pass: cur.row.pass,
                suite: cur.row.suite.clone(),
                bead_id: bead.clone(),
                from_tip: from.clone(),
                tip: to.clone(),
                ask_id: ask.map(str::to_string),
                checked: checked.to_vec(),
                diagnosis: cur.row.diagnosis.clone(),
                at,
            };
            c.put(self, &keys::mend(bead, planned_at, n), &m, &[])?;
        }
        self.commit_step(c, next.row.state, &row.key(), "Mended", req)
    }
}

fn kind_name(kind: &MendingEventKind) -> String {
    crate::kind_name(&serde_json::to_value(kind).unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests;
