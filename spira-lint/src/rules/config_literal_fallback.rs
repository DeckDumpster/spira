//! `config-literal-fallback` — no binary defaults a config value to a bare filesystem
//! path or a bare `SPIRA_*` identity/location value instead of resolving it through
//! `spira_config` (law-a-binary-resolves-the-config-it-reads). New (sp-ivfu3); no bash
//! fence precedes it. Contract: DESIGN.md.
//!
//! sp-ivfu3 found eight binaries reading `env SPIRA_RUN || "/tmp/spira"` — right under a
//! systemd unit (which always sets `SPIRA_RUN` itself), silently wrong from a bare
//! operator shell. This is the fourth such violation in one day
//! (law-a-binary-resolves-the-config-it-reads), so per the harness's own escalation
//! ladder it gets a MECHANISM, not another one-off fix: this rule, so the next copy of
//! the bug cannot land.

use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust;
use crate::rust_test::{shape, whole_test_files, Shape};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct ConfigLiteralFallback;

const NAME: &str = "config-literal-fallback";
const ALLOW_FILE: &str = "spira-lint/config-literal-fallback-allow";

/// `SPIRA_*` keys that name "where/who/which copy" — a bare, unresolved value here is
/// always wrong (never merely a command name on `$PATH`, which the generic `/`-shaped
/// check below already catches for everything else): the run directory, the checkout, the
/// instance, the chamber/fayth roster, the data store, the workspace root, the repo map,
/// and the config document's own location. A key NOT in this list (`SPIRA_SYSTEMCTL`,
/// `SPIRA_BD`, `SPIRA_GH`, `SPIRA_INCIDENT_SH`, `SPIRA_FORGE`, ...) is a PROGRAM NAME this
/// process shells out to, resolved via `$PATH` at exec time exactly like a bare `bash`/
/// `git` call would be — not a config value `spira_config` owns, so a short literal
/// default there (`"systemctl"`, `"bd"`) is not this defect. Such a key still trips this
/// rule the moment its own literal fallback is PATH-SHAPED (contains `/`): a command name
/// never has a `/` in it on this harness's own convention (every seam name here is bare).
const CONFIG_IDENTITY_KEYS: &[&str] = &[
    "SPIRA_RUN",
    "SPIRA_HOME",
    "SPIRA_REPO",
    "SPIRA_INSTANCE",
    "SPIRA_CHAMBER",
    "SPIRA_CHAMBER_OVERLAY",
    "SPIRA_FAYTHS",
    "SPIRA_DB",
    "SPIRA_WORKSPACES",
    "SPIRA_REPO_MAP",
    "SPIRA_TOML",
    "SPIRA_CONF",
    "SPIRA_HOME_REPO",
];

/// Passthrough combinators a chain may carry between the `env::var(...)` call and the
/// `unwrap_or*` this rule looks for — each one's own argument list is skipped (balanced),
/// never inspected, because none of them is the default this rule judges.
const PASSTHROUGH: &[&str] = &["ok", "map", "filter", "as_deref", "to_owned", "clone", "trim", "to_string"];

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

