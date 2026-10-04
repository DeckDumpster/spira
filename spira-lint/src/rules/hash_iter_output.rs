//! `hash-iter-output` — a `HashMap`/`HashSet` is never iterated into anything printed,
//! written, joined or collected into a `Vec` that is not sorted. Contract and known limits:
//! DESIGN.md.
//!
//! Existing violations are counted per file in `spira-lint/hash-iter-output-allow`, which
//! only shrinks: a file over its count fails, and a file under it fails until it is lowered.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust;
use crate::rules::call_deadline::{enclosing, fn_spans, parse_count_allow, statement};
use crate::rust_test::{shape, whole_test_files, Shape};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct HashIterOutput;

const NAME: &str = "hash-iter-output";
const ALLOW_FILE: &str = "spira-lint/hash-iter-output-allow";
const ITER: &str = "iter|iter_mut|keys|values|values_mut|into_iter|into_keys|into_values|drain";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

fn has(pat: &'static OnceLock<Regex>, src: &str, text: &[u8]) -> bool {
    re(pat, src).is_match(text)
}

fn hash_names(code: &[u8]) -> BTreeSet<Vec<u8>> {
    static TYPED: OnceLock<Regex> = OnceLock::new();
    static NEW: OnceLock<Regex> = OnceLock::new();
    static COLLECT: OnceLock<Regex> = OnceLock::new();
    let mut out = BTreeSet::new();
    let pats = [
        re(&TYPED, r"\b(\w+)\s*:\s*(?:&\s*(?:'\w+\s+)?(?:mut\s+)?)?(?:\w+::)*Hash(?:Map|Set)\b"),
        re(&NEW, r"\blet\s+(?:mut\s+)?(\w+)\s*=\s*(?:\w+::)*Hash(?:Map|Set)\s*::"),
        re(&COLLECT, r"\blet\s+(?:mut\s+)?(\w+)\s*(?::[^=;]*)?=[^;]*collect\s*::\s*<\s*(?:\w+::)*Hash(?:Map|Set)\b"),
    ];
    for p in pats {
        for c in p.captures_iter(code) {
            out.insert(c[1].to_vec());
        }
    }
    out
}

fn body_end(code: &[u8], open: usize) -> usize {
    let mut depth = 0i32;
    for (i, b) in code.iter().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
    }
    code.len()
}

const EMIT: &str = r"\b(?:print|println|eprint|eprintln|write|writeln|format)\s*!";
const PUSH: &str = r"\.\s*(?:push|push_str|write_all|send)\s*\(";
const SORTED: &str = r"\.\s*sort\w*\s*\(|\bBTree(?:Map|Set)\b|\bBinaryHeap\b|\bHash(?:Map|Set)\b";

/// Every offending iteration in one Rust source, as (1-based line, message); `in_test` says
/// whether a byte offset is test code, which is exempt.
pub fn scan(src: &[u8], in_test: &dyn Fn(usize) -> bool) -> Vec<(usize, String)> {
    static FOR: OnceLock<Regex> = OnceLock::new();
    static CHAIN: OnceLock<Regex> = OnceLock::new();
    static E: OnceLock<Regex> = OnceLock::new();
    static P: OnceLock<Regex> = OnceLock::new();
    static S: OnceLock<Regex> = OnceLock::new();
    static C: OnceLock<Regex> = OnceLock::new();
    static SORTFN: OnceLock<Regex> = OnceLock::new();
    let cl = rust::classify(src);
    let code = cl.code_only();
    let names = hash_names(&code);
    if names.is_empty() {
        return Vec::new();
    }
    let spans = fn_spans(&code);
    let fn_sorts = |at: usize| {
        let (a, b) = enclosing(&spans, at).map_or((0, code.len()), |s| (s.start, s.end));
        has(&SORTFN, r"\.\s*sort\w*\s*\(", &code[a..b])
    };
    let mut hits: BTreeMap<usize, String> = BTreeMap::new();
    let mut headers: Vec<(usize, usize)> = Vec::new();
    let for_re = re(
        &FOR,
        &format!(r"\bfor\b[^{{;]*?\bin\s+(?:&\s*(?:mut\s+)?)?(?:self\s*\.\s*)?(\w+)\s*(?:\.\s*(?:{ITER})\s*\(\s*\))?\s*\{{"),
    );
    for c in for_re.captures_iter(&code) {
        let m = c.get(0).unwrap();
        headers.push((m.start(), m.end()));
        if !names.contains(&c[1]) || in_test(m.start()) {
            continue;
        }
        let body = &code[m.end() - 1..body_end(&code, m.end() - 1)];
        if has(&E, EMIT, body) || (has(&P, PUSH, body) && !fn_sorts(m.start())) {
            let line = cl.line_of(m.start());
            hits.insert(line, "for-loop over a HashMap/HashSet feeds output — iteration order differs per process; iterate a BTreeMap/BTreeSet, or collect and sort first".into());
        }
    }
    let chain = re(&CHAIN, &format!(r"\b(?:self\s*\.\s*)?(\w+)\s*\.\s*(?:{ITER})\s*\(\s*\)"));
    for c in chain.captures_iter(&code) {
        let m = c.get(0).unwrap();
        if !names.contains(&c[1]) || in_test(m.start()) || headers.iter().any(|(a, b)| *a <= m.start() && m.start() < *b) {
            continue;
        }
        let stmt = statement(&code, m.start());
        let emits = has(&E, EMIT, stmt);
        let gathers = has(&C, r"\.\s*(?:collect|join)\s*[:(]", stmt) && !has(&S, SORTED, stmt) && !fn_sorts(m.start());
        if emits || gathers {
            let line = cl.line_of(m.start());
            hits.entry(line).or_insert_with(|| "HashMap/HashSet iteration is collected, joined or formatted — iteration order differs per process; iterate a BTreeMap/BTreeSet, or sort the result".into());
        }
    }
    hits.into_iter().collect()
}

