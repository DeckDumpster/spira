//! The config key registry — `spira/conf.d/<KEY>`, one file per key (sp-g3uwp). Read-only
//! here: this module mirrors `conf-gen.sh`'s own parser and topological sort so
//! [`crate::resolve::resolve`] can apply each key's generated default in-process, without
//! shelling out to bash for the 222 keys whose default is a plain `: "${KEY:=...}"`
//! statement. Never writes `conf.d` — enact a key by adding or editing its file under
//! `spira/conf.d/`, never here.
//!
//! WHAT THIS DOES NOT COVER (same fence `conf-gen.sh` itself names): a key whose `DEFAULT`
//! body carries no real statement — a `PROCEDURAL` stub (ordering constraints) or a
//! `NO DEFAULT` stub — contributes only its name to the allowlist. `spira::resolve::resolve`
//! hand-ports the procedural keys directly (see its module doc) and defaults a no-default key
//! to the empty string, exactly as `spira_conf_defaults` does today.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// One `spira/conf.d/<KEY>` file, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryKey {
    pub name: String,
    pub ty: String,
    pub group: String,
    pub doc: String,
    /// The raw text between the `DEFAULT<<'SPIRA_CONF_DEFAULT_EOF'` / `SPIRA_CONF_DEFAULT_EOF`
    /// markers, exactly as `conf-gen.sh`'s own `parse_one` reads it (comments and at most one
    /// `: "${KEY:=...}"` statement).
    pub default_body: String,
}

impl RegistryKey {
    /// The one `: "${KEY:=EXPR}"` statement in [`default_body`](Self::default_body), if it
    /// carries one — `None` for a `PROCEDURAL` or `NO DEFAULT` stub, which is allowlist-only
    /// (`conf-gen.sh`: "a DEFAULT body with no statement in it is read as allowlist membership
    /// only — not an error"). Only the `:=` form appears anywhere in the registry today (the
    /// `=` preserve-empty form is used only by the hand-written keys `conf.d`'s PROCEDURAL
    /// stubs name); a key that ever grows one would need this parser extended to match.
    pub fn default_expr(&self) -> Option<&str> {
        let needle = format!("${{{}:=", self.name);
        let idx = self.default_body.find(&needle)?;
        let start = idx + needle.len();
        extract_braced(&self.default_body, start)
    }

    /// Every OTHER DEFAULTED registry key named inside this key's own default text — same
    /// test `conf-gen.sh`'s dependency-edge loop uses (`*"\$$other"*` / `*'${'"$other"*`),
    /// and restricted the same way `conf-gen.sh` restricts it: only among `has_default` keys
    /// (`for other in "${has_default[@]}"`). A hand-written key like `SPIRA_RUN` is named in
    /// plenty of defaults but carries no registry default of its own, so it is never a node
    /// in this graph — counting it as one would inflate another key's in-degree past what
    /// [`topo_order`]'s Kahn's-algorithm pass could ever bring back to zero, misreporting an
    /// ordinary hand-written dependency as a cycle.
    fn deps_among(&self, all: &BTreeMap<String, RegistryKey>) -> Vec<String> {
        let Some(expr) = self.default_expr() else { return Vec::new() };
        all.iter()
            .filter(|(k, rk)| *k != &self.name && rk.default_expr().is_some())
            .filter(|(k, _)| expr.contains(k.as_str()))
            .map(|(k, _)| k.clone())
            .collect()
    }
}

