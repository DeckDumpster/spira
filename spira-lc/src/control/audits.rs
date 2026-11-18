//! The audit log read by time: every applied or refused machine event at or after a floor,
//! newest revisions first, or the whole log in order. The answer is complete or says it is not;
//! an unfinished scan is never read as "no events".

use serde_json::Value;
use state_store::{events_key, Backend, Item, Result, EVENTS_PREFIX};

use super::dwell::branch;
use super::feed::branch_audits;
use super::records::Audit;
use super::store::Store;

/// Revisions read per chunk of the newest-first scan.
const CHUNK_REVS: u64 = 500;
const PAGE: usize = 1_000;
/// The most `events/` keys one answer reads.
pub const MAX_SCANNED: usize = 200_000;

#[derive(Clone, Debug, PartialEq)]
pub struct Logged {
    pub rev: u64,
    pub audit: Audit,
}

#[derive(Debug)]
pub struct Scanned {
    pub audits: Vec<Logged>,
    /// The scan reached `floor` or the start of the log.
    pub complete: bool,
}

/// One audit read in place from its `events/` key: the fields the ops views fold, borrowed, with
/// no evidence and no allocation.
#[derive(Clone, Copy, Debug)]
pub struct Move<'a> {
    pub machine: &'a str,
    pub lc_key: &'a str,
    pub event: &'a str,
    pub from_state: &'a str,
    pub to_state: &'a str,
    pub applied: bool,
    pub refusal: Option<&'a str>,
    pub at: i64,
}

fn move_of(a: &Value) -> Option<Move<'_>> {
    Some(Move {
        machine: a["machine"].as_str()?,
        lc_key: a["lc_key"].as_str()?,
        event: a["event"].as_str()?,
        from_state: a["from_state"].as_str()?,
        to_state: a["to_state"].as_str()?,
        applied: a["applied"].as_bool()?,
        refusal: a["refusal"].as_str(),
        at: a["at"].as_i64()?,
    })
}

impl<B: Backend> Store<B> {
    /// Walks the log newest chunk first, `visit` given each chunk's `events/` items oldest first
    /// (at most one chunk is held). True when the walk reached `floor` or the start of the log.
    fn walk_chunks(&self, floor: i64, mut visit: impl FnMut(&[Item])) -> Result<bool> {
        let head = self.contract().head()?;
        let (mut hi, mut scanned, mut span) = (head, 0usize, CHUNK_REVS);
        while scanned < MAX_SCANNED {
            let lo = hi.saturating_sub(span);
            let mut chunk: Vec<Item> = Vec::new();
            let mut after = (lo > 0).then(|| events_key(lo));
            let end = events_key(hi);
            'chunk: loop {
                let page = self.contract().range_at(EVENTS_PREFIX, after.as_deref(), PAGE, head)?;
                let full = page.len() == PAGE;
                after = page.last().map(|i| i.key.clone());
                for item in page {
                    if item.key > end {
                        break 'chunk;
                    }
                    scanned += 1;
                    chunk.push(item);
                }
                if !full {
                    break;
                }
            }
            let oldest = chunk.iter().flat_map(branch).filter_map(|a| a["at"].as_i64()).min();
            visit(&chunk);
            if lo == 0 || oldest.is_some_and(|o| o < floor) {
                return Ok(true);
            }
            span = if oldest.is_some() { CHUNK_REVS } else { (span * 2).min(CHUNK_REVS * 64) };
            hi = lo;
        }
        Ok(false)
    }

    /// Every audit with `at >= floor`, newest first, folded by `visit` where it lies: the window
    /// is never held, only one chunk of revisions. True when the scan is complete.
    pub fn fold_audits_since(&self, floor: i64, mut visit: impl FnMut(Move<'_>)) -> Result<bool> {
        self.walk_chunks(floor, |chunk| {
            for item in chunk.iter().rev() {
                for a in branch(item).iter().rev() {
                    if let Some(m) = move_of(a).filter(|m| m.at >= floor) {
                        visit(m);
                    }
                }
            }
        })
    }

    /// The audits with `at >= floor`, oldest first.
    pub fn audits_since(&self, floor: i64) -> Result<Scanned> {
        let mut audits: Vec<Logged> = Vec::new();
        let complete = self.walk_chunks(floor, |chunk| {
            for item in chunk {
                for audit in branch_audits(item) {
                    if audit.at >= floor {
                        audits.push(Logged { rev: item.mod_rev.main, audit });
                    }
                }
            }
        })?;
        audits.sort_by_key(|l| l.rev);
        Ok(Scanned { audits, complete })
    }

    /// Every audit the store holds, oldest first: the imported history by its old seq, then each
    /// revision's. `after` skips what is at or below that revision.
    pub fn audits_after(&self, after: u64) -> Result<Scanned> {
        let head = self.contract().head()?;
        let mut audits: Vec<Logged> = Vec::new();
        let imported = if after == 0 { self.contract().list(&[super::keys::HISTORY], &state_store::Selector::Everything)?.0 } else { Vec::new() };
        for i in &imported {
            let d = &i.value.data;
            if let (Some(seq), Ok(audit)) = (d["seq"].as_u64(), serde_json::from_value::<Audit>(d["audit"].clone())) {
                if seq > after {
                    audits.push(Logged { rev: seq, audit });
                }
            }
        }
        let mut cursor = (after > 0).then(|| events_key(after));
        loop {
            let page = self.contract().range_at(EVENTS_PREFIX, cursor.as_deref(), PAGE, head)?;
            for item in &page {
                audits.extend(branch_audits(item).into_iter().map(|audit| Logged { rev: item.mod_rev.main, audit }));
            }
            if page.len() < PAGE {
                break;
            }
            cursor = page.last().map(|i| i.key.clone());
        }
        audits.sort_by_key(|l| l.rev);
        Ok(Scanned { audits, complete: true })
    }
}

