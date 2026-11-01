//! The writes no machine event carries: a record's `repo`, and the blocking prerequisites
//! `dep-add` and `dep-remove` publish into `readiness/<id>`. Each is a txn that compares what
//! it read, re-planned when it loses, so no write overwrites another it never saw.

use serde_json::Value;
use state_store::{Backend, Compare, Error, Object, Request, Result, Txn};

use super::{object_like, sub_request, Store, MERGE_ATTEMPTS};
use crate::control::keys;
use crate::control::records::{decode, encode, LifecycleRecord, Readiness};

/// What `set-repo` found.
#[derive(Clone, Debug, PartialEq)]
pub enum RepoSet {
    NoRecord,
    /// `--only-if` named a repo the record does not carry.
    NotNamed,
    /// A WORKING record keeps the repo its holder was briefed with.
    Working,
    Set,
}

/// How a `dep-add` or `dep-remove` changes the prerequisites a record waits on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Edge {
    Blocks,
    /// An edge of a kind that does not block, or no edge at all.
    Unblocks,
}

impl<B: Backend> Store<B> {
    /// Sets or clears the record's `repo`, unless it is WORKING or `only_if` names another.
    pub fn set_repo(
        &self,
        id: &str,
        repo: Option<&str>,
        only_if: Option<&str>,
        req: &Request,
    ) -> Result<RepoSet> {
        let key = keys::lifecycle(id);
        for attempt in 0..MERGE_ATTEMPTS {
            let Some(item) = self.contract().get(&key)? else {
                return Ok(RepoSet::NoRecord);
            };
            let mut rec: LifecycleRecord = decode(&key, &item.value)?;
            if only_if.is_some_and(|want| rec.repo.as_deref() != Some(want)) {
                return Ok(RepoSet::NotNamed);
            }
            if rec.row.state == lifecycle::bead::BeadState::Working {
                return Ok(RepoSet::Working);
            }
            rec.repo = repo.map(str::to_string);
            let t = Txn::new()
                .when(Compare::ModRev(key.clone(), Some(item.mod_rev)))
                .put(&key, object_like(&item, encode(&rec)?));
            let sub = sub_request(req, &format!("set-repo/{attempt}"), Value::Null);
            if self.contract().txn(&t, &sub)?.succeeded {
                return Ok(RepoSet::Set);
            }
        }
        Err(Error::Backend(format!(
            "{key}: set-repo lost {MERGE_ATTEMPTS} races in a row"
        )))
    }

    /// Adds `dep` to, or takes it from, the prerequisites `id`'s readiness names as blocking it.
    /// A record that does not exist gets no readiness, as a dispatcher pass gives it none.
    /// `false` when there is no record.
    pub fn set_edge(&self, id: &str, dep: &str, edge: Edge, req: &Request) -> Result<bool> {
        let owner = keys::lifecycle(id);
        let key = keys::readiness(id);
        for attempt in 0..MERGE_ATTEMPTS {
            let head = self.contract().head()?;
            if self.contract().get_at(&owner, head)?.is_none() {
                return Ok(false);
            }
            let prior = self.contract().get_at(&key, head)?;
            let (mut r, meta) = match &prior {
                Some(item) => (decode::<Readiness>(&key, &item.value)?, item.value.clone()),
                None => (
                    Readiness::default(),
                    Object::default().with_owners(&[owner.as_str()]),
                ),
            };
            let before = r.blocked_by.clone();
            r.blocked_by.retain(|b| b != dep);
            if edge == Edge::Blocks {
                r.blocked_by.push(dep.to_string());
                r.blocked_by.sort();
            }
            if r.blocked_by == before && prior.is_some() {
                return Ok(true);
            }
            let t = Txn::new()
                .when(Compare::ModRev(
                    key.clone(),
                    prior.as_ref().map(|p| p.mod_rev),
                ))
                .put(
                    &key,
                    Object {
                        data: encode(&r)?,
                        ..meta
                    },
                );
            let sub = sub_request(req, &format!("edge/{attempt}"), Value::Null);
            if self.contract().txn(&t, &sub)?.succeeded {
                return Ok(true);
            }
        }
        Err(Error::Backend(format!(
            "{key}: an edge write lost {MERGE_ATTEMPTS} races in a row"
        )))
    }
}

