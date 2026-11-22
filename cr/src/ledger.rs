//! The coverage ledger: `ledger.jsonl` on the review repository's default branch. Append-only;
//! `cr seal` writes a review's open row and `cr ledger-close` its close row. A unit counts as
//! reviewed only when a review of it closed as published.

use crate::cli::Outcome;
use crate::ports::{Ports, SourceTree};
use crate::review::{gh, repo_refusal_message, text_field};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

pub const FILE: &str = "ledger.jsonl";
const ATTEMPTS: u32 = 3;

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

fn endpoint(ports: &Ports) -> String {
    format!("repos/{}/{}/contents/{FILE}", ports.repo_owner, ports.repo_name)
}

fn fetch(ports: &Ports) -> Result<(String, String), String> {
    let ep = endpoint(ports);
    let v = gh(ports, "GET", &ep, None).map_err(|e| {
        if e.contains("404") {
            format!("{FILE} does not exist on the default branch of {}/{}; create it empty there once, then rerun", ports.repo_owner, ports.repo_name)
        } else {
            e
        }
    })?;
    let packed = text_field(&v, &["content"], &ep)?;
    let packed: String = packed.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD.decode(packed).map_err(|e| format!("{FILE} is not base64: {e}"))?;
    let text = String::from_utf8(bytes).map_err(|e| format!("{FILE} is not UTF-8: {e}"))?;
    Ok((text, text_field(&v, &["sha"], &ep)?))
}

/// The refusal for an unnamed review repository, or the forge's ledger and its blob sha.
pub fn load(ports: &Ports) -> Result<(Ledger, String), String> {
    if let Some(m) = repo_refusal_message(ports) {
        return Err(m);
    }
    let (text, sha) = fetch(ports)?;
    Ok((Ledger::parse(&text)?, sha))
}

/// Adds `rows` to the forge's ledger. A write that fails is retried only on a sha that moved.
pub fn append(ports: &Ports, rows: &[Row]) -> Result<(), String> {
    let mut added = String::new();
    for r in rows {
        added.push_str(&serde_json::to_string(r).map_err(|e| e.to_string())?);
        added.push('\n');
    }
    let message = format!("ledger: {}", rows.iter().map(|r| format!("{} {}", r.review, if r.closed_at.is_some() { "closed" } else { "opened" })).collect::<Vec<_>>().join(", "));
    rewrite(ports, &message, |text| Ok(format!("{text}{added}")))
}

/// Writes `row` as its review's only row: it takes the place of the first row of that review and
/// the others go; with none, it is appended. Other lines are kept as they stand.
pub fn put_review(ports: &Ports, row: &Row) -> Result<(), String> {
    let line = serde_json::to_string(row).map_err(|e| e.to_string())?;
    rewrite(ports, &format!("ledger: {} adopted", row.review), |text| {
        let mut out = String::new();
        let mut placed = false;
        for (n, l) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            let v: serde_json::Value = serde_json::from_str(l).map_err(|e| format!("{FILE} line {}: {e}", n + 1))?;
            if v.get("review").and_then(|r| r.as_str()) == Some(row.review.as_str()) {
                if !placed {
                    out.push_str(&line);
                    out.push('\n');
                    placed = true;
                }
            } else {
                out.push_str(l);
                out.push('\n');
            }
        }
        if !placed {
            out.push_str(&line);
            out.push('\n');
        }
        Ok(out)
    })
}

fn rewrite(ports: &Ports, message: &str, edit: impl Fn(&str) -> Result<String, String>) -> Result<(), String> {
    let ep = endpoint(ports);
    let repo = format!("repos/{}/{}", ports.repo_owner, ports.repo_name);
    let branch = text_field(&gh(ports, "GET", &repo, None)?, &["default_branch"], &repo)?;
    let (mut text, mut sha) = fetch(ports)?;
    for _ in 0..ATTEMPTS {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        let content = base64::engine::general_purpose::STANDARD.encode(edit(&text)?);
        let err = match gh(ports, "PUT", &ep, Some(&json!({"message": message, "content": content, "sha": sha, "branch": branch}))) {
            Ok(_) => return Ok(()),
            Err(e) => e,
        };
        let (now_text, now_sha) = fetch(ports)?;
        if now_sha == sha {
            return Err(err);
        }
        (text, sha) = (now_text, now_sha);
    }
    Err(format!("{FILE} moved under {ATTEMPTS} writes in a row; nothing was written"))
}

/// `cr ledger-close`: records how a review closed, once.
pub fn close(number: u32, at: u64, ports: &Ports) -> Outcome {
    let refuse = |code: i32, m: &str| Outcome { code, stdout: String::new(), stderr: format!("cr: {m}\n") };
    if let Some(m) = repo_refusal_message(ports) {
        return refuse(2, &m);
    }
    let r = match crate::record::load(&ports.run_dir, number) {
        Ok(r) => r,
        Err(e) => return refuse(2, &e),
    };
    let id = format!("{number:03}");
    let (ledger, _) = match load(ports) {
        Ok(l) => l,
        Err(e) => return refuse(1, &e),
    };
    let Some(latest) = ledger.latest_of(&id) else {
        return Outcome { code: 0, stdout: format!("review {id} has no open row in the ledger; nothing to close\n"), stderr: String::new() };
    };
    if latest.closed_at.is_some() {
        return Outcome { code: 0, stdout: format!("review {id} is already closed in the ledger\n"), stderr: String::new() };
    }
    let how = if r.published_at.is_some() { ClosedAs::Published } else { ClosedAs::Unpublished };
    let attention = match how {
        ClosedAs::Published => match attention_of(ports, number) {
            Ok(a) => Some(a),
            Err(e) => return refuse(1, &e),
        },
        ClosedAs::Unpublished => None,
    };
    match append(ports, &[latest.closing(at, how, attention)]) {
        Ok(()) => Outcome { code: 0, stdout: format!("review {id} closed as {}\n", if how == ClosedAs::Published { "published" } else { "unpublished" }), stderr: String::new() },
        Err(e) => refuse(1, &e),
    }
}

fn attention_of(ports: &Ports, number: u32) -> Result<Attention, String> {
    let mut a = Attention::default();
    for c in crate::thread::kept_comments(&ports.run_dir, number)?.iter().filter(|c| !c.bot) {
        if let Some(p) = &c.path {
            *a.comments_by_path.entry(p.clone()).or_default() += 1;
        }
    }
    for f in crate::findings::load(&ports.run_dir, number)? {
        *a.finding_kinds.entry(f.kind.as_str().to_string()).or_default() += 1;
    }
    Ok(a)
}
