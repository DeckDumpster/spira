//! What changed on the control store: a bead's history from its index, the applied bead events
//! since a revision, and a prefix's watch. Each reads one snapshot and names its revision.

use std::collections::BTreeSet;

use state_store::{
    contract::Cursor, events_key, Backend, Error, Event, Item, Result, Selector, Watched,
    EVENTS_PREFIX,
};

use super::keys;
use super::records::Audit;
use super::store::Store;

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
    /// The asked revision was compacted away: here is the prefix at `list_rev`, to resume from.
    Relist {
        compacted: u64,
        items: Vec<Item>,
        list_rev: u64,
    },
}

const PAGE: usize = 1_000;

/// The audits of the branch an `events/<rev>` key says ran. A key some other writer left
/// without audits has none.
fn branch_audits(events: &Item) -> Vec<Audit> {
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
        let (entries, _) = self
            .contract()
            .list(&[keys::history(id).as_str()], &Selector::Everything)?;
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
        for rev in revs {
            let Some(events) = self.contract().get(&events_key(rev))? else {
                return Err(Error::Backend(format!(
                    "{}: its history names rev {rev}, which has no events key",
                    keys::history(id)
                )));
            };
            out.extend(
                branch_audits(&events)
                    .into_iter()
                    .filter(|a| wanted(a))
                    .map(|audit| Change { rev, audit }),
            );
        }
        Ok(out)
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

    /// The contract's watch, with `TooOld` answered by the re-list a watcher resumes from.
    pub fn watch(&self, prefix: &str, from_rev: u64, wait_ms: u64) -> Result<Feed> {
        match self.contract().watch(prefix, from_rev, wait_ms) {
            Ok(Watched::Events { events, through }) => Ok(Feed::Events { events, through }),
            Ok(Watched::Progress { progress_rev }) => Ok(Feed::Progress { progress_rev }),
            Err(Error::TooOld { compacted }) => {
                let (items, list_rev) = self.contract().list(&[prefix], &Selector::Everything)?;
                Ok(Feed::Relist {
                    compacted,
                    items,
                    list_rev,
                })
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests;
