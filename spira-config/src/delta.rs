//! A release's config delta is `config-delta.toml` plus one fragment per change under
//! `config-delta.d/`; the delta is their union. Every reader goes through [`delta_text`] or
//! [`merge_delta_texts`], so no two branches share a line to edit.

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
/// `removed_tasks` are unioned, tables merged, and one key set to two values is an error
/// naming both files.
pub fn merge_delta_texts(parts: &[(String, String)]) -> Result<Option<String>, String> {
    match parts {
        [] => return Ok(None),
        [(_, text)] => return Ok(Some(text.clone())),
        _ => {}
    }
    let mut merged = Map::new();
    let mut owner: Map<String, Value> = Map::new();
    for (name, text) in parts {
        let Value::Table(t) = text.parse::<Value>().map_err(|e| format!("{name}: {e}"))? else {
            return Err(format!("{name}: not a TOML table"));
        };
        merge_table(&mut merged, &mut owner, t, "", name)?;
    }
    toml::to_string_pretty(&Value::Table(merged)).map(Some).map_err(|e| e.to_string())
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
            (Some(have), v) if *have == v => {}
            (Some(_), _) => {
                let first = match owner.get(&k) {
                    Some(Value::String(f)) => f.clone(),
                    _ => "an earlier file".into(),
                };
                return Err(format!("{path} is set to different values by {first} and {name}"));
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
    let tmp = base.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string()).and_then(|()| std::fs::rename(&tmp, &base)).map_err(|e| format!("cannot write {}: {e}", base.display()))?;
    for p in &frags {
        std::fs::remove_file(p).map_err(|e| format!("cannot remove {}: {e}", p.display()))?;
    }
    Ok(parts.into_iter().skip(1).map(|(n, _)| n).collect())
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
            other => {
                if !into.contains_key(k) {
                    into.insert(k, other.clone());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(n: &str, t: &str) -> (String, String) {
        (n.to_string(), t.to_string())
    }

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

    #[test]
    fn a_lone_file_is_returned_as_written() {
        let t = "# kept\nremoved = [\"a\"] # ffx-ok\n";
        assert_eq!(merge_delta_texts(&[part("config-delta.toml", t)]).unwrap().as_deref(), Some(t));
        assert_eq!(merge_delta_texts(&[]).unwrap(), None);
    }

    #[test]
    fn one_key_set_to_two_values_names_both_files_and_equal_values_agree() {
        let e = merge_delta_texts(&[part("config-delta.d/a.toml", "[spira]\nk = 1\n"), part("config-delta.d/b.toml", "[spira]\nk = 2\n")]).unwrap_err();
        assert!(e.contains("spira.k") && e.contains("config-delta.d/a.toml") && e.contains("config-delta.d/b.toml"), "{e}");
        assert!(merge_delta_texts(&[part("a", "[spira]\nk = 1\n"), part("b", "[spira]\nk = 1\n")]).is_ok());
    }

    #[test]
    fn renamed_tables_merge_and_a_conflicting_rename_is_refused() {
        let ok = merge_delta_texts(&[part("a", "[renamed]\n\"spira.a\" = \"spira.b\"\n"), part("b", "[renamed]\n\"spira.c\" = \"spira.d\"\n")]).unwrap().unwrap();
        assert_eq!(ok.parse::<Value>().unwrap()["renamed"].as_table().unwrap().len(), 2);
        assert!(merge_delta_texts(&[part("a", "[renamed]\n\"spira.a\" = \"spira.b\"\n"), part("b", "[renamed]\n\"spira.a\" = \"spira.z\"\n")]).is_err());
    }

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
}
