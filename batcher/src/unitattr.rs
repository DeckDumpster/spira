//! Package attribution of a red unit phase: which members' diffs reach the packages whose tests failed.

use std::collections::{BTreeMap, BTreeSet};

use crate::buildattr::MemberDiff;
use crate::core::Id;

/// The workspace's packages: each one's directory (repo-relative, no trailing slash; empty for
/// the root) and the workspace packages it depends on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackageGraph {
    pub dirs: BTreeMap<String, String>,
    pub deps: BTreeMap<String, Vec<String>>,
}

impl PackageGraph {
    /// `package` and everything it depends on, transitively.
    fn closure(&self, package: &str) -> BTreeSet<&str> {
        let mut seen = BTreeSet::new();
        let mut todo = vec![package];
        while let Some(p) = todo.pop() {
            if let Some((name, _)) = self.dirs.get_key_value(p) {
                if seen.insert(name.as_str()) {
                    todo.extend(self.deps.get(p).into_iter().flatten().map(String::as_str));
                }
            }
        }
        seen
    }

    fn owns(&self, package: &str, path: &str) -> bool {
        let under = |d: &str, path: &str| path.strip_prefix(d).is_some_and(|r| r.starts_with('/'));
        match self.dirs.get(package) {
            Some(d) if d.is_empty() => !self.dirs.values().any(|o| !o.is_empty() && under(o, path)),
            Some(d) => under(d, path),
            None => false,
        }
    }
}

impl PackageGraph {
    /// Each `dir:<directory>` word (what a plain `cargo test` log can name) as the package that
    /// lives there; a word no package owns stays as it is, and so maps to no member.
    pub fn resolve(&self, packages: &[String]) -> Vec<String> {
        packages
            .iter()
            .map(|p| match p.strip_prefix("dir:") {
                Some(dir) => self
                    .dirs
                    .iter()
                    .find(|(_, d)| d.as_str() == if dir == "." { "" } else { dir })
                    .map_or(p.clone(), |(name, _)| name.clone()),
                None => p.clone(),
            })
            .collect()
    }
}

