//! What changed on the control store: a bead's history from its index, the applied bead events
//! since a revision, and a prefix's watch. Each reads one snapshot and names its revision.

use std::collections::BTreeSet;

use state_store::{
    contract::Cursor, events_key, Backend, Error, Event, Item, Result, Selector, Watched,
    EVENTS_PREFIX,
};

use super::keys;
use super::records::Audit;
use super::store::{Store, RECORD_CHUNK};

/// One audited change: the txn's revision and the audit it carried.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub rev: u64,
    pub audit: Audit,
}

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
        let (imported, entries): (Vec<_>, Vec<_>) =
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
        let revs: BTreeSet<u64> = entries.iter().map(|e| e.mod_rev.main).collect();
        let revs: Vec<u64> = revs.into_iter().collect();
        for chunk in revs.chunks(RECORD_CHUNK) {
            let wanted_keys: Vec<String> = chunk.iter().map(|r| events_key(*r)).collect();
            let refs: Vec<&str> = wanted_keys.iter().map(String::as_str).collect();
            let found = self.contract().get_many_at(&refs, list_rev)?;
            for (rev, events) in chunk.iter().zip(found) {
                let Some(events) = events else {
                    return Err(Error::Backend(format!(
                        "{prefix}: its history names rev {rev}, which has no events key"
                    )));
                };
                out.extend(
                    branch_audits(&events)
                        .into_iter()
                        .filter(|a| wanted(a))
                        .map(|audit| Change { rev: *rev, audit }),
                );
            }
        }
        Ok(out)
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
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests;
