//! A repo's lane and the stack its live rounds make on it. The lane record names the inputs every
//! cut is tested on; a live round is *grounded* when its `tested_base` is the landing ref or the
//! head of a grounded round, which is exactly the set of rounds that can still land.

use std::collections::{BTreeMap, BTreeSet};

use lifecycle::batch::BatchState;
use serde::Deserialize;
use state_store::Selector;

use crate::ports::{Place, Ports};
use crate::refusal::Refusal;
use crate::resources::{round_key, Round};

/// The repo's stack depth when its lane record names none (design §Throughput: depth 2 is the
/// default target; depth 3 pays only at p >= 0.64).
pub const DEFAULT_DEPTH: usize = 2;

pub fn lane_key(repo: &str) -> String {
    format!("lanes/{repo}")
}

/// `lanes/<repo>`: what a cut on this repo is tested on.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lane {
    /// The sha the landing ref points at.
    pub landing_ref: String,
    pub image_digest: String,
    pub harness_version: String,
    #[serde(default = "all")]
    pub selection: String,
    /// The section's `concurrency` target: how deep rounds may stack.
    #[serde(default)]
    pub concurrency: Option<usize>,
}

fn all() -> String {
    "all".into()
}

impl Lane {
    pub fn depth(&self) -> usize {
        self.concurrency.unwrap_or(DEFAULT_DEPTH)
    }
}

/// The repo's lane, `None` when it has no record.
pub fn lane(p: &Ports, repo: &str) -> Result<Option<Lane>, Refusal> {
    let key = lane_key(repo);
    p.store
        .get(&key)
        .map_err(|e| Refusal::unreadable(&key, e))?
        .map(|i| serde_json::from_value(i.value.data).map_err(|e| Refusal::unreadable(&key, e)))
        .transpose()
}

/// A non-terminal round, and whether a runner holds its lease.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Live {
    pub batch: String,
    pub round: Round,
    pub leased: bool,
}

/// Every non-terminal round of `repo`, by batch id; the child keys under a round are not rounds.
pub fn live(p: &Ports, repo: &str) -> Result<Vec<Live>, Refusal> {
    let prefix = round_key(repo, "");
    let (items, _) = p
        .store
        .list(&[prefix.as_str()], &Selector::Everything)
        .map_err(|e| Refusal::unreadable(&prefix, e))?;
    let mut out = Vec::new();
    for i in items {
        let Some(batch) = i.key.strip_prefix(&prefix).filter(|b| !b.contains('/')) else {
            continue;
        };
        let round: Round =
            serde_json::from_value(i.value.data).map_err(|e| Refusal::unreadable(&i.key, e))?;
        if round.row.state.is_terminal() {
            continue;
        }
        let lock = format!("locks/round/{repo}/{batch}");
        let leased = p
            .store
            .get(&lock)
            .map_err(|e| Refusal::unreadable(&lock, e))?
            .is_some_and(|l| l.lock_expires_at_ms.is_some());
        out.push(Live {
            batch: batch.to_string(),
            round,
            leased,
        });
    }
    Ok(out)
}

/// The grounded rounds: each one's height above the landing ref and the round it is tested on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stack {
    pub height: BTreeMap<String, usize>,
    pub below: BTreeMap<String, String>,
}

impl Stack {
    pub fn of(lane: &Lane, rounds: &[Live]) -> Stack {
        let mut s = Stack::default();
        let mut heads: BTreeMap<String, String> = BTreeMap::new();
        loop {
            let before = s.height.len();
            for r in rounds {
                if s.height.contains_key(&r.batch) {
                    continue;
                }
                let Some(tb) = r.round.tested_base.as_deref() else {
                    continue;
                };
                let found = if tb == lane.landing_ref {
                    Some((1, None))
                } else {
                    heads.get(tb).map(|k| (s.height[k] + 1, Some(k.clone())))
                };
                if let Some((h, below)) = found {
                    s.height.insert(r.batch.clone(), h);
                    if let Some(b) = below {
                        s.below.insert(r.batch.clone(), b);
                    }
                    if let Some(head) = &r.round.row.head {
                        heads.entry(head.clone()).or_insert_with(|| r.batch.clone());
                    }
                }
            }
            if s.height.len() == before {
                return s;
            }
        }
    }

    pub fn grounded(&self, batch: &str) -> bool {
        self.height.contains_key(batch)
    }

    /// The highest grounded round; between equals, the newest batch id.
    pub fn top(&self) -> Option<(&str, usize)> {
        self.height
            .iter()
            .max_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0)))
            .map(|(b, h)| (b.as_str(), *h))
    }

    /// The top and every round under it: what a round cut on the top is tested above.
    pub fn under_top(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut at = self.top().map(|(b, _)| b.to_string());
        while let Some(b) = at {
            at = self.below.get(&b).cloned();
            out.insert(b);
        }
        out
    }

    /// Where the next round goes and the base it is tested on: on the landing ref with no live
    /// round, on the top while the stack is below `depth`, else nowhere.
    pub fn place(&self, lane: &Lane, rounds: &[Live]) -> Option<(Place, String)> {
        match self.top() {
            None => Some((Place::OnBase, lane.landing_ref.clone())),
            Some((_, h)) if h >= lane.depth() => None,
            Some((k, _)) => rounds
                .iter()
                .find(|r| r.batch == k)
                .and_then(|r| r.round.row.head.clone())
                .map(|head| (Place::StackOn(k.to_string()), head)),
        }
    }
}

/// The states `queue round supersede` moves to SUPERSEDED.
pub fn supersedable(s: BatchState) -> bool {
    matches!(
        s,
        BatchState::Green
            | BatchState::CiRunning
            | BatchState::Attributing
            | BatchState::Rebuilding
    )
}
