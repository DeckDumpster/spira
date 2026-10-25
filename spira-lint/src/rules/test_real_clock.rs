//! `test-real-clock` — test code does not sleep or compare `Instant::now()` / `.elapsed()` on
//! the real clock unless the line carries `clock-safe: <why>`. Contract: DESIGN.md.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust;
use crate::rust_test::{shape, whole_test_files};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct TestRealClock;

const NAME: &str = "test-real-clock";
const ALLOW_FILE: &str = "spira-lint/test-real-clock-allow";
const MARKER: &str = "clock-safe:";
const SLEEP: &str = "test sleeps on the real clock: synchronise on a channel, barrier, file or flag instead";
const COMPARE: &str = "test compares the real clock: assert what the code did, not how long it took";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

fn applies(path: &str) -> bool {
    path.ends_with(".rs") && !path.starts_with("target/") && !path.split('/').any(|c| c == "perf-checks" || c == "target")
}

type Hit = (usize, &'static str);

fn hits(src: &[u8], regions: &[(usize, usize)], whole: bool) -> Vec<Hit> {
    static SLEEP_RE: OnceLock<Regex> = OnceLock::new();
    static CLOCK: OnceLock<Regex> = OnceLock::new();
    static CMP: OnceLock<Regex> = OnceLock::new();
    let sleep = re(&SLEEP_RE, r"\bsleep\s*\(");
    let clock = re(&CLOCK, r"Instant::now\(\)|\.elapsed\(\)");
    let cmp = re(&CMP, r"[^-=<>]\s*(?:<=?|>=?)\s*[^=<>]");
    let cl = rust::classify(src);
    let code = cl.code_only();
    let in_test = |at: usize| whole || regions.iter().any(|(a, b)| *a <= at && at < *b);
    let line_bounds = |at: usize| {
        let ls = code[..at].iter().rposition(|b| *b == b'\n').map_or(0, |p| p + 1);
        let le = code[at..].iter().position(|b| *b == b'\n').map_or(code.len(), |p| at + p);
        (ls, le)
    };
    let marked = |at: usize| {
        let ls = src[..at].iter().rposition(|b| *b == b'\n').map_or(0, |p| p + 1);
        let prev = src[..ls.saturating_sub(1)].iter().rposition(|b| *b == b'\n').map_or(0, |p| p + 1);
        let end = src[at..].iter().position(|b| *b == b'\n').map_or(src.len(), |p| at + p);
        src[prev..end].windows(MARKER.len()).any(|w| w == MARKER.as_bytes())
    };
    let mut out = Vec::new();
    for m in sleep.find_iter(&code) {
        let at = m.start();
        if in_test(at) && !marked(at) && !code[..at].ends_with(b"fn ") {
            out.push((cl.line_of(at), SLEEP));
        }
    }
    for m in clock.find_iter(&code) {
        let at = m.start();
        let (ls, le) = line_bounds(at);
        if in_test(at) && !marked(at) && cmp.is_match(&code[ls..le]) {
            out.push((cl.line_of(at), COMPARE));
        }
    }
    out.sort();
    out.dedup_by_key(|h| h.0);
    out
}

fn parse_allow(text: &str) -> Result<BTreeMap<String, (usize, usize)>, LintError> {
    let mut m = BTreeMap::new();
    for (i, l) in text.lines().enumerate() {
        let t = l.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = t.split_whitespace().collect();
        let n = f.first().and_then(|n| n.parse::<usize>().ok());
        let (Some(n), 2) = (n, f.len()) else {
            return Err(LintError::BadAllow {
                file: ALLOW_FILE.to_string(),
                line: i + 1,
                reason: format!("want `<count> <path>`, got `{t}`"),
            });
        };
        m.insert(f[1].to_string(), (n, i + 1));
    }
    Ok(m)
}

impl Rule for TestRealClock {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        applies(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = crate::scope(tree, self)?;
        let allow = parse_allow(&tree.read_text(ALLOW_FILE))?;
        let shapes: Vec<(&Entry, crate::rust_test::Shape)> =
            files.iter().filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();
        let whole = whole_test_files(tree, &shapes);
        let mut by: BTreeMap<String, Vec<Hit>> = BTreeMap::new();
        for (e, s) in &shapes {
            let Some(src) = tree.content(e) else { continue };
            let h = hits(src, &s.regions, whole.contains(&e.path));
            if !h.is_empty() {
                by.insert(e.path.clone(), h);
            }
        }
        let mut out = Vec::new();
        for (path, h) in &by {
            let allowed = allow.get(path).map_or(0, |a| a.0);
            if h.len() <= allowed {
                continue;
            }
            let note = if allowed == 0 { String::new() } else { format!(" ({} allowed, {} found)", allowed, h.len()) };
            for (line, msg) in h {
                out.push(Finding { rule: NAME, path: path.clone(), line: Some(*line), message: format!("{msg}{note}") });
            }
        }
        for (path, (n, line)) in &allow {
            let now = by.get(path).map_or(0, Vec::len);
            if now < *n {
                let fix = if now == 0 { "remove the line".to_string() } else { format!("lower it to {now}") };
                out.push(Finding {
                    rule: NAME,
                    path: path.clone(),
                    line: None,
                    message: format!("{ALLOW_FILE}:{line} allows {n} for {path}, which now has {now} — {fix} (the list only shrinks)"),
                });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "A test that sleeps or races the real clock passes on an idle box and flips under load, \
ejecting a round member that never touched the code. Synchronise on a channel, barrier, marker \
file or flag; inject the clock. Where the real clock is a cap on a wait for a named signal, mark \
the line `// clock-safe: <why>`. spira-lint/test-real-clock-allow only shrinks."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        TestRealClock.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_test_sleep_and_a_clock_comparison_are_flagged() {
        let t = TempDir::new("trc-fail");
        t.write(ALLOW_FILE, "");
        t.write(
            "a/src/lib.rs",
            "pub fn f() { std::thread::sleep(D); }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        std::thread::sleep(D);\n        let t0 = Instant::now();\n        if t0.elapsed() > LIMIT { panic!(); }\n    }\n}\n",
        );
        assert_eq!(
            run(&t, &["a/src/lib.rs"]).unwrap(),
            vec![format!("test-real-clock: a/src/lib.rs:6: {SLEEP}"), format!("test-real-clock: a/src/lib.rs:8: {COMPARE}")]
        );
    }

    #[test]
    fn marked_lines_production_code_and_plain_clock_reads_pass() {
        let t = TempDir::new("trc-pass");
        t.write(ALLOW_FILE, "");
        t.write(
            "a/src/lib.rs",
            "pub fn f() { std::thread::sleep(D); }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        // clock-safe: polls a marker file under a cap\n        std::thread::sleep(D);\n        let t0 = Instant::now();\n        use_it(t0);\n    }\n}\n",
        );
        t.write("a/perf-checks/p.rs", "fn t() { sleep(D); }\n");
        assert_eq!(run(&t, &["a/src/lib.rs", "a/perf-checks/p.rs"]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn a_counted_allowance_passes_and_a_stale_or_malformed_one_is_refused() {
        let t = TempDir::new("trc-allow");
        t.write("a/tests/old.rs", "#[test]\nfn t() { sleep(D); }\n");
        t.write("a/tests/fixed.rs", "#[test]\nfn t() {}\n");
        t.write(ALLOW_FILE, "1 a/tests/old.rs\n1 a/tests/fixed.rs\n");
        assert_eq!(
            run(&t, &["a/tests/old.rs", "a/tests/fixed.rs"]).unwrap(),
            vec!["test-real-clock: a/tests/fixed.rs: spira-lint/test-real-clock-allow:2 allows 1 for a/tests/fixed.rs, which now has 0 — remove the line (the list only shrinks)"]
        );
        t.write(ALLOW_FILE, "old.rs\n");
        assert!(run(&t, &["a/tests/old.rs"]).is_err());
    }

    #[test]
    fn no_candidate_files_is_a_refusal() {
        let t = TempDir::new("trc-empty");
        t.write("a.txt", "x\n");
        assert_eq!(run(&t, &["a.txt"]), Err(LintError::EmptyScope));
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(TestRealClock)]
}
