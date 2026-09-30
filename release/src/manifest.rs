//! `MANIFEST`: the commit a release was built from, and a sha256 per file (DESIGN.md
//! "MANIFEST").

use crate::fsutil::{self, Node};
use std::collections::BTreeMap;
use std::path::Path;

/// The file name, at the release root. It does not list itself.
pub const FILE: &str = "MANIFEST";

/// Header keys. A tracked file whose path is one of these is a build refusal, so a header
/// line can never be mistaken for an entry.
pub const HEADER_KEYS: &[&str] = &["commit", "built", "repo", "release-repo"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A regular file and its sha256.
    File(String),
    /// A symlink and where it points.
    Link(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Manifest {
    pub commit: String,
    pub built: Option<String>,
    pub repo: Option<String>,
    pub entries: BTreeMap<String, Entry>,
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Manifest {
    /// Scan `root` into entries (everything but `MANIFEST` itself and directories).
    pub fn scan(root: &Path) -> Result<BTreeMap<String, Entry>, String> {
        let mut entries = BTreeMap::new();
        for (rel, node) in fsutil::walk(root)? {
            if rel == FILE {
                continue;
            }
            match node {
                Node::Dir => {}
                Node::Link(t) => {
                    entries.insert(rel, Entry::Link(t));
                }
                Node::File => {
                    let h = fsutil::sha256_file(&root.join(&rel))?;
                    entries.insert(rel, Entry::File(h));
                }
            }
        }
        Ok(entries)
    }

    /// The text written to `MANIFEST`.
    pub fn render(&self) -> String {
        let mut s = format!("commit {}\n", self.commit);
        if let Some(b) = &self.built {
            s.push_str(&format!("built {b}\n"));
        }
        if let Some(r) = &self.repo {
            s.push_str(&format!("repo {r}\n"));
        }
        for (p, e) in &self.entries {
            match e {
                Entry::File(h) => s.push_str(&format!("{p} {h}\n")),
                Entry::Link(t) => s.push_str(&format!("{p} -> {t}\n")),
            }
        }
        s
    }

    pub fn parse(text: &str) -> Result<Manifest, String> {
        let mut m = Manifest::default();
        for (i, line) in text.lines().enumerate() {
            if line.is_empty() {
                continue;
            }
            let bad = || format!("MANIFEST line {}: cannot parse {line:?}", i + 1);
            if let Some((k, v)) = line.split_once(' ') {
                if HEADER_KEYS.contains(&k) && !v.contains(' ') {
                    match k {
                        "commit" => m.commit = v.to_string(),
                        "built" => m.built = Some(v.to_string()),
                        "repo" => m.repo = Some(v.to_string()),
                        _ => {}
                    }
                    continue;
                }
            }
            if let Some((p, t)) = line.split_once(" -> ") {
                m.entries.insert(p.to_string(), Entry::Link(t.to_string()));
                continue;
            }
            let (p, h) = line.rsplit_once(' ').ok_or_else(bad)?;
            if !is_hex64(h) || p.is_empty() {
                return Err(bad());
            }
            m.entries.insert(p.to_string(), Entry::File(h.to_string()));
        }
        if !crate::is_sha(&m.commit) {
            return Err(format!("MANIFEST has no commit line naming a full sha (got {:?})", m.commit));
        }
        Ok(m)
    }

    pub fn load(root: &Path) -> Result<Manifest, String> {
        let p = root.join(FILE);
        let text = std::fs::read_to_string(&p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
        Manifest::parse(&text)
    }

    /// Every way `root` differs from this manifest: a missing, changed or unlisted file or
    /// symlink. Empty means the tree is exactly what was built.
    pub fn check(&self, root: &Path) -> Result<Vec<String>, String> {
        let actual = Manifest::scan(root)?;
        let mut problems = Vec::new();
        for (p, want) in &self.entries {
            match (want, actual.get(p)) {
                (_, None) => problems.push(format!("{p}: listed in MANIFEST but missing")),
                (Entry::File(w), Some(Entry::File(g))) if w != g => problems.push(format!("{p}: sha256 {g}, MANIFEST says {w}")),
                (Entry::Link(w), Some(Entry::Link(g))) if w != g => problems.push(format!("{p}: points at {g}, MANIFEST says {w}")),
                (Entry::File(_), Some(Entry::Link(_))) => problems.push(format!("{p}: a symlink, MANIFEST says a file")),
                (Entry::Link(_), Some(Entry::File(_))) => problems.push(format!("{p}: a file, MANIFEST says a symlink")),
                _ => {}
            }
        }
        for p in actual.keys() {
            if !self.entries.contains_key(p) {
                problems.push(format!("{p}: present but not in MANIFEST"));
            }
        }
        Ok(problems)
    }
}