#[cfg(test)]
mod tests {
    use runtime::ports::VirtualClock;
    use state_store::{Config, Contract, MemBackend, Ports, Request};

    use super::*;

    fn store() -> Store<MemBackend> {
        let c = Contract::open(MemBackend::new(), Ports::virtual_clock(&VirtualClock::new(0)), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        Store::new(c, 1_000)
    }

    #[test]
    fn the_scan_returns_the_audits_at_or_after_the_floor_and_says_it_reached_it() {
        let s = store();
        s.create("sp-a", None, Some("0123456789abcdef0123456789abcdef01234567"), 100, &Request::new("c1", "t", "t")).unwrap();
        s.create("sp-b", None, Some("0123456789abcdef0123456789abcdef01234567"), 200, &Request::new("c2", "t", "t")).unwrap();
        let all = s.audits_since(0).unwrap();
        assert!(all.complete);
        let keys: Vec<&str> = all.audits.iter().filter(|l| l.audit.applied).map(|l| l.audit.lc_key.as_str()).collect();
        assert!(keys.contains(&"sp-a") && keys.contains(&"sp-b"), "{keys:?}");
        let later = s.audits_since(150).unwrap();
        assert!(later.complete);
        assert!(later.audits.iter().all(|l| l.audit.at >= 150), "{:?}", later.audits);
        assert!(later.audits.iter().all(|l| l.audit.lc_key != "sp-a"));
    }

    #[test]
    fn audits_after_a_revision_skip_it_and_everything_before() {
        let s = store();
        s.create("sp-a", None, Some("0123456789abcdef0123456789abcdef01234567"), 100, &Request::new("c1", "t", "t")).unwrap();
        let first = s.audits_after(0).unwrap().audits.iter().map(|l| l.rev).max().unwrap();
        s.create("sp-b", None, Some("0123456789abcdef0123456789abcdef01234567"), 200, &Request::new("c2", "t", "t")).unwrap();
        let rest = s.audits_after(first).unwrap();
        assert!(rest.audits.iter().all(|l| l.rev > first && l.audit.lc_key == "sp-b"), "{:?}", rest.audits);
        assert!(!rest.audits.is_empty());
    }
}
