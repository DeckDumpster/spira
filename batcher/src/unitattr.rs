//! Package attribution of a red unit phase: which members' diffs reach the packages whose tests failed.

use std::collections::{BTreeMap, BTreeSet};

use crate::buildattr::MemberDiff;
use crate::core::Id;
use suite_select::readers::readers_of;

/// The workspace's packages: each one's directory (repo-relative, no trailing slash; empty for
/// the root) and the workspace packages it depends on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackageGraph {
    pub dirs: BTreeMap<String, String>,
    pub deps: BTreeMap<String, Vec<String>>,
    /// Test target name -> the packages with a test target of that name.
    pub tests: BTreeMap<String, Vec<String>>,
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
    /// Each `dir:<directory>` word as the package that lives there, and each `test:<target>` word
    /// as the sole package with a test target of that name (what a plain `cargo test` log can
    /// name); a word no package, or more than one, owns stays as it is (`owners` names them all).
    pub fn resolve(&self, packages: &[String]) -> Vec<String> {
        packages
            .iter()
            .map(|p| match (p.strip_prefix("dir:"), p.strip_prefix("test:")) {
                (Some(dir), _) => self
                    .dirs
                    .iter()
                    .find(|(_, d)| d.as_str() == if dir == "." { "" } else { dir })
                    .map_or(p.clone(), |(name, _)| name.clone()),
                (_, Some(target)) => match self.tests.get(target).map(Vec::as_slice) {
                    Some([only]) => only.clone(),
                    _ => p.clone(),
                },
                _ => p.clone(),
            })
            .collect()
    }

    /// Like `resolve` for one word, but a `test:<target>` word shared by several packages names
    /// every one of them.
    pub fn owners(&self, word: &str) -> Vec<String> {
        match word.strip_prefix("test:").and_then(|t| self.tests.get(t)) {
            Some(all) if all.len() > 1 => all.clone(),
            _ => self.resolve(&[word.to_string()]),
        }
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

fn token_of(package: &str, test: &str) -> String {
    format!("{package}={test}")
}

/// Whether a scope token (`package` or `package=test`) is made only of what a package or test
/// filter can carry, so the VM script never takes a name it would have to quote.
pub fn scope_token_ok(token: &str) -> bool {
    let word = |w: &str, extra: &[u8]| !w.is_empty() && w.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || extra.contains(&b));
    match token.split_once('=') {
        Some((p, test)) => word(p, b"") && word(test, b":"),
        None => word(token, b""),
    }
}

/// Whether a scope names exactly the failing tests: non-empty, every token `package=test`.
pub fn scope_is_exact(scope: &[String]) -> bool {
    !scope.is_empty() && scope.iter().all(|t| t.contains('=') && scope_token_ok(t))
}

/// What a rerun of a red test phase judges: `package=test` for each failing test, the bare
/// `package` for a failure whose test name cannot be handed to a test filter. A run of exactly
/// the failed tests is never narrower than the red.
pub fn rerun_scope(failed: &[String], graph: Option<&PackageGraph>) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for l in failed {
        let mut w = l.split_whitespace();
        let (Some(_), Some(bin), Some(test)) = (w.next(), w.next(), w.next()) else {
            continue;
        };
        let package = bin.split("::").next().unwrap_or(bin).to_string();
        let packages = match graph {
            Some(g) => g.owners(&package),
            None => vec![package],
        };
        for package in packages {
            let token = token_of(&package, test);
            let token = if scope_token_ok(&token) { token } else { package.clone() };
            if !out.contains(&token) {
                out.push(token);
            }
        }
    }
    let whole: Vec<String> = out.iter().filter(|t| !t.contains('=')).cloned().collect();
    out.retain(|t| t.split_once('=').is_none_or(|(p, _)| !whole.iter().any(|w| w == p)));
    out
}

/// The failing-test lines of a unit phase's output: `<STATUS> <binary> <test>`, once each, in order.
pub fn failing_lines(out: &str) -> Vec<String> {
    let mut lines: Vec<String> = vec![];
    for l in out.lines() {
        if let Some(w) = failing_words(l) {
            let line = w.join(" ");
            if !lines.contains(&line) {
                lines.push(line);
            }
        }
    }
    lines
}

