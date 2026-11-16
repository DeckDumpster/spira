//! `registry-readers` — the crates whose sources read the config registry or delta are exactly
//! `suite_select::readers::REGISTRY_READERS`, the crates a registry edit selects.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::lex::rust::classify;
use crate::{Entry, Finding, LintError, Rule, Tree};

#[derive(Default)]
pub struct RegistryReaders {
    checked: Mutex<Option<usize>>,
}

const NAME: &str = "registry-readers";
const ROOT: &str = "Cargo.toml";
const LIST_FILE: &str = "suite-select/src/readers.rs";
const HINT: &str = "a crate whose tests read the registry or delta must be in suite_select::readers::REGISTRY_READERS, \
or a registry edit does not select it; a listed crate that reads nothing is selected for nothing.";

const MANIFEST_DIR: &str = concat!("CARGO_MANIFEST", "_DIR");
const REGISTRY_PATHS: [&str; 2] = [concat!("spira/", "conf.d"), concat!("spira/", "config-delta")];
const DELTA_LOAD: &str = concat!("config_delta::", "load(");

/// The 1-based line of the file's first registry read, in its comment-stripped text.
fn first_read(src: &[u8]) -> Option<usize> {
    let code = String::from_utf8_lossy(&classify(src).without_comments()).into_owned();
    if !code.contains(MANIFEST_DIR) {
        return None;
    }
    code.lines()
        .position(|l| (l.contains(MANIFEST_DIR) && REGISTRY_PATHS.iter().any(|p| l.contains(p))) || l.contains(DELTA_LOAD))
        .map(|i| i + 1)
}

fn package_name(tree: &Tree, dir: &str) -> Result<String, LintError> {
    let path = format!("{dir}/Cargo.toml");
    let manifest: toml::Table = tree.read_text(&path).parse().map_err(|e| LintError::Refused(format!("{path}: {e}")))?;
    manifest
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(str::to_string)
        .ok_or_else(|| LintError::Refused(format!("{path} has no [package] name")))
}

impl Rule for RegistryReaders {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path == ROOT || e.path.ends_with(".rs")
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        *self.checked.lock().unwrap() = None;
        let scope = crate::scope(tree, self)?;
        let root: toml::Table = tree.read_text(ROOT).parse().map_err(|e| LintError::Refused(format!("{ROOT}: {e}")))?;
        let mut members: Vec<String> = root
            .get("workspace")
            .and_then(|w| w.get("members"))
            .and_then(|m| m.as_array())
            .ok_or_else(|| LintError::Refused(format!("{ROOT} has no [workspace] members — nothing to judge")))?
            .iter()
            .filter_map(|m| m.as_str().map(|s| s.trim_end_matches('/').to_string()))
            .collect();
        members.sort();
        members.dedup();
        if members.is_empty() {
            return Err(LintError::Refused(format!("{ROOT} lists no workspace member — nothing to judge")));
        }

        let rs: Vec<&Entry> = scope.into_iter().filter(|e| e.path.ends_with(".rs")).collect();
        if rs.is_empty() {
            return Err(LintError::Refused("no .rs file in scope — nothing to judge".into()));
        }

        let mut readers: BTreeMap<String, (String, usize)> = BTreeMap::new();
        for e in rs {
            let Some(dir) = members
                .iter()
                .filter(|m| e.path.strip_prefix(m.as_str()).is_some_and(|r| r.starts_with('/')))
                .max_by_key(|m| m.len())
            else {
                continue;
            };
            let Some(line) = tree.content(e).and_then(first_read) else { continue };
            let name = package_name(tree, dir)?;
            readers.entry(name).or_insert((e.path.clone(), line));
        }

