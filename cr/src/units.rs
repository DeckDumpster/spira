//! Candidate review units: related sets of files the code's own edges define.

use crate::index::{ext, Crate, Index};
use crate::rustsrc;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Module,
    Trait,
    Boundary,
    Script,
    Persona,
}

pub const KINDS: [Kind; 5] = [Kind::Module, Kind::Trait, Kind::Boundary, Kind::Script, Kind::Persona];

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Module => "module",
            Kind::Trait => "trait",
            Kind::Boundary => "boundary",
            Kind::Script => "script",
            Kind::Persona => "persona",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitFile {
    pub path: String,
    pub regions: Option<Vec<(u32, u32)>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub id: String,
    pub kind: Kind,
    /// The first file is the primary one.
    pub files: Vec<UnitFile>,
    pub title: String,
}

fn unit(id: String, kind: Kind, title: String, paths: Vec<String>) -> Unit {
    let mut seen = BTreeSet::new();
    let files = paths.into_iter().filter(|p| seen.insert(p.clone())).map(|path| UnitFile { path, regions: None }).collect();
    Unit { id, kind, files, title }
}

pub fn all(ix: &Index) -> Vec<Unit> {
    let mut v = module_units(ix);
    v.extend(trait_units(ix));
    v.extend(boundary_units(ix));
    v.extend(script_units(ix));
    v.extend(persona_units(ix));
    v.sort_by(|a, b| a.id.cmp(&b.id));
    v
}

/// `(module stem, is crate root)` for a `.rs` file under `<crate>/src/`.
fn module_stem(path: &str, c: &Crate) -> Option<(String, bool)> {
    let rel = path.strip_prefix(&format!("{}/src/", c.dir))?;
    let file = rel.rsplit('/').next()?;
    let stem = file.strip_suffix(".rs")?;
    match stem {
        "lib" | "main" if !rel.contains('/') => Some((stem.to_string(), true)),
        "mod" => Some((rel.rsplit('/').nth(1)?.to_string(), false)),
        s => Some((s.to_string(), false)),
    }
}

fn test_sibling(path: &str, root: bool) -> String {
    let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
    let stem = path.rsplit('/').next().unwrap_or("").trim_end_matches(".rs");
    if root || stem == "mod" {
        format!("{dir}/tests.rs")
    } else {
        format!("{dir}/{stem}/tests.rs")
    }
}

fn is_test_file(path: &str) -> bool {
    path.ends_with("/tests.rs") || path == "tests.rs"
}

pub fn module_units(ix: &Index) -> Vec<Unit> {
    let mut out = Vec::new();
    for c in &ix.crates {
        for path in ix.files.iter().filter(|p| ext(p) == "rs") {
            if is_test_file(path) {
                continue;
            }
            let Some((stem, root)) = module_stem(path, c) else { continue };
            if ix.crate_of(path).map(|k| &k.dir) != Some(&c.dir) {
                continue;
            }
            let mut paths = vec![path.clone()];
            let sibling = test_sibling(path, root);
            if ix.has_file(&sibling) {
                paths.push(sibling);
            }
            if !root {
                let prefix = format!("{}/tests/", c.dir);
                paths.extend(ix.files_naming_pair(&c.lib, &stem).iter().filter(|f| f.starts_with(&prefix)).cloned());
            }
            out.push(unit(format!("module:{path}"), Kind::Module, format!("{}::{}", c.lib, stem), paths));
        }
    }
    out
}

pub fn trait_units(ix: &Index) -> Vec<Unit> {
    let mut out = Vec::new();
    for (path, text) in ix.text.iter().filter(|(p, _)| ext(p) == "rs") {
        for line in rustsrc::code_lines(text).iter() {
            let mut toks = line.split_whitespace();
            if toks.next() != Some("pub") || toks.next() != Some("trait") {
                continue;
            }
            let name: String = toks.next().unwrap_or("").chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if name.is_empty() {
                continue;
            }
            let mut paths = vec![path.clone()];
            let mut impls = Vec::new();
            let mut callers = Vec::new();
            for f in ix.files_naming(&name) {
                if f == path {
                    continue;
                }
                let code = rustsrc::code_lines(&ix.text[f]);
                if code.iter().any(|l| rustsrc::is_impl_of(l, &name)) {
                    impls.push(f.clone());
                } else if rustsrc::names_trait_object(&code.join("\n"), &name) {
                    callers.push(f.clone());
                }
            }
            paths.extend(impls);
            paths.extend(callers);
            out.push(unit(format!("trait:{path}::{name}"), Kind::Trait, format!("trait {name}"), paths));
        }
    }
    out
}