/// Scans forward from `start` (the byte offset right after an already-consumed, unmatched
/// `{`) for the matching `}`, tracking brace depth so a nested `${...}` inside a default
/// (`${SPIRA_WIKI:-$SPIRA_REPO}`'s own braces notwithstanding — none of today's registry
/// defaults nest a SECOND `${` inside the first, but this stays depth-aware rather than
/// "find the next `}`" in case one ever does) is not mistaken for the close. Returns the text
/// strictly between, or `None` if the braces never balance.
fn extract_braced(s: &str, start: usize) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut depth: i32 = 1;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..i]);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Parses every file directly inside `dir` the way `conf-gen.sh`'s `parse_one` does: `TYPE=`,
/// `GROUP=`, `DOC=` lines, then a `DEFAULT<<'SPIRA_CONF_DEFAULT_EOF'` heredoc closed by a line
/// that is exactly `SPIRA_CONF_DEFAULT_EOF`. A filename that is not an uppercase `KEY` (no
/// lowercase, no punctuation beyond `_`) is skipped, mirroring `parse_one`'s own `case "$key"
/// in [A-Z_]*` guard — a directory holding `.gitkeep` or similar must not become a spurious
/// key.
///
/// A MISSING DIRECTORY IS A NAMED ERROR, NOT AN EMPTY REGISTRY (sp-1cdgq round 3). It used
/// to return `Ok(empty)` so the caller (`resolve`) could fall back to its ~29 hand-written
/// keys alone — meant for "a release that has not shipped `conf.d` yet" but in practice hit
/// by a test fixture's `--home` that simply forgot to copy `conf.d` in, twice
/// (sp-8qm8g, then sp-1cdgq itself): the whole generic registry pass silently resolved to
/// nothing, with no error anywhere, and a config knob the fixture's own `export` believed it
/// had set (`SPIRA_MAX_AEONS`, `SPIRA_SUMMON_JITTER`) never reached `Conf`. Production always
/// ships `conf.d` beside the release's binaries, so this changes nothing real — it only turns
/// a silently-degraded test fixture into a named failure the first time it is run, which is
/// the whole point. An EXISTING-but-empty directory is still not an error (conf-gen.sh itself
/// is the one place that is fatal, since it would otherwise regenerate an empty allowlist);
/// only a directory `read_dir` cannot open at all — missing, or something else wrong with it —
/// is refused.
pub fn embedded() -> Result<BTreeMap<String, RegistryKey>, String> {
    EMBEDDED_CONF_D.iter().map(|(name, text)| Ok((name.to_string(), parse_one(name, text)?))).collect()
}

include!(concat!(env!("OUT_DIR"), "/embedded_conf_d.rs"));

pub fn load(dir: &Path) -> Result<BTreeMap<String, RegistryKey>, String> {
    let mut out = BTreeMap::new();
    let entries = fs::read_dir(dir).map_err(|_| format!("no config registry at {}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if !name.chars().all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit()) {
            continue;
        }
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        out.insert(name.clone(), parse_one(&name, &text)?);
    }
    Ok(out)
}

fn parse_one(name: &str, text: &str) -> Result<RegistryKey, String> {
    let mut ty = String::new();
    let mut group = String::new();
    let mut doc = String::new();
    let mut body = String::new();
    let mut in_default = false;
    let mut closed = true;
    for line in text.lines() {
        if in_default {
            if line == "SPIRA_CONF_DEFAULT_EOF" {
                in_default = false;
                closed = true;
                continue;
            }
            body.push_str(line);
            body.push('\n');
            continue;
        }
        if let Some(v) = line.strip_prefix("TYPE=") {
            ty = v.to_string();
        } else if let Some(v) = line.strip_prefix("GROUP=") {
            group = v.to_string();
        } else if let Some(v) = line.strip_prefix("DOC=") {
            doc = v.to_string();
        } else if line == "DEFAULT<<'SPIRA_CONF_DEFAULT_EOF'" {
            in_default = true;
            closed = false;
        }
        // An unrecognised line is ignored, same as conf-gen.sh (which only warns to stderr).
    }
    if !closed {
        return Err(format!("{name}: DEFAULT heredoc never closed (missing SPIRA_CONF_DEFAULT_EOF)"));
    }
    Ok(RegistryKey { name: name.to_string(), ty, group, doc, default_body: body })
}

