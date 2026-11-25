//! The reference backend: the live row of every key, and recent history in a bounded
//! [`History`] ring. A clone shares the same store, which is how a conformance rig reopens it to
//! stage a restart.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::backend::{expires_at_of, owners_of, Backend, LogEntry, Version, Write};
use crate::history::{overlay, AsOf, History, Written};
use crate::{Error, Result, Rev};

#[derive(Clone)]
pub struct MemBackend(Arc<Mutex<State>>);

impl Default for MemBackend {
    fn default() -> Self {
        Self::with_history_bytes(usize::MAX)
    }
}

struct State {
    head: u64,
    live: BTreeMap<String, Version>,
    history: History,
    /// `(owner, key)` for every live key at the head, kept in `commit`.
    owned: BTreeSet<(String, String)>,
    /// `(expiresAt, key)` for every live key at the head that carries one, kept in `commit`.
    expiring: BTreeSet<(u64, String)>,
}

impl MemBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// A store whose history ring holds at most `bytes`.
    pub fn with_history_bytes(bytes: usize) -> Self {
        MemBackend(Arc::new(Mutex::new(State {
            head: 0,
            live: BTreeMap::new(),
            history: History::new(0, bytes),
            owned: BTreeSet::new(),
            expiring: BTreeSet::new(),
        })))
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl State {
    fn readable(&self, at: u64) -> Result<()> {
        self.history.readable(at)?;
        if at > self.head {
            return Err(Error::FutureRev {
                rev: at,
                head: self.head,
            });
        }
        Ok(())
    }
}

impl Backend for MemBackend {
    fn head(&self) -> Result<u64> {
        Ok(self.state().head)
    }

    fn get(&self, key: &str, at: u64) -> Result<Option<Version>> {
        let s = self.state();
        s.readable(at)?;
        Ok(match s.history.as_of(key, at)? {
            AsOf::Live => s.live.get(key).cloned(),
            AsOf::Was(v) => v,
        })
    }

    fn range(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        let s = self.state();
        s.readable(at)?;
        let start = match after {
            Some(a) if a >= prefix => std::ops::Bound::Excluded(a.to_string()),
            _ => std::ops::Bound::Included(prefix.to_string()),
        };
        let mut hi = prefix.as_bytes().to_vec();
        let hi = loop {
            match hi.pop() {
                None => break None,
                Some(b) if b < 0xff => {
                    hi.push(b + 1);
                    break Some(hi);
                }
                Some(_) => {}
            }
        };
        let live = s
            .live
            .range((start, std::ops::Bound::Unbounded))
            .take_while(|(k, _)| k.starts_with(prefix));
        if at == s.head {
            return Ok(live.take(limit).map(|(k, v)| (k.clone(), v.clone())).collect());
        }
        let live: Vec<_> = live.map(|(k, v)| (k.clone(), v.clone())).collect();
        let touched = s.history.touched_after(prefix.as_bytes(), hi.as_deref(), at)?;
        let mut rows = overlay(live, &touched, after.filter(|a| *a >= prefix), None);
        rows.retain(|(k, _)| k.starts_with(prefix));
        rows.truncate(limit);
        Ok(rows)
    }

    fn owned_by(
        &self,
        owner: &str,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<String>> {
        let s = self.state();
        if at != s.head {
            return Err(Error::Stale {
                expected: at,
                head: s.head,
            });
        }
        let from = (owner.to_string(), after.map_or(String::new(), |a| format!("{a}\0")));
        Ok(s.owned
            .range(from..)
            .take_while(|(o, _)| o == owner)
            .map(|(_, k)| k.clone())
            .take(limit)
            .collect())
    }

    fn due(&self, now: u64, limit: usize, at: u64) -> Result<Vec<String>> {
        let s = self.state();
        if at != s.head {
            return Err(Error::Stale {
                expected: at,
                head: s.head,
            });
        }
        Ok(s.expiring
            .range(..(now.saturating_add(1), String::new()))
            .map(|(_, k)| k.clone())
            .take(limit)
            .collect())
    }

    fn commit(&self, expect: u64, batch: &[Write]) -> Result<u64> {
        let mut s = self.state();
        if s.head != expect {
            return Err(Error::Stale {
                expected: expect,
                head: s.head,
            });
        }
        let main = expect + 1;
        let mut written = Vec::with_capacity(batch.len());
        for (i, w) in batch.iter().enumerate() {
            let rev = Rev::new(main, i as u32);
            let (version, entry) = match w {
                Write::Put {
                    key,
                    value,
                    create_rev,
                    prev,
                } => (
                    Some(Version {
                        value: value.clone(),
                        mod_rev: rev,
                        create_rev: *create_rev,
                    }),
                    LogEntry {
                        key: key.clone(),
                        rev,
                        value: Some(value.clone()),
                        create_rev: Some(*create_rev),
                        prev: *prev,
                    },
                ),
                Write::Delete { key, prev } => (
                    None,
                    LogEntry {
                        key: key.clone(),
                        rev,
                        value: None,
                        create_rev: None,
                        prev: Some(*prev),
                    },
                ),
            };
            let key = w.key().to_string();
            let previous = match version {
                Some(v) => s.live.insert(key.clone(), v),
                None => s.live.remove(&key),
            };
            let prev = previous.as_ref().map(|v| v.value.as_slice());
            if let Some(at) = prev.and_then(expires_at_of) {
                s.expiring.remove(&(at, key.clone()));
            }
            if let Write::Put { value, .. } = w {
                if let Some(at) = expires_at_of(value) {
                    s.expiring.insert((at, key.clone()));
                }
            }
            for o in prev.map(owners_of).unwrap_or_default() {
                s.owned.remove(&(o, key.clone()));
            }
            if let Write::Put { value, .. } = w {
                for o in owners_of(value) {
                    s.owned.insert((o, key.clone()));
                }
            }
            written.push(Written { entry, previous });
        }
        s.history.push(main, written);
        s.head = main;
        Ok(main)
    }

    fn log(&self, from: u64, max_mains: usize) -> Result<Vec<LogEntry>> {
        let s = self.state();
        s.history.log(from, s.head, max_mains)
    }

    fn compacted(&self) -> Result<u64> {
        Ok(self.state().history.floor())
    }

    fn compact(&self, rev: u64) -> Result<()> {
        let mut s = self.state();
        let rev = rev.min(s.head);
        s.history.forget_through(rev);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(key: &str, main: u64) -> Write {
        Write::Put {
            key: key.into(),
            value: b"v".to_vec(),
            create_rev: Rev::new(main, 0),
            prev: None,
        }
    }

    #[test_id(ttosisa)]
    #[test]
    fn the_log_returns_whole_txns_and_stops_at_the_page() {
        let b = MemBackend::new();
        b.commit(0, &[put("a", 1), put("b", 1)]).unwrap();
        b.commit(1, &[put("c", 2)]).unwrap();
        let page = b.log(0, 1).unwrap();
        assert_eq!(
            page.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(b.log(1, 1).unwrap().len(), 1);
        assert!(b.log(0, 0).unwrap().is_empty());
    }

    #[test_id(tqv9ru7)]
    #[test]
    fn forgetting_history_twice_never_lowers_the_floor() {
        let b = MemBackend::new();
        b.commit(0, &[put("a", 1)]).unwrap();
        b.commit(
            1,
            &[Write::Delete {
                key: "a".into(),
                prev: Rev::new(1, 0),
            }],
        )
        .unwrap();
        b.compact(2).unwrap();
        assert!(b.state().live.is_empty());
        b.compact(1).unwrap();
        assert_eq!(b.get("a", 1), Err(Error::TooOld { compacted: 2 }));
        assert_eq!(b.get("a", 3), Err(Error::FutureRev { rev: 3, head: 2 }));
    }

    #[test_id(tez7ol7)]
    #[test]
    fn a_revision_the_ring_has_turned_past_is_too_old_to_read_or_watch() {
        use crate::contract::{Config, Ports, Request, Txn};
        use crate::{Contract, Object, Watched};
        use runtime::ports::VirtualClock;
        let clock = VirtualClock::new(1_000_000);
        let c = Contract::open(
            MemBackend::with_history_bytes(4_000),
            Ports::virtual_clock(&clock),
            Config { min_lock_ttl_ms: 1_000 },
        )
        .unwrap();
        let put = |i: usize| {
            let t = Txn::new().put("t/a", Object::new(serde_json::json!(i)));
            c.txn(&t, &Request::new(&format!("r{i}"), "t", "t.write")).unwrap().rev
        };
        let r1 = put(0);
        assert!(c.get_at("t/a", r1).is_ok());
        for i in 1..60 {
            put(i);
        }
        let floor = c.backend().compacted().unwrap();
        assert!(floor > r1);
        assert_eq!(c.get_at("t/a", r1), Err(Error::TooOld { compacted: floor }));
        assert_eq!(c.watch("t/", r1, 0), Err(Error::TooOld { compacted: floor }));
        assert!(matches!(c.watch("t/", floor, 0), Ok(Watched::Events { .. })));
    }
}
