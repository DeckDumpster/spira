//! Candidate review units: related sets of files the code's own edges define.

use crate::index::{ext, Crate, Index};
use crate::rustsrc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitFile {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unit {
    pub id: String,
    pub kind: Kind,
    /// The first file is the primary one.
    pub files: Vec<UnitFile>,
    pub title: String,
}

fn unit(id: String, kind: Kind, title: String, paths: Vec<String>) -> Unit {
    let mut seen = BTreeSet::new();
    let files = paths.into_iter().filter(|p| seen.insert(p.clone())).map(|path| UnitFile { path }).collect();
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

fn is_test_path(path: &str, c: &Crate) -> bool {
    let tests_dir = format!("{}/", crate::index::join_norm(&c.dir, "tests"));
    let stem = path.rsplit('/').next().and_then(|n| n.split('.').next()).unwrap_or("");
    is_test_file(path) || path.starts_with(&tests_dir) || stem.ends_with("_tests")
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

fn mod_path(path: &str, c: &Crate) -> Option<Vec<String>> {
    let rel = path.strip_prefix(&format!("{}/", crate::index::join_norm(&c.dir, "src")))?.strip_suffix(".rs")?;
    let mut segs: Vec<String> = rel.split('/').map(String::from).collect();
    let last = segs.last().map(String::as_str);
    if last == Some("mod") || (segs.len() == 1 && matches!(last, Some("lib" | "main"))) {
        segs.pop();
    }
    Some(segs)
}

/// How one file sees a trait: which paths it can write that reach the trait.
struct Scope<'a> {
    name: &'a str,
    lib: &'a str,
    exported: &'a [Vec<String>],
    in_def: bool,
    fm: Option<Vec<String>>,
    uses: Vec<rustsrc::Use>,
    shadowed: bool,
}

impl<'a> Scope<'a> {
    fn new(ix: &Index, file: &str, def: &'a Crate, name: &'a str, exported: &'a [Vec<String>]) -> Option<Scope<'a>> {
        let k = ix.crate_of(file)?;
        let in_def = k.dir == def.dir;
        if !in_def && !k.deps.contains(&def.name) {
            return None;
        }
        let code = rustsrc::code_lines(&ix.text[file]);
        Some(Scope {
            name,
            lib: &def.lib,
            exported,
            in_def,
            fm: if in_def { mod_path(file, def) } else { None },
            uses: rustsrc::uses(&code.join("\n")),
            shadowed: code.iter().any(|l| rustsrc::defines_type(l, name, false).is_some()),
        })
    }

    fn absolute(&self, path: &[String]) -> Vec<Vec<String>> {
        let Some((first, rest)) = path.split_first() else { return Vec::new() };
        let mut out = Vec::new();
        match first.as_str() {
            "crate" => out.extend(self.fm.as_ref().map(|_| rest.to_vec())),
            "self" => out.extend(self.fm.as_ref().map(|m| [m.as_slice(), rest].concat())),
            "super" => {
                let ups = path.iter().take_while(|s| *s == "super").count();
                out.extend(self.fm.as_ref().filter(|m| m.len() >= ups).map(|m| [&m[..m.len() - ups], &path[ups..]].concat()));
            }
            f => {
                if f == self.lib {
                    out.push(rest.to_vec());
                }
                if self.in_def {
                    out.push(path.to_vec());
                    out.extend(self.fm.as_ref().map(|m| [m.as_slice(), path].concat()));
                }
            }
        }
        out
    }

    fn reaches(&self, path: &[String]) -> bool {
        self.absolute(path).iter().any(|a| self.exported.contains(a))
    }

    fn resolves(&self, path: &[String]) -> bool {
        let Some(first) = path.first() else { return false };
        let mut bound = false;
        for u in self.uses.iter().filter(|u| !u.glob && u.alias == *first) {
            bound = true;
            if self.reaches(&[u.path.as_slice(), &path[1..]].concat()) {
                return true;
            }
        }
        if path.len() > 1 {
            return self.reaches(path);
        }
        !bound && !self.shadowed && first == self.name && self.uses.iter().any(|u| u.glob && self.reaches(&[u.path.as_slice(), path].concat()))
    }
}

/// Every path in the defining crate that names the trait: its own, and each `pub use` of it.
fn exported_paths(ix: &Index, def: &Crate, file: &str, name: &str) -> Vec<Vec<String>> {
    let mut exported: Vec<Vec<String>> = mod_path(file, def).map(|m| [m, vec![name.to_string()]].concat()).into_iter().collect();
    let reexporters: Vec<&String> = ix.files_naming(name).iter().filter(|f| *f != file && ix.crate_of(f).is_some_and(|k| k.dir == def.dir)).collect();
    loop {
        let mut found = Vec::new();
        for g in &reexporters {
            let Some(scope) = Scope::new(ix, g, def, name, &exported) else { continue };
            let Some(gm) = mod_path(g, def) else { continue };
            for u in scope.uses.iter().filter(|u| u.public && !u.glob && scope.reaches(&u.path)) {
                let e = [gm.clone(), vec![u.alias.clone()]].concat();
                if !exported.contains(&e) && !found.contains(&e) {
                    found.push(e);
                }
            }
        }
        if found.is_empty() {
            return exported;
        }
        exported.extend(found);
    }
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
            if let Some(def) = ix.crate_of(path) {
                let exported = exported_paths(ix, def, path, &name);
                for f in ix.files_naming(&name) {
                    if f == path {
                        continue;
                    }
                    let Some(scope) = Scope::new(ix, f, def, &name, &exported) else { continue };
                    let code = rustsrc::code_lines(&ix.text[f]);
                    let resolves = |p: &[String]| scope.resolves(p);
                    if code.iter().any(|l| rustsrc::is_impl_of(l, &resolves)) {
                        impls.push(f.clone());
                    } else if rustsrc::names_trait_object(&code.join("\n"), &resolves) {
                        callers.push(f.clone());
                    }
                }
            }
            let in_tests = |f: &String| ix.crate_of(f).is_some_and(|c| is_test_path(f, c));
            let (test_impls, impls): (Vec<_>, Vec<_>) = impls.into_iter().partition(in_tests);
            let (test_callers, callers): (Vec<_>, Vec<_>) = callers.into_iter().partition(in_tests);
            paths.extend([impls, callers, test_impls, test_callers].concat());
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
