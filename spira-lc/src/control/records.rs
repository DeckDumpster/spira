//! The values stored under the control plane's keys. A lifecycle record holds the state
//! machine's fields plus `repo` and the claim rule's inputs (`claim`, `carried_by`); a title,
//! a priority or a body is never copied onto it.

use std::collections::BTreeMap;

use lifecycle::batch::BatchRow;
use lifecycle::bead::{LifecycleRow, BeadState};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use spira_config::edges::Gate;
use state_store::{Error, Object, Result};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LifecycleRecord {
    #[serde(flatten)]
    pub row: LifecycleRow,
    /// Fixed at create: the lane's identity and the routing key for rounds and batches.
    #[serde(default)]
    pub repo: Option<String>,
    /// Epoch seconds of the last write, for display only.
    pub updated_at: i64,
    /// The claim rule's content inputs as the last `sync-claim-inputs` read them; `None` until
    /// one has, which is not the same as no edges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<ClaimInputs>,
    /// The integration whose record line names this bead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carried_by: Option<String>,
}

/// What the claim rule reads of a bead from the content store.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClaimInputs {
    /// The ids `rank::declared_blockers` yields: blocking edges the bead declares, its parent excluded.
    pub blocks: Vec<String>,
    pub parent: Option<String>,
    /// The full label set: the labels the rule tests are configuration.
    pub labels: Vec<String>,
}

impl LifecycleRecord {
    /// A record as `create-bead` writes it: READY, or born SUBMITTED at `submitted_tip`.
    pub fn born(id: &str, repo: Option<&str>, submitted_tip: Option<&str>, at: i64) -> Self {
        let mut row = LifecycleRow::filed(id);
        row.since = Some(at);
        if let Some(tip) = submitted_tip {
            row.state = lifecycle::bead::BeadState::Submitted;
            row.tip = Some(tip.to_string());
            row.version = 1;
        }
        LifecycleRecord {
            row,
            repo: repo.map(str::to_string),
            updated_at: at,
            claim: None,
            carried_by: None,
        }
    }

    /// A record whose filing has not finished: OPEN, so nothing claims it before its edges exist.
    pub fn born_open(id: &str, repo: Option<&str>, at: i64) -> Self {
        let mut r = Self::born(id, repo, None, at);
        r.row.state = BeadState::Open;
        r
    }

    /// The projection a blocker's dependents compare.
    pub fn gate(&self) -> Gate {
        spira_config::edges::gate(self.row.state, self.row.tip.as_deref(), lifecycle::bead::known_red(&self.row))
    }

    /// Whether `self` is the record `other` was born as, whenever each was stamped.
    pub fn same_birth(&self, other: &LifecycleRecord) -> bool {
        let unstamped = |r: &LifecycleRecord| LifecycleRecord {
            row: LifecycleRow {
                since: None,
                ..r.row.clone()
            },
            updated_at: 0,
            claim: None,
            carried_by: None,
            ..r.clone()
        };
        unstamped(self) == unstamped(other)
    }
}

/// The dispatcher's reading of one record, under its own key, so publishing it never moves a
/// lifecycle record's revision.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Readiness {
    #[serde(default)]
    pub claimable: bool,
    #[serde(default)]
    pub blocked_by: Vec<String>,
    /// Why the record was judged as it was: the dispatcher's verdict, or the state it read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    /// The producer's own clock, as it stamped the pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judged_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judged_at_planning_watermark: Option<String>,
    #[serde(default)]
    pub judged_at_rev: u64,
    /// Every blocker the judgement read, with its gate as judged (`None`: it had no record). A
    /// claim compares each, so one that moved since conflicts the claim.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub gates: BTreeMap<String, Option<Gate>>,
    /// A readiness pass has judged the record for claims: `claimable` and `gates` are its
    /// answer, and a verdict pass no longer restamps `judged_at_rev`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub judged_for_claim: bool,
    /// The record was escalated for staying OPEN with no open bead behind it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub open_escalated: bool,
}

/// One member of a round, at the tip it was cut at.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub bead_id: String,
    pub tip: String,
}

/// A round under `batches/<repo>/<id>`: the batch machine's row with its members inside it, so
/// a cut of N members is one txn on its own key and theirs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BatchRecord {
    #[serde(flatten)]
    pub row: BatchRow,
    pub members: Vec<Member>,
    /// Epoch seconds of the cut, for display only.
    pub opened_at: i64,
    /// The queue's own fields of the round (its assembled head, worktree, reds): written only by
    /// `queue`, and carried unchanged through every machine step.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub round: BTreeMap<String, String>,
    /// The round's applied batch events, folded in by the txn that wrote each ([`BATCH_EVENTS`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_events: Option<Value>,
}

/// Under `batch-ids/<id>`: the repo a round was cut in, so a round is found by id alone and a
/// cut compares its id absent in every repo with one key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BatchIndex {
    pub repo: String,
}

