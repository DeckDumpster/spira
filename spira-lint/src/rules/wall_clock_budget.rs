//! `wall-clock-budget` — a corpus test never asserts a measured duration against a constant
//! (law-no-wall-clock-budgets-in-the-corpus). Contract: DESIGN.md.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust;
use crate::rust_test::{shape, whole_test_files, Shape};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct WallClockBudget;

const NAME: &str = "wall-clock-budget";
const ALLOW_FILE: &str = "spira-lint/wall-clock-budget-allow";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

fn is_perf_path(path: &str) -> bool {
    path.split('/').any(|c| c == "perf-checks")
}

impl Rule for WallClockBudget {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        (e.path.ends_with(".rs") && !e.path.starts_with("target/")) && !is_perf_path(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        static RS: OnceLock<Regex> = OnceLock::new();
        let files = crate::scope(tree, self)?;
        let rs = re(&RS, r"(?:elapsed\w*|took\w*|latency\w*|\w+_ms|\w+_secs?|as_(?:millis|micros|secs|secs_f64|secs_f32))\b[^;{]*?<=?\s*(?:[0-9]|(?:[a-z_]+::)*Duration::|[A-Z][A-Z0-9_]{2,}\b)");

        let allow: Vec<(usize, String)> = tree
            .read_text(ALLOW_FILE)
            .lines()
            .enumerate()
            .filter(|(_, l)| {
                let t = l.trim_start();
                !(t.is_empty() || t.starts_with('#'))
            })
            .map(|(i, l)| (i + 1, l.trim().to_string()))
            .collect();

        let shapes: Vec<(&Entry, Shape)> =
            files.iter().filter(|e| e.path.ends_with(".rs")).filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();
        let whole = whole_test_files(tree, &shapes);

        let mut hits: Vec<(String, usize, &'static str)> = Vec::new();
        for e in &files {
            let Some(src) = tree.content(e) else { continue };
            if let Some((_, s)) = shapes.iter().find(|(x, _)| x.path == e.path) {
                let cl = rust::classify(src);
                let code = cl.code_only();
                let file_is_test = whole.contains(&e.path);
                let mut seen = BTreeSet::new();
                for m in rs.find_iter(&code) {
                    let at = m.start();
                    if !(file_is_test || s.regions.iter().any(|(a, b)| *a <= at && at < *b)) {
                        continue;
                    }
                    let ls = code[..at].iter().rposition(|b| *b == b'\n').map_or(0, |p| p + 1);
                    let stmt_start = code[..at].iter().rposition(|b| *b == b';' || *b == b'{').map_or(0, |p| p + 1).max(ls.saturating_sub(200));
                    if !code[stmt_start..at].windows(6).any(|w| w == b"assert") && !code[ls..at].windows(6).any(|w| w == b"assert") {
                        continue;
                    }
                    let line = cl.line_of(at);
                    if seen.insert(line) {
                        hits.push((e.path.clone(), line, "test asserts a measured duration against a constant"));
                    }
                }
            }
        }

        let offenders: BTreeSet<&str> = hits.iter().map(|h| h.0.as_str()).collect();
        let mut out: Vec<Finding> = hits
            .iter()
            .filter(|(p, _, _)| !allow.iter().any(|(_, a)| a == p))
            .map(|(p, l, m)| Finding { rule: NAME, path: p.clone(), line: Some(*l), message: (*m).into() })
            .collect();
        for (line, p) in &allow {
            if !offenders.contains(p.as_str()) {
                out.push(Finding {
                    rule: NAME,
                    path: ALLOW_FILE.to_string(),
                    line: Some(*line),
                    message: format!("lists {p}, which no longer needs an exception — remove the line (the list only shrinks)"),
                });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "A duration budget in the round corpus fails on a loaded box and passes on an idle one, so \
it measures the machine. Assert what the code did — rows examined, calls made, bytes read. A real \
latency check belongs in a perf-checks/ path outside the corpus. spira-lint/wall-clock-budget-allow only shrinks."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(WallClockBudget),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        WallClockBudget.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    const RS_MSG: &str = "test asserts a measured duration against a constant";

    #[test_id(tzyr3ra)]
    #[test]
    fn duration_budgets_are_caught_naming_file_and_line() {
        let t = TempDir::new("wcb-fail");
        t.write(ALLOW_FILE, "# shrink-only\n");
        t.write(
            "a/src/lib.rs",
            "pub fn f() {}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        let t0 = std::time::Instant::now();\n        assert!(t0.elapsed() < std::time::Duration::from_millis(50));\n    }\n}\n",
        );
        let got = run(&t, &["a/src/lib.rs"]).unwrap();
        assert_eq!(got, vec![format!("wall-clock-budget: a/src/lib.rs:7: {RS_MSG}")]);
    }

    #[test_id(t1gy5yt)]
    #[test]
    fn counts_rows_lower_bounds_perf_paths_and_non_test_code_pass() {
        let t = TempDir::new("wcb-pass");
        t.write(ALLOW_FILE, "");
        t.write(
            "a/src/lib.rs",
            "pub fn g(t0: std::time::Instant) -> bool { t0.elapsed() < std::time::Duration::from_secs(1) }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() { assert_eq!(rows_examined, 3); assert!(calls < 4); }\n}\n",
        );
        t.write("a/perf-checks/p.rs", "fn t() { assert!(t0.elapsed() < D); }\n");
        assert_eq!(
            run(&t, &["a/src/lib.rs", "a/perf-checks/p.rs"]).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test_id(ts5a28w)]
    #[test]
    fn a_listed_exception_passes_and_a_stale_one_is_refused() {
        let t = TempDir::new("wcb-allow");
        t.write("a/src/old.rs", "#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() { assert!(t0.elapsed() < std::time::Duration::from_millis(50)); }\n}\n");
        t.write("a/src/fixed.rs", "pub fn f() {}\n");
        t.write(ALLOW_FILE, "# header\na/src/old.rs\na/src/fixed.rs\n");
        assert_eq!(
            run(&t, &["a/src/old.rs", "a/src/fixed.rs"]).unwrap(),
            vec!["wall-clock-budget: spira-lint/wall-clock-budget-allow:3: lists a/src/fixed.rs, which no longer needs an exception — remove the line (the list only shrinks)"]
        );
    }

    #[test_id(tafuvfd)]
    #[test]
    fn no_candidate_files_is_a_refusal() {
        let t = TempDir::new("wcb-empty");
        t.write("a.txt", "x\n");
        assert_eq!(run(&t, &["a.txt"]), Err(LintError::EmptyScope));
    }
}
