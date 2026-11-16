//! `compile-time-path` — no `.rs` file fixes a path at compile time. Contract: DESIGN.md.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust::{self, Class};
use crate::{Entry, Finding, LintError, Rule, Tree};

#[derive(Default)]
pub struct CompileTimePath {
    scanned: AtomicUsize,
}

const NAME: &str = "compile-time-path";
const OWN_SOURCE: &str = "spira-lint/src/rules/compile_time_path.rs";
const SYSTEM_PATHS_SOURCE: &str = "system-paths/src/lib.rs";
const ALLOW_FILE: &str = "spira-lint/compile-time-path-allow.d";
const ROOTS: [&str; 22] = [
    "/home", "/srv", "/root", "/Users", "/proc", "/sys", "/dev", "/tmp", "/var", "/etc", "/run", "/usr", "/bin", "/sbin", "/lib",
    "/nonexistent", "/workspace", "/work", "/src", "/shots", "/spira-shots", "/.dockerenv",
];
const LITERAL_MESSAGE: &str = "an absolute path literal: a fixture is relative, or joined to a `testkit::TempDir`; a real system path is a `system_paths` constant";
const MARKER_MESSAGE: &str = "a `not a path` marker needs a reason and cannot excuse a filesystem root";

fn env_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"\b(?:option_)?env!\s*\(\s*"(CARGO_MANIFEST_DIR|OUT_DIR|CARGO_BIN_EXE_[^"]*)""#).expect("static regex")
    })
}

fn include_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\binclude(?:_str|_bytes)?!\s*\(").expect("static regex"))
}

fn marker_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"//\s*not a path:([^\n]*)$").expect("static regex"))
}

fn whole_arg_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\.(?:pointer|pointer_mut|route)\s*\(\s*$").expect("static regex"))
}

fn after_env_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"\benv!\s*\(\s*"[^"]*"\s*\)\s*,\s*$"#).expect("static regex"))
}

fn fragment_arg_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"\.(?:contains|starts_with|ends_with|find|rfind|strip_prefix|strip_suffix|split_once|rsplit_once|replace)\s*\(\s*&?\s*$")
            .expect("static regex")
    })
}

/// The text of a string literal run, without prefix or quotes; `None` for a char literal.
fn literal_text(run: &[u8]) -> Option<&[u8]> {
    let mut i = 0;
    if matches!(run.first(), Some(b'b' | b'c')) {
        i += 1;
    }
    let raw = run.get(i) == Some(&b'r');
    let mut hashes = 0;
    if raw {
        i += 1;
        while run.get(i) == Some(&b'#') {
            hashes += 1;
            i += 1;
        }
    }
    if run.get(i) != Some(&b'"') {
        return None;
    }
    let body = &run[i + 1..];
    Some(&body[..body.len().saturating_sub(1 + hashes)])
}

fn is_candidate(text: &[u8]) -> bool {
    text.first() == Some(&b'/') && text.get(1).is_some_and(|c| c.is_ascii_alphanumeric() || b"_.~@$-".contains(c))
}

fn is_root_literal(text: &[u8]) -> bool {
    let t = String::from_utf8_lossy(text);
    ROOTS.iter().any(|r| t == *r || t.starts_with(&format!("{r}/")) || t.starts_with(&format!("{r}:")))
}

fn not_a_path(code_before: &[u8], text: &[u8]) -> bool {
    let window = &code_before[code_before.len().saturating_sub(48)..];
    if text.starts_with(b"/org/freedesktop/") || whole_arg_re().is_match(window) {
        return true;
    }
    let seg = &text[1..];
    let seg = seg.strip_suffix(b"/").unwrap_or(seg);
    !seg.contains(&b'/') && fragment_arg_re().is_match(window)
}

fn parse_allow(tree: &Tree) -> Result<BTreeSet<String>, LintError> {
    let mut m = BTreeSet::new();
    for (krate, bead) in crate::allow_dir(tree, ALLOW_FILE) {
        if !bead.starts_with("sp-") || bead.contains(char::is_whitespace) {
            return Err(LintError::BadAllow { file: format!("{ALLOW_FILE}/{krate}"), line: 1, reason: format!("want the converting bead id, got `{bead}`") });
        }
        m.insert(krate);
    }
    Ok(m)
}

