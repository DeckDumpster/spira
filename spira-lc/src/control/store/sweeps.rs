//! The sweeps that keep records from being stranded: a claim whose lock expired goes back to
//! the state it was claimed from, and a record left OPEN by a filing that never finished is
//! promoted, or escalated once when no open bead stands behind it.

use std::collections::BTreeMap;

use lifecycle::bead::{BeadEvent, BeadEventKind, BeadState};
use state_store::{Backend, Request, Result};

use super::{sub_request, Outcome, Store};
use crate::control::records::LifecycleRecord;

/// What the planning plane says of an OPEN record's bead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filing {
    /// The bead is open: its filing finished, and only the promote was lost.
    Open,
    /// The bead is closed or was never written.
    Gone,
}

/// What one OPEN sweep did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OpenSweep {
    pub promoted: Vec<String>,
    pub escalated: Vec<String>,
}

fn event(rec: &LifecycleRecord, kind: BeadEventKind, actor: &str, at: i64) -> BeadEvent {
    BeadEvent {
        expect: rec.row.state,
        version: rec.row.version,
        kind,
        actor: actor.into(),
        at: Some(at),
    }
}

impl<B: Backend> Store<B> {
    /// Every WORKING record whose claim lock is gone takes `HolderDead`, which returns it to the
    /// state it was claimed from, once `stop_holder` has stopped whoever still runs under it. A
    /// holder that cannot be stopped keeps its record. Returns the ids returned.
    pub fn expire_claims(
        &self,
        mut stop_holder: impl FnMut(&LifecycleRecord) -> bool,
        actor: &str,
        at: i64,
        req: &Request,
    ) -> Result<Vec<String>> {
        let (records, _) = self.records()?;
        let mut returned = Vec::new();
        for r in records {
            let rec = &r.value;
            let id = rec.row.bead_id.as_str();
            if rec.row.state != BeadState::Working || self.holder_fence(id)?.is_some() {
                continue;
            }
            if !stop_holder(rec) {
                continue;
            }
            let ev = event(rec, BeadEventKind::HolderDead, actor, at);
            let sub = sub_request(req, &format!("expire/{id}"), serde_json::Value::Null);
            if let Outcome::Applied { .. } = self.apply(id, &ev, &sub)? {
                returned.push(id.to_string());
            }
        }
        Ok(returned)
    }

    /// Promotes each OPEN record older than `grace_secs` whose bead `filing` says is open, and
    /// hands `escalate` each one whose bead is gone, once: a record escalated before is not
    /// escalated again. A record `filing` does not answer for is left as it is.
    pub fn sweep_open(
        &self,
        grace_secs: i64,
        now: i64,
        filing: &BTreeMap<String, Filing>,
        mut escalate: impl FnMut(&str) -> bool,
        actor: &str,
        req: &Request,
    ) -> Result<OpenSweep> {
        let (records, readiness, _) = self.records_with_readiness()?;
        let mut out = OpenSweep::default();
        for r in records {
            let rec = &r.value;
            let id = rec.row.bead_id.as_str();
            let born = rec.row.since.unwrap_or(rec.updated_at);
            if rec.row.state != BeadState::Open || born.saturating_add(grace_secs) > now {
                continue;
            }
            match filing.get(id) {
                Some(Filing::Open) => {
                    let sub = sub_request(req, &format!("promote/{id}"), serde_json::Value::Null);
                    if let Outcome::Applied { .. } = self.promote(id, actor, now, &sub)? {
                        out.promoted.push(id.to_string());
                    }
                }
                Some(Filing::Gone)
                    if !readiness.get(id).is_some_and(|r| r.open_escalated) && escalate(id) =>
                {
                    self.merge_readiness(
                        &[id],
                        |i: &&str| *i,
                        &format!("escalated/{id}"),
                        req,
                        |r, _, _| {
                            r.open_escalated = true;
                        },
                    )?;
                    out.escalated.push(id.to_string());
                }
                _ => {}
            }
        }
        Ok(out)
    }
}
