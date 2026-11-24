//! A release's config delta is `config-delta.toml` plus one fragment per change under
//! `config-delta.d/`; the delta is their union. Every reader goes through [`delta_text`] or
//! [`merge_delta_texts`], so no two branches share a line to edit.
//!
//! A fragment's value for a key the base file already sets replaces the base's, and the fragment
//! then owns the key: two fragments that differ, a table against a non-table, and any conflict
//! under `renamed` stay errors. Folding writes the replacement over the base line.

use std::path::{Path, PathBuf};
use toml::map::Map;
use toml::Value;

pub const DELTA_FILE: &str = "config-delta.toml";
pub const DELTA_DIR: &str = "config-delta.d";
const UNION_LISTS: [&str; 2] = ["removed", "removed_tasks"];

/// The fragment paths under `<spira_dir>/config-delta.d`, sorted.
pub fn fragment_paths(spira_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let dir = spira_dir.join(DELTA_DIR);
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {}: {e}", dir.display())),
    };
    let mut out: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "toml")).collect();
    out.sort();
    Ok(out)
}

/// The delta of the tree whose `spira/` directory is `spira_dir`; `None` when it has neither
/// the base file nor a fragment.
pub fn delta_text(spira_dir: &Path) -> Result<Option<String>, String> {
    let mut parts = Vec::new();
    let base = spira_dir.join(DELTA_FILE);
    match std::fs::read_to_string(&base) {
        Ok(t) => parts.push((DELTA_FILE.to_string(), t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot read {}: {e}", base.display())),
    }
    for p in fragment_paths(spira_dir)? {
        let t = std::fs::read_to_string(&p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
        parts.push((format!("{DELTA_DIR}/{}", p.file_name().unwrap_or_default().to_string_lossy()), t));
    }
    merge_delta_texts(&parts)
}

/// The union of `(name, text)` delta files. A lone file is returned as written; `removed` and
/// `removed_tasks` are unioned and tables merged. The base file (named `DELTA_FILE`, wherever
/// listed) is merged first and a fragment's value replaces the base's; one key set to two values
/// by anything else is an error naming both files.
pub fn merge_delta_texts(parts: &[(String, String)]) -> Result<Option<String>, String> {
    match parts {
        [] => return Ok(None),
        [(_, text)] => return Ok(Some(text.clone())),
        _ => {}
    }
    let mut merged = Map::new();
    let mut owner: Map<String, Value> = Map::new();
    let (base, frags): (Vec<_>, Vec<_>) = parts.iter().partition(|(n, _)| is_base(n));
    for (name, text) in base.into_iter().chain(frags) {
        let Value::Table(t) = text.parse::<Value>().map_err(|e| format!("{name}: {e}"))? else {
            return Err(format!("{name}: not a TOML table"));
        };
        merge_table(&mut merged, &mut owner, t, "", name)?;
    }
    drop_retired_values(&mut merged);
    toml::to_string_pretty(&Value::Table(merged)).map(Some).map_err(|e| e.to_string())
}

fn is_base(name: &str) -> bool {
    let p = Path::new(name);
    p.file_name().is_some_and(|f| f == DELTA_FILE) && p.parent().and_then(Path::file_name).is_none_or(|d| d != DELTA_DIR)
}

fn drop_retired_values(merged: &mut Map<String, Value>) {
    let removed: Vec<String> = match merged.get("removed") {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        _ => return,
    };
    for path in removed {
        let parts: Vec<&str> = path.split('.').collect();
        remove_value(merged, &parts);
    }
}

fn remove_value(t: &mut Map<String, Value>, path: &[&str]) {
    match path {
        [] => {}
        [leaf] => {
            t.remove(*leaf);
        }
        [head, rest @ ..] => {
            if let Some(Value::Table(next)) = t.get_mut(*head) {
                remove_value(next, rest);
            }
        }
    }
}

fn merge_table(into: &mut Map<String, Value>, owner: &mut Map<String, Value>, from: Map<String, Value>, prefix: &str, name: &str) -> Result<(), String> {
    for (k, v) in from {
        let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        match (into.get_mut(&k), v) {
            (None, v) => {
                owner.insert(k.clone(), owner_of(&v, name));
                into.insert(k, v);
            }
            (Some(Value::Array(have)), Value::Array(more)) if prefix.is_empty() && UNION_LISTS.contains(&k.as_str()) => {
                for item in more {
                    if !have.contains(&item) {
                        have.push(item);
                    }
                }
            }
            (Some(Value::Table(have)), Value::Table(more)) => {
                let Some(Value::Table(o)) = owner.get_mut(&k) else { return Err(format!("{path}: ownership lost")) };
                merge_table(have, o, more, &path, name)?;
            }
            (Some(have), v) => {
                let first = match owner.get(&k) {
                    Some(Value::String(f)) => f.clone(),
                    _ => "an earlier file".into(),
                };
                let scalar = !matches!(have, Value::Table(_)) && !matches!(v, Value::Table(_));
                if scalar && !path.starts_with("renamed.") && is_base(&first) {
                    *have = v;
                    owner.insert(k, Value::String(name.to_string()));
                } else if *have == v {
                } else {
                    return Err(format!("{path} is set to different values by {first} and {name}"));
                }
            }
        }
    }
    Ok(())
}

fn owner_of(v: &Value, name: &str) -> Value {
    match v {
        Value::Table(t) => Value::Table(t.iter().map(|(k, v)| (k.clone(), owner_of(v, name))).collect()),
        _ => Value::String(name.to_string()),
    }
}

/// Folds every fragment into the base file, comments and layout kept, and removes the
/// fragments; the names of the files folded.
pub fn fold_fragments(spira_dir: &Path) -> Result<Vec<String>, String> {
    let frags = fragment_paths(spira_dir)?;
    if frags.is_empty() {
        return Ok(Vec::new());
    }
    let base = spira_dir.join(DELTA_FILE);
    let base_text = std::fs::read_to_string(&base).unwrap_or_default();
    let mut parts = vec![(DELTA_FILE.to_string(), base_text.clone())];
    for p in &frags {
        parts.push((format!("{DELTA_DIR}/{}", p.file_name().unwrap_or_default().to_string_lossy()), std::fs::read_to_string(p).map_err(|e| format!("cannot read {}: {e}", p.display()))?));
    }
    merge_delta_texts(&parts)?;
    let mut doc: toml_edit::DocumentMut = base_text.parse().map_err(|e| format!("{DELTA_FILE}: {e}"))?;
    for (name, text) in &parts[1..] {
        let frag: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{name}: {e}"))?;
        fold_table(doc.as_table_mut(), frag.as_table(), true);
    }
    drop_retired_additions(&mut doc);
    let tmp = base.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string()).and_then(|()| std::fs::rename(&tmp, &base)).map_err(|e| format!("cannot write {}: {e}", base.display()))?;
    for p in &frags {
        std::fs::remove_file(p).map_err(|e| format!("cannot remove {}: {e}", p.display()))?;
    }
    Ok(parts.into_iter().skip(1).map(|(n, _)| n).collect())
}

/// A key the folded delta both sets and removes is dropped from its table: a fragment cannot
/// delete a line of the base, and the delta refuses a key that is both added and removed.
fn drop_retired_additions(doc: &mut toml_edit::DocumentMut) {
    let removed: Vec<String> = doc.get("removed").and_then(|r| r.as_array()).into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
    for path in removed {
        let parts: Vec<&str> = path.split('.').collect();
        let Some((leaf, tables)) = parts.split_last() else { continue };
        remove_at(doc.as_table_mut(), tables, leaf);
    }
}

fn remove_at(table: &mut toml_edit::Table, tables: &[&str], leaf: &str) {
    match tables.split_first() {
        None => {
            table.remove(leaf);
        }
        Some((head, rest)) => {
            if let Some(next) = table.get_mut(head).and_then(|i| i.as_table_mut()) {
                remove_at(next, rest, leaf);
            }
        }
    }
}

fn fold_table(into: &mut toml_edit::Table, from: &toml_edit::Table, top: bool) {
    for (k, item) in from.iter() {
        match item {
            toml_edit::Item::Table(t) => {
                let dst = into.entry(k).or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
                if let Some(dst) = dst.as_table_mut() {
                    fold_table(dst, t, false);
                }
            }
            toml_edit::Item::Value(toml_edit::Value::Array(a)) if top && UNION_LISTS.contains(&k) => {
                let dst = into.entry(k).or_insert_with(|| toml_edit::value(toml_edit::Array::new()));
                if let Some(dst) = dst.as_array_mut() {
                    for v in a.iter() {
                        if !dst.iter().any(|h| h.as_str() == v.as_str()) {
                            dst.push(v.clone());
                        }
                    }
                }
            }
            other => match (into.get_mut(k), other) {
                (None, _) => {
                    into.insert(k, other.clone());
                }
                (Some(have), toml_edit::Item::Value(new)) => {
                    if let toml_edit::Item::Value(old) = have {
                        let bare = |v: &toml_edit::Value| {
                            let mut v = v.clone();
                            *v.decor_mut() = toml_edit::Decor::default();
                            v.to_string()
                        };
                        if bare(old) != bare(new) {
                            let mut new = new.clone();
                            *new.decor_mut() = old.decor().clone();
                            *have = toml_edit::Item::Value(new);
                        }
                    }
                }
                (Some(_), _) => {}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(n: &str, t: &str) -> (String, String) {
        (n.to_string(), t.to_string())
    }

    #[test_id(tpj32rc)]
    #[test]
    fn two_fragments_each_with_a_removed_list_yield_the_union() {
        let text = merge_delta_texts(&[part("config-delta.toml", "[spira]\nk = 1\n"), part("config-delta.d/a.toml", "removed = [\"spira.x\"]\n"), part("config-delta.d/b.toml", "removed = [\"spira.y\"]\nremoved_tasks = [\"t\"]\n")]).unwrap().unwrap();
        let v: Value = text.parse().unwrap();
        let list = |k: &str| v[k].as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(list("removed"), ["spira.x", "spira.y"]);
        assert_eq!(list("removed_tasks"), ["t"]);
        assert_eq!(v["spira"]["k"].as_integer(), Some(1));
        assert!(crate::delta_added(&text).is_ok());
    }

    #[test_id(t732p1y)]
    #[test]
    fn a_lone_file_is_returned_as_written() {
        let t = "# kept\nremoved = [\"a\"] # ffx-ok\n";
        assert_eq!(merge_delta_texts(&[part("config-delta.toml", t)]).unwrap().as_deref(), Some(t));
        assert_eq!(merge_delta_texts(&[]).unwrap(), None);
    }

    #[test_id(ts5kbeb)]
    #[test]
    fn one_key_set_to_two_values_names_both_files_and_equal_values_agree() {
        let e = merge_delta_texts(&[part("config-delta.d/a.toml", "[spira]\nk = 1\n"), part("config-delta.d/b.toml", "[spira]\nk = 2\n")]).unwrap_err();
        assert!(e.contains("spira.k") && e.contains("config-delta.d/a.toml") && e.contains("config-delta.d/b.toml"), "{e}");
        assert!(merge_delta_texts(&[part("a", "[spira]\nk = 1\n"), part("b", "[spira]\nk = 1\n")]).is_ok());
    }

    #[test_id(tazklwe)]
    #[test]
    fn renamed_tables_merge_and_a_conflicting_rename_is_refused() {
        let ok = merge_delta_texts(&[part("a", "[renamed]\n\"spira.a\" = \"spira.b\"\n"), part("b", "[renamed]\n\"spira.c\" = \"spira.d\"\n")]).unwrap().unwrap();
        assert_eq!(ok.parse::<Value>().unwrap()["renamed"].as_table().unwrap().len(), 2);
        assert!(merge_delta_texts(&[part("a", "[renamed]\n\"spira.a\" = \"spira.b\"\n"), part("b", "[renamed]\n\"spira.a\" = \"spira.z\"\n")]).is_err());
    }

    #[test_id(t6czwoj)]
    #[test]
    fn the_directory_reader_reads_base_and_fragments_and_ignores_other_files() {
        let d = testkit::TempDir::new("delta-dir");
        let s = d.path();
        assert_eq!(delta_text(s).unwrap(), None);
        std::fs::create_dir_all(s.join(DELTA_DIR)).unwrap();
        std::fs::write(s.join(DELTA_DIR).join("sp-a.toml"), "removed = [\"spira.x\"]\n").unwrap();
        std::fs::write(s.join(DELTA_DIR).join("README"), "not toml {{").unwrap();
        assert_eq!(delta_text(s).unwrap().as_deref(), Some("removed = [\"spira.x\"]\n"));
        std::fs::write(s.join(DELTA_FILE), "removed = [\"spira.w\"]\n").unwrap();
        let v: Value = delta_text(s).unwrap().unwrap().parse().unwrap();
        assert_eq!(v["removed"].as_array().unwrap().len(), 2);
    }

    #[test_id(t473gy2)]
    #[test]
    fn folding_drops_a_pinned_key_a_fragment_retires() {
        let d = testkit::TempDir::new("delta-fold-retired");
        let s = d.path();
        std::fs::create_dir_all(s.join(DELTA_DIR)).unwrap();
        std::fs::write(s.join(DELTA_FILE), "[spira]\nkeep = 1\ngone = 2\n").unwrap();
        std::fs::write(s.join(DELTA_DIR).join("a.toml"), "removed = [\"spira.gone\"]\n").unwrap();
        fold_fragments(s).unwrap();
        let text = std::fs::read_to_string(s.join(DELTA_FILE)).unwrap();
        assert!(text.contains("keep = 1") && !text.contains("gone = 2") && text.contains("spira.gone"), "{text}");
    }

    #[test_id(tv9brti)]
    #[test]
    fn the_loaded_delta_drops_a_pinned_key_a_fragment_retires() {
        let d = testkit::TempDir::new("delta-load-retired");
        let s = d.path();
        std::fs::create_dir_all(s.join(DELTA_DIR)).unwrap();
        std::fs::write(s.join(DELTA_FILE), "[spira]\nkeep = 1\ngone = 2\n").unwrap();
        std::fs::write(s.join(DELTA_DIR).join("a.toml"), "removed = [\"spira.gone\"]\n").unwrap();
        let text = delta_text(s).unwrap().unwrap();
        assert!(text.contains("keep = 1") && !text.contains("gone = 2") && text.contains("spira.gone"), "{text}");
    }

    #[test_id(tdmx7w2)]
    #[test]
    fn folding_moves_fragments_into_the_base_keeping_its_comments_and_removes_them() {
        let d = testkit::TempDir::new("delta-fold");
        let s = d.path();
        std::fs::create_dir_all(s.join(DELTA_DIR)).unwrap();
        std::fs::write(s.join(DELTA_FILE), "# head\nremoved = [\"spira.w\"] # ffx-ok\n[spira]\nk = 1\n").unwrap();
        std::fs::write(s.join(DELTA_DIR).join("a.toml"), "removed = [\"spira.x\"]\nremoved_tasks = [\"t\"]\n[spira]\nn = 2\n").unwrap();
        std::fs::write(s.join(DELTA_DIR).join("b.toml"), "removed = [\"spira.w\", \"spira.y\"]\n").unwrap();
        let before: Value = delta_text(s).unwrap().unwrap().parse().unwrap();
        assert_eq!(fold_fragments(s).unwrap().len(), 2);
        assert!(fragment_paths(s).unwrap().is_empty());
        let text = std::fs::read_to_string(s.join(DELTA_FILE)).unwrap();
        assert!(text.contains("# head") && text.contains("# ffx-ok"), "{text}");
        let after: Value = delta_text(s).unwrap().unwrap().parse().unwrap();
        assert_eq!(after, before);
    }

    #[test_id(t9wgqsy)]
    #[test]
    fn a_fragment_value_replaces_the_bases_whatever_the_base_name_or_position() {
        for base in ["config-delta.toml", "spira/config-delta.toml"] {
            let b = part(base, "[spira]\nk = 1\n");
            let f = part("config-delta.d/a.toml", "[spira]\nk = 2\n");
            for parts in [[b.clone(), f.clone()], [f, b]] {
                let v: Value = merge_delta_texts(&parts).unwrap().unwrap().parse().unwrap();
                assert_eq!(v["spira"]["k"].as_integer(), Some(2));
            }
        }
    }

    #[test_id(tlx40kd)]
    #[test]
    fn two_fragments_that_disagree_over_a_base_value_are_refused_in_either_order() {
        let b = part("config-delta.toml", "[spira]\nk = 1\n");
        let (a, c) = (part("config-delta.d/a.toml", "[spira]\nk = 1\n"), part("config-delta.d/b.toml", "[spira]\nk = 2\n"));
        for frags in [[a.clone(), c.clone()], [c, a]] {
            let e = merge_delta_texts(&[b.clone(), frags[0].clone(), frags[1].clone()]).unwrap_err();
            assert!(e.contains("config-delta.d/a.toml") && e.contains("config-delta.d/b.toml") && !e.contains(" config-delta.toml"), "{e}");
        }
    }

    #[test_id(tdsq7hp)]
    #[test]
    fn folding_a_superseding_fragment_rewrites_the_base_line_keeping_its_comment() {
        let d = testkit::TempDir::new("delta-fold-supersede");
        let s = d.path();
        std::fs::create_dir_all(s.join(DELTA_DIR)).unwrap();
        std::fs::write(s.join(DELTA_FILE), "# head\n[spira]\n# about k\nk = 1 # note\nz = 9\n").unwrap();
        std::fs::write(s.join(DELTA_DIR).join("a.toml"), "[spira]\nk = 2\n").unwrap();
        let before = delta_text(s).unwrap();
        fold_fragments(s).unwrap();
        let text = std::fs::read_to_string(s.join(DELTA_FILE)).unwrap();
        assert_eq!(text, "# head\n[spira]\n# about k\nk = 2 # note\nz = 9\n");
        assert!(fragment_paths(s).unwrap().is_empty());
        assert_eq!(delta_text(s).unwrap().map(|t| t.parse::<Value>().unwrap()), before.map(|t| t.parse::<Value>().unwrap()));
    }

    #[test_id(tv54gxs)]
    #[test]
    fn a_fragment_may_not_remap_a_renamed_key_or_set_a_table_over_a_scalar() {
        let b = part("config-delta.toml", "[renamed]\n\"spira.a\" = \"spira.b\"\n[spira]\nk = 1\n");
        assert!(merge_delta_texts(&[b.clone(), part("config-delta.d/a.toml", "[renamed]\n\"spira.a\" = \"spira.z\"\n")]).is_err());
        assert!(merge_delta_texts(&[b, part("config-delta.d/a.toml", "[spira.k]\nx = 1\n")]).is_err());
    }
}
