//! A suite's INPUT CLOSURE (over-broad-scopes #1, sp-ipwlic; the full-suite statute as
//! amended 2026-11-04: a landing needs every suite to have passed on an input closure identical
//! to the landing tree's, keyed per suite, never by the whole tree).
//!
//! A suite's inputs are what the test plan says it reads: its own file; every tracked file its
//! `# covers:` names (a `file#function` token claims the whole file — the hash is of bytes, not
//! of functions); the harness files every suite is declared to read ([`HARNESS`] and the
//! always-covers set); the scripts it runs, followed through the scripts they run; and, for
//! every workspace binary it invokes, the sources of every crate that binary is built from plus
//! the lockfile, the workspace manifest and the toolchain pin. The key is a digest of those
//! paths and their blob ids, so two trees that differ only outside the closure share it.
//!
//! A suite whose closure cannot be computed — no `# covers:` (it claims to read anything), or a
//! tree listing that failed — has no key, and is always run, never skipped.

use crate::corpus::{Corpus, Suite};
use crate::glob::case_match;
use crate::reach::Reach;
use std::collections::{BTreeMap, BTreeSet};

/// Files every suite reads whatever it declares.
pub const HARNESS: &[&str] = &["spira/testlib.sh"];

/// Workspace-wide build inputs: any binary a suite runs is built from them.
pub const BUILD_ROOTS: &[&str] = &["Cargo.lock", "Cargo.toml", "rust-toolchain.toml"];

/// The scripts in `files` (`spira/<name>.sh`, not suites) by basename.
fn scripts(files: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    files
        .keys()
        .filter_map(|p| {
            let name = p.strip_prefix("spira/")?;
            (!name.contains('/') && name.ends_with(".sh") && !crate::corpus::is_suite_file(name)).then(|| (name.to_string(), p.clone()))
        })
        .collect()
}

/// The repo-relative paths in `suite`'s closure, or `None` when it cannot be computed.
pub fn closure_paths(
    corpus: &Corpus,
    suite: &Suite,
    reach: Option<&Reach>,
    files: &BTreeMap<String, String>,
    always: &[String],
) -> Option<BTreeSet<String>> {
    let covers = suite.covers.as_ref()?;
    let mut pats: Vec<String> = vec![format!("spira/{}", suite.name)];
    pats.extend(covers.iter().map(|t| t.split('#').next().unwrap_or(t).to_string()).filter(|p| !p.is_empty()));
    pats.extend(HARNESS.iter().map(|s| s.to_string()));
    pats.extend(always.iter().cloned());
    let mut out: BTreeSet<String> = files.keys().filter(|f| pats.iter().any(|p| case_match(p, f))).cloned().collect();
    // The scripts it runs, and the scripts those run.
    let by_name = scripts(files);
    let mut todo: Vec<String> = suite.tokens.iter().filter(|t| by_name.contains_key(*t)).cloned().collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    while let Some(n) = todo.pop() {
        if !seen.insert(n.clone()) {
            continue;
        }
        let path = &by_name[&n];
        out.insert(path.clone());
        for t in corpus.shell.tokens_of_script(&n) {
            if by_name.contains_key(t) && !seen.contains(t) {
                todo.push(t.clone());
            }
        }
    }
    // The workspace binaries it invokes, by the crates each is built from.
    if let Some(r) = reach {
        let dirs: BTreeSet<&str> = suite.invokes.iter().flat_map(|b| r.sources_of(b)).map(String::as_str).collect();
        if !dirs.is_empty() {
            out.extend(files.keys().filter(|f| BUILD_ROOTS.contains(&f.as_str())).cloned());
            out.extend(files.keys().filter(|f| dirs.iter().any(|d| f.strip_prefix(d).is_some_and(|r| r.starts_with('/')))).cloned());
        }
    }
    Some(out)
}

/// Every suite's input key: its closure's paths and blob ids as one canonical text (the caller
/// hashes it), or `None` (always run). `files` is the tree's `path → blob id`; `None` when the
/// tree could not be listed.
pub fn suite_inputs(
    corpus: &Corpus,
    reach: Option<&Reach>,
    files: Option<&BTreeMap<String, String>>,
    always: &[String],
) -> BTreeMap<String, Option<String>> {
    corpus
        .suites
        .iter()
        .map(|s| {
            let key = files.and_then(|files| {
                let paths = closure_paths(corpus, s, reach, files, always)?;
                let mut text = String::from("suite-inputs-v1\n");
                for p in &paths {
                    text.push_str(&format!("{p} {}\n", files[p]));
                }
                Some(text)
            });
            (s.name.clone(), key)
        })
        .collect()
}

