//! The audit log read by time: every applied or refused machine event at or after a floor,
//! newest revisions first, or the whole log in order. The answer is complete or says it is not;
//! an unfinished scan is never read as "no events".

use serde_json::Value;
use state_store::{events_key, Backend, Error, Item, Result, EVENTS_PREFIX};

use super::dwell::branch;
use super::feed::branch_audits;
use super::records::Audit;
use super::store::Store;

/// Revisions read per chunk of the newest-first scan.
pub(super) const CHUNK_REVS: u64 = 100;
const PAGE: usize = 1_000;
const EVENTS_PER_READ: usize = 100;
/// The most `events/` keys one answer reads.
pub const MAX_SCANNED: usize = 200_000;
/// The most `events/` keys one limited read past a revision reads, so it stays inside the store's read deadline.
pub const AUDIT_SCAN_KEYS: usize = 1_000;

#[derive(Clone, Debug, PartialEq)]
pub struct Logged {
    pub rev: u64,
    pub audit: Audit,
}

#[derive(Debug)]
pub struct Scanned {
    pub audits: Vec<Logged>,
    /// Where to resume a read that stopped at its key cap short of the head.
    pub next: Option<u64>,
}

/// One audit read in place from its `events/` key: the fields the ops views fold, borrowed, with
/// no evidence and no allocation.
#[derive(Clone, Copy, Debug)]
pub struct Move<'a> {
    pub rev: u64,
    pub machine: &'a str,
    pub lc_key: &'a str,
    pub event: &'a str,
    pub from_state: &'a str,
    pub to_state: &'a str,
    pub applied: bool,
    pub refusal: Option<&'a str>,
    pub actor: &'a str,
    pub at: i64,
}

pub(super) fn move_of(rev: u64, a: &Value) -> Option<Move<'_>> {
    Some(Move {
        rev,
        machine: a["machine"].as_str()?,
        lc_key: a["lc_key"].as_str()?,
        event: a["event"].as_str()?,
        from_state: a["from_state"].as_str()?,
        to_state: a["to_state"].as_str()?,
        applied: a["applied"].as_bool()?,
        refusal: a["refusal"].as_str(),
        actor: a["actor"].as_str()?,
        at: a["at"].as_i64()?,
    })
}

