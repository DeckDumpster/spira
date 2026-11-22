//! Every persona prompt in the tree names only tools and paths the release ships.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    testkit::manifest_dir().join("..")
}

fn rs_text(dir: &Path, out: &mut String) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            rs_text(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
        }
    }
}

/// What `release build` puts in `bin/` (every declared `[[bin]]` plus the compat aliases), each
/// with its crate's source as the text a verb is looked for in.
fn release_bins(root: &Path) -> BTreeMap<String, String> {
    let wanted: BTreeSet<String> = release::workspace::expected_bins(root).expect("workspace bins").into_iter().collect();
    let manifest: toml::Value = std::fs::read_to_string(root.join("Cargo.toml")).expect("root manifest").parse().expect("parse root manifest");
    let mut out = BTreeMap::new();
    for m in manifest["workspace"]["members"].as_array().expect("members") {
        let dir = root.join(m.as_str().expect("member"));
        let doc: toml::Value = std::fs::read_to_string(dir.join("Cargo.toml")).expect("member manifest").parse().expect("parse member manifest");
        let mut names: Vec<String> = doc.get("bin").and_then(|b| b.as_array()).into_iter().flatten().filter_map(|b| b["name"].as_str().map(String::from)).collect();
        names.extend(doc["package"]["name"].as_str().map(String::from));
        let mut src = String::new();
        rs_text(&dir.join("src"), &mut src);
        for n in names.into_iter().filter(|n| wanted.contains(n)) {
            out.insert(n, src.clone());
        }
    }
    for (name, alias) in release::compat::read(root) {
        let src = out.get(&name).cloned().unwrap_or_default();
        out.insert(alias, src);
    }
    out
}

/// Base names resolve as the tree ships them: the built bins, then `spira/`, the root and `cockpit/`.
fn lookup<'a>(root: &'a Path, bins: &'a BTreeMap<String, String>) -> impl Fn(&str) -> Option<String> + 'a {
    let read = move |name: &str| {
        if let Some(src) = bins.get(name) {
            return Some(src.clone());
        }
        [root.join("spira"), root.to_path_buf(), root.join("cockpit")]
            .iter()
            .map(|d| d.join(name))
            .find(|p| p.is_file())
            .map(|p| String::from_utf8_lossy(&std::fs::read(p).unwrap_or_default()).into_owned())
    };
    move |name: &str| read(name).map(|t| rule::audit::follow_shim(&t, &read))
}

#[test]
fn every_persona_prompt_names_only_what_the_release_ships() {
    let root = repo_root();
    let bins = release_bins(&root);
    let names: BTreeSet<String> = bins.keys().cloned().collect();
    let resolve = rule::audit::placeholder_resolver(&root, &names);
    let look = lookup(&root, &bins);
    let mut seen = 0;
    let mut bad = Vec::new();
    for e in std::fs::read_dir(root.join("spira/runes")).expect("read runes").flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".md") {
            continue;
        }
        seen += 1;
        let text = std::fs::read_to_string(e.path()).expect("read prompt");
        bad.extend(rule::audit::prompt_findings(&name, &text, &resolve, &look));
    }
    assert!(seen > 0, "no persona prompts under spira/runes");
    bad.sort();
    assert!(bad.is_empty(), "\n{}\n", bad.join("\n"));
}