/// What a red phase's output says about its failures: without the run's progress lines (`Running`
/// and `Doc-tests` headers, passing and ignored tests) and, once a failing test is listed, the
/// log tail after `--- last output of the log ---`, which shows binaries that passed.
pub fn failure_evidence(out: &str) -> String {
    const MARKER: &str = "--- last output of the log ---";
    let cut_tail = !failing_lines(out.split(MARKER).next().unwrap_or("")).is_empty();
    let mut kept: Vec<&str> = vec![];
    for l in out.lines() {
        if cut_tail && l.trim() == MARKER {
            break;
        }
        let t = l.trim_start();
        let progress = t.starts_with("Running ")
            || t.starts_with("Doc-tests ")
            || (t.starts_with("test ") && (t.ends_with(" ... ok") || t.ends_with(" ... ignored")));
        if !progress {
            kept.push(l);
        }
    }
    kept.join("\n")
}

/// The `[status, binary, test]` words of a failing-test line, `None` for any other line.
pub fn failing_words(line: &str) -> Option<[&str; 3]> {
    const STATUS: [&str; 5] = ["FAIL", "TIMEOUT", "LEAK-FAIL", "EXEC-FAIL", "SIGSEGV"];
    let mut w = line.split_whitespace();
    let (Some(s), Some(b), Some(t), None) = (w.next(), w.next(), w.next(), w.next()) else {
        return None;
    };
    let ok = (STATUS.contains(&s) || s.starts_with("SIG"))
        && s.chars().all(|c| c.is_ascii_uppercase() || c == '-');
    ok.then_some([s, b, t])
}

/// Each member whose diff touches one of `packages`, a package one of them depends on, or a file
/// outside it that the failing package's own tests read, with the first such thing it reaches. Empty when the graph does not know every failing package:
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
                let via = match closure.iter().find(|p| d.paths.iter().any(|path| graph.owns(p, path))) {
                    Some(hit) if hit == failing => failing.to_string(),
                    Some(hit) => format!("{hit} (a dependency of {failing})"),
                    None => {
                        let path = d.paths.iter().find(|path| readers_of(path).contains(failing))?;
                        format!("{path} (read by {failing}'s tests)")
                    }
                };
                Some((d.id.clone(), via))
            })
        })
        .collect()
}

fn debug_string(s: &str) -> String {
    let s = s.trim();
    let Some(inner) = s.strip_prefix('"').and_then(|r| r.strip_suffix('"')) else {
        return s.to_string();
    };
    let mut out = String::new();
    let mut it = inner.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(o) => out.push(o),
            None => out.push('\\'),
        }
    }
    out
}

/// The trimmed, non-empty lines that one side of each `assertion `left == right` failed` block
/// holds and the other lacks, once each.
pub fn assertion_delta(out: &str) -> Vec<String> {
    let mut delta: Vec<String> = vec![];
    let mut lines = out.lines();
    while let Some(l) = lines.next() {
        if !l.contains("assertion `left == right` failed") {
            continue;
        }
        let (mut left, mut right) = (None, None);
        for l in lines.by_ref().take(8) {
            let l = l.trim();
            if let Some(v) = l.strip_prefix("left:") {
                left = Some(debug_string(v));
            } else if let Some(v) = l.strip_prefix("right:") {
                right = Some(debug_string(v));
            }
            if left.is_some() && right.is_some() {
                break;
            }
        }
        let (Some(left), Some(right)) = (left, right) else { continue };
        let side = |t: &str| -> BTreeSet<String> {
            t.lines().map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect()
        };
        let (a, b) = (side(&left), side(&right));
        for line in left.lines().chain(right.lines()).map(str::trim) {
            if !line.is_empty() && a.contains(line) != b.contains(line) && !delta.iter().any(|d| d == line) {
                delta.push(line.to_string());
            }
        }
    }
    delta
}