/// The package of each failing-test line (`FAIL <binary> <test>`, the binary id's package being
/// the part before `::`), once each.
pub fn failing_packages(lines: &[String]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for l in lines {
        let mut w = l.split_whitespace();
        if let (Some(_status), Some(bin), Some(_test)) = (w.next(), w.next(), w.next()) {
            let p = bin.split("::").next().unwrap_or(bin).to_string();
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// What a rerun of a red test phase judges: `package=test` for each failing test, the bare
/// `package` for a failure whose test name cannot be handed to a test filter. A run of exactly
/// the failed tests is never narrower than the red.
pub fn rerun_scope(failed: &[String], graph: Option<&PackageGraph>) -> Vec<String> {
    let name_ok = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b':');
    let mut out: Vec<String> = vec![];
    for l in failed {
        let mut w = l.split_whitespace();
        let (Some(_), Some(bin), Some(test)) = (w.next(), w.next(), w.next()) else {
            continue;
        };
        let package = bin.split("::").next().unwrap_or(bin).to_string();
        let package = match graph {
            Some(g) => g.resolve(&[package]).remove(0),
            None => package,
        };
        let token = if name_ok(test) { format!("{package}={test}") } else { package.clone() };
        if !out.contains(&token) {
            out.push(token);
        }
    }
    let whole: Vec<String> = out.iter().filter(|t| !t.contains('=')).cloned().collect();
    out.retain(|t| t.split_once('=').is_none_or(|(p, _)| !whole.iter().any(|w| w == p)));
    out
}

/// The failing-test lines of a unit phase's output: `<STATUS> <binary> <test>`, once each, in order.
pub fn failing_lines(out: &str) -> Vec<String> {
    const STATUS: [&str; 5] = ["FAIL", "TIMEOUT", "LEAK-FAIL", "EXEC-FAIL", "SIGSEGV"];
    let mut lines: Vec<String> = vec![];
    for l in out.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        let ok = w.len() == 3
            && (STATUS.contains(&w[0]) || w[0].starts_with("SIG"))
            && w[0].chars().all(|c| c.is_ascii_uppercase() || c == '-');
        if ok && !lines.iter().any(|x| x == &w.join(" ")) {
            lines.push(w.join(" "));
        }
    }
    lines
}

/// Each member whose diff touches one of `packages` or a package one of them depends on, with
/// the first such package it reaches. Empty when the graph does not know every failing package:
/// a partial map would stop looking while a culprit sits in the part it cannot see.
pub fn reaching(
    graph: &PackageGraph,
    packages: &[String],
    diffs: &[MemberDiff],
) -> Vec<(Id, String)> {
    if packages.iter().any(|p| !graph.dirs.contains_key(p)) {
        return vec![];
    }
    let reach: Vec<(&str, BTreeSet<&str>)> = packages
        .iter()
        .filter(|p| graph.dirs.contains_key(*p))
        .map(|p| (p.as_str(), graph.closure(p)))
        .collect();
    diffs
        .iter()
        .filter_map(|d| {
            reach.iter().find_map(|(failing, closure)| {
                let hit = closure
                    .iter()
                    .find(|p| d.paths.iter().any(|path| graph.owns(p, path)))?;
                Some((
                    d.id.clone(),
                    if hit == failing {
                        failing.to_string()
                    } else {
                        format!("{hit} (a dependency of {failing})")
                    },
                ))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> PackageGraph {
        let dirs = [
            ("loom", "loom"),
            ("runtime", "spira-runtime"),
            ("gate", "gate"),
        ];
        PackageGraph {
            dirs: dirs
                .iter()
                .map(|(p, d)| (p.to_string(), d.to_string()))
                .collect(),
            deps: BTreeMap::from([("loom".to_string(), vec!["runtime".to_string()])]),
        }
    }

    fn diff(id: &str, path: &str) -> MemberDiff {
        MemberDiff {
            id: id.into(),
            paths: vec![path.into()],
            added: String::new(),
        }
    }

    #[test]
    fn a_red_naming_one_integration_test_reruns_that_test_alone() {
        let g = PackageGraph {
            dirs: [("testenv".to_string(), "testenv".to_string())].into(),
            deps: BTreeMap::new(),
        };
        let failed = vec!["FAIL dir:testenv lint_does_not_refuse_a_key".to_string()];
        assert_eq!(rerun_scope(&failed, Some(&g)), vec!["testenv=lint_does_not_refuse_a_key"]);
    }

    #[test]
    fn a_failure_whose_name_a_test_filter_cannot_take_reruns_its_whole_package() {
        let failed = vec!["FAIL runtime::bin odd$name".to_string(), "FAIL runtime::bin ok::t".to_string()];
        assert_eq!(rerun_scope(&failed, None), vec!["runtime"]);
    }

    #[test]
    fn failing_lines_are_read_once_each_and_a_log_tail_is_not_mistaken_for_them() {
        let out = "FAIL loom factory::tests::a\nFAIL loom factory::tests::a\nTIMEOUT loom::bin/x t::b\ntest result: FAILED. 1 passed\nFAIL [ 0.1s] ( 1/2) loom t::c\n";
        assert_eq!(
            failing_lines(out),
            ["FAIL loom factory::tests::a", "TIMEOUT loom::bin/x t::b"]
        );
        assert_eq!(failing_packages(&failing_lines(out)), ["loom"]);
    }

    #[test]
    fn a_member_reaches_a_failing_package_through_its_own_dir_or_a_dependency_and_no_other() {
        let diffs = [
            diff("a", "gate/src/engine.rs"),
            diff("b", "spira-runtime/src/x.rs"),
            diff("c", "loom/src/y.rs"),
            diff("d", "spira-runtime-extra/z.rs"),
        ];
        let got = reaching(&graph(), &["loom".to_string()], &diffs);
        assert_eq!(
            got.iter().map(|(i, _)| i.as_str()).collect::<Vec<_>>(),
            ["b", "c"]
        );
        assert!(got[0].1.contains("dependency of loom"), "{got:?}");
    }

    #[test]
    fn a_dir_word_resolves_to_the_package_in_that_directory_and_an_unowned_one_stays() {
        assert_eq!(
            graph().resolve(&[
                "dir:spira-runtime".into(),
                "gate".into(),
                "dir:nowhere".into()
            ]),
            ["runtime", "gate", "dir:nowhere"]
        );
        let root = PackageGraph {
            dirs: BTreeMap::from([("root".to_string(), String::new())]),
            deps: BTreeMap::new(),
        };
        assert_eq!(root.resolve(&["dir:.".into()]), ["root"]);
    }

    #[test]
    fn a_package_the_graph_does_not_know_reaches_no_member_even_beside_one_it_does() {
        let d = [diff("a", "gate/src/x.rs"), diff("b", "loom/src/y.rs")];
        assert!(reaching(&graph(), &["ghost".to_string()], &d).is_empty());
        assert!(reaching(&graph(), &["loom".to_string(), "ghost".to_string()], &d).is_empty());
    }

    #[test]
    fn a_root_package_owns_only_the_paths_no_other_package_directory_holds() {
        let g = PackageGraph {
            dirs: BTreeMap::from([
                ("root".to_string(), String::new()),
                ("gate".to_string(), "gate".to_string()),
            ]),
            deps: BTreeMap::new(),
        };
        let d = [diff("a", "gate/src/x.rs"), diff("b", "Cargo.toml")];
        assert_eq!(
            reaching(&g, &["root".to_string()], &d)
                .iter()
                .map(|(i, _)| i.to_string())
                .collect::<Vec<_>>(),
            ["b"]
        );
    }
}
