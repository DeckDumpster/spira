//! Readiness on its own key: one publish pass for every live record, merged into what other
//! publishers wrote there, so publishing never moves a lifecycle record's revision.

use std::collections::BTreeMap;

use serde_json::Value;
use spira_config::edges::Gate;
use state_store::{Backend, Compare, Error, Object, Request, Result, Txn};

use super::{sub_request, Store, MERGE_ATTEMPTS, PASS_CHUNK};
use crate::control::keys;
use crate::control::records::{encode, LifecycleRecord, Readiness};
use lifecycle::bead::BeadState;

/// One record as the dispatcher judged it: whether it may be claimed, what holds it back, and
/// every blocker it read with that blocker's gate.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Judged {
    pub id: String,
    pub claimable: bool,
    pub blocked_by: Vec<String>,
    pub verdict: Option<String>,
    pub gates: BTreeMap<String, Option<Gate>>,
}

/// What a whole pass was judged at.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pass {
    /// The producer's own clock, as it stamped the pass.
    pub judged_at: String,
    pub planning_watermark: Option<String>,
    /// The control-plane revision the records were read at; the head when the producer has none.
    pub judged_at_rev: Option<u64>,
}

/// Whether two readings differ only in when they were judged.
fn same_judgement(a: &Readiness, b: &Readiness) -> bool {
    let unstamped = |r: &Readiness| Readiness {
        judged_at: None,
        judged_at_planning_watermark: None,
        judged_at_rev: 0,
        ..r.clone()
    };
    unstamped(a) == unstamped(b)
}

impl<B: Backend> Store<B> {
    /// The READY and REWORK records the dispatcher's last pass judged claimable, each with that
    /// judgement, from one revision, and how many no pass has judged yet.
    pub fn judged_claimable(&self) -> Result<(Vec<(LifecycleRecord, Readiness)>, usize)> {
        let rev = self.contract.head()?;
        let records = self.records_page(rev, Some(&[BeadState::Ready, BeadState::Rework]), None, None, None, |_| true)?;
        let ids: Vec<&str> = records.iter().map(|r| r.value.row.bead_id.as_str()).collect();
        let mut judged = self.readiness_of(&ids, rev)?;
        let mut unjudged = 0;
        let mut claimable = Vec::new();
        for r in records {
            match judged.remove(&r.value.row.bead_id) {
                Some(j) if j.judged_for_claim => {
                    if j.claimable {
                        claimable.push((r.value, j));
                    }
                }
                _ => unjudged += 1,
            }
        }
        Ok((claimable, unjudged))
    }

    /// Publishes each judgement onto `readiness/<id>`. A record that does not exist gets none,
    /// and one judged as it already reads is not written: its `judged_at*` stay the last write's.
    pub fn publish_readiness(&self, judged: &[Judged], pass: &Pass, req: &Request) -> Result<u64> {
        self.merge_readiness(
            judged,
            |j| &j.id,
            "readiness",
            req,
            |r, j, head| {
                r.claimable = j.claimable;
                r.blocked_by = j.blocked_by.clone();
                r.verdict = j.verdict.clone();
                r.gates = j.gates.clone();
                r.judged_for_claim = true;
                r.judged_at = Some(pass.judged_at.clone());
                r.judged_at_planning_watermark = pass.planning_watermark.clone();
                r.judged_at_rev = pass.judged_at_rev.unwrap_or(head);
            },
        )
    }

    /// A planning write changed what `id`'s claim reads: its record is judged unclaimable,
    /// naming `blocked_by`, until the publisher judges it again; a record no pass has judged is left as it is. Its revision moves, so a claim
    /// that read the old judgement loses its compare.
    pub fn mark_rejudge(&self, id: &str, blocked_by: &[String], req: &Request) -> Result<u64> {
        if !self.readiness(id)?.is_some_and(|r| r.value.judged_for_claim) {
            return self.contract.head();
        }
        self.merge_readiness(&[id], |i| i, "mark-rejudge", req, |r, _, _| {
            r.claimable = false;
            r.judged_for_claim = true;
            r.blocked_by = blocked_by.to_vec();
            r.verdict = Some("rejudge".into());
        })
    }

    /// Merges each item into its record's readiness, in chunks within the op limit, putting
    /// only what the merge changed. Each put compares the readiness it merged into, and a chunk
    /// that lost a race is merged again.
    pub(super) fn merge_readiness<T>(
        &self,
        items: &[T],
        id_of: impl Fn(&T) -> &str,
        part: &str,
        req: &Request,
        merge: impl Fn(&mut Readiness, &T, u64),
    ) -> Result<u64> {
        let mut rev = self.contract.head()?;
        for (n, chunk) in items.chunks(PASS_CHUNK).enumerate() {
            let part = format!("{part}/{n}");
            rev = self.merge_chunk(chunk, &id_of, &part, req, &merge)?;
        }
        Ok(rev)
    }

    fn merge_chunk<T>(
        &self,
        chunk: &[T],
        id_of: &impl Fn(&T) -> &str,
        part: &str,
        req: &Request,
        merge: &impl Fn(&mut Readiness, &T, u64),
    ) -> Result<u64> {
        for attempt in 0..MERGE_ATTEMPTS {
            let head = self.contract.head()?;
            let mut t = Txn::new();
            for item in chunk {
                let id = id_of(item);
                let owner = keys::lifecycle(id);
                if self.contract.get_at(&owner, head)?.is_none() {
                    continue;
                }
                let key = keys::readiness(id);
                let prior = self.read::<Readiness>(&key, Some(head))?;
                let unread = Compare::ModRev(key.clone(), prior.as_ref().map(|(r, _)| r.mod_rev));
                let (prior, value) = match prior {
                    Some((r, item)) => (Some(r.value), item.value),
                    None => (None, Object::default().with_owners(&[owner.as_str()])),
                };
                let mut r = prior.clone().unwrap_or_default();
                merge(&mut r, item, head);
                if prior.is_some_and(|p| same_judgement(&p, &r)) {
                    continue;
                }
                t = t.when(unread).put(
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
}