fn owning_crate<'a>(allow: &'a BTreeSet<String>, path: &str) -> Option<&'a String> {
    allow.iter().filter(|k| path.starts_with(&format!("{k}/"))).max_by_key(|k| k.len())
}

/// Byte ranges of the argument lists of `include!`-family calls; `code` has literals blanked
/// so a paren inside a string cannot unbalance the walk.
fn include_args(code: &[u8]) -> Vec<(usize, usize)> {
    include_re()
        .find_iter(code)
        .map(|m| {
            let mut depth = 1usize;
            let mut end = m.end();
            for (i, &b) in code.iter().enumerate().skip(m.end()) {
                match b {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            (m.end(), end)
        })
        .collect()
}

impl Rule for CompileTimePath {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path.ends_with(".rs") && !e.path.starts_with("target/")
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = crate::scope(tree, self)?;
        let allow = parse_allow(tree)?;
        let prior = crate::allow_dir_at(tree, ALLOW_FILE);
        let mut out = Vec::new();
        let mut scanned = 0;
        let mut suppressed: BTreeMap<String, usize> = BTreeMap::new();
        for e in files {
            if e.path == OWN_SOURCE || e.path == SYSTEM_PATHS_SOURCE {
                continue;
            }
            let Some(src) = tree.content(e) else { continue };
            scanned += 1;
            let cl = rust::classify(src);
            let text = cl.without_comments();
            let skip = include_args(&cl.code_only());
            for c in env_re().captures_iter(&text) {
                let m = c.get(0).unwrap();
                if skip.iter().any(|(a, b)| *a <= m.start() && m.start() < *b) {
                    continue;
                }
                let message = if c[1].starts_with(b"CARGO_BIN_EXE_") {
                    "compile-time path: use testkit::bin_exe(name)".to_string()
                } else {
                    "compile-time path: read the tree at run time: spira_config::resolve::locate_home_for_process() for spira/…, checkout_root_for_process() for the checkout, testkit::manifest_dir() for this crate".to_string()
                };
                out.push(Finding { rule: NAME, path: e.path.clone(), line: Some(cl.line_of(m.start())), message });
            }
            let code = cl.code_only();
            let texts = &text;
            let mut at = 0;
            while at < src.len() {
                if cl.class[at] != Class::Literal {
                    at += 1;
                    continue;
                }
                let start = at;
                while at < src.len() && cl.class[at] == Class::Literal {
                    at += 1;
                }
                let Some(text) = literal_text(&src[start..at]) else { continue };
                let before = &texts[..start];
                if !is_candidate(text) || after_env_re().is_match(&before[before.len().saturating_sub(96)..]) || not_a_path(&code[..start], text) || skip.iter().any(|(a, b)| *a <= start && start < *b) {
                    continue;
                }
                let line = cl.line_of(start);
                let line_start = src[..start].iter().rposition(|b| *b == b'\n').map_or(0, |p| p + 1);
                let line_end = src[start..].iter().position(|b| *b == b'\n').map_or(src.len(), |p| start + p);
                let message = match marker_re().captures(&src[line_start..line_end]) {
                    Some(c) if c[1].iter().all(u8::is_ascii_whitespace) || is_root_literal(text) => MARKER_MESSAGE,
                    Some(_) => continue,
                    None => LITERAL_MESSAGE,
                };
                if message == LITERAL_MESSAGE {
                    if let Some(k) = owning_crate(&allow, &e.path) {
                        *suppressed.entry(k.clone()).or_default() += 1;
                        continue;
                    }
                }
                out.push(Finding { rule: NAME, path: e.path.clone(), line: Some(line), message: message.into() });
            }
        }
        for krate in &allow {
            let entry = format!("{ALLOW_FILE}/{krate}");
            if !suppressed.contains_key(krate) {
                out.push(Finding {
                    rule: NAME,
                    path: entry.clone(),
                    line: None,
                    message: format!("{entry} allows {krate}, which has no absolute path literal left — stale: delete it (the list only shrinks)"),
                });
            } else if prior.as_ref().is_some_and(|p| !p.contains(krate)) {
                out.push(Finding {
                    rule: NAME,
                    path: entry.clone(),
                    line: None,
                    message: format!("new entry {krate} — the list only shrinks; make the literals relative or take a `system_paths` constant"),
                });
            }
        }
        self.scanned.store(scanned, Ordering::Relaxed);
        out.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
        Ok(out)
    }

    fn checked(&self) -> Option<(usize, String)> {
        Some((self.scanned.load(Ordering::Relaxed), "rust files".to_string()))
    }

    fn hint(&self) -> &'static str {
        "A path baked in at compile time does not fail closed: when the builder's tree still \
exists, the test or binary silently reads that tree instead of the one it runs in. Resolve the \
path at run time through spira-config or testkit. No absolute path literal is compiled in: a \
fixture is relative or joined to a testkit::TempDir, a real system path is a system_paths \
constant, and a literal that is not a filesystem path says so with `// not a path: <reason>`. \
include!/include_str!/include_bytes! arguments and CARGO_PKG_* are content, not paths. \
spira-lint/compile-time-path-allow.d only shrinks."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(CompileTimePath::default())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(src: &str) -> Vec<String> {
        let src = &src.replace("@H", "/home").replace("@U", "/Users");
        let t = TempDir::new("ctp");
        t.write("a/src/lib.rs", src);
        let tree = Tree::from_paths(t.path(), ["a/src/lib.rs"], std::iter::empty::<&str>());
        CompileTimePath::default().check(&tree).unwrap().iter().map(|f| f.to_string()).collect()
    }

    #[test]
    fn each_compile_time_path_shape_is_a_finding_at_its_line() {
        for src in [
            "fn f() {\n    let _ = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"../systemd\");\n}\n",
            "fn f() {\n    let _ = concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/src\");\n}\n",
            "fn f() {\n    let _ = env!(\"CARGO_BIN_EXE_x\");\n}\n",
            "fn f() {\n    let _ = option_env!(\"OUT_DIR\");\n}\n",
            "fn f() {\n    let _ = \"@H/someone/spira/run/x\";\n}\n",
            "fn f() {\n    let _ = \"/srv/spira-releases/x\";\n}\n",
            "fn f() {\n    let _ = \"/root/spira/run/x\";\n}\n",
            "fn f() {\n    let _ = \"@U/someone/spira/x\";\n}\n",
        ] {
            let got = run(src);
            assert_eq!(got.len(), 1, "{src}: {got:?}");
            assert!(got[0].starts_with("compile-time-path: a/src/lib.rs:2: "), "{got:?}");
        }
        assert!(run("fn f() { env!(\"CARGO_BIN_EXE_x\"); }\n")[0].contains("testkit::bin_exe(name)"));
        assert!(run("fn f() { \"/srv/spira/run/y\"; }\n")[0].contains("an absolute path literal"));
    }

    #[test]
    fn the_registry_carries_the_rule_and_fires_it_on_a_planted_violation() {
        let rule = crate::all_rules().into_iter().find(|r| r.name() == NAME).expect("registered by build.rs");
        let t = TempDir::new("ctp-reg");
        t.write("a/src/lib.rs", "const B: &str = env!(\"CARGO_MANIFEST_DIR\");\n");
        let tree = Tree::from_paths(t.path(), ["a/src/lib.rs"], std::iter::empty::<&str>());
        assert_eq!(rule.check(&tree).unwrap().len(), 1);
        assert_eq!(rule.checked().map(|c| c.0), Some(1));
    }

    #[test]
    fn content_and_non_paths_are_clean() {
        for src in [
            "const A: &str = include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/a.txt\"));\n",
            "include!(concat!(env!(\"OUT_DIR\"), \"/x.rs\"));\n",
            "const V: &str = env!(\"CARGO_PKG_VERSION\");\n",
            "const R: &str = \"relative/x\";\n",
            "// env!(\"CARGO_MANIFEST_DIR\") and \"@H/someone/spira/run/x\"\n",
        ] {
            assert_eq!(run(src), Vec::<String>::new(), "{src}");
        }
    }

    #[test]
    fn every_absolute_path_literal_is_a_finding_at_its_line() {
        for lit in ["\"/r/spira-releases/x\"", "\"/home/x\"", "\"/tmp/w1\"", "r\"/h/spira\"", "b\"/dev/null\"", "r#\"/h/x\"#", "\"/usr/bin:/rel/bin\""] {
            let got = run(&format!("fn f() {{\n    let _ = {lit};\n}}\n"));
            assert_eq!(got.len(), 1, "{lit}: {got:?}");
            assert!(got[0].starts_with("compile-time-path: a/src/lib.rs:2: an absolute path literal"), "{got:?}");
        }
    }

    #[test]
    fn forms_that_are_not_filesystem_paths_are_clean() {
        for src in [
            "const A: &str = \"/{file}\";\n",
            "const A: &str = \"/org/freedesktop/systemd1\";\n",
            "fn f(v: V) { v.pointer(\"/a/b\"); }\n",
            "fn f(r: R) { r.route(\"/api/x\", h); }\n",
            "fn f(p: &str) -> bool { p.ends_with(\"/tests/\") }\n",
            "fn f(p: &str) -> bool { p.contains(\"/seg\") }\n",
            "const A: &str = include_str!(\"/abs\");\n",
            "const A: &str = \"/u/x\"; // not a path: url route\n",
            "const A: char = '/';\n",
            "fn f(v: V) { v.pointer_mut(\"/a/b\"); }\n",
            "const A: &[u8] = b\"/u/x\"; // not a path: x\n",
            "const A: &CStr = c\"/u/x\"; // not a path: x\n",
            "fn f(p: &str) { p.starts_with(\"/s\"); p.find(\"/s/\"); p.rfind(\"/s\"); p.strip_prefix(\"/s\"); p.split_once(\"/s\"); p.rsplit_once(\"/s\"); p.replace(\"/s\", \"\"); }\n",
        ] {
            assert_eq!(run(src), Vec::<String>::new(), "{src}");
        }
        assert_eq!(run("fn f(p: &str) { p.strip_suffix(\"/src/a.rs\"); }\n").len(), 1);
    }

    #[test]
    fn a_marker_needs_a_reason_and_cannot_excuse_a_root() {
        for src in ["const A: &str = \"/u/x\"; // not a path:\n", "const A: &str = \"/u/x\"; // not a path:   \n", "const A: &str = \"/home/x\"; // not a path: url\n", "const A: &str = \"/proc/x\"; // not a path: url\n", "const A: &str = \"/nonexistent\"; // not a path: x\n", "const A: &str = \"/srv/x\"; // not a path: x\n", "const A: &str = \"@U/x\"; // not a path: x\n", "const A: &[u8] = b\"/work/x\"; // not a path: x\n"] {
            let got = run(src);
            assert_eq!(got.len(), 1, "{src}: {got:?}");
            assert!(got[0].contains("needs a reason and cannot excuse a filesystem root"), "{got:?}");
        }
    }

    #[test]
    fn only_the_system_paths_crate_may_name_a_real_path() {
        let t = TempDir::new("ctp-sys");
        let src = "pub const PROC: &str = \"/proc\";\n";
        t.write("system-paths/src/lib.rs", src);
        t.write("a/src/lib.rs", src);
        let tree = Tree::from_paths(t.path(), ["system-paths/src/lib.rs", "a/src/lib.rs"], std::iter::empty::<&str>());
        let got = CompileTimePath::default().check(&tree).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, "a/src/lib.rs");
    }

    fn pending(t: &TempDir, entries: &[(&str, &str)], files: &[&str]) -> Result<Vec<String>, LintError> {
        let mut all: Vec<String> = files.iter().map(|f| f.to_string()).collect();
        for (k, b) in entries {
            let rel = format!("{ALLOW_FILE}/{k}");
            t.write(&rel, &format!("{b}\n"));
            all.push(rel);
        }
        let tree = Tree::from_paths(t.path(), all.iter(), std::iter::empty::<&str>());
        CompileTimePath::default().check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_literal_in_a_pending_crate_is_clean_and_env_is_not() {
        let t = TempDir::new("ctp-pend");
        t.write("a/b/src/lib.rs", "const A: &str = \"/x/y\";\n");
        assert_eq!(pending(&t, &[("a/b", "sp-1")], &["a/b/src/lib.rs"]).unwrap(), Vec::<String>::new());
        t.write("a/b/src/lib.rs", "const A: &str = \"/x/y\";\nconst B: &str = env!(\"OUT_DIR\");\n");
        assert_eq!(pending(&t, &[("a/b", "sp-1")], &["a/b/src/lib.rs"]).unwrap().len(), 1);
    }

    #[test]
    fn a_pending_entry_with_no_literal_left_is_stale_and_a_bad_id_is_refused() {
        let t = TempDir::new("ctp-stale");
        t.write("a/src/lib.rs", "const A: &str = \"rel\";\n");
        let got = pending(&t, &[("a", "sp-1")], &["a/src/lib.rs"]).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("stale: delete it"), "{got:?}");
        assert!(pending(&t, &[("a", "nope")], &["a/src/lib.rs"]).is_err());
    }

    #[test]
    fn an_entry_the_base_did_not_have_is_a_growth_finding() {
        let t = TempDir::new("ctp-grow");
        let git = |args: &[&str]| {
            assert!(std::process::Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).current_dir(t.path()).status().unwrap().success());
        };
        git(&["init", "-q", "-b", "main"]);
        t.write("a/src/lib.rs", "const A: &str = \"/x/y\";\n");
        t.write("b/src/lib.rs", "const A: &str = \"/x/y\";\n");
        t.write(&format!("{ALLOW_FILE}/a"), "sp-1\n");
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "base"]);
        t.write(&format!("{ALLOW_FILE}/b"), "sp-1\n");
        let files = ["a/src/lib.rs", "b/src/lib.rs", "spira-lint/compile-time-path-allow.d/a", "spira-lint/compile-time-path-allow.d/b"];
        let tree = Tree::from_paths(t.path(), files, std::iter::empty::<&str>()).with_base(Some("main".into()));
        let got = CompileTimePath::default().check(&tree).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].message.contains("the list only shrinks") && got[0].path.ends_with("/b"), "{got:?}");
    }

    #[test]
    fn an_env_read_after_an_include_call_is_still_a_finding() {
        let got = run("const A: &str = include_str!(\"a.txt\");\nconst B: &str = env!(\"CARGO_MANIFEST_DIR\");\n");
        assert_eq!(got.len(), 1);
        assert!(got[0].contains(":2:"), "{got:?}");
    }

    #[test]
    fn manifest_dir_message_names_every_run_time_door() {
        let m = &run("fn f() { env!(\"CARGO_MANIFEST_DIR\"); }\n")[0];
        for door in ["locate_home_for_process()", "checkout_root_for_process()", "testkit::manifest_dir()"] {
            assert!(m.contains(door), "{m}");
        }
    }

    #[test]
    fn a_finding_deep_in_a_file_names_its_own_line() {
        let got = run("// c\nconst A: &str = include_bytes!(\"a\");\n\n// env!(\"OUT_DIR\")\nfn f() { env!(\"OUT_DIR\"); }\n");
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains(":5:"), "{got:?}");
    }

    #[test]
    fn an_unbalanced_include_does_not_exempt_the_rest_of_the_file() {
        let got = run("const A: &str = include_str!(\"a.txt\";\nconst B: &str = env!(\"CARGO_MANIFEST_DIR\");\n");
        assert_eq!(got.len(), 1, "{got:?}");
    }

    #[test]
    fn a_string_with_a_paren_inside_an_include_does_not_end_the_exemption() {
        assert_eq!(run("const A: &str = include_str!(concat!(\")\", env!(\"OUT_DIR\")));\n"), Vec::<String>::new());
    }

    #[test]
    fn own_source_and_target_are_not_scanned_and_checked_counts_the_rest() {
        let t = TempDir::new("ctp-scope");
        let bad = "fn f() { env!(\"OUT_DIR\"); }\n";
        for p in [OWN_SOURCE, "target/x.rs", "b/src/lib.rs"] {
            t.write(p, bad);
        }
        let tree = Tree::from_paths(t.path(), [OWN_SOURCE, "target/x.rs", "b/src/lib.rs"], std::iter::empty::<&str>());
        let rule = CompileTimePath::default();
        let got = rule.check(&tree).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(rule.checked(), Some((1, "rust files".to_string())));
    }

    #[test]
    fn no_rust_files_is_a_refusal() {
        let t = TempDir::new("ctp-empty");
        t.write("a.sh", "x\n");
        let tree = Tree::from_paths(t.path(), ["a.sh"], std::iter::empty::<&str>());
        assert_eq!(CompileTimePath::default().check(&tree).map(|_| ()), Err(LintError::EmptyScope));
    }
}
