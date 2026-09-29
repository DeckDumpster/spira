//! `tmp-leak` — test code gets its scratch space from `testkit::TempDir`, never from a bare
//! `temp_dir()` it may forget to remove (sp-qgfdi). Contract: DESIGN.md.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust;
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct TmpLeak;

const NAME: &str = "tmp-leak";
const ALLOW_FILE: &str = "spira-lint/tmp-leak-allow";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

/// A file that is test code as a whole, by where it sits: an integration test
/// (`<crate>/tests/…`) or a module file named `tests.rs`.
fn test_by_path(path: &str) -> bool {
    path.starts_with("tests/") || path.contains("/tests/") || path == "tests.rs" || path.ends_with("/tests.rs")
}

/// The index just past the bracket that closes the one opening at `open`, over code-only
/// bytes; `None` when it never closes.
fn close_of(code: &[u8], open: usize) -> Option<usize> {
    let (o, c) = match code[open] {
        b'{' => (b'{', b'}'),
        b'(' => (b'(', b')'),
        b'[' => (b'[', b']'),
        _ => return None,
    };
    let mut depth = 0usize;
    for (i, &b) in code.iter().enumerate().skip(open) {
        if b == o {
            depth += 1;
        } else if b == c {
            depth -= 1;
            if depth == 0 {
                return Some(i + 1);
            }
        }
    }
    None
}

