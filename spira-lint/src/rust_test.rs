//! Where Rust test code lives in a file — shared by any rule that must not judge a fixture
//! literal or a test-only call the way it judges production code (sp-qgfdi's `tmp-leak`,
//! sp-9y0gf's `script-callers`). One classifier, so the two rules cannot drift on what
//! counts as "test code" the way `literal-lint`'s hardcoded label copy drifted from
//! `schema.sh` (law-schema-over-code).
//!
//! A file is test code as a WHOLE by where it sits (`tests/…`, `tests.rs`) or by its own
//! inner `#![cfg(test)]`. Otherwise, an item under `#[cfg(test)]` (or `cfg(all(test, …))`,
//! never `cfg(not(test))`) is a test region: a `{…}` item's byte range, or — for a `mod
//! name;` — the whole file(s) that declaration loads, transitively.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust;
use crate::{Entry, Tree};

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

/// A file that is test code as a whole, by where it sits: an integration test
/// (`<crate>/tests/…`) or a module file named `tests.rs`.
pub fn test_by_path(path: &str) -> bool {
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
pub struct Shape {
    /// The whole file is test code by its own inner `#![cfg(test)]`.
    pub inner_test: bool,
    /// Byte ranges of items under `#[cfg(test)]`.
    pub regions: Vec<(usize, usize)>,
    /// Out-of-line modules declared under `#[cfg(test)]`: name, and `#[path]` if any.
    test_mods: Vec<(String, Option<String>)>,
    /// Every `mod name;` in the file: where, the name, and its `#[path]` if it has one.
    all_mods: Vec<(usize, String, Option<String>)>,
}

impl Shape {
    /// Whether byte offset `at` falls in a test region of this file (not counting
    /// [`Shape::inner_test`], which makes the whole file test code — check that separately).
    pub fn covers(&self, at: usize) -> bool {
        self.regions.iter().any(|(a, b)| *a <= at && at < *b)
    }
}

/// Walk the file's attributes. An item under `#[cfg(test)]` (or `cfg(all(test, …))`,
/// never `cfg(not(test))`) is test code: a `{…}` item is a region, a `mod name;` is a
/// test module file.
pub fn shape(src: &[u8]) -> Shape {
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

/// The set of file paths that are test code as a whole: by path, by inner `#![cfg(test)]`,
/// or transitively loaded by a `mod name;` declared inside test code — to a fixpoint, so a
/// test module's own test submodules count too. `shapes` is every `.rs` entry paired with
/// its [`shape`].
pub fn whole_test_files(tree: &Tree, shapes: &[(&Entry, Shape)]) -> BTreeSet<String> {
    let mut whole: BTreeSet<String> =
        shapes.iter().filter(|(e, s)| s.inner_test || test_by_path(&e.path)).map(|(e, _)| e.path.clone()).collect();
    loop {
        let before = whole.len();
        for (e, s) in shapes {
            let file_is_test = whole.contains(&e.path);
            let decls = s.all_mods.iter().filter(|(at, _, _)| file_is_test || s.covers(*at));
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
    whole
}
