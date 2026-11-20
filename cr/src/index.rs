//! What `cr` knows about one commit: its files, the workspace crates and their path
//! dependencies, and which files name which identifiers and `a::b` paths.

use crate::ports::SourceTree;
use crate::rustsrc;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone)]
pub struct Bin {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct Crate {
    pub name: String,
    pub lib: String,
    pub dir: String,
    pub deps: Vec<String>,
    pub bins: Vec<Bin>,
}

pub struct Index {
    pub commit: String,
    pub files: Vec<String>,
    pub crates: Vec<Crate>,
    pub text: BTreeMap<String, String>,
    pub blobs: BTreeMap<String, String>,
    ident_files: HashMap<String, Vec<String>>,
    pair_files: HashMap<(String, String), Vec<String>>,
}

const TEXT_EXTENSIONS: [&str; 3] = ["rs", "sh", "md"];

pub fn ext(path: &str) -> &str {
    path.rsplit_once('.').map_or("", |(_, e)| e)
}

pub fn join_norm(dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

impl Index {
    pub fn build(tree: &dyn SourceTree) -> Result<Index, String> {
        let commit = tree.commit()?;
        let files = tree.files()?;
        let mut text = BTreeMap::new();
        let mut blobs = BTreeMap::new();
        for f in &files {
            if let Some(b) = tree.blob(f) {
                blobs.insert(f.clone(), b);
            }
            let wanted = TEXT_EXTENSIONS.contains(&ext(f)) || f.rsplit('/').next() == Some("Cargo.toml") || f.contains("/hooks/") || f.starts_with("hooks/");
            if wanted {
                if let Some(bytes) = tree.read(f) {
                    text.insert(f.clone(), String::from_utf8_lossy(&bytes).into_owned());
                }
            }
        }
        let crates = read_crates(&files, &text);
        let mut ident_files: HashMap<String, Vec<String>> = HashMap::new();
        let mut pair_files: HashMap<(String, String), Vec<String>> = HashMap::new();
        for (path, t) in text.iter().filter(|(p, _)| ext(p) == "rs") {
            let code = rustsrc::code_lines(t).join("\n");
            let r = rustsrc::refs(&code);
            for i in r.idents {
                ident_files.entry(i).or_default().push(path.clone());
            }
            for p in r.pairs {
                pair_files.entry(p).or_default().push(path.clone());
            }
        }
        Ok(Index { commit, files, crates, text, blobs, ident_files, pair_files })
    }

    pub fn has_file(&self, path: &str) -> bool {
        self.blobs.contains_key(path)
    }

    pub fn lines(&self, path: &str) -> u32 {
        self.text.get(path).map_or(0, |t| t.lines().count() as u32)
    }

    pub fn files_naming(&self, ident: &str) -> &[String] {
        self.ident_files.get(ident).map_or(&[], Vec::as_slice)
    }

    pub fn files_naming_pair(&self, a: &str, b: &str) -> &[String] {
        self.pair_files.get(&(a.to_string(), b.to_string())).map_or(&[], Vec::as_slice)
    }

    /// The `b` of every `a::b` pair some file names, for one `a`.
    pub fn pair_keys_for(&self, a: &str) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self.pair_files.keys().filter(|(x, _)| x == a).cloned().collect();
        v.sort();
        v
    }

    pub fn crate_of(&self, path: &str) -> Option<&Crate> {
        self.crates.iter().filter(|c| path.starts_with(&format!("{}/", c.dir)) || c.dir.is_empty()).max_by_key(|c| c.dir.len())
    }

    pub fn crate_named(&self, name: &str) -> Option<&Crate> {
        self.crates.iter().find(|c| c.name == name || c.dir == name)
    }
}

fn read_crates(files: &[String], text: &BTreeMap<String, String>) -> Vec<Crate> {
    let Some(root) = text.get("Cargo.toml").and_then(|t| t.parse::<toml::Table>().ok()) else { return Vec::new() };
    let members: Vec<String> = root
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.trim_end_matches('/').to_string())).collect())
        .unwrap_or_default();
    let mut dirs: Vec<String> = Vec::new();
    for m in members {
        if !dirs.contains(&m) {
            dirs.push(m);
        }
    }
    let mut raw: Vec<(Crate, Vec<String>)> = Vec::new();
    for dir in dirs {
        let Some(doc) = text.get(&format!("{dir}/Cargo.toml")).and_then(|t| t.parse::<toml::Table>().ok()) else { continue };
        let Some(name) = doc.get("package").and_then(|p| p.get("name")).and_then(|n| n.as_str()) else { continue };
        let lib = doc.get("lib").and_then(|l| l.get("name")).and_then(|n| n.as_str()).map_or_else(|| name.replace('-', "_"), str::to_string);
        let mut bins = Vec::new();
        if let Some(arr) = doc.get("bin").and_then(|b| b.as_array()) {
            for b in arr {
                let bname = b.get("name").and_then(|n| n.as_str()).unwrap_or(name);
                let path = b.get("path").and_then(|n| n.as_str()).unwrap_or("src/main.rs");
                bins.push(Bin { name: bname.to_string(), path: join_norm(&dir, path) });
            }
        }
        let default_main = join_norm(&dir, "src/main.rs");
        if bins.is_empty() && files.contains(&default_main) {
            bins.push(Bin { name: name.to_string(), path: default_main });
        }
        let mut dep_dirs = Vec::new();
        for table in ["dependencies", "dev-dependencies", "build-dependencies"] {
            if let Some(t) = doc.get(table).and_then(|t| t.as_table()) {
                for (_, v) in t {
                    if let Some(p) = v.get("path").and_then(|p| p.as_str()) {
                        dep_dirs.push(join_norm(&dir, p));
                    }
                }
            }
        }
        raw.push((Crate { name: name.to_string(), lib, dir, deps: Vec::new(), bins }, dep_dirs));
    }
    let by_dir: HashMap<String, String> = raw.iter().map(|(c, _)| (c.dir.clone(), c.name.clone())).collect();
    raw.into_iter()
        .map(|(mut c, dep_dirs)| {
            c.deps = dep_dirs.iter().filter_map(|d| by_dir.get(d).cloned()).collect();
            c
        })
        .collect()
}