        let finding = |path: &str, line: Option<usize>, message: String| Finding { rule: NAME, path: path.into(), line, message };
        let mut out = Vec::new();
        for (name, (path, line)) in &readers {
            if !suite_select::readers::REGISTRY_READERS.contains(&name.as_str()) {
                out.push(finding(path, Some(*line), format!("{name} reads the config registry or delta and is missing from REGISTRY_READERS ({LIST_FILE})")));
            }
        }
        for name in suite_select::readers::REGISTRY_READERS {
            if !readers.contains_key(*name) {
                out.push(finding(LIST_FILE, None, format!("REGISTRY_READERS names {name}, whose sources read neither the registry nor the delta — remove it")));
            }
        }
        *self.checked.lock().unwrap() = Some(members.len());
        Ok(out)
    }

    fn checked(&self) -> Option<(usize, String)> {
        self.checked.lock().unwrap().map(|n| (n, "crates".to_string()))
    }

    fn hint(&self) -> &'static str {
        HINT
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(RegistryReaders::default())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn reader_src() -> String {
        format!("fn f() {{ let _ = concat!(env!(\"{MANIFEST_DIR}\"), \"/../{}/X\"); }}\n", REGISTRY_PATHS[0])
    }

    fn workspace(t: &TempDir, crates: &[(&str, &str)]) -> Tree {
        let members: Vec<String> = crates.iter().map(|(d, _)| format!("\"{d}\"")).collect();
        t.write(ROOT, &format!("[workspace]\nmembers = [{}]\n", members.join(", ")));
        let mut files = vec![ROOT.to_string()];
        for (d, src) in crates {
            t.write(&format!("{d}/Cargo.toml"), &format!("[package]\nname = \"{d}-crate\"\n"));
            t.write(&format!("{d}/src/lib.rs"), src);
            files.push(format!("{d}/src/lib.rs"));
        }
        Tree::from_paths(t.path(), files, std::iter::empty::<&str>())
    }

    fn messages(tree: &Tree) -> Vec<String> {
        RegistryReaders::default().check(tree).unwrap().iter().map(|f| f.message.clone()).collect()
    }

    #[test]
    fn a_planted_reader_missing_from_the_list_is_reported() {
        let t = TempDir::new("rr-plant");
        let tree = workspace(&t, &[("planted", &reader_src())]);
        let m = messages(&tree);
        assert!(m.iter().any(|x| x.starts_with("planted-crate reads") && x.contains("missing from REGISTRY_READERS")), "{m:?}");
    }

    #[test]
    fn a_listed_crate_whose_sources_read_nothing_is_stale() {
        let t = TempDir::new("rr-stale");
        let tree = workspace(&t, &[("planted", "fn f() {}\n")]);
        let m = messages(&tree);
        assert!(m.iter().any(|x| x.starts_with("REGISTRY_READERS names watchdog")), "{m:?}");
        assert!(!m.iter().any(|x| x.contains("planted-crate")), "{m:?}");
    }

    #[test]
    fn a_read_in_a_comment_or_doc_line_is_not_a_read() {
        let t = TempDir::new("rr-comment");
        let src = format!("// {}/// {}fn f() {{}}\n", reader_src(), reader_src());
        let tree = workspace(&t, &[("planted", &src)]);
        assert!(!messages(&tree).iter().any(|x| x.contains("planted-crate")));
        let live = workspace(&t, &[("planted", &format!("/// doc\n{}", reader_src()))]);
        assert!(messages(&live).iter().any(|x| x.contains("planted-crate")));
    }

    #[test]
    fn a_file_outside_the_walk_is_never_read() {
        let t = TempDir::new("rr-target");
        let tree = workspace(&t, &[("planted", "fn f() {}\n")]);
        t.write("planted/target/debug/x.rs", &reader_src());
        assert!(!messages(&tree).iter().any(|x| x.contains("planted-crate")));
    }

    #[test]
    fn a_delta_load_counts_only_beside_the_manifest_dir() {
        let t = TempDir::new("rr-load");
        let load = format!("fn f() {{ let _ = config_delta::{}); }}\n", "load(x");
        let without = workspace(&t, &[("planted", &load)]);
        assert!(!messages(&without).iter().any(|x| x.contains("planted-crate")));
        let with = workspace(&t, &[("planted", &format!("const D: &str = env!(\"{MANIFEST_DIR}\");\n{load}"))]);
        assert!(messages(&with).iter().any(|x| x.contains("planted-crate")));
    }

    #[test]
    fn a_workspace_with_no_rust_file_is_refused() {
        let t = TempDir::new("rr-empty");
        t.write(ROOT, "[workspace]\nmembers = [\"a\"]\n");
        let tree = Tree::from_paths(t.path(), [ROOT], std::iter::empty::<&str>());
        assert!(matches!(RegistryReaders::default().check(&tree), Err(LintError::EmptyScope | LintError::Refused(_))));
    }
}