fn in_scope(path: &str) -> bool {
    path.ends_with(".rs") && !path.starts_with("target/") && !path.starts_with("testkit/")
}

/// Every violation in the walk, before the allow list: path and (line, message), sorted.
pub fn violations(tree: &Tree) -> Result<Vec<(String, usize, String)>, LintError> {
    let files = crate::scope(tree, &HashIterOutput)?;
    let shapes: Vec<(&Entry, Shape)> = files.iter().filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();
    let whole = whole_test_files(tree, &shapes);
    let mut out = Vec::new();
    for (e, s) in &shapes {
        let Some(src) = tree.content(e) else { continue };
        if whole.contains(&e.path) {
            continue;
        }
        for (line, msg) in scan(src, &|at| s.covers(at)) {
            out.push((e.path.clone(), line, msg));
        }
    }
    out.sort();
    Ok(out)
}

/// The allow file's text for the tree as it stands: what `--emit-allow` prints.
pub fn render_allow(tree: &Tree) -> Result<String, LintError> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (p, _, _) in violations(tree)? {
        *counts.entry(p).or_default() += 1;
    }
    let mut s = String::from(
        "# hash-iter-output: files that iterate a HashMap/HashSet into output and predate the rule, as\n\
# `<count> <path>`. Generated: spira-lint --only hash-iter-output --emit-allow.\n\
#\n\
# SHRINK-ONLY. A count goes down as the iteration moves to a BTreeMap/BTreeSet or is sorted;\n\
# nothing is raised for a NEW iteration.\n",
    );
    for (p, n) in counts {
        s.push_str(&format!("{n} {p}\n"));
    }
    Ok(s)
}