/// The index just past the bracket that closes the one opening at `open`, over code-only
/// bytes (brackets balanced on code, never on a literal's own content) — same rule
/// `config_fence.rs`'s own `nth_arg` already leans on.
fn close_of(code: &[u8], open: usize) -> Option<usize> {
    let (o, c) = match code[open] {
        b'(' => (b'(', b')'),
        b'[' => (b'[', b']'),
        b'{' => (b'{', b'}'),
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

/// One `.method(...)` read at `i` (which must sit on the `.`): the method's name, and the
/// index just past its argument list's close.
fn read_method<'a>(code: &'a [u8], i: usize) -> Option<(&'a [u8], usize)> {
    if code.get(i) != Some(&b'.') {
        return None;
    }
    let mut j = i + 1;
    let start = j;
    while j < code.len() && (code[j].is_ascii_alphanumeric() || code[j] == b'_') {
        j += 1;
    }
    if j == start {
        return None;
    }
    let name = &code[start..j];
    let j = skip_ws(code, j);
    if code.get(j) != Some(&b'(') {
        return None;
    }
    let end = close_of(code, j)?;
    Some((name, end))
}

/// A violation found at one `env::var`/`env::var_os` call: the `SPIRA_*` key, the method
/// that supplied the literal default (`unwrap_or`/`unwrap_or_else`), and the byte offset
/// of the whole call (for the finding's line).
struct Hit {
    at: usize,
    key: String,
}

/// Scan `code`/`text` (same file, `code` with every literal and comment blanked, `text`
/// with only comments blanked) from `start` (just past `env::var(...)`'s own closing paren)
/// for
/// the chain's next `unwrap_or`/`unwrap_or_else`/`unwrap_or_default`, skipping any run of
/// [`PASSTHROUGH`] combinators in between. Returns the literal default's own text (from
/// `text`, with surrounding whitespace trimmed) when the chain ends in `unwrap_or`/
/// `unwrap_or_else` — `unwrap_or_default` (the type's own default, never a guessed path)
/// and anything else (the chain does not continue, or continues some other way) read as
/// "no literal fallback here", not a finding.
fn literal_default_after<'t>(code: &[u8], text: &'t [u8], start: usize) -> Option<&'t [u8]> {
    let mut i = skip_ws(code, start);
    loop {
        let (name, end) = read_method(code, i)?;
        match name {
            b"unwrap_or_default" => return None,
            b"unwrap_or" | b"unwrap_or_else" => {
                // The call's own argument text, trimmed — `end` sits just past the `)`
                // read_method found; its matching `(` is the first non-ws byte after the
                // method name, found again here only to slice the argument out of `text`.
                let open = code[i..end].iter().position(|&b| b == b'(')? + i;
                let arg = text[open + 1..end - 1].trim_ascii();
                return Some(arg);
            }
            n if PASSTHROUGH.iter().any(|p| p.as_bytes() == n) => {
                i = skip_ws(code, end);
                continue;
            }
            _ => return None,
        }
    }
}