fn skip_ws(code: &[u8], mut i: usize) -> usize {
    while i < code.len() && code[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// What one file says about test code.
#[derive(Default)]
struct Shape {
    /// The whole file is test code by its own inner `#![cfg(test)]`.
    inner_test: bool,
    /// Byte ranges of items under `#[cfg(test)]`.
    regions: Vec<(usize, usize)>,
    /// Out-of-line modules declared under `#[cfg(test)]`: name, and `#[path]` if any.
    test_mods: Vec<(String, Option<String>)>,
    /// Every `mod name;` in the file: where, the name, and its `#[path]` if it has one.
    all_mods: Vec<(usize, String, Option<String>)>,
}

/// Walk the file's attributes. An item under `#[cfg(test)]` (or `cfg(all(test, …))`,
/// never `cfg(not(test))`) is test code: a `{…}` item is a region, a `mod name;` is a
/// test module file.
fn shape(src: &[u8]) -> Shape {
    static CFG_TEST: OnceLock<Regex> = OnceLock::new();
    static MOD_DECL: OnceLock<Regex> = OnceLock::new();
    static PATH_ATTR: OnceLock<Regex> = OnceLock::new();
    let cl = rust::classify(src);
    let code = cl.code_only();
    let text = cl.without_comments();
    let cfg_test = re(&CFG_TEST, r"^#\s*\[\s*cfg\s*\((?:[^\]]*[^A-Za-z0-9_])?test(?:[^A-Za-z0-9_]|$)");
    let mod_decl = re(&MOD_DECL, r"^(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;");
    let path_attr = re(&PATH_ATTR, r#"^#\s*\[\s*path\s*=\s*"([^"]*)"\s*\]"#);
    static INNER: OnceLock<Regex> = OnceLock::new();
    let mut s = Shape {
        inner_test: re(&INNER, r"#\s*!\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]").is_match(&code),
        ..Shape::default()
    };
    let mut paths: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    while i < code.len() {
        if code[i] != b'#' || (i > 0 && (code[i - 1].is_ascii_alphanumeric() || code[i - 1] == b'_')) {
            i += 1;
            continue;
        }
        // One run of outer attributes, then the item they sit on.
        let mut j = i;
        let mut is_test = false;
        let mut path: Option<String> = None;
        loop {
            let a = skip_ws(&code, j);
            if a >= code.len() || code[a] != b'#' {
                j = a;
                break;
            }
            let b = skip_ws(&code, a + 1);
            if b >= code.len() || code[b] != b'[' {
                j = a;
                break;
            }
            let Some(end) = close_of(&code, b) else {
                j = code.len();
                break;
            };
            let attr = &code[a..end];
            if cfg_test.is_match(attr) && !attr.windows(4).any(|w| w == b"not(") {
                is_test = true;
            }
            if let Some(c) = path_attr.captures(&text[a..end]) {
                path = Some(String::from_utf8_lossy(&c[1]).into_owned());
            }
            j = end;
        }
        if j >= code.len() {
            break;
        }
        // Inner attributes (`#![…]`) and anything else not followed by an item: move on.
        if j == skip_ws(&code, i) {
            i += 1;
            continue;
        }
        if let Some(c) = mod_decl.captures(&code[j..]) {
            let name = String::from_utf8_lossy(&c[1]).into_owned();
            if let Some(p) = &path {
                paths.push((name.clone(), p.clone()));
            }
            if is_test {
                s.test_mods.push((name, path));
            }
            i = j + c.get(0).map_or(1, |m| m.end());
            continue;
        }
        if is_test {
            // The item runs to its first `{…}` block, or to a `;` before any block.
            let mut k = j;
            let mut end = None;
            while k < code.len() {
                match code[k] {
                    b'(' | b'[' => k = close_of(&code, k).unwrap_or(code.len()),
                    b';' => {
                        end = Some(k + 1);
                        break;
                    }
                    b'{' => {
                        end = Some(close_of(&code, k).unwrap_or(code.len()));
                        break;
                    }
                    _ => k += 1,
                }
            }
            let end = end.unwrap_or(code.len());
            s.regions.push((j, end));
            i = end;
            continue;
        }
        i = j;
    }
    // Every `mod name;`: one inside a test region, or in a test file, declares a test module.
    static ANY: OnceLock<Regex> = OnceLock::new();
    let any = re(&ANY, r"(?:^|[^A-Za-z0-9_])mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;");
    for c in any.captures_iter(&code) {
        let name = String::from_utf8_lossy(&c[1]).into_owned();
        let path = paths.iter().find(|(n, _)| *n == name).map(|(_, p)| p.clone());
        s.all_mods.push((c.get(1).map_or(0, |m| m.start()), name, path));
    }
    s
}

/// The file `mod name;` in `decl` loads.
fn mod_file(decl: &str, name: &str, path: Option<&str>) -> Vec<String> {
    let dir = decl.rsplit_once('/').map_or("", |(d, _)| d);
    let join = |d: &str, f: &str| if d.is_empty() { f.to_string() } else { format!("{d}/{f}") };
    if let Some(p) = path {
        return vec![join(dir, p)];
    }
    let base = decl.rsplit('/').next().unwrap_or(decl);
    let home = if matches!(base, "lib.rs" | "main.rs" | "mod.rs") {
        dir.to_string()
    } else {
        join(dir, base.trim_end_matches(".rs"))
    };
    vec![join(&home, &format!("{name}.rs")), join(&home, &format!("{name}/mod.rs"))]
}

impl Rule for TmpLeak {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path.ends_with(".rs") && !e.path.starts_with("testkit/") && !e.path.starts_with("target/")
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        static CALL: OnceLock<Regex> = OnceLock::new();
        let files = crate::scope(tree, self)?;
        let shapes: Vec<(&Entry, Shape)> =
            files.iter().filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();

        // Whole-file test code: by path, then every module test code declares, to a fixpoint.
        let mut whole: BTreeSet<String> =
            shapes.iter().filter(|(e, s)| s.inner_test || test_by_path(&e.path)).map(|(e, _)| e.path.clone()).collect();
        loop {
            let before = whole.len();
            for (e, s) in &shapes {
                let file_is_test = whole.contains(&e.path);
                let decls = s.all_mods.iter().filter(|(at, _, _)| {
                    file_is_test || s.regions.iter().any(|(a, b)| a <= at && at < b)
                });
                let named = decls.map(|(_, n, p)| (n.clone(), p.clone())).chain(s.test_mods.iter().cloned());
                for (n, p) in named {
                    for f in mod_file(&e.path, &n, p.as_deref()) {
                        if tree.entry(&f).is_some() {
                            whole.insert(f);
                        }
                    }
                }
            }
            if whole.len() == before {
                break;
            }
        }

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
        let call = re(&CALL, r"(?:^|[^A-Za-z0-9_])(temp_dir)\s*\(\s*\)");
        let mut out = Vec::new();
        let mut offenders = BTreeSet::new();
        for (e, s) in &shapes {
            let Some(src) = tree.content(e) else { continue };
            let cl = rust::classify(src);
            let code = cl.code_only();
            let file_is_test = whole.contains(&e.path);
            for c in call.captures_iter(&code) {
                let at = c.get(1).map_or(0, |m| m.start());
                if !(file_is_test || s.regions.iter().any(|(a, b)| *a <= at && at < *b)) {
                    continue;
                }
                offenders.insert(e.path.clone());
                if allow.iter().any(|(_, p)| p == &e.path) {
                    continue;
                }
                out.push(Finding {
                    rule: NAME,
                    path: e.path.clone(),
                    line: Some(cl.line_of(at)),
                    message: "temp_dir() in test code — take a testkit::TempDir, which removes itself on drop".into(),
                });
            }
        }
        for (line, p) in &allow {
            if !offenders.contains(p) {
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
        "A test's scratch directory under the system temp dir outlives the test unless something \
removes it, and /tmp is a tmpfs with a fixed inode budget (sp-qgfdi: one workspace run leaked \
157 entries, inode exhaustion 2026-09-29). Use testkit::TempDir::new(tag), which removes the \
directory on drop, panics included. spira-lint/tmp-leak-allow only shrinks."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        TmpLeak.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    // The fixtures spell the call through concat! so this file carries no call in code.
    const CALL: &str = concat!("std::env::", "temp_dir", "()");

    fn leaking_helper() -> String {
        format!(
            "fn dir(tag: &str) -> std::path::PathBuf {{\n    let d = {CALL}.join(tag);\n    std::fs::create_dir_all(&d).unwrap();\n    d\n}}\n"
        )
    }

    /// The failing fixture: the leak in each place test code lives.
    #[test]
    fn a_bare_temp_dir_in_test_code_is_caught_wherever_test_code_lives() {
        let t = TempDir::new("tmpleak-fail");
        t.write(ALLOW_FILE, "# shrink-only\n");
        // 1. inside #[cfg(test)] mod tests { … }
        t.write(
            "a/src/lib.rs",
            &format!("pub fn f() {{}}\n\n#[cfg(test)]\nmod tests {{\n    {}}}\n", leaking_helper()),
        );
        // 2. an integration test
        t.write("b/tests/it.rs", &leaking_helper());
        // 3. an out-of-line test module, declared under #[cfg(test)], and a module it declares
        t.write("c/src/main.rs", "fn main() {}\n#[cfg(test)]\nmod testutil;\n");
        t.write("c/src/testutil.rs", &format!("mod deeper;\n{}", leaking_helper()));
        t.write("c/src/testutil/deeper.rs", &leaking_helper());
        // 4. #[path] and a cfg(all(test, …)) fn
        t.write(
            "d/src/x.rs",
            &format!(
                "#[cfg(test)]\n#[path = \"x_tests.rs\"]\nmod t;\n#[cfg(all(test, unix))]\nfn h() {{ let _ = {CALL}; }}\n"
            ),
        );
        t.write("d/src/x_tests.rs", &leaking_helper());
        // 5. a file that is test code by its own #![cfg(test)]
        t.write("e/src/helpers.rs", &format!("#![cfg(test)]\n{}", leaking_helper()));
        let got = run(
            &t,
            &["a/src/lib.rs", "b/tests/it.rs", "c/src/main.rs", "c/src/testutil.rs", "c/src/testutil/deeper.rs", "d/src/x.rs", "d/src/x_tests.rs", "e/src/helpers.rs"],
        )
        .unwrap();
        let msg = "temp_dir() in test code — take a testkit::TempDir, which removes itself on drop";
        assert_eq!(
            got,
            vec![
                format!("tmp-leak: a/src/lib.rs:6: {msg}"),
                format!("tmp-leak: b/tests/it.rs:2: {msg}"),
                format!("tmp-leak: c/src/testutil.rs:3: {msg}"),
                format!("tmp-leak: c/src/testutil/deeper.rs:2: {msg}"),
                format!("tmp-leak: d/src/x.rs:5: {msg}"),
                format!("tmp-leak: d/src/x_tests.rs:2: {msg}"),
                format!("tmp-leak: e/src/helpers.rs:3: {msg}"),
            ]
        );
    }

    /// The passing fixture: testkit in test code, and temp_dir() where it is not test code,
    /// is in a comment or string, or sits under cfg(not(test)).
    #[test]
    fn testkit_and_non_test_code_pass() {
        let t = TempDir::new("tmpleak-pass");
        t.write(ALLOW_FILE, "");
        t.write(
            "a/src/lib.rs",
            &format!(
                "pub fn scratch() -> std::path::PathBuf {{ {CALL} }}\n\
#[cfg(not(test))]\nfn prod() {{ let _ = {CALL}; }}\n\
mod real;\n\
#[cfg(test)]\nmod tests {{\n    // never {CALL} in a test\n    const S: &str = \"{CALL}\";\n    #[test]\n    fn t() {{ let d = testkit::TempDir::new(\"a\"); let _ = d.join(\"x\"); }}\n}}\n\
pub fn after() -> std::path::PathBuf {{ {CALL} }}\n"
            ),
        );
        t.write("a/src/real.rs", &leaking_helper());
        t.write("testkit/src/lib.rs", &leaking_helper());
        assert_eq!(run(&t, &["a/src/lib.rs", "a/src/real.rs", "testkit/src/lib.rs"]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn a_listed_exception_passes_and_a_stale_one_is_refused() {
        let t = TempDir::new("tmpleak-allow");
        t.write("b/tests/old.rs", &leaking_helper());
        t.write("b/tests/fixed.rs", "fn f() {}\n");
        t.write(ALLOW_FILE, "# header\nb/tests/old.rs\nb/tests/fixed.rs\n");
        assert_eq!(
            run(&t, &["b/tests/old.rs", "b/tests/fixed.rs"]).unwrap(),
            vec!["tmp-leak: spira-lint/tmp-leak-allow:3: lists b/tests/fixed.rs, which no longer needs an exception — remove the line (the list only shrinks)"]
        );
    }

    #[test]
    fn no_rust_files_is_a_refusal() {
        let t = TempDir::new("tmpleak-empty");
        t.write("a.sh", "x\n");
        assert_eq!(run(&t, &["a.sh", "testkit/src/lib.rs"]), Err(LintError::EmptyScope));
    }
}