fn module_file(ix: &Index, c: &Crate, module: &str) -> Option<String> {
    let flat = format!("{}/src/{module}.rs", c.dir);
    let nested = format!("{}/src/{module}/mod.rs", c.dir);
    [flat, nested].into_iter().find(|p| ix.has_file(p))
}

pub fn boundary_units(ix: &Index) -> Vec<Unit> {
    let mut out = Vec::new();
    for caller in &ix.crates {
        for dep_name in &caller.deps {
            let Some(callee) = ix.crate_named(dep_name) else { continue };
            for (_, module) in ix.pair_keys_for(&callee.lib) {
                let Some(target) = module_file(ix, callee, &module) else { continue };
                for f in ix.files_naming_pair(&callee.lib, &module) {
                    let in_caller_src = f.starts_with(&format!("{}/src/", caller.dir));
                    if in_caller_src && !is_test_file(f) {
                        out.push(unit(
                            format!("boundary:{f}->{target}"),
                            Kind::Boundary,
                            format!("{f} uses {}::{module}", callee.lib),
                            vec![f.clone(), target.clone()],
                        ));
                    }
                }
            }
        }
    }
    out
}

pub fn command_words(line: &str) -> Vec<String> {
    let t = line.trim_start();
    if t.starts_with('#') {
        return Vec::new();
    }
    t.split(['|', '&', ';', '(', '`', '{', '}'])
        .filter_map(|seg| {
            seg.split_whitespace().find(|w| !matches!(*w, "exec" | "sudo" | "env" | "!" | "then" | "do" | "else" | "time") && !(w.contains('=') && !w.starts_with('-') && !w.contains('/')))
        })
        .map(|w| w.trim_matches(|c| c == '"' || c == '\'').rsplit('/').next().unwrap_or("").to_string())
        .collect()
}

pub fn script_units(ix: &Index) -> Vec<Unit> {
    let bins: Vec<(&str, &str)> = ix.crates.iter().flat_map(|c| c.bins.iter().map(|b| (b.name.as_str(), b.path.as_str()))).collect();
    let mut out = Vec::new();
    for (path, text) in ix.text.iter().filter(|(p, _)| ext(p) == "sh") {
        let words: BTreeSet<String> = text.lines().flat_map(command_words).collect();
        for (bin, main) in &bins {
            if words.contains(*bin) && ix.has_file(main) {
                out.push(unit(format!("script:{path}:{bin}"), Kind::Script, format!("{path} runs {bin}"), vec![path.clone(), (*main).to_string()]));
            }
        }
    }
    out
}

pub fn persona_units(ix: &Index) -> Vec<Unit> {
    let mut groups: BTreeSet<(String, String)> = BTreeSet::new();
    for f in &ix.files {
        let Some((dir, name)) = f.rsplit_once('/') else { continue };
        if dir.rsplit('/').next() != Some("hooks") {
            continue;
        }
        let prefix = name.split('-').next().unwrap_or(name);
        let prefix = if prefix == name { name.split('.').next().unwrap_or(name) } else { prefix };
        groups.insert((dir.to_string(), prefix.to_string()));
    }
    let mut out = Vec::new();
    for (dir, prefix) in groups {
        let mut paths: Vec<String> = ix
            .files
            .iter()
            .filter(|f| f.rsplit_once('/').is_some_and(|(d, n)| d == dir && (n.starts_with(&format!("{prefix}-")) || n.split('.').next() == Some(prefix.as_str()))))
            .cloned()
            .collect();
        paths.sort();
        let prompt = ix.files.iter().find(|f| ext(f) == "md" && !f.contains("/hooks/") && f.rsplit('/').next().and_then(|n| n.strip_suffix(".md")) == Some(prefix.as_str()));
        paths.extend(prompt.cloned());
        out.push(unit(format!("persona:{dir}/{prefix}"), Kind::Persona, format!("persona {prefix}"), paths));
    }
    out
}
