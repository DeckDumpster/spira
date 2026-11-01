//! The round's resources in the store: the round as the controller reads it, a pass's identity,
//! and the attempts and summary the ledger keeps under that identity.

use lifecycle::batch::BatchRow;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use state_store::limits::{MAX_TXN_COMPARES, MAX_TXN_OPS};
use state_store::Rev;

use crate::ports::{Member, Store};

/// The keys a cut adds beside its two per member: the round, its lease and the slot record.
pub const CUT_FIXED_OPS: usize = 3;
/// The compares a cut makes beside one per member: the round key absent and the slot fence.
pub const CUT_FIXED_COMPARES: usize = 2;
/// The contract's own keys in every audited, idempotent txn: `requests/<id>` and `events/<main>`.
pub const TXN_BOOKKEEPING: usize = 2;

/// The largest round whose cut fits the contract's limits: `2N + CUT_FIXED_OPS + TXN_BOOKKEEPING`
/// ops within `MAX_TXN_OPS`, and `N + CUT_FIXED_COMPARES` compares within `MAX_TXN_COMPARES`.
pub const MAX_MEMBERS: usize = min(
    (MAX_TXN_OPS - CUT_FIXED_OPS - TXN_BOOKKEEPING) / 2,
    MAX_TXN_COMPARES - CUT_FIXED_COMPARES,
);

const fn min(a: usize, b: usize) -> usize {
    if a < b {
        a
    } else {
        b
    }
}

pub fn round_key(repo: &str, batch: &str) -> String {
    format!("batches/{repo}/{batch}")
}

/// The value at `batches/<repo>/<id>`: the lifecycle's batch row and the members inside it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Round {
    #[serde(flatten)]
    pub row: BatchRow,
    #[serde(default)]
    pub members: Vec<Member>,
    /// The commit this round is tested on: the landing ref, or the head of the live round it is
    /// stacked on. A round cut before stacking carries none.
    #[serde(default)]
    pub tested_base: Option<String>,
    /// The token of the lease its runner was last launched under, as the lease writes it.
    #[serde(default)]
    pub last_lease_token: Option<Rev>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoundView {
    pub key: String,
    pub mod_rev: Rev,
    pub round: Round,
}

impl RoundView {
    pub fn read(
        store: &dyn Store,
        repo: &str,
        batch: &str,
    ) -> Result<Option<RoundView>, ResourceError> {
        let key = round_key(repo, batch);
        let Some(item) = store.get(&key).map_err(ResourceError::Store)? else {
            return Ok(None);
        };
        let round =
            serde_json::from_value(item.value.data).map_err(|e| ResourceError::Undecodable {
                key: key.clone(),
                text: e.to_string(),
            })?;
        Ok(Some(RoundView {
            key,
            mod_rev: item.mod_rev,
            round,
        }))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ResourceError {
    Store(state_store::Error),
    Undecodable { key: String, text: String },
    NoMembers,
    TooManyMembers { max: usize, got: usize },
    DuplicateMember { id: String },
}

impl std::fmt::Display for ResourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// What a pass tests: an identity that holds a verdict is never run again unless one of these
/// changes (law-a-retry-must-change-an-input).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct PassIdentity {
    pub repo: String,
    /// Sorted by id, each id once.
    pub members: Vec<Member>,
    pub base: String,
    pub image_digest: String,
    pub harness_version: String,
    pub selection: String,
}

impl PassIdentity {
    pub fn new(
        repo: &str,
        mut members: Vec<Member>,
        base: &str,
        image_digest: &str,
        harness_version: &str,
        selection: &str,
    ) -> Result<Self, ResourceError> {
        if members.is_empty() {
            return Err(ResourceError::NoMembers);
        }
        if members.len() > MAX_MEMBERS {
            return Err(ResourceError::TooManyMembers {
                max: MAX_MEMBERS,
                got: members.len(),
            });
        }
        members.sort();
        if let Some(w) = members.windows(2).find(|w| w[0].id == w[1].id) {
            return Err(ResourceError::DuplicateMember {
                id: w[0].id.clone(),
            });
        }
        Ok(PassIdentity {
            repo: repo.into(),
            members,
            base: base.into(),
            image_digest: image_digest.into(),
            harness_version: harness_version.into(),
            selection: selection.into(),
        })
    }