impl Rule for HashIterOutput {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        in_scope(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let allow = parse_count_allow(ALLOW_FILE, &tree.read_text(ALLOW_FILE))?;
        let mut by_path: BTreeMap<String, Vec<(usize, String)>> = BTreeMap::new();
        for (p, line, msg) in violations(tree)? {
            by_path.entry(p).or_default().push((line, msg));
        }
        let mut out = Vec::new();
        for (path, hits) in &by_path {
            let allowed = allow.get(path).map_or(0, |a| a.0);
            if hits.len() <= allowed {
                continue;
            }
            let note = if allowed == 0 { String::new() } else { format!(" ({} allowed, {} found)", allowed, hits.len()) };
            for (line, msg) in hits {
                out.push(Finding { rule: NAME, path: path.clone(), line: Some(*line), message: format!("{msg}{note}") });
            }
        }
        for (path, (n, line)) in &allow {
            let now = by_path.get(path).map_or(0, Vec::len);
            if now < *n {
                let fix = if now == 0 { "remove the line".to_string() } else { format!("lower it to {now}") };
                out.push(Finding {
                    rule: NAME,
                    path: ALLOW_FILE.to_string(),
                    line: Some(*line),
                    message: format!("allows {n} for {path}, which now has {now} — {fix} (the list only shrinks)"),
                });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "a HashMap/HashSet is seeded per process, so iterating one into output makes the order \
differ run to run and breaks replay determinism. Use a BTreeMap/BTreeSet, or collect into a Vec \
and sort it before it is printed, written or sent; lookups (get, contains, insert) are fine. \
Existing violations are counted in spira-lint/hash-iter-output-allow, which only shrinks; \
regenerate with `spira-lint --only hash-iter-output --emit-allow`."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn rs(src: &str) -> Vec<usize> {
        scan(src.as_bytes(), &|_| false).into_iter().map(|h| h.0).collect()
    }

    #[test]
    fn iteration_into_println_fails() {
        assert_eq!(rs("fn f(m: HashMap<String, u32>) {\n    for (k, v) in &m {\n        println!(\"{k} {v}\");\n    }\n}\n"), vec![2]);
        assert_eq!(rs("fn f() {\n    let s: HashSet<u32> = HashSet::new();\n    for x in s.iter() {\n        writeln!(out, \"{x}\").unwrap();\n    }\n}\n"), vec![3]);
    }

    #[test]
    fn collecting_an_iteration_into_an_unsorted_vec_fails() {
        assert_eq!(rs("fn f(m: &HashMap<String, u32>) -> Vec<String> {\n    m.keys().cloned().collect()\n}\n"), vec![2]);
        assert_eq!(rs("struct S { seen: HashSet<u32> }\nimpl S {\n    fn f(&self) -> String {\n        self.seen.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(\",\")\n    }\n}\n"), vec![4]);
        assert_eq!(rs("fn f() {\n    let m = HashMap::new();\n    println!(\"{:?}\", m.values().collect::<Vec<_>>());\n}\n"), vec![3]);
    }

    #[test]
    fn a_lookup_passes() {
        assert!(rs("fn f(m: HashMap<String, u32>, s: HashSet<u32>) -> bool {\n    println!(\"{:?}\", m.get(\"a\"));\n    m.contains_key(\"b\") && s.contains(&1) && m.len() > 0\n}\n").is_empty());
        assert!(rs("fn f(m: HashMap<String, u32>) -> u32 {\n    m.values().sum()\n}\n").is_empty());
        assert!(rs("fn f(m: HashMap<String, u32>) {\n    for (k, v) in &m {\n        total += v;\n    }\n}\n").is_empty());
    }

    #[test]
    fn a_btree_iteration_passes() {
        assert!(rs("fn f(m: BTreeMap<String, u32>) {\n    for (k, v) in &m {\n        println!(\"{k} {v}\");\n    }\n    let v: Vec<_> = m.keys().cloned().collect();\n}\n").is_empty());
    }

    #[test]
    fn sorting_the_result_passes() {
        assert!(rs("fn f(m: &HashMap<String, u32>) -> Vec<String> {\n    let mut v: Vec<String> = m.keys().cloned().collect();\n    v.sort();\n    v\n}\n").is_empty());
        assert!(rs("fn f(m: &HashMap<String, u32>) -> BTreeSet<String> {\n    m.keys().cloned().collect::<BTreeSet<_>>()\n}\n").is_empty());
    }

    #[test]
    fn prose_and_literals_are_not_code() {
        assert!(rs("fn f(m: HashMap<String, u32>) {\n    // for x in m { println!(\"x\"); }\n    let _ = \"for x in m { println!(); }\";\n}\n").is_empty());
    }

    #[test]
    fn test_code_is_exempt() {
        let src = "fn f(m: HashMap<String, u32>) {\n    for (k, v) in &m {\n        println!(\"{k}\");\n    }\n}\n";
        assert!(scan(src.as_bytes(), &|_| true).is_empty());
    }

    fn run(t: &TempDir, files: &[&str]) -> Vec<String> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        HashIterOutput.check(&tree).unwrap().iter().map(|f| f.to_string()).collect()
    }

    const OFFENDER: &str = "fn f(m: HashMap<String, u32>) {\n    for k in &m {\n        println!(\"{k:?}\");\n    }\n}\n";

    #[test]
    fn the_allow_list_only_shrinks() {
        let t = TempDir::new("hashiter-allow");
        t.write("a/src/lib.rs", OFFENDER);
        t.write(ALLOW_FILE, "");
        let got = run(&t, &["a/src/lib.rs"]);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].starts_with("hash-iter-output: a/src/lib.rs:2:"), "{got:?}");
        t.write(ALLOW_FILE, "1 a/src/lib.rs\n");
        assert!(run(&t, &["a/src/lib.rs"]).is_empty());
        t.write(ALLOW_FILE, "2 a/src/lib.rs\n");
        let got = run(&t, &["a/src/lib.rs"]);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("lower it to 1"), "{got:?}");
        t.write("a/src/lib.rs", "fn f() {}\n");
        t.write(ALLOW_FILE, "1 a/src/lib.rs\n");
        assert!(run(&t, &["a/src/lib.rs"])[0].contains("remove the line"));
    }
}