#[cfg(test)]
mod tests {
    use runtime::ports::VirtualClock;
    use state_store::{Config, Contract, MemBackend, Ports, Request};

    use super::*;

    fn store() -> Store<MemBackend> {
        let c = Contract::open(
            MemBackend::new(),
            Ports::virtual_clock(&VirtualClock::new(0)),
            Config {
                min_lock_ttl_ms: 1_000,
            },
        )
        .unwrap();
        Store::new(c, 1_000)
    }

    fn req(id: &str) -> Request {
        Request::new(id, "test", "t")
    }

    #[test]
    fn an_edge_is_kept_sorted_once_and_a_non_blocking_kind_takes_it_back() {
        let s = store();
        assert!(!s
            .set_edge("sp-a", "sp-b", Edge::Blocks, &req("e0"))
            .unwrap());
        assert!(
            s.readiness("sp-a").unwrap().is_none(),
            "no record, no readiness"
        );
        s.create("sp-a", Some("harness"), None, 100, &req("c"))
            .unwrap();
        for (n, dep) in ["sp-c", "sp-b", "sp-c"].iter().enumerate() {
            assert!(s
                .set_edge("sp-a", dep, Edge::Blocks, &req(&format!("e{n}")))
                .unwrap());
        }
        let r = s.readiness("sp-a").unwrap().unwrap();
        assert_eq!(r.value.blocked_by, ["sp-b", "sp-c"]);
        let item = s.contract().get("readiness/sp-a").unwrap().unwrap();
        assert_eq!(item.value.metadata.owner_references, ["lifecycle/sp-a"]);
        s.set_edge("sp-a", "sp-c", Edge::Unblocks, &req("e9"))
            .unwrap();
        assert_eq!(
            s.readiness("sp-a").unwrap().unwrap().value.blocked_by,
            ["sp-b"]
        );
    }

    #[test]
    fn an_edge_write_keeps_the_verdict_a_pass_published() {
        let s = store();
        s.create("sp-a", None, None, 100, &req("c")).unwrap();
        let v = super::super::Verdict {
            id: "sp-a".into(),
            verdict: "blocked".into(),
            blocker: Some("sp-b".into()),
            runes: vec![],
        };
        s.publish_verdicts(&[v], "2026-01-01T00:00:00Z", &req("p"))
            .unwrap();
        s.set_edge("sp-a", "sp-b", Edge::Blocks, &req("e")).unwrap();
        let r = s.readiness("sp-a").unwrap().unwrap().value;
        assert_eq!(
            (r.verdict.as_deref(), r.blocked_by),
            (Some("blocked"), vec!["sp-b".to_string()])
        );
    }

    #[test]
    fn set_repo_changes_only_the_repo_and_leaves_a_working_record_alone() {
        let s = store();
        assert_eq!(
            s.set_repo("sp-a", Some("x"), None, &req("r0")).unwrap(),
            RepoSet::NoRecord
        );
        s.create("sp-a", Some("harness"), None, 100, &req("c"))
            .unwrap();
        let before = s.record("sp-a").unwrap().unwrap();
        assert_eq!(
            s.set_repo("sp-a", Some("x"), Some("other"), &req("r1"))
                .unwrap(),
            RepoSet::NotNamed
        );
        assert_eq!(
            s.set_repo("sp-a", Some("x"), Some("harness"), &req("r2"))
                .unwrap(),
            RepoSet::Set
        );
        let after = s.record("sp-a").unwrap().unwrap();
        assert_eq!(after.value.repo.as_deref(), Some("x"));
        assert_eq!(after.value.row, before.value.row);
        let item = s.contract().get("lifecycle/sp-a").unwrap().unwrap();
        assert_eq!(item.value.metadata.finalizers, [keys::PLANNING_CLOSED]);
        s.set_repo("sp-a", None, None, &req("r3")).unwrap();
        assert_eq!(s.record("sp-a").unwrap().unwrap().value.repo, None);
    }
}
