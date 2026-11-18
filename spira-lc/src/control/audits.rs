//! The audit log read by time: every applied or refused machine event at or after a floor,
//! newest revisions first, or the whole log in order. The answer is complete or says it is not;
//! an unfinished scan is never read as "no events".

use state_store::{events_key, Backend, Result, EVENTS_PREFIX};

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

impl<B: Backend> Store<B> {
    /// The audits with `at >= floor`, oldest first.
    pub fn audits_since(&self, floor: i64) -> Result<Scanned> {
        let head = self.contract().head()?;
        let (mut hi, mut scanned, mut span) = (head, 0usize, CHUNK_REVS);
        let mut audits: Vec<Logged> = Vec::new();
        let mut complete = false;
        while scanned < MAX_SCANNED {
            let lo = hi.saturating_sub(span);
            let mut oldest: Option<i64> = None;
            let mut after = (lo > 0).then(|| events_key(lo));
            let end = events_key(hi);
            'chunk: loop {
                let page = self.contract().range_at(EVENTS_PREFIX, after.as_deref(), PAGE, head)?;
                for item in &page {
                    if item.key > end {
                        break 'chunk;
                    }
                    scanned += 1;
                    for audit in branch_audits(item) {
                        oldest = Some(oldest.map_or(audit.at, |o| o.min(audit.at)));
                        if audit.at >= floor {
                            audits.push(Logged { rev: item.mod_rev.main, audit });
                        }
                    }
                }
                if page.len() < PAGE {
                    break;
                }
                after = page.last().map(|i| i.key.clone());
            }
            if lo == 0 || oldest.is_some_and(|o| o < floor) {
                complete = true;
                break;
            }
            span = if oldest.is_some() { CHUNK_REVS } else { (span * 2).min(CHUNK_REVS * 64) };
            hi = lo;
        }
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
