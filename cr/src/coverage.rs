//! Review coverage at the landing-ref commit: which files a published review has reached.

use crate::glob;
use crate::index::Index;
use crate::ledger::{ClosedAs, Ledger};
use crate::ports::{Config, SourceTree};
use crate::units::{self, KINDS};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tally {
    pub reviewed: u32,
    pub current: u32,
    pub total: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    pub path: String,
    pub review: String,
    pub changed_lines: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Never {
    pub path: String,
    pub first_commit_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coverage {
    pub commit: String,
    pub files: Tally,
    pub areas: Vec<(String, Tally)>,
    pub kinds: Vec<(&'static str, u32, u32)>,
    /// Every never-reviewed file, oldest first; the caller cuts the list.
    pub never_reviewed: Vec<Never>,
    /// Every changed file, most lines first; the caller cuts the list.
    pub changed: Vec<Changed>,
}

/// The blob of `path` at its latest published review, and that review's number.
fn latest_published(ledger: &Ledger, path: &str) -> Option<(String, String)> {
    ledger
        .folded()
        .into_iter()
        .filter(|r| r.closed_as == Some(ClosedAs::Published))
        .filter_map(|r| Some((r.closed_at?, r.review.clone(), r.files.iter().find(|f| f.path == path)?.blob.clone())))
        .max_by_key(|(at, _, _)| *at)
        .map(|(_, review, blob)| (blob, review))
}

/// `ledger` carries today's paths (`Ledger::at_tree`).
pub fn coverage(ix: &Index, tree: &dyn SourceTree, ledger: &Ledger, cfg: &Config) -> Coverage {
    let units = units::all(ix);
    let universe: BTreeSet<&str> = units.iter().flat_map(|u| u.files.iter().map(|f| f.path.as_str())).filter(|p| !glob::any(&cfg.exclude, p)).collect();
    let mut files = Tally { reviewed: 0, current: 0, total: 0 };
    let mut areas: BTreeMap<String, Tally> = BTreeMap::new();
    let mut never_reviewed = Vec::new();
    let mut changed = Vec::new();
    let mut reviewed_paths = BTreeSet::new();
    for path in universe {
        let area = areas.entry(crate::ledger::area_of(path).to_string()).or_insert(Tally { reviewed: 0, current: 0, total: 0 });
        files.total += 1;
        area.total += 1;
        match latest_published(ledger, path) {
            None => never_reviewed.push(Never { path: path.to_string(), first_commit_ms: tree.history(path, 0).oldest_ms }),
            Some((blob, review)) => {
                files.reviewed += 1;
                area.reviewed += 1;
                reviewed_paths.insert(path);
                if ix.blobs.get(path) == Some(&blob) {
                    files.current += 1;
                    area.current += 1;
                } else {
                    // A blob the tree cannot diff counts as wholly changed.
                    let changed_lines = tree.changed_lines(&blob, path).unwrap_or_else(|| ix.lines(path));
                    changed.push(Changed { path: path.to_string(), review, changed_lines });
                }
            }
        }
    }
    never_reviewed.sort_by(|a, b| a.first_commit_ms.unwrap_or(u64::MAX).cmp(&b.first_commit_ms.unwrap_or(u64::MAX)).then_with(|| a.path.cmp(&b.path)));
    changed.sort_by(|a, b| b.changed_lines.cmp(&a.changed_lines).then_with(|| a.path.cmp(&b.path)));
    let kinds = KINDS
        .iter()
        .map(|k| {
            let of_kind: Vec<_> = units.iter().filter(|u| u.kind == *k && !glob::any(&cfg.exclude, &u.files[0].path)).collect();
            let reviewed = of_kind.iter().filter(|u| reviewed_paths.contains(u.files[0].path.as_str())).count() as u32;
            (k.as_str(), reviewed, of_kind.len() as u32)
        })
        .collect();
    Coverage { commit: ix.commit.clone(), files, areas: areas.into_iter().collect(), kinds, never_reviewed, changed }
}
