//! What changed on the control store: a bead's history from its index, the applied bead events
//! since a revision, and a prefix's watch. Each reads one snapshot and names its revision.

use std::collections::BTreeSet;

use state_store::{
    contract::Cursor, events_key, Backend, Compare, Error, Event, Item, Request, Result,
    Selector, Txn, Watched, EVENTS_PREFIX,
};

use super::keys;
use super::records::Audit;
use super::store::{history_value, Store, RECORD_CHUNK};

/// One audited change: the txn's revision and the audit it carried.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub rev: u64,
    pub audit: Audit,
}

/// What `backfill_history_audits` did, or on a dry run would do.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Backfill {
    pub rewritten: usize,
    pub inline: usize,
    /// The keys of entries whose `events/` key is gone.
    pub lost: Vec<String>,
    /// The call stopped at its cap with entries still unread.
    pub more: bool,
}

/// Entries one rewrite txn carries, under the store's per-txn op limit.
const BACKFILL_TXN: usize = 100;

/// Legacy entries one call rewrites, so a call answers inside the service's query deadline.
pub const BACKFILL_PER_CALL: usize = 20 * RECORD_CHUNK;

#[derive(Clone, Debug, PartialEq)]
pub enum Feed {
    /// Every event under the prefix after the asked revision; resume from `through`.
    Events {
        events: Vec<Event>,
        through: u64,
    },
    Progress {
        progress_rev: u64,
    },
    /// The asked revision was compacted away: resume from `list_rev` after reloading by a filtered read.
    Relist {
        compacted: u64,
        list_rev: u64,
    },
    /// The asked revision is above the head: the watcher holds a revision the store does not, and reloads.
    Ahead {
        rev: u64,
        head: u64,
    },
}

const PAGE: usize = 1_000;

/// The audits of the branch an `events/<rev>` key says ran. A key some other writer left
/// without audits has none.
pub(super) fn branch_audits(events: &Item) -> Vec<Audit> {
    let data = &events.value.data;
    let branch = if data["refused"].as_bool() == Some(true) {
        "else"
    } else {
        "then"
    };
    data["evidence"][branch]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| serde_json::from_value(a.clone()).ok())
        .collect()
}

impl<B: Backend> Store<B> {
    /// Every audited change to `id`, in revision order, refusals and lost races included. The
    /// changes imported from the old store come first, each at its old seq, which is below
    /// every revision since.
    pub fn history(&self, id: &str, machine: Option<&str>) -> Result<Vec<Change>> {
        let prefix = keys::history(id);
        let mut entries = Vec::new();
        let mut cursor = None;
        let list_rev = loop {
            let mut page = self.contract().list_page(
                &[prefix.as_str()],
                &Selector::Everything,
                RECORD_CHUNK,
                cursor.as_ref(),
            )?;
            entries.append(&mut page.items);
            match page.next {
                Some(next) => cursor = Some(next),
                None => break page.list_rev,
            }
        };
        let wanted = |a: &Audit| a.lc_key == id && machine.is_none_or(|m| a.machine == m);
        let (imported, rest): (Vec<_>, Vec<_>) =
            entries.iter().partition(|e| e.value.data.get("seq").is_some());
        let mut out = Vec::new();
        for e in imported {
            let (Some(rev), Ok(audit)) = (
                e.value.data["seq"].as_u64(),
                serde_json::from_value::<Audit>(e.value.data["audit"].clone()),
            ) else {
                return Err(Error::Backend(format!("{}: an undecodable imported change", e.key)));
            };
            if wanted(&audit) {
                out.push(Change { rev, audit });
            }
        }
        out.sort_by_key(|c| c.rev);
        let mut changes = Vec::new();
        let mut legacy = BTreeSet::new();
        for e in rest {
            if !e.value.data["audits"].is_array() {
                legacy.insert(e.mod_rev.main);
                continue;
            }
            let rev = e.value.data["rev"].as_u64().unwrap_or(e.mod_rev.main);
            for a in e.value.data["audits"].as_array().into_iter().flatten() {
                let Ok(audit) = serde_json::from_value::<Audit>(a.clone()) else {
                    return Err(Error::Backend(format!("{}: an undecodable audit", e.key)));
                };
                if wanted(&audit) {
                    changes.push(Change { rev, audit });
                }
            }
        }
        let legacy: Vec<u64> = legacy.into_iter().collect();
        for chunk in legacy.chunks(RECORD_CHUNK) {
            let wanted_keys: Vec<String> = chunk.iter().map(|r| events_key(*r)).collect();
            let refs: Vec<&str> = wanted_keys.iter().map(String::as_str).collect();
            let found = self.contract().get_many_at(&refs, list_rev)?;
            for (rev, events) in chunk.iter().zip(found) {
                let Some(events) = events else {
                    return Err(Error::Backend(format!(
                        "{prefix}: its history names rev {rev}, which has no events key"
                    )));
                };
                changes.extend(
                    branch_audits(&events)
                        .into_iter()
                        .filter(|a| wanted(a))
                        .map(|audit| Change { rev: *rev, audit }),
                );
            }
        }
        changes.sort_by_key(|c| c.rev);
        out.append(&mut changes);
        Ok(out)
    }

