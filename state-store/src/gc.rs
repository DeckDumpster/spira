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
