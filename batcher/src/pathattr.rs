//! Path attribution of a red round: which single member's diff a red's own output names.
//!
//! Deterministic and pure — evidence text and each member's changed paths in, a verdict out.
//! Per red: the paths its output names that some member touched; if none, the suite's own
//! declared paths (its source, the crate it tests) that some member touched. Exactly one
//! member per red → that red has an owner; every red owned → eject the owners. A red with no
//! member or several, or an empty red set, is `Ambiguous` and the round pauses as before.

use std::collections::{BTreeMap, BTreeSet};

use crate::core::Id;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Red {
    pub suite: String,
    pub evidence: String,
    /// Path globs (`*` matches anything, a trailing `/` is a prefix) the suite itself covers.
    pub own: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberPaths {
    pub id: Id,
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub suite: String,
    /// Member -> the paths that implicate it for this red.
    pub candidates: BTreeMap<Id, Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Every red has exactly one owner; each owner with the evidence line that names it.
    Eject(Vec<(Id, String)>),
    Ambiguous(Vec<Row>),
}

const GENERIC: [&str; 7] = ["mod.rs", "main.rs", "lib.rs", "tests.rs", "Cargo.toml", "Cargo.lock", "README.md"];

fn trim_token(raw: &str) -> Option<String> {
    let mut t = raw.trim_matches(':');
    while let Some((head, tail)) = t.rsplit_once(':') {
        if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
            t = head;
        } else {
            break;
        }
    }
    let t = t.trim_end_matches(['.', ':']).trim_start_matches('.').trim_start_matches('/');
    let t = t.trim_start_matches("./");
    if t.is_empty() {
        return None;
    }
    let base = t.rsplit('/').next().unwrap_or(t);
    let ext_ok = base.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty() && (2..=10).contains(&ext.len()) && ext.starts_with(|c: char| c.is_ascii_alphabetic()) && ext.chars().all(|c| c.is_ascii_alphanumeric())
    });
    if !ext_ok && !t.contains('/') {
        return None;
    }
    if !ext_ok && base.is_empty() {
        return None;
    }
    Some(t.to_string())
}

/// Every path-looking token in `text`: contains a `/`, or a basename with an extension.
pub fn named_paths(text: &str) -> BTreeSet<String> {
    text.split(|c: char| c.is_whitespace() || "\"'()[]<>,;=`{}|".contains(c)).filter_map(trim_token).collect()
}

fn names(token: &str, path: &str) -> bool {
    if path == token || path.ends_with(&format!("/{token}")) || token.ends_with(&format!("/{path}")) {
        return true;
    }
    let base = path.rsplit('/').next().unwrap_or(path);
    !token.contains('/') && token == base && !GENERIC.contains(&token)
}

fn glob(pat: &str, s: &str) -> bool {
    if let Some(dir) = pat.strip_suffix('/') {
        return s.starts_with(&format!("{dir}/"));
    }
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == s;
    }
    let mut rest = s;
    for (i, p) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(p) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(p);
        } else {
            match rest.find(p) {
                Some(at) => rest = &rest[at + p.len()..],
                None => return false,
            }
        }
    }
    true
}

fn row(red: &Red, members: &[MemberPaths]) -> Row {
    let tokens = named_paths(&red.evidence);
    let mut candidates: BTreeMap<Id, Vec<String>> = BTreeMap::new();
    for m in members {
        let hit: BTreeSet<&String> = m.paths.iter().filter(|p| tokens.iter().any(|t| names(t, p))).collect();
        if !hit.is_empty() {
            candidates.insert(m.id.clone(), hit.into_iter().cloned().collect());
        }
    }
    if candidates.is_empty() {
        for m in members {
            let hit: BTreeSet<&String> = m.paths.iter().filter(|p| red.own.iter().any(|g| glob(g, p))).collect();
            if !hit.is_empty() {
                candidates.insert(m.id.clone(), hit.into_iter().cloned().collect());
            }
        }
    }
    Row { suite: red.suite.clone(), candidates }
}

pub fn attribute(reds: &[Red], members: &[MemberPaths]) -> Verdict {
    let rows: Vec<Row> = reds.iter().map(|r| row(r, members)).collect();
    if rows.is_empty() || rows.iter().any(|r| r.candidates.len() != 1) {
        return Verdict::Ambiguous(rows);
    }
    let mut owners: BTreeMap<Id, String> = BTreeMap::new();
    for r in &rows {
        let (id, paths) = r.candidates.iter().next().expect("one candidate");
        let line = format!("{}: names {}", r.suite, paths.join(", "));
        owners.entry(id.clone()).and_modify(|e| *e = format!("{e}; {line}")).or_insert(line);
    }
    Verdict::Eject(owners.into_iter().collect())
}

/// Reds file: `@@ <suite> [own=<glob>,<glob>]` opens a block; its following lines are evidence.
pub fn parse_reds(text: &str) -> Result<Vec<Red>, String> {
    let mut out: Vec<Red> = Vec::new();
    for line in text.lines() {
        if let Some(head) = line.strip_prefix("@@ ") {
            let mut w = head.split_whitespace();
            let suite = w.next().ok_or("reds: '@@' block with no suite name")?.to_string();
            let own = w.filter_map(|a| a.strip_prefix("own=")).flat_map(|o| o.split(',')).filter(|g| !g.is_empty()).map(str::to_string).collect();
            out.push(Red { suite, evidence: String::new(), own });
        } else if let Some(last) = out.last_mut() {
            last.evidence.push_str(line);
            last.evidence.push('\n');
        } else if !line.trim().is_empty() {
            return Err("reds: evidence before the first '@@ <suite>' line".into());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
