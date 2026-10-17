//! `otel-binaries` — every workspace crate that builds a binary depends on `spira-otel`, the one
//! way a component sends OTLP to the metrics store. A crate that does not is listed in
//! `spira-lint/otel-binaries-allow.d/<crate dir>` (the file holds that bead) against the open sweep bead that instruments it; the list
//! only shrinks and a crate added to the workspace after it was written can never join it.

use std::collections::BTreeSet;

use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct OtelBinaries;

const NAME: &str = "otel-binaries";
const ALLOW_FILE: &str = "spira-lint/otel-binaries-allow.d";
const ROOT: &str = "Cargo.toml";
const OTEL: &str = "spira-otel";

const HINT: &str = "every component sends OTLP to the metrics store through spira-otel: add the spira-otel \
dependency and push at least the standard run metrics (invocations, duration, outcome), fire-and-forget \
with a short deadline. spira-lint/otel-binaries-allow.d holds one file per uninstrumented binary, only shrinks, \
and takes no new crate.";

fn bead_id(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("sp-") else { return false };
    let (head, tail) = rest.split_once('.').map_or((rest, None), |(h, t)| (h, Some(t)));
    !head.is_empty()
        && head.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && tail.map_or(true, |t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
}

fn has_dependency(manifest: &toml::Table, name: &str) -> bool {
    manifest.get("dependencies").and_then(|d| d.as_table()).is_some_and(|d| d.contains_key(name))
}

fn builds_binary(tree: &Tree, dir: &str, manifest: &toml::Table) -> bool {
    let pre = if dir.is_empty() { String::new() } else { format!("{dir}/") };
    manifest.contains_key("bin")
        || tree.entries.iter().any(|e| {
            e.path.strip_prefix(&pre).is_some_and(|r| {
                r == "src/main.rs" || (r.starts_with("src/bin/") && r.ends_with(".rs"))
            })
        })
}

impl Rule for OtelBinaries {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path == ROOT
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        crate::scope(tree, self)?;
        let root: toml::Table = tree
            .read_text(ROOT)
            .parse()
            .map_err(|e| LintError::Refused(format!("{ROOT}: {e}")))?;
        let members: BTreeSet<String> = root
            .get("workspace")
            .and_then(|w| w.get("members"))
            .and_then(|m| m.as_array())
            .ok_or_else(|| LintError::Refused(format!("{ROOT} has no [workspace] members — nothing to judge")))?
            .iter()
            .filter_map(|m| m.as_str().map(str::to_string))
            .collect();

        let mut uninstrumented = BTreeSet::new();
        for dir in &members {
            let manifest: toml::Table = tree
                .read_text(&format!("{dir}/Cargo.toml"))
                .parse()
                .map_err(|e| LintError::Refused(format!("{dir}/Cargo.toml: {e}")))?;
            if builds_binary(tree, dir, &manifest) && !has_dependency(&manifest, OTEL) {
                uninstrumented.insert(dir.clone());
            }
        }

        let allow = crate::allow_dir(tree, ALLOW_FILE);
        let prior = crate::allow_dir_at(tree, ALLOW_FILE);
        let listed: BTreeSet<&str> = allow.iter().map(|(c, _)| c.as_str()).collect();
        let finding = |path: &str, line: Option<usize>, message: String| Finding { rule: NAME, path: path.into(), line, message };
        let mut out = Vec::new();

        for dir in &uninstrumented {
            if !listed.contains(dir.as_str()) {
                out.push(finding(
                    &format!("{dir}/Cargo.toml"),
                    None,
                    format!("{dir} builds a binary and does not depend on {OTEL} — every component sends OTLP to the metrics store; add the dependency and push the standard run metrics (invocations, duration, outcome)"),
                ));
            }
        }
        for (dir, bead) in &allow {
            let entry = format!("{ALLOW_FILE}/{dir}");
            if !uninstrumented.contains(dir) {
                out.push(finding(
                    &entry,
                    None,
                    format!("lists {dir}, which is not an uninstrumented binary crate any more — remove the file (the list only shrinks)"),
                ));
            } else if !bead_id(bead) {
                out.push(finding(&entry, None, format!("{dir} names no sweep bead (the file holds the open bead that instruments it)")));
            } else if prior.as_ref().is_some_and(|p| !p.contains(dir)) {
                out.push(finding(
                    &entry,
                    None,
                    format!("new entry {dir} — the list takes no new crate; depend on {OTEL} and push at least the standard run metrics (invocations, duration, outcome)"),
                ));
            }
        }
        Ok(out)
    }

    fn checked(&self) -> Option<(usize, String)> {
        None
    }

    fn hint(&self) -> &'static str {
        HINT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const OTEL_DEP: &str = "[package]\nname = \"x\"\n[dependencies]\nspira-otel = { path = \"../spira-otel\" }\n";
    const PLAIN: &str = "[package]\nname = \"x\"\n";

    fn repo(allow: &str) -> TempDir {
        let t = repo_files(allow);
        t.git(&["add", "."]);
        t.git(&["commit", "-qm", "base"]);
        t.git(&["tag", "base"]);
        t
    }

    fn set_allow(t: &TempDir, allow: &str) {
        let _ = std::fs::remove_dir_all(t.path().join(ALLOW_FILE));
        for l in allow.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')) {
            let mut w = l.split_whitespace();
            let key = w.next().unwrap();
            t.write(&format!("{ALLOW_FILE}/{key}"), &format!("{}\n", w.next().unwrap_or("")));
        }
    }

    fn repo_files(allow: &str) -> TempDir {
        let t = TempDir::new("otel-binaries");
        t.git_init();
        t.write(ROOT, "[workspace]\nmembers = [\"lib\", \"good\", \"old\", \"fresh\"]\n");
        t.write("lib/Cargo.toml", PLAIN);
        t.write("lib/src/lib.rs", "");
        t.write("good/Cargo.toml", OTEL_DEP);
        t.write("good/src/main.rs", "fn main() {}\n");
        t.write("old/Cargo.toml", PLAIN);
        t.write("old/src/main.rs", "fn main() {}\n");
        t.write("fresh/Cargo.toml", OTEL_DEP);
        t.write("fresh/src/bin/a.rs", "fn main() {}\n");
        set_allow(&t, allow);
        t
    }

    fn run(t: &TempDir) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_git(t.path()).unwrap().with_base(Some("base".into()));
        OtelBinaries.check(&tree).map(|v| v.iter().map(|f| f.message.clone()).collect())
    }

    #[test]
    fn listed_uninstrumented_binary_and_instrumented_ones_are_clean() {
        let t = repo("# h\nold sp-abc.1\n");
        assert_eq!(run(&t), Ok(vec![]));
    }

    #[test]
    fn a_binary_without_spira_otel_that_is_not_listed_is_red() {
        let t = repo("");
        let got = run(&t).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].starts_with("old builds a binary and does not depend on spira-otel"), "{got:?}");
    }

    #[test]
    fn a_new_crate_cannot_join_the_list() {
        let t = repo("old sp-abc\n");
        t.write("fresh/Cargo.toml", PLAIN);
        set_allow(&t, "old sp-abc\nfresh sp-abc\n");
        let got = run(&t).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].starts_with("new entry fresh"), "{got:?}");
    }

    #[test]
    fn a_stale_entry_and_an_entry_with_no_bead_are_refused() {
        let t = repo("old\ngood sp-abc\n");
        let got = run(&t).unwrap();
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got.iter().any(|m| m.starts_with("old names no sweep bead")));
        assert!(got.iter().any(|m| m.starts_with("lists good")));
    }

    #[test]
    fn bead_ids() {
        assert!(bead_id("sp-vpfp3i") && bead_id("sp-vpfp3i.2"));
        assert!(!bead_id("") && !bead_id("sp-") && !bead_id("vpfp3i") && !bead_id("sp-a.b"));
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(OtelBinaries)]
}