/// `git ls-tree -r <tree>` output (`<mode> <type> <blob>\t<path>`) as `path → blob`.
pub fn parse_ls_tree(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let (meta, path) = l.split_once('\t')?;
            let blob = meta.split_whitespace().nth(2)?;
            Some((path.to_string(), blob.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries.iter().map(|(p, b)| (p.to_string(), b.to_string())).collect()
    }

    /// Over-broad-scopes #1 (sp-ipwlic): two trees differing only outside a suite's closure
    /// share its key; differing inside it — a covered file, its own text, a script it runs, a
    /// crate its binary is built from, the harness — they do not; and a suite with no declared
    /// inputs has no key, so it always runs.
    #[test]
    fn two_trees_share_a_suites_key_only_when_its_closure_is_byte_identical() {
        let ws = testkit::TempDir::new("suite-select-closure");
        let p = ws.path();
        std::fs::write(p.join("Cargo.toml"), "[workspace]\nmembers = [\"lib\", \"bead\", \"other\"]\n").unwrap();
        for (d, toml, main) in [
            ("lib", "[package]\nname = \"lib\"\n", false),
            ("bead", "[package]\nname = \"bead\"\n[dependencies]\nlib = { path = \"../lib\" }\n", true),
            ("other", "[package]\nname = \"other\"\n", true),
        ] {
            std::fs::create_dir_all(p.join(d).join("src")).unwrap();
            std::fs::write(p.join(d).join("Cargo.toml"), toml).unwrap();
            if main {
                std::fs::write(p.join(d).join("src/main.rs"), "").unwrap();
            }
        }
        let reach = Reach::load(p).unwrap().unwrap();
        let mut corpus = Corpus::new(vec![
            Suite::parse("test-a.sh", "# covers: spira/a.sh\n\"$HERE/run.sh\"\nbead show x\n").unwrap(),
            Suite::parse("test-free.sh", "echo no covers\n").unwrap(),
        ]);
        corpus.shell.add("run.sh", "\"$HERE/inner.sh\"\n");
        corpus.shell.add("inner.sh", ":\n");
        let base = [
            ("spira/test-a.sh", "1"), ("spira/a.sh", "2"), ("spira/run.sh", "3"), ("spira/inner.sh", "4"),
            ("spira/testlib.sh", "5"), ("spira/lib.sh", "6"), ("lib/src/lib.rs", "7"), ("bead/src/main.rs", "8"),
            ("other/src/main.rs", "9"), ("Cargo.lock", "10"), ("docs/x.md", "11"), ("spira/unrelated.sh", "12"),
        ];
        let always = vec!["spira/lib.sh".to_string()];
        let key = |changed: Option<(&str, &str)>| {
            let mut t = tree(&base);
            if let Some((p, b)) = changed {
                t.insert(p.into(), b.into());
            }
            suite_inputs(&corpus, Some(&reach), Some(&t), &always)
        };
        let k0 = key(None);
        assert!(k0["test-a.sh"].is_some());
        for outside in [("docs/x.md", "x"), ("other/src/main.rs", "x"), ("spira/unrelated.sh", "x"), ("spira/new.sh", "x")] {
            assert_eq!(key(Some(outside))["test-a.sh"], k0["test-a.sh"], "{outside:?} is outside the closure");
        }
        for inside in [
            ("spira/a.sh", "x"), ("spira/test-a.sh", "x"), ("spira/run.sh", "x"), ("spira/inner.sh", "x"),
            ("lib/src/lib.rs", "x"), ("bead/src/main.rs", "x"), ("Cargo.lock", "x"), ("spira/testlib.sh", "x"),
            ("spira/lib.sh", "x"),
        ] {
            assert_ne!(key(Some(inside))["test-a.sh"], k0["test-a.sh"], "{inside:?} is inside the closure");
        }
        assert_eq!(k0["test-free.sh"], None, "no declared inputs: no key, always run");
        assert_eq!(suite_inputs(&corpus, Some(&reach), None, &always)["test-a.sh"], None, "an unlisted tree keys nothing");
    }

    #[test]
    fn ls_tree_lines_parse_to_path_and_blob() {
        let t = parse_ls_tree("100644 blob abc\tspira/a b.sh\n040000 tree def\tx\n");
        assert_eq!(t["spira/a b.sh"], "abc");
    }
}