/// Every `env::var`/`env::var_os` call in `code`/`text` whose first argument is a
/// `"SPIRA_*"` string literal, chained to a literal `unwrap_or`/`unwrap_or_else` default
/// this rule judges a violation: the key is one of [`CONFIG_IDENTITY_KEYS`], or the
/// default literal itself is path-shaped (contains `/`).
fn scan(src: &[u8]) -> Vec<Hit> {
    static CALL: OnceLock<Regex> = OnceLock::new();
    let cl = rust::classify(src);
    let code = cl.code_only();
    let text = cl.without_comments();
    let call = re(&CALL, r#"(?:^|[^A-Za-z0-9_:])(?:std::)?env::var(?:_os)?\s*\(\s*"(SPIRA_[A-Z0-9_]*)"\s*\)"#);
    let mut out = Vec::new();
    for c in call.captures_iter(&text) {
        let whole = c.get(0).unwrap();
        let key = String::from_utf8_lossy(&c[1]).into_owned();
        let Some(default) = literal_default_after(&code, &text, whole.end()) else { continue };
        // `unwrap_or_else`'s argument is a closure, so the literal default usually sits
        // nested inside it (`|_| PathBuf::from("...")`, `|| "...".to_string()`), not as
        // the bare top-level argument `unwrap_or` alone could take — a quote ANYWHERE in
        // the default's own text is enough to call it a literal; a default built purely
        // from an identifier, field or call with no literal in it at all has none.
        let is_literal =
            default.contains(&b'"') || (!default.is_empty() && default.iter().all(|b| b.is_ascii_digit()));
        if !is_literal {
            continue; // the default is an identifier/expression, not itself a literal
        }
        let path_shaped = default.contains(&b'/');
        if CONFIG_IDENTITY_KEYS.contains(&key.as_str()) || path_shaped {
            out.push(Hit { at: whole.start(), key });
        }
    }
    out
}

/// Also: a bare `"/tmp/spira"` string literal anywhere in code (not a comment, not a
/// longer path that merely starts with it — `/tmp/spira-run`, `/tmp/spira/sub` do not
/// match, matching the bead's own "a /tmp/spira literal", not a prefix test).
fn tmp_spira_literal_hits(src: &[u8]) -> Vec<usize> {
    static LIT: OnceLock<Regex> = OnceLock::new();
    let cl = rust::classify(src);
    let text = cl.without_comments();
    let lit = re(&LIT, r#""/tmp/spira""#);
    lit.find_iter(&text).map(|m| m.start()).collect()
}

impl Rule for ConfigLiteralFallback {
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
        let shapes: Vec<(&Entry, Shape)> =
            files.iter().filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();
        let whole = whole_test_files(tree, &shapes);

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

        let mut out = Vec::new();
        let mut offenders = std::collections::BTreeSet::new();
        for (e, s) in &shapes {
            if e.path == "spira-lint/src/rules/config_literal_fallback.rs" {
                continue; // this rule's own source names every string it hunts
            }
            let Some(src) = tree.content(e) else { continue };
            let cl = rust::classify(src);
            let file_is_test = whole.contains(&e.path);
            let in_test = |at: usize| file_is_test || s.covers(at);

            let mut hits: Vec<(usize, String)> = Vec::new();
            for h in scan(src) {
                if !in_test(h.at) {
                    hits.push((h.at, format!("env::var(\"{}\").unwrap_or* with a literal default — resolve it through spira_config instead", h.key)));
                }
            }
            for at in tmp_spira_literal_hits(src) {
                if !in_test(at) {
                    hits.push((at, "\"/tmp/spira\" literal — resolve spira.run through spira_config instead, never a guessed path".to_string()));
                }
            }
            if hits.is_empty() {
                continue;
            }
            hits.sort_by_key(|(at, _)| *at);
            offenders.insert(e.path.clone());
            if allow.iter().any(|(_, p)| p == &e.path) {
                continue;
            }
            for (at, msg) in hits {
                out.push(Finding { rule: NAME, path: e.path.clone(), line: Some(cl.line_of(at)), message: msg });
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
        "A binary resolves the config it reads (law-a-binary-resolves-the-config-it-reads, sp-ivfu3): \
call spira_config::resolve::resolve_for_process (or the resolve_run_dir/resolve_instance wrappers) \
in-process, with the environment variable as an override only, and REFUSE, named, when spira_config \
itself cannot resolve — never fall back to a literal path or identity value. \
spira-lint/config-literal-fallback-allow only shrinks."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        ConfigLiteralFallback.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn catches_the_tmp_spira_literal_and_the_spira_run_fallback() {
        let t = TempDir::new("clf-fail");
        t.write(ALLOW_FILE, "# shrink-only\n");
        t.write(
            "a/src/main.rs",
            "fn spira_run() -> std::path::PathBuf {\n    \
             std::env::var(\"SPIRA_RUN\").map(std::path::PathBuf::from).unwrap_or_else(|_| std::path::PathBuf::from(\"/tmp/spira\"))\n}\n",
        );
        let got = run(&t, &["a/src/main.rs"]).unwrap();
        assert_eq!(
            got,
            vec![
                "config-literal-fallback: a/src/main.rs:2: env::var(\"SPIRA_RUN\").unwrap_or* with a literal default — resolve it through spira_config instead",
                "config-literal-fallback: a/src/main.rs:2: \"/tmp/spira\" literal — resolve spira.run through spira_config instead, never a guessed path",
            ]
        );
    }

    #[test]
    fn catches_a_non_path_identity_default_and_a_path_shaped_default_on_any_key() {
        let t = TempDir::new("clf-fail2");
        t.write(ALLOW_FILE, "");
        t.write(
            "b/src/main.rs",
            "fn f() {\n    \
             let a = env::var(\"SPIRA_INSTANCE\").unwrap_or_else(|_| \"prod\".to_string());\n    \
             let b = env::var(\"SPIRA_MEMORIES_CACHE\").unwrap_or_else(|_| \"/some/path\".to_string());\n}\n",
        );
        let got = run(&t, &["b/src/main.rs"]).unwrap();
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].contains(":2:") && got[0].contains("SPIRA_INSTANCE"));
        assert!(got[1].contains(":3:") && got[1].contains("SPIRA_MEMORIES_CACHE"));
    }

    #[test]
    fn a_bare_command_name_default_on_a_non_identity_key_is_not_flagged() {
        let t = TempDir::new("clf-pass-cmd");
        t.write(ALLOW_FILE, "");
        t.write(
            "c/src/main.rs",
            "fn f() -> String {\n    env::var(\"SPIRA_SYSTEMCTL\").unwrap_or_else(|_| \"systemctl\".to_string())\n}\n",
        );
        assert!(run(&t, &["c/src/main.rs"]).unwrap().is_empty());
    }

    #[test]
    fn unwrap_or_default_is_never_a_finding() {
        let t = TempDir::new("clf-pass-default");
        t.write(ALLOW_FILE, "");
        t.write("d/src/main.rs", "fn f() -> String {\n    env::var(\"SPIRA_DB\").unwrap_or_default()\n}\n");
        assert!(run(&t, &["d/src/main.rs"]).unwrap().is_empty());
    }

    #[test]
    fn a_longer_path_that_merely_starts_with_tmp_spira_is_not_a_prefix_match() {
        let t = TempDir::new("clf-pass-prefix");
        t.write(ALLOW_FILE, "");
        t.write("e/src/main.rs", "const P: &str = \"/tmp/spira-run\";\n");
        assert!(run(&t, &["e/src/main.rs"]).unwrap().is_empty());
    }

    #[test]
    fn test_code_is_exempt_wherever_it_lives() {
        let t = TempDir::new("clf-pass-test");
        t.write(ALLOW_FILE, "");
        t.write(
            "f/src/lib.rs",
            "pub fn g() {}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        \
             let _ = std::env::var(\"SPIRA_RUN\").unwrap_or_else(|_| \"/tmp/spira\".to_string());\n    }\n}\n",
        );
        t.write(
            "f/tests/it.rs",
            "#[test]\nfn t() {\n    let _ = std::env::var(\"SPIRA_RUN\").unwrap_or_else(|_| \"/tmp/spira\".to_string());\n}\n",
        );
        assert!(run(&t, &["f/src/lib.rs", "f/tests/it.rs"]).unwrap().is_empty());
    }

    #[test]
    fn comments_and_strings_naming_the_pattern_are_not_findings() {
        let t = TempDir::new("clf-pass-comment");
        t.write(ALLOW_FILE, "");
        t.write(
            "g/src/main.rs",
            "// env::var(\"SPIRA_RUN\").unwrap_or_else(|_| \"/tmp/spira\".to_string())\nfn f() {}\n",
        );
        assert!(run(&t, &["g/src/main.rs"]).unwrap().is_empty());
    }

    #[test]
    fn allow_listed_file_is_skipped_and_a_stale_entry_is_refused() {
        let t = TempDir::new("clf-allow");
        t.write(
            "h/src/main.rs",
            "fn f() -> String { env::var(\"SPIRA_RUN\").unwrap_or_else(|_| \"/tmp/spira\".to_string()) }\n",
        );
        t.write("h/src/clean.rs", "fn f() {}\n");
        t.write(ALLOW_FILE, "# header\nh/src/main.rs\nh/src/clean.rs\n");
        let got = run(&t, &["h/src/main.rs", "h/src/clean.rs"]).unwrap();
        assert_eq!(
            got,
            vec!["config-literal-fallback: spira-lint/config-literal-fallback-allow:3: lists h/src/clean.rs, which no longer needs an exception — remove the line (the list only shrinks)"]
        );
    }

    #[test]
    fn no_rust_files_is_a_refusal() {
        let t = TempDir::new("clf-empty");
        assert_eq!(run(&t, &["README"]), Err(LintError::EmptyScope));
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(ConfigLiteralFallback),
    ]
}