impl<B: Backend> Store<B> {
    /// Walks the log newest chunk first, `visit` given each chunk's `events/` items oldest first
    /// (at most one chunk is held). True when the walk reached `floor` or the start of the log,
    /// or `visit` answered true to stop.
    fn walk_chunks(&self, floor: i64, mut visit: impl FnMut(&[Item]) -> bool) -> Result<bool> {
        let head = self.contract().head()?;
        let (mut hi, mut scanned, mut span) = (head, 0usize, CHUNK_REVS);
        while scanned < MAX_SCANNED {
            let lo = hi.saturating_sub(span);
            let mut chunk: Vec<Item> = Vec::new();
            let mut after = (lo > 0).then(|| events_key(lo));
            let end = events_key(hi);
            let limit = PAGE.min((hi - lo) as usize).max(1);
            let empty = self.contract().range_at(EVENTS_PREFIX, after.as_deref(), 1, head)?.first().is_none_or(|i| i.key > end);
            if !empty && span > CHUNK_REVS {
                span = CHUNK_REVS;
                continue;
            }
            'chunk: loop {
                if empty {
                    break;
                }
                let page = self.contract().range_at(EVENTS_PREFIX, after.as_deref(), limit, head)?;
                let full = page.len() == limit;
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
            if visit(&chunk) || lo == 0 || oldest.is_some_and(|o| o < floor) {
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
                    if let Some(m) = move_of(item.mod_rev.main, a).filter(|m| m.at >= floor) {
                        visit(m);
                    }
                }
            }
            false
        })
    }

    /// The largest `(at, lc_key)` among audits with `at >= floor` that `want` accepts, read from
    /// the newest chunk holding one. `Err` when the walk is cut short; `Ok(None)` when none exists.
    pub fn newest_audit(&self, floor: i64, want: impl Fn(&Move<'_>) -> bool) -> Result<Option<(i64, String)>> {
        let mut best: Option<(i64, String)> = None;
        let done = self.walk_chunks(floor, |chunk| {
            for item in chunk {
                for a in branch(item) {
                    if let Some(m) = move_of(item.mod_rev.main, a).filter(|m| m.at >= floor && want(m)) {
                        if best.as_ref().is_none_or(|(at, _)| m.at > *at) {
                            best = Some((m.at, m.lc_key.to_string()));
                        }
                    }
                }
            }
            best.is_some()
        })?;
        if !done && best.is_none() {
            return Err(Error::Backend("the scan for the newest audit was cut short".into()));
        }
        Ok(best)
    }

    /// Every audit the store holds, oldest first: the imported history by its old seq, then each
    /// revision's. `after` skips what is at or below that revision.
    pub fn audits_after(&self, after: u64) -> Result<Scanned> {
        self.read_audits(after, usize::MAX, usize::MAX)
    }

    /// The first `limit` audits past `after`, oldest first: the log is read only as far as they reach,
    /// and past a revision no further than [`AUDIT_SCAN_KEYS`] keys.
    pub fn audits_after_upto(&self, after: u64, limit: usize) -> Result<Scanned> {
        let cap = if after > 0 { AUDIT_SCAN_KEYS } else { usize::MAX };
        self.read_audits(after, limit, cap)
    }

    fn read_audits(&self, after: u64, limit: usize, cap: usize) -> Result<Scanned> {
        let head = self.contract().head()?;
        let mut audits: Vec<Logged> = Vec::new();
        let mut seq_cursor = 0u64;
        while after == 0 && audits.len() < limit {
            let want = PAGE.min(limit - audits.len());
            let page = self.contract().range_by_seq(super::keys::HISTORY, seq_cursor, want, head)?;
            for i in &page {
                let d = &i.value.data;
                if let (Some(seq), Ok(audit)) = (d["seq"].as_u64(), serde_json::from_value::<Audit>(d["audit"].clone())) {
                    audits.push(Logged { rev: seq, audit });
                }
            }
            let last = page.last().and_then(|i| i.value.data["seq"].as_u64());
            match last {
                Some(seq) if page.len() == want => seq_cursor = seq,
                _ => break,
            }
        }
        let mut cursor = (after > 0).then(|| events_key(after));
        let (mut keys, mut next) = (0usize, None);
        while audits.len() < limit {
            // Past a revision, pages stay full: shrinking them to the audits still wanted costs a read per key.
            let want = EVENTS_PER_READ.min(if after > 0 { cap - keys } else { limit - audits.len() });
            let page = self.contract().range_at(EVENTS_PREFIX, cursor.as_deref(), want, head)?;
            for item in &page {
                audits.extend(branch_audits(item).into_iter().map(|audit| Logged { rev: item.mod_rev.main, audit }));
            }
            if page.len() < want {
                break;
            }
            keys += page.len();
            if keys >= cap && audits.len() < limit {
                next = page.last().map(|i| i.mod_rev.main).filter(|rev| *rev < head);
                break;
            }
            cursor = page.last().map(|i| i.key.clone());
        }
        audits.sort_by_key(|l| l.rev);
        audits.truncate(limit);
        Ok(Scanned { audits, next })
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

    #[test_id(t99rz9l)]
    #[test]
    fn the_scan_returns_the_audits_at_or_after_the_floor_and_says_it_reached_it() {
        let s = store();
        s.create("sp-a", None, Some("0123456789abcdef0123456789abcdef01234567"), 100, &Request::new("c1", "t", "t")).unwrap();
        s.create("sp-b", None, Some("0123456789abcdef0123456789abcdef01234567"), 200, &Request::new("c2", "t", "t")).unwrap();
        let seen = |floor: i64| {
            let mut keys: Vec<(String, bool, i64)> = Vec::new();
            assert!(s.fold_audits_since(floor, |m| keys.push((m.lc_key.to_string(), m.applied, m.at))).unwrap());
            keys
        };
        let all = seen(0);
        assert!(all.iter().any(|k| k.1 && k.0 == "sp-a") && all.iter().any(|k| k.1 && k.0 == "sp-b"), "{all:?}");
        let later = seen(150);
        assert!(later.iter().all(|l| l.2 >= 150), "{later:?}");
        assert!(later.iter().all(|l| l.0 != "sp-a"));
    }

    #[test_id(tx0lcp3)]
    #[test]
    fn a_limited_read_from_zero_keeps_the_lowest_imported_seqs_across_history_pages() {
        use serde_json::json;
        use state_store::{Object, Txn};
        let s = store();
        let audit = |n: u64| json!({ "machine": "bead", "lc_key": format!("sp-{n}"), "event": "Claim", "expect": "READY", "from_state": "READY", "to_state": "WORKING", "applied": true, "refusal": null, "evidence": {}, "actor": "a", "at": 1 });
        let total = PAGE as u64 + 5;
        for chunk in (1..=total).rev().collect::<Vec<_>>().chunks(100) {
            let txn = chunk.iter().fold(Txn::new(), |t, n| t.put(&super::super::super::keys::history_entry(&format!("sp-{:04}", total - n), 0, "import"), Object::new(json!({ "seq": n, "audit": audit(*n) }))));
            s.contract().txn(&txn, &Request::new(&format!("import-{}", chunk[0]), "t", "t")).unwrap();
        }
        let first = s.audits_after_upto(0, 3).unwrap().audits;
        assert_eq!(first.iter().map(|l| l.rev).collect::<Vec<_>>(), [1, 2, 3]);
        let all = s.audits_after(0).unwrap().audits;
        assert_eq!(all.iter().map(|l| l.rev).collect::<Vec<_>>(), (1..=total).collect::<Vec<_>>());
    }

    #[test_id(tjm4yf7)]
    #[test]
    fn a_limited_read_from_zero_reads_the_imported_rows_through_the_seq_index_and_stops_at_its_limit() {
        use super::super::super::store::counting::Counting;
        use serde_json::json;
        use state_store::{Object, Txn};
        let c = Contract::open(Counting::default(), Ports::virtual_clock(&VirtualClock::new(0)), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        let s = Store::new(c, 1_000);
        let audit = |n: u64| json!({ "machine": "bead", "lc_key": format!("sp-{n}"), "event": "Claim", "expect": "READY", "from_state": "READY", "to_state": "WORKING", "applied": true, "refusal": null, "evidence": {}, "actor": "a", "at": 1 });
        let put = |keys: Vec<(String, serde_json::Value)>, tag: &str| {
            for (n, chunk) in keys.chunks(100).enumerate() {
                let txn = chunk.iter().fold(Txn::new(), |t, (k, v)| t.put(k, Object::new(v.clone())));
                s.contract().txn(&txn, &Request::new(&format!("{tag}-{n}"), "t", "t")).unwrap();
            }
        };
        put((1..=300u64).map(|n| (super::super::super::keys::history_entry(&format!("imp-{n:04}"), 0, "import"), json!({ "seq": n, "audit": audit(n) }))).collect(), "import");
        put((0..3_000u64).map(|n| (super::super::super::keys::history_entry(&format!("txn-{n:05}"), 7, "claim"), json!({ "audit": audit(n) }))).collect(), "txn");
        s.contract().backend().take();
        let got = s.audits_after_upto(0, 50).unwrap().audits;
        assert_eq!(got.iter().map(|l| l.rev).collect::<Vec<_>>(), (1..=50).collect::<Vec<_>>());
        let reads = s.contract().backend().take();
        assert!(reads.iter().all(|(k, p, _)| !(*k == "range" && p.starts_with(super::super::super::keys::HISTORY))), "{reads:?}");
        let indexed: usize = reads.iter().filter(|(k, _, _)| *k == "range_by_seq").map(|(_, _, n)| n).sum();
        assert!(indexed > 0 && indexed <= 50 + PAGE, "{reads:?}");
        assert!(reads.iter().all(|(k, _, _)| *k != "range"), "a filled limit reads no events: {reads:?}");
    }

    #[test_id(tww8q1r)]
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

    #[test_id(tteomrn)]
    #[test]
    fn a_limited_read_past_a_revision_asks_the_events_log_for_at_most_a_hundred_at_a_time() {
        use super::super::super::store::counting::Counting;
        let c = Contract::open(Counting::default(), Ports::virtual_clock(&VirtualClock::new(0)), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        let s = Store::new(c, 1_000);
        for n in 0..1_500u64 {
            s.create(&format!("sp-{n:04}"), None, Some("0123456789abcdef0123456789abcdef01234567"), 100, &Request::new(&format!("c{n}"), "t", "t")).unwrap();
        }
        s.contract().backend().take();
        let got = s.audits_after_upto(1, 250).unwrap().audits;
        let reads = s.contract().backend().take();
        let asked: Vec<usize> = reads.iter().filter(|(k, p, _)| *k == "range" && p.starts_with(EVENTS_PREFIX)).map(|(_, _, n)| *n).collect();
        assert!(!asked.is_empty() && asked.iter().all(|n| *n <= 100), "{asked:?}");
        assert!(asked.iter().sum::<usize>() <= 250 + 100, "{asked:?}");
        assert_eq!(got.iter().map(|l| l.rev).collect::<Vec<_>>(), (2..=251).collect::<Vec<_>>());
    }

    #[test_id(tjgyuas)]
    #[test]
    fn a_read_past_a_revision_stops_at_the_key_cap_in_an_audit_less_stretch_and_resumes_to_the_next_audit() {
        use super::super::super::store::counting::Counting;
        use serde_json::json;
        use state_store::{Object, Txn};
        let c = Contract::open(Counting::default(), Ports::virtual_clock(&VirtualClock::new(0)), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        let s = Store::new(c, 1_000);
        let create = |id: &str| {
            s.create(id, None, Some("0123456789abcdef0123456789abcdef01234567"), 100, &Request::new(id, "t", "t")).unwrap();
        };
        create("sp-a");
        let first = s.audits_after(0).unwrap().audits.iter().map(|l| l.rev).max().unwrap();
        for n in 0..5_000 {
            s.contract().txn(&Txn::new().put("pad/k", Object::new(json!(n))), &Request::new(&format!("pad-{n}"), "t", "t")).unwrap();
        }
        create("sp-b");
        s.contract().backend().take();
        let (mut cursor, mut found) = (first, Vec::new());
        for call in 0.. {
            assert!(call < 20, "the cursor never reached the head: {cursor}");
            let got = s.audits_after_upto(cursor, 100).unwrap();
            let keys: usize = s.contract().backend().take().iter().filter(|(k, p, _)| *k == "range" && p.starts_with(EVENTS_PREFIX)).map(|(_, _, n)| *n).sum();
            assert!(keys <= AUDIT_SCAN_KEYS + 100, "call {call} asked for {keys} events keys");
            if call == 0 {
                assert!(got.audits.is_empty(), "{:?}", got.audits);
                assert!(got.next.is_some(), "a read cut at the cap names where to resume");
            }
            found.extend(got.audits.into_iter().map(|l| l.audit.lc_key));
            let Some(next) = got.next else { break };
            assert!(next > cursor, "{next} is not past {cursor}");
            cursor = next;
        }
        assert_eq!(found, ["sp-b"]);
        let all = s.audits_after(first).unwrap();
        assert_eq!((all.audits.len(), all.next), (1, None), "the unbounded read ignores the cap");
    }

    #[test_id(tk9kx7r)]
    #[test]
    fn a_small_limit_still_reads_full_pages_and_a_limit_met_on_the_capping_page_names_no_next() {
        use super::super::super::store::counting::Counting;
        use serde_json::json;
        use state_store::{Object, Txn};
        let c = Contract::open(Counting::default(), Ports::virtual_clock(&VirtualClock::new(0)), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        let s = Store::new(c, 1_000);
        let pad = |from: usize, to: usize| {
            for n in from..to {
                s.contract().txn(&Txn::new().put("pad/k", Object::new(json!(n))), &Request::new(&format!("pad-{n}"), "t", "t")).unwrap();
            }
        };
        pad(0, 1);
        let start = s.contract().head().unwrap();
        pad(1, AUDIT_SCAN_KEYS - 9);
        s.create("sp-b", None, Some("0123456789abcdef0123456789abcdef01234567"), 100, &Request::new("b", "t", "t")).unwrap();
        pad(AUDIT_SCAN_KEYS, AUDIT_SCAN_KEYS + 200);
        s.contract().backend().take();
        let got = s.audits_after_upto(start, 1).unwrap();
        let calls = s.contract().backend().take().iter().filter(|(k, p, _)| *k == "range" && p.starts_with(EVENTS_PREFIX)).count();
        assert!(calls <= AUDIT_SCAN_KEYS / EVENTS_PER_READ, "{calls} reads of the events log");
        assert_eq!((got.audits.len(), got.next), (1, None), "the caller resumes from its last row");
    }
}
