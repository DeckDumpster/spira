//! The GC controller: one `tick(now)` removes finalized keys, collects owner-reference orphans
//! and expires keys whose TTL has run out.

use std::collections::BTreeSet;

use crate::backend::Backend;
use crate::contract::Contract;
use crate::{Item, Result};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tick {
    pub finalized: Vec<String>,
    pub orphans: Vec<String>,
    pub expired: Vec<String>,
}

pub struct Controller<'a, B: Backend> {
    contract: &'a Contract<B>,
}

fn finalized(i: &Item) -> bool {
    i.value.metadata.deletion_timestamp.is_some() && i.value.metadata.finalizers.is_empty()
}

impl<'a, B: Backend> Controller<'a, B> {
    pub fn new(contract: &'a Contract<B>) -> Self {
        Controller { contract }
    }

    /// `now_ms` is on the contract's monotonic clock.
    pub fn tick(&self, now_ms: u64) -> Result<Tick> {
        let c = self.contract;
        let (items, _) = c.gc_items()?;
        let ready: Vec<String> = items
            .iter()
            .filter(|i| finalized(i))
            .map(|i| i.key.clone())
            .collect();
        let finalized = c.collect(&ready, "gc.finalize", |i, _| Ok(finalized(i)))?;

        let present: BTreeSet<&str> = items.iter().map(|i| i.key.as_str()).collect();
        let orphan = |i: &Item, alive: &dyn Fn(&str) -> Result<bool>| -> Result<bool> {
            let m = &i.value.metadata;
            if m.owner_references.is_empty() || m.deletion_timestamp.is_some() {
                return Ok(false);
            }
            for o in &m.owner_references {
                if alive(o)? {
                    return Ok(false);
                }
            }
            Ok(true)
        };
        let mut candidates = Vec::new();
        for i in &items {
            if orphan(i, &|o| {
                Ok(present.contains(o) && !finalized.iter().any(|f| f == o))
            })? {
                candidates.push(i.key.clone());
            }
        }
        let orphans = c.collect(&candidates, "gc.orphan", |i, head| {
            orphan(i, &|o| c.exists_at(o, head))
        })?;

        let due = c.due(now_ms);
        let expired = c.expire(&due, now_ms)?;
        Ok(Tick {
            finalized,
            orphans,
            expired,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{LogEntry, Version, Write};
    use crate::contract::{Config, Ports, Request, Txn};
    use crate::{MemBackend, Object, Rev};
    use runtime::ports::VirtualClock;
    use serde_json::json;

    /// Refuses a scan of the whole key space, so a caller has to go through `gc_range`.
    struct NoFullScan(MemBackend);

    impl Backend for NoFullScan {
        fn head(&self) -> Result<u64> {
            self.0.head()
        }
        fn get(&self, key: &str, at: u64) -> Result<Option<Version>> {
            self.0.get(key, at)
        }
        fn range(
            &self,
            prefix: &str,
            after: Option<&str>,
            limit: usize,
            at: u64,
        ) -> Result<Vec<(String, Version)>> {
            assert!(!prefix.is_empty(), "a scan of every key, events/ included");
            self.0.range(prefix, after, limit, at)
        }
        fn commit(&self, expect: u64, batch: &[Write]) -> Result<u64> {
            self.0.commit(expect, batch)
        }
        fn log(&self, from: u64, max_mains: usize) -> Result<Vec<LogEntry>> {
            self.0.log(from, max_mains)
        }
        fn compact(&self, rev: u64) -> Result<()> {
            self.0.compact(rev)
        }
        fn gc_range(
            &self,
            after: Option<&str>,
            limit: usize,
            at: u64,
        ) -> Result<Vec<(String, Version)>> {
            self.0.gc_range(after, limit, at)
        }
    }

    fn put(key: &str, main: u64) -> Write {
        Write::Put {
            key: key.into(),
            value: b"{}".to_vec(),
            create_rev: Rev::new(main, 0),
            prev: None,
        }
    }

    #[test]
    fn open_and_tick_read_the_key_space_through_gc_range_never_a_full_scan() {
        let clock = VirtualClock::new(1_000);
        let mem = MemBackend::new();
        let cfg = Config {
            min_lock_ttl_ms: 1_000,
        };
        let c = Contract::open(NoFullScan(mem.clone()), Ports::virtual_clock(&clock), cfg).unwrap();
        let req = |id: &str| Request::new(id, "t", "t.write");
        let t = Txn::new()
            .put("owner", Object::new(json!(1)))
            .put("child", Object::new(json!(2)).with_owners(&["owner"]))
            .put("intents/x", Object::new(json!(3)).with_ttl(60));
        assert!(c.txn(&t, &req("1")).unwrap().succeeded);
        assert!(
            c.txn(&Txn::new().delete("owner"), &req("2"))
                .unwrap()
                .succeeded
        );
        drop(c);
        let c = Contract::open(NoFullScan(mem), Ports::virtual_clock(&clock), cfg).unwrap();
        clock.advance(60_000);
        let tick = Controller::new(&c).tick(c.now_ms()).unwrap();
        assert_eq!(tick.orphans, ["child"]);
        assert_eq!(tick.expired, ["intents/x"]);
    }

    #[test]
    fn the_default_gc_range_skips_events_and_pages_across_them() {
        let b = MemBackend::new();
        let keys = ["a", "events/1", "events/2", "events/3", "f", "g"];
        b.commit(0, &keys.iter().map(|k| put(k, 1)).collect::<Vec<_>>())
            .unwrap();
        let mut got = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let page = b.gc_range(after.as_deref(), 1, 1).unwrap();
            let Some((k, _)) = page.last() else { break };
            after = Some(k.clone());
            got.push(k.clone());
        }
        assert_eq!(got, ["a", "f", "g"]);
        assert_eq!(
            b.gc_range(None, 10, 1)
                .unwrap()
                .iter()
                .map(|(k, _)| k.as_str())
                .collect::<Vec<_>>(),
            ["a", "f", "g"]
        );
    }
}