/// One applied or refused machine event, as the dolt `event` table held it. A txn's audit is
/// the array of these it carries as evidence into `events/<rev>`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Audit {
    pub machine: String,
    pub lc_key: String,
    pub event: String,
    pub expect: String,
    pub from_state: String,
    pub to_state: String,
    pub applied: bool,
    pub refusal: Option<String>,
    pub evidence: Value,
    pub actor: String,
    pub at: i64,
}

/// The field a delivery's stored object keeps beside its row: `{STATE: at}`, the newest
/// applied delivery event into each state, which is what the dolt path's `entered_at` join
/// reads (`MAX(at)` per key and state). Every txn that writes a delivery folds its own audits
/// in, so `list --delivery` answers from the record and never reads history (sp-8ugeek).
pub const ENTERED: &str = "entered";

/// `entered` with each applied delivery audit of `id` folded in, by the newest `at` per state.
pub fn fold_entered<'a>(
    entered: &mut serde_json::Map<String, Value>,
    id: &str,
    audits: impl IntoIterator<Item = &'a Audit>,
) {
    for a in audits {
        if a.machine != "delivery" || a.lc_key != id || !a.applied {
            continue;
        }
        let newer = entered
            .get(&a.to_state)
            .and_then(Value::as_i64)
            .is_none_or(|t| a.at > t);
        if newer {
            entered.insert(a.to_state.clone(), Value::from(a.at));
        }
    }
}

/// The field a round's stored object keeps beside its row: `{"fetched": [{event, evidence, at}],
/// "last_at"}`. `fetched` is the round's applied batch audits whose event is in
/// [`crate::passes::FETCHED`], in revision order; `last_at` is the newest `at` of any applied batch
/// audit. Every txn that writes a round folds its own audits in, so `list --batches` answers from
/// the record and never reads history (sp-aiti78).
pub const BATCH_EVENTS: &str = "batch_events";

/// `events` (a [`BATCH_EVENTS`] object) with each applied batch audit of round `id` folded in.
pub fn fold_batch_events<'a>(
    events: &mut serde_json::Map<String, Value>,
    id: &str,
    audits: impl IntoIterator<Item = &'a Audit>,
) {
    let mut fetched = match events.remove("fetched") {
        Some(Value::Array(a)) => a,
        _ => Vec::new(),
    };
    let mut last_at = events.get("last_at").and_then(Value::as_i64);
    for a in audits {
        if a.machine != "batch" || a.lc_key != id || !a.applied {
            continue;
        }
        if crate::passes::is_fetched(&a.event) {
            fetched.push(serde_json::json!({ "event": a.event, "evidence": a.evidence, "at": a.at }));
        }
        last_at = last_at.max(Some(a.at));
    }
    events.insert("fetched".into(), Value::Array(fetched));
    events.insert("last_at".into(), last_at.map_or(Value::Null, Value::from));
}

/// When the delivery entered `state`, as its stored object's `entered` says: `None` when the
/// object predates the field, `Some(None)` when no applied event ever entered that state.
pub fn entered_at(data: &Value, state: &str) -> Option<Option<i64>> {
    data.get(ENTERED)
        .and_then(Value::as_object)
        .map(|m| m.get(state).and_then(Value::as_i64))
}

pub fn encode<T: Serialize>(v: &T) -> Result<Value> {
    serde_json::to_value(v).map_err(|e| Error::Backend(format!("unencodable record: {e}")))
}

pub fn decode<T: for<'de> Deserialize<'de>>(key: &str, o: &Object) -> Result<T> {
    serde_json::from_value(o.data.clone())
        .map_err(|e| Error::Backend(format!("{key}: undecodable record: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(t90uji3)]
    #[test]
    fn a_record_round_trips_and_carries_no_planning_field() {
        let r = LifecycleRecord::born("sp-a", Some("harness"), None, 100);
        let v = encode(&r).unwrap();
        for planning in ["title", "priority", "description"] {
            assert!(v.get(planning).is_none(), "{planning} in {v}");
        }
        assert_eq!(v["repo"], "harness");
        assert!(v.get("claim").is_none() && v.get("carried_by").is_none(), "never written is absent: {v}");
        assert_eq!(
            decode::<LifecycleRecord>("lifecycle/sp-a", &Object::new(v)).unwrap(),
            r
        );
    }

    #[test_id(t44myax)]
    #[test]
    fn a_birth_is_the_same_whenever_it_was_stamped_and_differs_by_repo_or_tip() {
        let a = LifecycleRecord::born("sp-a", Some("harness"), None, 100);
        assert!(a.same_birth(&LifecycleRecord::born("sp-a", Some("harness"), None, 250)));
        assert!(!a.same_birth(&LifecycleRecord::born("sp-a", Some("other"), None, 100)));
        assert!(!a.same_birth(&LifecycleRecord::born(
            "sp-a",
            Some("harness"),
            Some("abc"),
            100
        )));
        let s = LifecycleRecord::born("sp-a", None, Some("abc"), 100);
        assert_eq!(
            (s.row.state.as_str(), s.row.version, s.row.tip.as_deref()),
            ("SUBMITTED", 1, Some("abc"))
        );
    }
}
