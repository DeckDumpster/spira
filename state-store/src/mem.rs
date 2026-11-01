//! The reference backend: every version kept in memory until compaction. A clone shares the
//! same store, which is how a conformance rig reopens it to stage a restart.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::backend::{Backend, LogEntry, Version, Write};
use crate::{Error, Result, Rev};

#[derive(Clone, Default)]
pub struct MemBackend(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    head: u64,
    compacted: u64,
    /// Each key's versions in main order; `None` is a delete.
    keys: BTreeMap<String, Vec<(u64, Option<Version>)>>,
    log: Vec<LogEntry>,
}

impl MemBackend {
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl State {
    fn readable(&self, at: u64) -> Result<()> {
        if at < self.compacted {
            return Err(Error::TooOld {
                compacted: self.compacted,
            });
        }
        if at > self.head {
            return Err(Error::FutureRev {
                rev: at,
                head: self.head,
            });
        }
        Ok(())
    }
}

fn visible(versions: &[(u64, Option<Version>)], at: u64) -> Option<&Version> {
    versions
        .iter()
        .rev()
        .find(|(m, _)| *m <= at)
        .and_then(|(_, v)| v.as_ref())
}

impl Backend for MemBackend {
    fn head(&self) -> Result<u64> {
        Ok(self.state().head)
    }

    fn get(&self, key: &str, at: u64) -> Result<Option<Version>> {
        let s = self.state();
        s.readable(at)?;
        Ok(s.keys.get(key).and_then(|v| visible(v, at)).cloned())
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
            Some(a) => std::ops::Bound::Excluded(a.to_string()),
            None => std::ops::Bound::Included(prefix.to_string()),
        };
        Ok(s.keys
            .range((start, std::ops::Bound::Unbounded))
            .take_while(|(k, _)| k.starts_with(prefix))
            .filter_map(|(k, v)| visible(v, at).map(|v| (k.clone(), v.clone())))
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
            s.keys
                .entry(w.key().to_string())
                .or_default()
                .push((main, version));
            s.log.push(entry);
        }
        s.head = main;
        Ok(main)
    }

    fn log(&self, from: u64, max_mains: usize) -> Result<Vec<LogEntry>> {
        let s = self.state();
        if from < s.compacted {
            return Err(Error::TooOld {
                compacted: s.compacted,
            });
        }
        let first = s.log.partition_point(|e| e.rev.main <= from);
        let mut mains = 0;
        let mut last = 0;
        let mut out = Vec::new();
        for e in &s.log[first..] {
            if e.rev.main != last {
                if mains == max_mains {
                    break;
                }
                mains += 1;
                last = e.rev.main;
            }
            out.push(e.clone());
        }
        Ok(out)
    }

    fn compact(&self, rev: u64) -> Result<()> {
        let mut s = self.state();
        let rev = rev.min(s.head);
        if rev <= s.compacted {
            return Ok(());
        }
        s.compacted = rev;
        s.log.retain(|e| e.rev.main > rev);
        s.keys.retain(|_, versions| {
            let keep_from = versions.iter().rposition(|(m, _)| *m <= rev).unwrap_or(0);
            versions.drain(..keep_from);
            if versions
                .first()
                .is_some_and(|(m, v)| *m <= rev && v.is_none())
            {
                versions.remove(0);
            }
            !versions.is_empty()
        });
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

    #[test]
    fn compaction_drops_a_deleted_key_and_a_second_compaction_below_it_is_a_no_op() {
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
        assert!(b.state().keys.is_empty());
        b.compact(1).unwrap();
        assert_eq!(b.get("a", 1), Err(Error::TooOld { compacted: 2 }));
        assert_eq!(b.get("a", 3), Err(Error::FutureRev { rev: 3, head: 2 }));
    }
}