    /// Gives each legacy (`{}`) history entry of `ids`, or of every bead, its audits and its
    /// revision, read from its `events/` key, and stops after [`BACKFILL_PER_CALL`] of them so a
    /// backlog is worked down over repeated calls. A dry run counts the legacy entries and writes
    /// nothing. An entry whose events key is gone is left as it is and named in `lost`.
    pub fn backfill_history_audits(&self, ids: &[String], apply: bool) -> Result<Backfill> {
        let prefixes: Vec<String> = if ids.is_empty() {
            vec![keys::HISTORY.to_string()]
        } else {
            ids.iter().map(|i| keys::history(i)).collect()
        };
        let mut done = Backfill::default();
        for prefix in &prefixes {
            let mut cursor = None;
            loop {
                let page = self.contract().list_page(
                    &[prefix.as_str()],
                    &Selector::Everything,
                    RECORD_CHUNK,
                    cursor.as_ref(),
                )?;
                let (legacy, inline): (Vec<&Item>, Vec<&Item>) = page
                    .items
                    .iter()
                    .partition(|e| !e.value.data["audits"].is_array() && e.value.data.get("seq").is_none());
                done.inline += inline.len();
                if apply {
                    self.backfill_page(&legacy, page.list_rev, &mut done)?;
                } else {
                    done.rewritten += legacy.len();
                }
                if apply && done.rewritten + done.lost.len() >= BACKFILL_PER_CALL {
                    done.more = page.next.is_some();
                    return Ok(done);
                }
                match page.next {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
            }
        }
        Ok(done)
    }

    fn backfill_page(&self, legacy: &[&Item], list_rev: u64, done: &mut Backfill) -> Result<()> {
        if legacy.is_empty() {
            return Ok(());
        }
        let event_keys: Vec<String> = legacy.iter().map(|e| events_key(e.mod_rev.main)).collect();
        let refs: Vec<&str> = event_keys.iter().map(String::as_str).collect();
        let found = self.contract().get_many_at(&refs, list_rev)?;
        let mut rewrites = Vec::new();
        for (entry, events) in legacy.iter().zip(found) {
            let Some(events) = events else {
                done.lost.push(entry.key.clone());
                continue;
            };
            let id = entry.key.strip_prefix(keys::HISTORY).and_then(|k| k.split('/').next()).unwrap_or_default();
            let mut value = history_value(id, &branch_audits(&events), Some(entry.mod_rev.main))?;
            value.metadata = entry.value.metadata.clone();
            rewrites.push((entry, value));
        }
        for group in rewrites.chunks(BACKFILL_TXN) {
            let mut t = Txn::new();
            for (entry, value) in group {
                t = t.when(Compare::ModRev(entry.key.clone(), Some(entry.mod_rev))).put(&entry.key, value.clone());
            }
            let req = Request::new(&super::served::call_request_id(), "backfill-history-audits", "backfill-history-audits");
            if !self.contract().txn(&t, &req)?.succeeded {
                return Err(Error::Backend("a history entry changed during the backfill; its txn wrote nothing, run again".into()));
            }
            done.rewritten += group.len();
        }
        Ok(())
    }

    /// The revision alone: no event is read.
    pub fn event_head(&self) -> Result<u64> {
        self.contract().head()
    }

    /// The revision read at, and the ids with an applied bead event after `since` up to it.
    pub fn event_seq(&self, since: u64) -> Result<(u64, Vec<String>)> {
        let head = self.contract().head()?;
        let mut keys = BTreeSet::new();
        let mut cursor = Some(Cursor {
            list_rev: head,
            prefix: 0,
            after: Some(events_key(since)),
        });
        while let Some(c) = cursor.take() {
            let page = self.contract().list_page(
                &[EVENTS_PREFIX],
                &Selector::Everything,
                PAGE,
                Some(&c),
            )?;
            for events in &page.items {
                keys.extend(
                    branch_audits(events)
                        .into_iter()
                        .filter(|a| a.applied && a.machine == "bead")
                        .map(|a| a.lc_key),
                );
            }
            cursor = page.next;
        }
        Ok((head, keys.into_iter().collect()))
    }

    /// The contract's watch; `TooOld` is answered with the revision to resume from and no rows, for the watcher to reload by its own filtered read.
    pub fn watch(&self, prefixes: &[&str], from_rev: u64, wait_ms: u64) -> Result<Feed> {
        match self.contract().watch_any(prefixes, from_rev, wait_ms) {
            Ok(Watched::Events { events, through }) => Ok(Feed::Events { events, through }),
            Ok(Watched::Progress { progress_rev }) => Ok(Feed::Progress { progress_rev }),
            Err(Error::TooOld { compacted }) => {
                let list_rev = self.contract().head()?;
                Ok(Feed::Relist {
                    compacted,
                    list_rev,
                })
            }
            Err(Error::FutureRev { rev, head }) => Ok(Feed::Ahead { rev, head }),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests;
