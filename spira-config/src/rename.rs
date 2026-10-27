//! Renaming a persona across the one source: its `[persona.<old>]` table (lease included) and
//! its `lane` when that is its own name, and its name in `spira.runes` and the space-separated `spira.lanes`. `set` and `unset` cannot
//! do this: they edit through the typed schema, which refuses a half-built persona table.

use toml_edit::{value, Array, DocumentMut, Item};

fn rewrite_words(words: impl Iterator<Item = String>, old: &str, new: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in words {
        let w = if w == old { new.to_string() } else { w };
        if !out.contains(&w) {
            out.push(w);
        }
    }
    out
}

/// Strict: `old` must be declared. See [`rename_persona_lenient`] for the activation form.
pub fn rename_persona_text(text: &str, old: &str, new: &str) -> Result<String, String> {
    rename_persona_with(text, old, new, true)
}

/// A layer without `persona.<old>` still has its roster and lane entries rewritten, and a
/// layer already holding `persona.<new>` with no `<old>` is left as it is, so applying a
/// rename twice changes nothing.
pub fn rename_persona_lenient(text: &str, old: &str, new: &str) -> Result<String, String> {
    rename_persona_with(text, old, new, false)
}

fn rename_persona_with(text: &str, old: &str, new: &str, strict: bool) -> Result<String, String> {
    if old.is_empty() || new.is_empty() || old.contains(['.', ' ']) || new.contains(['.', ' ']) {
        return Err(format!("persona names are single words, got {old:?} -> {new:?}"));
    }
    let mut doc: DocumentMut = text.parse().map_err(|e| format!("not valid TOML: {e}"))?;
    let personas = doc.get_mut("persona").and_then(Item::as_table_like_mut);
    match personas {
        Some(personas) if personas.contains_key(old) => {
            if personas.contains_key(new) {
                return Err(format!("persona.{new} already exists"));
            }
            let table = personas.remove(old).ok_or("unreachable")?;
            personas.insert(new, table);
            if let Some(lane) = personas.get_mut(new).and_then(Item::as_table_like_mut).and_then(|t| t.get_mut("lane")) {
                if lane.as_str() == Some(old) {
                    *lane = value(new);
                }
            }
        }
        _ if strict => return Err(format!("persona.{old} is not declared")),
        _ => {}
    }
    if let Some(spira) = doc.get_mut("spira").and_then(Item::as_table_like_mut) {
        if let Some(item) = spira.get_mut("runes") {
            if let Some(arr) = item.as_array() {
                let words = rewrite_words(arr.iter().filter_map(|v| v.as_str().map(String::from)), old, new);
                let mut out = Array::new();
                for w in words {
                    out.push(w);
                }
                *item = value(out);
            } else if let Some(s) = item.as_str() {
                *item = value(rewrite_words(s.split_whitespace().map(String::from), old, new).join(" "));
            }
        }
        if let Some(item) = spira.get_mut("lanes") {
            if let Some(s) = item.as_str() {
                *item = value(rewrite_words(s.split_whitespace().map(String::from), old, new).join(" "));
            }
        }
    }
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "[spira]\nrunes = [\"ops\", \"maechen\"]\nlanes = \"ops maechen czar\"\n\n[persona.maechen]\nmodel = \"m\"\n\n[persona.maechen.lease]\nttl = 5\n\n[persona.ops]\nmodel = \"m\"\n";

    #[test]
    fn renames_table_lease_roster_and_lanes() {
        let out = rename_persona_text(SRC, "maechen", "mimir").unwrap();
        let v: toml::Value = out.parse().unwrap();
        assert!(v["persona"].get("maechen").is_none());
        assert_eq!(v["persona"]["mimir"]["lease"]["ttl"].as_integer(), Some(5));
        assert_eq!(v["spira"]["lanes"].as_str(), Some("ops mimir czar"));
        assert_eq!(v["spira"]["runes"][1].as_str(), Some("mimir"));
    }

    #[test]
    fn refuses_a_missing_source_and_an_existing_target() {
        assert!(rename_persona_text(SRC, "nope", "x").is_err());
        assert!(rename_persona_text(SRC, "maechen", "ops").is_err());
    }
}