/// The keys that carry a real default statement, in the same deterministic topological order
/// `conf-gen.sh` writes them in (Kahn's algorithm, alphabetical tie-break) — a key's default
/// always comes after any other registry key its own default text names, regardless of this
/// directory's merely-alphabetical file order. Refuses (rather than guessing an order) on a
/// dependency cycle, same as `conf-gen.sh`.
pub fn topo_order(registry: &BTreeMap<String, RegistryKey>) -> Result<Vec<String>, String> {
    let defaulted: Vec<String> = registry
        .iter()
        .filter(|(_, k)| k.default_expr().is_some())
        .map(|(n, _)| n.clone())
        .collect();
    let deps: BTreeMap<String, Vec<String>> =
        defaulted.iter().map(|n| (n.clone(), registry[n].deps_among(registry))).collect();
    let mut indeg: BTreeMap<String, usize> =
        deps.iter().map(|(k, ds)| (k.clone(), ds.len())).collect();

    let mut remaining: Vec<String> = defaulted;
    let mut order: Vec<String> = Vec::new();
    while !remaining.is_empty() {
        let mut avail: Vec<String> =
            remaining.iter().filter(|k| indeg[*k] == 0).cloned().collect();
        if avail.is_empty() {
            let mut cyc = remaining.clone();
            cyc.sort();
            return Err(format!("conf.d: dependency cycle among registry defaults: {}", cyc.join(", ")));
        }
        avail.sort();
        for k in &avail {
            order.push(k.clone());
            for other in &remaining {
                if deps[other].contains(k) {
                    *indeg.get_mut(other).unwrap() -= 1;
                }
            }
        }
        remaining.retain(|k| !avail.contains(k));
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_embedded_registry_is_conf_d_itself() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/conf.d");
        assert_eq!(super::embedded().unwrap(), super::load(&dir).unwrap());
    }

    #[test]
    fn a_key_added_to_conf_d_is_known_to_the_embedded_registry() {
        let dir = testkit::TempDir::new("spira-config-registry-drift");
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/conf.d");
        for e in std::fs::read_dir(&src).unwrap().flatten() {
            std::fs::copy(e.path(), dir.join(e.file_name())).unwrap();
        }
        assert_eq!(super::embedded().unwrap(), super::load(dir.path()).unwrap());
        std::fs::write(dir.join("SPIRA_DRIFT_PROBE"), "TYPE=string\nGROUP=test\nDOC=x\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    # NO DEFAULT.\nSPIRA_CONF_DEFAULT_EOF\n").unwrap();
        assert_ne!(super::embedded().unwrap(), super::load(dir.path()).unwrap(), "the comparison must be able to see a key conf.d gained");
    }

    use super::*;
    use std::collections::BTreeMap;

    fn key(name: &str, body: &str) -> RegistryKey {
        RegistryKey {
            name: name.to_string(),
            ty: "string".into(),
            group: "test".into(),
            doc: String::new(),
            default_body: body.to_string(),
        }
    }

    /// sp-1cdgq round 3: a missing directory is now a NAMED error, not a silent empty
    /// registry — this is the test that used to assert the opposite. Two base reds
    /// (sp-8qm8g, sp-1cdgq) came from a test fixture's `--home` lacking `conf.d`, and
    /// `resolve()` quietly proceeding on the ~29 hand-written keys alone masked both until
    /// something that specifically needed a registry-resolved key went looking.
    #[test]
    fn missing_directory_is_a_named_error_not_an_empty_registry() {
        let dir = testkit::TempDir::new("spira-config-registry-missing");
        let missing = dir.join("does-not-exist");
        let err = load(&missing).unwrap_err();
        assert_eq!(err, format!("no config registry at {}", missing.display()));
    }

    /// An EXISTING but empty directory is still not an error — a release that genuinely
    /// ships zero registry files (or a fixture that only cares about the hand-written keys)
    /// keeps working. Only "cannot even read this path" is refused.
    #[test]
    fn existing_empty_directory_is_still_an_empty_registry() {
        let dir = testkit::TempDir::new("spira-config-registry-empty");
        let empty = dir.join("conf.d");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(load(&empty).unwrap(), BTreeMap::new());
    }

    #[test]
    fn parses_type_group_doc_and_default_body() {
        let dir = testkit::TempDir::new("spira-config-registry-parse");
        std::fs::write(
            dir.join("SPIRA_EXAMPLE"),
            "TYPE=u32\nGROUP=test\nDOC=an example\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${SPIRA_EXAMPLE:=7}\"\nSPIRA_CONF_DEFAULT_EOF\n",
        )
        .unwrap();
        let reg = load(dir.path()).unwrap();
        let k = &reg["SPIRA_EXAMPLE"];
        assert_eq!(k.ty, "u32");
        assert_eq!(k.group, "test");
        assert_eq!(k.doc, "an example");
        assert_eq!(k.default_expr(), Some("7"));
    }

    #[test]
    fn a_lowercase_or_dotfile_name_is_skipped() {
        let dir = testkit::TempDir::new("spira-config-registry-skip");
        std::fs::write(dir.join(".gitkeep"), "").unwrap();
        std::fs::write(dir.join("readme.md"), "").unwrap();
        let reg = load(dir.path()).unwrap();
        assert!(reg.is_empty());
    }

    #[test]
    fn unterminated_heredoc_is_refused() {
        let dir = testkit::TempDir::new("spira-config-registry-unterminated");
        std::fs::write(dir.join("SPIRA_BAD"), "DEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\nno close\n").unwrap();
        assert!(load(dir.path()).is_err());
    }

    #[test]
    fn procedural_and_no_default_stubs_have_no_expr() {
        let k1 = key("SPIRA_X", "    # PROCEDURAL — ...\n");
        let k2 = key("SPIRA_Y", "    # NO DEFAULT.\n");
        assert_eq!(k1.default_expr(), None);
        assert_eq!(k2.default_expr(), None);
    }

    #[test]
    fn default_expr_is_brace_balanced_for_a_nested_substitution() {
        let k = key(
            "SPIRA_OVERRIDES",
            "    : \"${SPIRA_OVERRIDES:=${XDG_CONFIG_HOME:-$HOME/.config}/spira/overrides}\"\n",
        );
        assert_eq!(
            k.default_expr(),
            Some("${XDG_CONFIG_HOME:-$HOME/.config}/spira/overrides")
        );
    }

    #[test]
    fn topo_order_runs_a_dependency_after_what_it_names() {
        let mut reg = BTreeMap::new();
        reg.insert("SPIRA_B".to_string(), key("SPIRA_B", "    : \"${SPIRA_B:=$SPIRA_A/x}\"\n"));
        reg.insert("SPIRA_A".to_string(), key("SPIRA_A", "    : \"${SPIRA_A:=1}\"\n"));
        let order = topo_order(&reg).unwrap();
        let pos_a = order.iter().position(|k| k == "SPIRA_A").unwrap();
        let pos_b = order.iter().position(|k| k == "SPIRA_B").unwrap();
        assert!(pos_a < pos_b, "{order:?}");
    }

    #[test]
    fn topo_order_refuses_a_cycle() {
        let mut reg = BTreeMap::new();
        reg.insert("SPIRA_A".to_string(), key("SPIRA_A", "    : \"${SPIRA_A:=$SPIRA_B}\"\n"));
        reg.insert("SPIRA_B".to_string(), key("SPIRA_B", "    : \"${SPIRA_B:=$SPIRA_A}\"\n"));
        assert!(topo_order(&reg).is_err());
    }

    #[test]
    fn stub_keys_are_excluded_from_topo_order() {
        let mut reg = BTreeMap::new();
        reg.insert("SPIRA_A".to_string(), key("SPIRA_A", "    : \"${SPIRA_A:=1}\"\n"));
        reg.insert("SPIRA_STUB".to_string(), key("SPIRA_STUB", "    # NO DEFAULT.\n"));
        let order = topo_order(&reg).unwrap();
        assert_eq!(order, vec!["SPIRA_A".to_string()]);
    }
}