/// The one member of `reach` whose diff adds a line whole-line equal to a `delta` line, with
/// that line; `None` when no member or several do.
pub fn assertion_owner(
    delta: &[String],
    diffs: &[MemberDiff],
    reach: &BTreeMap<Id, String>,
) -> Option<(Id, String)> {
    let mut hits: Vec<(Id, String)> = vec![];
    for d in diffs.iter().filter(|d| reach.contains_key(&d.id)) {
        let found = d.added.lines().filter_map(|l| l.strip_prefix('+')).map(str::trim).find(|a| delta.iter().any(|x| x == a));
        if let Some(line) = found {
            hits.push((d.id.clone(), line.to_string()));
        }
    }
    match hits.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[test_id(ta5s1v0)]
    #[test]
    fn a_scope_is_exact_only_when_every_token_is_a_package_and_a_test() {
        let v = |t: &[&str]| t.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(super::scope_is_exact(&v(&["a=x::y"])));
        assert!(!super::scope_is_exact(&v(&["a"])));
        assert!(!super::scope_is_exact(&v(&[])));
        assert!(!super::scope_is_exact(&v(&["test:it"])));
        assert!(!super::scope_is_exact(&v(&["a=x::y", "b"])));
    }

    #[test_id(t5lh2ze)]
    #[test]
    fn assertion_delta_is_the_line_one_side_of_the_failed_assertion_holds_alone() {
        let out = "thread 'x' panicked at gate/src/tests.rs:2701:5:\nassertion `left == right` failed\n  left: \"[profile.default]\\nslow-timeout = { period = \\\"60s\\\", terminate-after = 2 }\\nstatus-level = \\\"skip\\\"\\n\"\n right: \"[profile.default]\\nslow-timeout = { period = \\\"60s\\\", terminate-after = 2 }\\n\"\n";
        assert_eq!(assertion_delta(out), ["status-level = \"skip\""]);
    }

    #[test_id(totvqof)]
    #[test]
    fn assertion_delta_is_empty_without_an_assertion_block_or_a_difference_in_lines() {
        assert!(assertion_delta("FAIL x y\nplain output\n").is_empty());
        let same = "assertion `left == right` failed\n  left: \"a\\nb\"\n right: \"b\\na\"\n";
        assert!(assertion_delta(same).is_empty());
    }

    #[test_id(tca1klv)]
    #[test]
    fn assertion_owner_wants_exactly_one_reaching_member_with_the_whole_line() {
        let add = |id: &str, a: &str| MemberDiff { id: id.into(), paths: vec![], added: a.into() };
        let reach: BTreeMap<Id, String> = [("a".into(), "x".to_string()), ("b".into(), "x".to_string())].into();
        let delta = vec!["k = 1".to_string()];
        let one = [add("a", "+other\n"), add("b", "+  k = 1\n")];
        assert_eq!(assertion_owner(&delta, &one, &reach), Some(("b".into(), "k = 1".into())));
        let sub = [add("a", "+k = 11\n"), add("b", "+k = 1\n")];
        assert_eq!(assertion_owner(&delta, &sub, &reach).map(|o| o.0), Some("b".into()));
        let two = [add("a", "+k = 1\n"), add("b", "+k = 1\n")];
        assert_eq!(assertion_owner(&delta, &two, &reach), None);
        let unreached = [add("c", "+k = 1\n")];
        assert_eq!(assertion_owner(&delta, &unreached, &reach), None);
    }

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
            ..Default::default()
        }
    }

    fn diff(id: &str, path: &str) -> MemberDiff {
        MemberDiff {
            id: id.into(),
            paths: vec![path.into()],
            added: String::new(),
        }
    }

    #[test_id(tvq8fvv)]
    #[test]
    fn a_red_naming_one_integration_test_reruns_that_test_alone() {
        let g = PackageGraph {
            dirs: [("testenv".to_string(), "testenv".to_string())].into(),
            deps: BTreeMap::new(),
            ..Default::default()
        };
        let failed = vec!["FAIL dir:testenv lint_does_not_refuse_a_key".to_string()];
        assert_eq!(rerun_scope(&failed, Some(&g)), vec!["testenv=lint_does_not_refuse_a_key"]);
    }

    #[test_id(tzsqs4h)]
    #[test]
    fn a_failure_whose_name_a_test_filter_cannot_take_reruns_its_whole_package() {
        let failed = vec!["FAIL runtime::bin odd$name".to_string(), "FAIL runtime::bin ok::t".to_string()];
        assert_eq!(rerun_scope(&failed, None), vec!["runtime"]);
    }

    #[test_id(t3eea3l)]
    #[test]
    fn failing_lines_are_read_once_each_and_a_log_tail_is_not_mistaken_for_them() {
        let out = "FAIL loom factory::tests::a\nFAIL loom factory::tests::a\nTIMEOUT loom::bin/x t::b\ntest result: FAILED. 1 passed\nFAIL [ 0.1s] ( 1/2) loom t::c\n";
        assert_eq!(
            failing_lines(out),
            ["FAIL loom factory::tests::a", "TIMEOUT loom::bin/x t::b"]
        );
        assert_eq!(failing_packages(&failing_lines(out)), ["loom"]);
    }

    #[test_id(taxozov)]
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

    #[test_id(txb39co)]
    #[test]
    fn a_member_reaches_the_package_whose_tests_read_a_file_it_touches_and_no_other() {
        let g = PackageGraph {
            dirs: [("release", "release"), ("spira-world", "spira-world")].iter().map(|(p, d)| (p.to_string(), d.to_string())).collect(),
            deps: BTreeMap::new(),
            ..Default::default()
        };
        let d = [diff("a", "spira/config-delta.d/x.toml"), diff("b", "docs/x.md")];
        let got = reaching(&g, &["release".to_string()], &d);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].0.as_str(), "a");
        assert!(got[0].1.contains("spira/config-delta.d/x.toml (read by release's tests)"), "{got:?}");
        assert!(reaching(&g, &["spira-world".to_string()], &d).is_empty());
    }

    #[test_id(t8h223d)]
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
            ..Default::default()
        };
        assert_eq!(root.resolve(&["dir:.".into()]), ["root"]);
    }

    #[test_id(tbs0ney)]
    #[test]
    fn a_package_the_graph_does_not_know_reaches_no_member_even_beside_one_it_does() {
        let d = [diff("a", "gate/src/x.rs"), diff("b", "loom/src/y.rs")];
        assert!(reaching(&graph(), &["ghost".to_string()], &d).is_empty());
        assert!(reaching(&graph(), &["loom".to_string(), "ghost".to_string()], &d).is_empty());
    }

    #[test_id(t1lrlq4)]
    #[test]
    fn a_root_package_owns_only_the_paths_no_other_package_directory_holds() {
        let g = PackageGraph {
            dirs: BTreeMap::from([
                ("root".to_string(), String::new()),
                ("gate".to_string(), "gate".to_string()),
            ]),
            deps: BTreeMap::new(),
            ..Default::default()
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

    #[test_id(tj3hg6g)]
    #[test]
    fn a_test_word_resolves_to_its_sole_owner_and_stays_unresolved_otherwise() {
        let g = PackageGraph {
            dirs: [("a".to_string(), "a".to_string()), ("b".to_string(), "b".to_string())].into(),
            tests: [
                ("solo".to_string(), vec!["a".to_string()]),
                ("shared".to_string(), vec!["a".to_string(), "b".to_string()]),
            ]
            .into(),
            ..Default::default()
        };
        let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            g.resolve(&words(&["test:solo", "test:shared", "test:none"])),
            words(&["a", "test:shared", "test:none"])
        );
    }

    #[test_id(t0vr7up)]
    #[test]
    fn a_test_word_two_packages_share_reruns_the_test_in_each_owner() {
        let g = PackageGraph {
            dirs: [("a".to_string(), "a".to_string()), ("b".to_string(), "b".to_string())].into(),
            tests: [("it".to_string(), vec!["a".to_string(), "b".to_string()])].into(),
            ..Default::default()
        };
        assert_eq!(
            rerun_scope(&["FAIL test:it x::y".to_string()], Some(&g)),
            vec!["a=x::y".to_string(), "b=x::y".to_string()]
        );
    }
}
