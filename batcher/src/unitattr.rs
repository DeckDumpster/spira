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
        self.dirs.get(package).is_some_and(|d| d.is_empty() || path.strip_prefix(d.as_str()).is_some_and(|r| r.starts_with('/')))
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

/// The failing-test lines of a unit phase's output: `<STATUS> <binary> <test>`, once each, in order.
pub fn failing_lines(out: &str) -> Vec<String> {
    const STATUS: [&str; 5] = ["FAIL", "TIMEOUT", "LEAK-FAIL", "EXEC-FAIL", "SIGSEGV"];
    let mut lines: Vec<String> = vec![];
    for l in out.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        let ok = w.len() == 3 && (STATUS.contains(&w[0]) || w[0].starts_with("SIG")) && w[0].chars().all(|c| c.is_ascii_uppercase() || c == '-');
        if ok && !lines.iter().any(|x| x == &w.join(" ")) {
            lines.push(w.join(" "));
        }
    }
    lines
}

/// Each member whose diff touches one of `packages` or a package one of them depends on, with
/// the first such package it reaches. Empty when the graph does not know a failing package.
pub fn reaching(graph: &PackageGraph, packages: &[String], diffs: &[MemberDiff]) -> Vec<(Id, String)> {
    let reach: Vec<(&str, BTreeSet<&str>)> = packages.iter().filter(|p| graph.dirs.contains_key(*p)).map(|p| (p.as_str(), graph.closure(p))).collect();
    diffs
        .iter()
        .filter_map(|d| {
            reach.iter().find_map(|(failing, closure)| {
                let hit = closure.iter().find(|p| d.paths.iter().any(|path| graph.owns(p, path)))?;
                Some((d.id.clone(), if hit == failing { format!("{failing}") } else { format!("{hit} (a dependency of {failing})") }))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> PackageGraph {
        let dirs = [("loom", "loom"), ("runtime", "spira-runtime"), ("gate", "gate")];
        PackageGraph {
            dirs: dirs.iter().map(|(p, d)| (p.to_string(), d.to_string())).collect(),
            deps: BTreeMap::from([("loom".to_string(), vec!["runtime".to_string()])]),
        }
    }

    fn diff(id: &str, path: &str) -> MemberDiff {
        MemberDiff { id: id.into(), paths: vec![path.into()], added: String::new() }
    }

    #[test]
    fn failing_lines_are_read_once_each_and_a_log_tail_is_not_mistaken_for_them() {
        let out = "FAIL loom factory::tests::a\nFAIL loom factory::tests::a\nTIMEOUT loom::bin/x t::b\ntest result: FAILED. 1 passed\nFAIL [ 0.1s] ( 1/2) loom t::c\n";
        assert_eq!(failing_lines(out), ["FAIL loom factory::tests::a", "TIMEOUT loom::bin/x t::b"]);
        assert_eq!(failing_packages(&failing_lines(out)), ["loom"]);
    }

    #[test]
    fn a_member_reaches_a_failing_package_through_its_own_dir_or_a_dependency_and_no_other() {
        let diffs = [diff("a", "gate/src/engine.rs"), diff("b", "spira-runtime/src/x.rs"), diff("c", "loom/src/y.rs"), diff("d", "spira-runtime-extra/z.rs")];
        let got = reaching(&graph(), &["loom".to_string()], &diffs);
        assert_eq!(got.iter().map(|(i, _)| i.as_str()).collect::<Vec<_>>(), ["b", "c"]);
        assert!(got[0].1.contains("dependency of loom"), "{got:?}");
    }

    #[test]
    fn a_package_the_graph_does_not_know_reaches_no_member() {
        assert!(reaching(&graph(), &["ghost".to_string()], &[diff("a", "gate/src/x.rs")]).is_empty());
    }
}
