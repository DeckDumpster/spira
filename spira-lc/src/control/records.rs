//! The values stored under the control plane's keys. A lifecycle record holds the state
//! machine's fields plus `repo`; nothing here copies a planning-plane field.

use std::collections::BTreeMap;

use lifecycle::batch::BatchRow;
use lifecycle::bead::{BeadRow, BeadState};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use spira_config::edges::Gate;
use state_store::{Error, Object, Result};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LifecycleRecord {
    #[serde(flatten)]
    pub row: BeadRow,
    /// Fixed at create: the lane's identity and the routing key for rounds and batches.
    #[serde(default)]
    pub repo: Option<String>,
    /// Epoch seconds of the last write, for display only.
    pub updated_at: i64,
}

impl LifecycleRecord {
    /// A record as `create-bead` writes it: READY, or born SUBMITTED at `submitted_tip`.
    pub fn born(id: &str, repo: Option<&str>, submitted_tip: Option<&str>, at: i64) -> Self {
        let mut row = BeadRow::filed(id);
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
        spira_config::edges::gate(self.row.state, self.row.tip.as_deref())
    }

    /// Whether `self` is the record `other` was born as, whenever each was stamped.
    pub fn same_birth(&self, other: &LifecycleRecord) -> bool {
        let unstamped = |r: &LifecycleRecord| LifecycleRecord {
            row: BeadRow {
                since: None,
                ..r.row.clone()
            },
            updated_at: 0,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runes: Vec<String>,
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

    #[test]
    fn a_record_round_trips_and_carries_no_planning_field() {
        let r = LifecycleRecord::born("sp-a", Some("harness"), None, 100);
        let v = encode(&r).unwrap();
        for planning in ["title", "priority", "blocked_by", "labels"] {
            assert!(v.get(planning).is_none(), "{planning} in {v}");
        }
        assert_eq!(v["repo"], "harness");
        assert_eq!(
            decode::<LifecycleRecord>("lifecycle/sp-a", &Object::new(v)).unwrap(),
            r
        );
    }

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