    /// The identity as JSON with its fields in declaration order and members sorted, so equal
    /// identities serialise to equal bytes.
    pub fn canonical(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// sha256 of [`Self::canonical`], lowercase hex.
    pub fn hash(&self) -> String {
        Sha256::digest(self.canonical().as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    pub fn has_member(&self, id: &str) -> bool {
        self.members.iter().any(|m| m.id == id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "cause")]
pub enum Cause {
    Image { digest: String },
    Base,
    Install,
    Vm,
    OverCap,
    FencesCap,
}

impl Cause {
    /// Base, Image and Install stop the repo's lane; the rest back the identity off.
    pub fn is_lane(&self) -> bool {
        matches!(self, Cause::Image { .. } | Cause::Base | Cause::Install)
    }

    pub fn word(&self) -> &'static str {
        match self {
            Cause::Image { .. } => "image",
            Cause::Base => "base",
            Cause::Install => "install",
            Cause::Vm => "vm",
            Cause::OverCap => "over-cap",
            Cause::FencesCap => "fences-cap",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "class")]
pub enum Class {
    Green,
    Red {
        step: String,
    },
    Fault {
        cause: Cause,
    },
    /// A red whose plain rerun was green.
    Flaky,
}

impl Class {
    /// Red and fault verdicts are kept while their members live; green and flaky with the round.
    pub fn retained_by_members(&self) -> bool {
        matches!(self, Class::Red { .. } | Class::Fault { .. })
    }

    pub fn word(&self) -> &'static str {
        match self {
            Class::Green => "green",
            Class::Red { .. } => "red",
            Class::Fault { .. } => "fault",
            Class::Flaky => "flaky",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InitiatedBy {
    Planner,
    Judge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SubPassKind {
    Rerun,
    Base,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubPass {
    pub kind: SubPassKind,
    pub green: bool,
}

/// One attempt's verdict, at `verdicts/<repo>/<hash>/attempt/<n>`, with its sub-passes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    #[serde(flatten)]
    pub class: Class,
    pub initiated_by: InitiatedBy,
    #[serde(default)]
    pub sub_passes: Vec<SubPass>,
    #[serde(default)]
    pub evidence: String,
    /// Epoch milliseconds on the controller's clock.
    pub at: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "backoff", content = "until")]
pub enum Backoff {
    /// Epoch milliseconds the identity may be cut again at.
    Until(u64),
    /// Retries exhausted: only a changed input admits it.
    Standing,
}

/// The identity's summary at `verdicts/<repo>/<hash>`, updated by CAS with each attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    #[serde(flatten)]
    pub class: Class,
    pub attempts: u32,
    /// Transient faults in a row, ending with the latest attempt; the back-off doubles on it.
    #[serde(default)]
    pub transient_faults: u32,
    #[serde(default)]
    pub backoff_until: Option<Backoff>,
    /// The latest attempt's `at`.
    pub last_attempt: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use state_store::limits::check;
    use state_store::{Compare, Object, Txn};

    fn m(id: &str, tip: &str) -> Member {
        Member {
            id: id.into(),
            tip: tip.into(),
        }
    }

    fn ident(members: Vec<Member>) -> Result<PassIdentity, ResourceError> {
        PassIdentity::new("lane", members, "b0", "sha256:i1", "h1", "all")
    }

    #[test]
    fn the_hash_is_independent_of_member_order_and_moves_with_every_input() {
        let ab = ident(vec![m("sp-a", "1"), m("sp-b", "2")]).unwrap();
        let ba = ident(vec![m("sp-b", "2"), m("sp-a", "1")]).unwrap();
        assert_eq!(ab.hash(), ba.hash());
        assert_eq!(ab.hash().len(), 64);
        assert!(ab.hash().bytes().all(|b| b.is_ascii_hexdigit()));
        let mut variants = vec![ident(vec![m("sp-a", "1"), m("sp-b", "3")]).unwrap()];
        for f in [
            |i: &mut PassIdentity| i.repo = "other".into(),
            |i: &mut PassIdentity| i.base = "b1".into(),
            |i: &mut PassIdentity| i.image_digest = "sha256:i2".into(),
            |i: &mut PassIdentity| i.harness_version = "h2".into(),
            |i: &mut PassIdentity| i.selection = "some".into(),
        ] {
            let mut v = ab.clone();
            f(&mut v);
            variants.push(v);
        }
        for v in variants {
            assert_ne!(v.hash(), ab.hash(), "{v:?}");
        }
    }

    #[test]
    fn an_identity_refuses_no_members_a_duplicate_and_one_over_the_cap() {
        assert_eq!(ident(vec![]), Err(ResourceError::NoMembers));
        assert_eq!(
            ident(vec![m("sp-a", "1"), m("sp-a", "2")]),
            Err(ResourceError::DuplicateMember { id: "sp-a".into() })
        );
        let n = |k: usize| (0..k).map(|i| m(&format!("sp-{i:03}"), "t")).collect();
        assert_eq!(ident(n(MAX_MEMBERS)).unwrap().members.len(), MAX_MEMBERS);
        assert_eq!(
            ident(n(MAX_MEMBERS + 1)),
            Err(ResourceError::TooManyMembers {
                max: MAX_MEMBERS,
                got: MAX_MEMBERS + 1
            })
        );
    }

    /// A cut's shape: the round, its lease and slot record, and two keys per member, with one
    /// compare per member beside the round's absence and the slot fence.
    fn cut(n: usize) -> Txn {
        let mut t = Txn::new()
            .when(Compare::ModRev("batches/lane/b1".into(), None))
            .when(Compare::CreateRev("locks/runner-slot/p/0".into(), None));
        for k in ["batches/lane/b1", "slots/p/0", "batches/lane/b1/progress"] {
            t = t.put(k, Object::new(serde_json::json!(1)));
        }
        for i in 0..n {
            t = t
                .when(Compare::ModRev(format!("lifecycle/sp-{i}"), None))
                .put(
                    &format!("lifecycle/sp-{i}"),
                    Object::new(serde_json::json!(1)),
                )
                .put(
                    &format!("readiness/sp-{i}"),
                    Object::new(serde_json::json!(1)),
                );
        }
        t
    }

    #[test]
    fn a_cut_at_max_members_fits_the_limits_and_one_more_is_refused() {
        assert_eq!(MAX_MEMBERS, 61);
        assert_eq!(cut(MAX_MEMBERS).ops.len(), 2 * MAX_MEMBERS + CUT_FIXED_OPS);
        assert_eq!(check(&cut(MAX_MEMBERS), TXN_BOOKKEEPING), Ok(()));
        assert!(matches!(
            check(&cut(MAX_MEMBERS + 1), TXN_BOOKKEEPING),
            Err(state_store::Error::Limit(state_store::Limit::Ops {
                max: 128,
                got: 129
            }))
        ));
    }

    #[test]
    fn a_round_is_read_with_its_members_and_an_absent_or_undecodable_one_is_named() {
        use runtime::ports::VirtualClock;
        use state_store::{Config, Contract, MemBackend, Request};
        let clock = VirtualClock::new(0);
        let c = Contract::open(
            MemBackend::new(),
            state_store::Ports::virtual_clock(&clock),
            Config {
                min_lock_ttl_ms: 1000,
            },
        )
        .unwrap();
        let mut round = Round {
            row: BatchRow::cut("b1", "lane", "h1", "b0"),
            members: vec![m("sp-a", "1")],
            tested_base: Some("b0".into()),
            last_lease_token: None,
        };
        round.row.pass = 2;
        let put = |k: &str, v: serde_json::Value| {
            c.txn(
                &Txn::new().put(k, Object::new(v)),
                &Request::new(k, "t", "t"),
            )
            .unwrap();
        };
        put("batches/lane/b1", serde_json::to_value(&round).unwrap());
        put("batches/lane/b2", serde_json::json!({ "state": 3 }));
        let v = RoundView::read(&c, "lane", "b1").unwrap().unwrap();
        assert_eq!((v.key.as_str(), &v.round), ("batches/lane/b1", &round));
        assert_eq!(RoundView::read(&c, "lane", "b9"), Ok(None));
        let e = RoundView::read(&c, "lane", "b2").unwrap_err();
        assert!(
            matches!(&e, ResourceError::Undecodable { key, .. } if key == "batches/lane/b2"),
            "{e}"
        );
        assert!(matches!(
            RoundView::read(&crate::real::Unserved, "lane", "b1"),
            Err(ResourceError::Store(_))
        ));
    }

    #[test]
    fn red_and_fault_are_kept_by_their_members_and_green_and_flaky_by_the_round() {
        assert!(Class::Red { step: "s".into() }.retained_by_members());
        assert!(Class::Fault { cause: Cause::Vm }.retained_by_members());
        assert!(!Class::Green.retained_by_members());
        assert!(!Class::Flaky.retained_by_members());
        assert!(Cause::Image { digest: "d".into() }.is_lane() && Cause::Base.is_lane());
        assert!(Cause::Install.is_lane());
        assert!(![Cause::Vm, Cause::OverCap, Cause::FencesCap]
            .iter()
            .any(Cause::is_lane));
    }
}
