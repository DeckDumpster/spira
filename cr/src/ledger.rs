//! The coverage ledger: `ledger.jsonl` in the findings repository (`SPIRA_CR_FINDINGS_DIR`), committed
//! by `cr`. Append-only; `cr seal` writes a review's open row and `cr close` its close row. A unit
//! counts as reviewed only when a review of it closed as published.

use crate::ports::{Ports, SourceTree};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const FILE: &str = "ledger.jsonl";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerFile {
    pub path: String,
    pub blob: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClosedAs {
    Published,
    Unpublished,
}

/// Where his comments on a published review went.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attention {
    pub comments_by_path: BTreeMap<String, u32>,
    pub finding_kinds: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub review: String,
    pub kind: String,
    pub unit_id: String,
    pub commit: String,
    pub files: Vec<LedgerFile>,
    /// Milliseconds since the epoch.
    pub opened_at: u64,
    pub closed_at: Option<u64>,
    pub closed_as: Option<ClosedAs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<Attention>,
}

/// The first path component.
pub fn area_of(path: &str) -> &str {
    path.split('/').next().unwrap_or(path)
}

impl Row {
    pub fn closing(&self, at: u64, how: ClosedAs, attention: Option<Attention>) -> Row {
        Row { closed_at: Some(at), closed_as: Some(how), attention, ..self.clone() }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Ledger {
    pub rows: Vec<Row>,
}

impl Ledger {
    pub fn from_rows(rows: Vec<Row>) -> Ledger {
        Ledger { rows }
    }

    /// Each review's state: its latest row.
    pub fn folded(&self) -> Vec<&Row> {
        let mut latest: BTreeMap<&str, &Row> = BTreeMap::new();
        for r in &self.rows {
            latest.insert(r.review.as_str(), r);
        }
        latest.into_values().collect()
    }

    pub fn latest_of(&self, review: &str) -> Option<&Row> {
        self.rows.iter().rev().find(|r| r.review == review)
    }

    /// A review that has opened and not closed holds its files.
    pub fn in_flight(&self, path: &str) -> bool {
        self.folded().iter().any(|r| r.closed_at.is_none() && r.files.iter().any(|f| f.path == path))
    }

    /// The blob of `path` at its latest published review, and when that review closed.
    pub fn reviewed(&self, path: &str) -> Option<(String, u64)> {
        self.folded()
            .iter()
            .filter(|r| r.closed_as == Some(ClosedAs::Published))
            .filter_map(|r| Some((r.files.iter().find(|f| f.path == path)?.blob.clone(), r.closed_at?)))
            .max_by_key(|(_, at)| *at)
    }

    pub fn last_opened_of_kind(&self, kind: &str) -> Option<u64> {
        self.rows.iter().filter(|r| r.kind == kind).map(|r| r.opened_at).max()
    }

    /// His comment counts per area over published reviews, and their total.
    pub fn attention_by_area(&self) -> (BTreeMap<String, u32>, u32) {
        let mut by_area: BTreeMap<String, u32> = BTreeMap::new();
        let mut total = 0;
        for a in self.folded().iter().filter(|r| r.closed_as == Some(ClosedAs::Published)).filter_map(|r| r.attention.as_ref()) {
            for (path, n) in &a.comments_by_path {
                *by_area.entry(area_of(path).to_string()).or_default() += n;
                total += n;
            }
        }
        (by_area, total)
    }

    /// The ledger with every path renamed to its name at `tree`'s commit.
    pub fn at_tree(&self, tree: &dyn SourceTree) -> Result<Ledger, String> {
        let mut by_commit: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        let mut rows = self.rows.clone();
        for r in &mut rows {
            if r.files.is_empty() && r.attention.is_none() {
                continue;
            }
            if !by_commit.contains_key(&r.commit) {
                by_commit.insert(r.commit.clone(), tree.renamed_since(&r.commit)?);
            }
            let renames = &by_commit[&r.commit];
            for f in &mut r.files {
                if let Some(new) = renames.get(&f.path) {
                    f.path = new.clone();
                }
            }
            if let Some(a) = &mut r.attention {
                let mut moved = BTreeMap::new();
                for (path, n) in std::mem::take(&mut a.comments_by_path) {
                    *moved.entry(renames.get(&path).cloned().unwrap_or(path)).or_default() += n;
                }
                a.comments_by_path = moved;
            }
        }
        Ok(Ledger { rows })
    }

    pub fn parse(text: &str) -> Result<Ledger, String> {
        let mut rows = Vec::new();
        for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            rows.push(serde_json::from_str(line).map_err(|e| format!("{FILE} line {}: {e}", n + 1))?);
        }
        Ok(Ledger { rows })
    }
}

fn dir_of(ports: &Ports) -> Result<PathBuf, String> {
    if ports.findings_dir.trim().is_empty() {
        return Err(format!("{} is empty; name the findings directory in the config, it has no default", crate::findings::DIR_KEY));
    }
    Ok(PathBuf::from(&ports.findings_dir))
}

/// The ledger's text; empty only when the file was never committed.
fn read(ports: &Ports, dir: &Path) -> Result<String, String> {
    match std::fs::read_to_string(dir.join(FILE)) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if ports.committer.has_history(dir, FILE)? {
                return Err(format!("{FILE} was deleted from {}; restore it from git", dir.display()));
            }
            Ok(String::new())
        }
        Err(e) => Err(format!("reading {}: {e}", dir.join(FILE).display())),
    }
}

pub fn load(ports: &Ports) -> Result<Ledger, String> {
    Ledger::parse(&read(ports, &dir_of(ports)?)?)
}

/// Adds `rows` to the ledger and commits the file alone. A commit that fails leaves the file as it was.
pub fn append(ports: &Ports, rows: &[Row]) -> Result<(), String> {
    let dir = dir_of(ports)?;
    let before = read(ports, &dir)?;
    Ledger::parse(&before)?;
    let mut text = before.clone();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    for r in rows {
        text.push_str(&serde_json::to_string(r).map_err(|e| e.to_string())?);
        text.push('\n');
    }
    let path = dir.join(FILE);
    crate::record::write_atomic(&path, text.as_bytes())?;
    let message = format!("ledger: {}", rows.iter().map(|r| format!("{} {}", r.review, if r.closed_at.is_some() { "closed" } else { "opened" })).collect::<Vec<_>>().join(", "));
    ports.committer.commit(&dir, &[FILE], &message).inspect_err(|_| {
        if before.is_empty() {
            let _ = std::fs::remove_file(&path);
        } else {
            let _ = crate::record::write_atomic(&path, before.as_bytes());
        }
    })
}

/// His comments by path on a published review's collection, and the findings' kinds.
pub fn attention_of(ports: &Ports, review: &crate::record::Review) -> Result<Attention, String> {
    let mut a = Attention::default();
    for c in crate::thread::his_comments(ports, review)? {
        *a.comments_by_path.entry(c.path).or_default() += 1;
    }
    for f in crate::findings::load(&ports.run_dir, review.number)? {
        *a.finding_kinds.entry(f.kind.as_str().to_string()).or_default() += 1;
    }
    Ok(a)
}
