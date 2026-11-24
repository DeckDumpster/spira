//! `script-callers` — a caller that invokes, sources or names a `.sh`/`.py` script absent
//! from the tree fails the branch that deleted or renamed it, not the next one to touch it
//! (sp-9y0gf). Contract: DESIGN.md.
//!
//! Widened to Rust string literals, and to unit templates and the watchers manifest, after
//! sp-yv4b3 re-violated the day sp-9y0gf itself was filed: queue-watch, batcher-cut and
//! czar-pass each defaulted `SPIRA_FORGE` to the retired `forge.sh`, and nothing at the gate
//! had read a Rust source for the name of a script.

use crate::SyncCell as Cell;
use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;

use crate::lex::rust::{self, Class};
use crate::rust_test::{shape, whole_test_files, Shape};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct ScriptCallers {
    checked: Cell<Option<usize>>,
}

impl Default for ScriptCallers {
    fn default() -> Self {
        ScriptCallers { checked: Cell::new(None) }
    }
}

const NAME: &str = "script-callers";
/// Exact repo-relative paths, shrink-only — the `fence-scripts`/`tmp-leak` pattern: a file
/// already carrying this defect at authorship is listed rather than blocking every branch
/// on debt this change did not add; a listed path with no findings left is itself a
/// finding, so the list stays exact.
const ALLOW_FILE: &str = "spira-lint/script-callers-allow";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

/// `@SOME_VAR@/x.sh` — the unit-template and watchers-manifest placeholder form.
fn placeholder_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    re(&RE, r"@([A-Z_]+)@/([A-Za-z0-9_./-]+\.(?:sh|py))")
}

/// A bare `name.sh`/`name.py` token at a word boundary (line start, whitespace or `|`),
/// for the watchers manifest's `target|health` columns and plain shell/Rust bare names.
fn bare_token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    re(&RE, r"(?:^|[\s|])([A-Za-z0-9_-]+\.(?:sh|py))\b")
}

/// A literal `*.sh`/`*.py` argument to `PathBuf::from`, `Path::new` or `Command::new` — the
/// three constructs that make a literal a path or a spawned program, not a log line.
/// `sp-yv4b3`'s shape (`unwrap_or_else(|| PathBuf::from("forge.sh"))`) and the production
/// fleet-control defect this rule's own run turned up (`Command::new("slay.sh")`) are both
/// this shape; a log message naming the old script for an operator's benefit
/// (`"escape.sh {rune}: …"`) and a `queue.sh …` usage string are not, and a literal-text
/// scan with no such anchor flagged both — over a thousand hits, nearly all of them this.
fn literal_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    re(&RE, r#"(?:PathBuf::from|Path::new|Command::new)\s*\(\s*"([A-Za-z0-9_./-]*\.(?:sh|py))""#)
}

/// A placeholder's base directory in this tree, or `None` for one this rule does not know
/// how to resolve — never a guess (law-detection-outranks-rejection cuts the other way: an
/// unowned guess is a worse failure mode than a known gap).
fn placeholder_base(ph: &str) -> Option<&'static str> {
    match ph {
        "SPIRA_HOME" | "SPIRA_PROD" => Some("spira"),
        "SPIRA_PROD_ROOT" => Some(""),
        "SPIRA_COCKPIT" | "SPIRA_PROD_COCK" => Some("cockpit"),
        _ => None,
    }
}

fn join(base: &str, suffix: &str) -> String {
    let raw = if base.is_empty() { suffix.to_string() } else { format!("{base}/{suffix}") };
    normalize_path(&raw)
}

/// Collapse `a/b/../c` to `a/c` and drop `.`/empty segments — `$HERE/../install.sh` from a
/// suite in `spira/` names the repo-root `install.sh`, not a literal `spira/../install.sh`
/// that cannot exist under any root.
fn normalize_path(path: &str) -> String {
    let mut stack: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            s => stack.push(s),
        }
    }
    stack.join("/")
}

fn is_unit(path: &str) -> bool {
    path.starts_with("systemd/") && (path.ends_with(".service") || path.ends_with(".timer"))
}

const WATCHERS: &str = "spira/watchers";

// ───────────────────────────── Rust string literals ─────────────────────────────

/// Every `.sh`/`.py`-named literal argument to `PathBuf::from`/`Path::new`/`Command::new`
/// in `src`, as (byte offset of the literal text, the text) — only where the capture itself
/// is bytes the lexer classified as a literal (defensive; the regex already requires a `"`).
fn literal_candidates(cl: &rust::Classified) -> Vec<(usize, String)> {
    let text = cl.without_comments();
    let s = String::from_utf8_lossy(&text);
    let mut out = Vec::new();
    for c in literal_re().captures_iter(&s) {
        let m = c.get(1).unwrap();
        if cl.class.get(m.start()) == Some(&Class::Literal) {
            out.push((m.start(), m.as_str().to_string()));
        }
    }
    out
}

/// A literal path's tree-relative resolution: a slash-bearing literal is read as the exact
/// path the author wrote (`spira/forge.sh`); a bare literal resolves under `spira/`, where
/// the release's own scripts live.
fn resolve_literal(text: &str) -> String {
    if text.contains('/') {
        normalize_path(text)
    } else {
        normalize_path(&format!("spira/{text}"))
    }
}

/// A production write of the named file in this same source — `fs::write`/`File::create*`/
/// `fs::rename`/`fs::copy` with the basename anywhere in its call — the "a fixture that
/// creates the file is exempt" clause, read for non-test code (test code is excluded from
/// this scan entirely, upstream).
fn rust_creates(code_and_literals: &[u8], basename: &str) -> bool {
    static CALL: OnceLock<Regex> = OnceLock::new();
    let call = re(&CALL, r"(?:fs::write|File::create_new|File::create|fs::rename|fs::copy)\s*\(");
    let text = String::from_utf8_lossy(code_and_literals);
    for m in call.find_iter(&text) {
        let window_end = (m.end() + 200).min(text.len());
        if text[m.end()..window_end].contains(basename) {
            return true;
        }
    }
    false
}

fn check_rust(tree: &Tree, e: &Entry, src: &[u8], shape: &Shape) -> Vec<Finding> {
    let cl = rust::classify(src);
    let candidates = literal_candidates(&cl);
    if candidates.is_empty() {
        return Vec::new();
    }
    let without_comments = cl.without_comments();
    let mut out = Vec::new();
    let mut reported: BTreeSet<(usize, String)> = BTreeSet::new();
    for (at, text) in candidates {
        if shape.covers(at) {
            continue; // a #[cfg(test)] region of an otherwise-production file
        }
        let path = resolve_literal(&text);
        if tree.entry(&path).is_some() {
            continue;
        }
        let basename = path.rsplit('/').next().unwrap_or(&path);
        if rust_creates(&without_comments, basename) {
            continue;
        }
        let line = cl.line_of(at);
        if reported.insert((line, path.clone())) {
            out.push(Finding {
                rule: NAME,
                path: e.path.clone(),
                line: Some(line),
                message: format!("names {path}, which is not in the tree"),
            });
        }
    }
    out
}

// ───────────────────────── unit templates & the watchers manifest ─────────────────────────

/// Lines whose first non-blank character opens a comment (`#` for both formats; `;` for
/// systemd ini) blanked, so neither prose nor the watchers manifest's extensive header
/// comments are scanned as data.
fn strip_comments(text: &str) -> String {
    text.lines()
        .map(|l| if matches!(l.trim_start().as_bytes().first(), Some(b'#') | Some(b';')) { "" } else { l })
        .collect::<Vec<_>>()
        .join("\n")
}

fn check_text_paths(tree: &Tree, e: &Entry, text: &str) -> Vec<Finding> {
    let stripped = strip_comments(text);
    let mut out = Vec::new();
    let mut reported: BTreeSet<(usize, String)> = BTreeSet::new();
    for (i, line) in stripped.lines().enumerate() {
        let lineno = i + 1;
        for c in placeholder_re().captures_iter(line) {
            let Some(base) = placeholder_base(&c[1]) else { continue };
            let path = join(base, &c[2]);
            if tree.entry(&path).is_none() && reported.insert((lineno, path.clone())) {
                out.push(Finding {
                    rule: NAME,
                    path: e.path.clone(),
                    line: Some(lineno),
                    message: format!("names {path}, which is not in the tree"),
                });
            }
        }
        for c in bare_token_re().captures_iter(line) {
            let path = normalize_path(&format!("spira/{}", &c[1]));
            if tree.entry(&path).is_none() && reported.insert((lineno, path.clone())) {
                out.push(Finding {
                    rule: NAME,
                    path: e.path.clone(),
                    line: Some(lineno),
                    message: format!("names {path}, which is not in the tree"),
                });
            }
        }
    }
    out
}

// ───────────────────────────────────── the rule ─────────────────────────────────────

impl Rule for ScriptCallers {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        (e.path.ends_with(".rs") && !e.path.starts_with("target/"))
            || is_unit(&e.path)
            || e.path == WATCHERS
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        self.checked.set(None);
        let files = crate::scope(tree, self)?;
        let allow: Vec<String> = crate::allow_lines(&tree.read_text(ALLOW_FILE));
        let mut raw = Vec::new();
        let mut checked = 0usize;

        let rust_files: Vec<&Entry> = files.iter().copied().filter(|e| e.path.ends_with(".rs")).collect();
        let shapes: Vec<(&Entry, Shape)> =
            rust_files.iter().filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();
        let whole = whole_test_files(tree, &shapes);
        for (e, s) in &shapes {
            if whole.contains(&e.path) {
                continue; // test code: fixture literals are not tree references
            }
            let Some(src) = tree.content(e) else { continue };
            checked += 1;
            raw.extend(check_rust(tree, e, src, s));
        }

        for e in files.iter().filter(|e| is_unit(&e.path) || e.path == WATCHERS) {
            let Some(c) = tree.content(e) else { continue };
            checked += 1;
            raw.extend(check_text_paths(tree, e, &String::from_utf8_lossy(c)));
        }

        let offenders: BTreeSet<String> = raw.iter().map(|f| f.path.clone()).collect();
        let mut out: Vec<Finding> = raw.into_iter().filter(|f| !allow.iter().any(|p| p == &f.path)).collect();
        for p in &allow {
            if !offenders.contains(p.as_str()) {
                out.push(Finding {
                    rule: NAME,
                    path: ALLOW_FILE.to_string(),
                    line: None,
                    message: format!("lists {p}, which no longer needs an exception — remove the line (the list only shrinks)"),
                });
            }
        }

        if out.is_empty() {
            self.checked.set(Some(checked));
        }
        Ok(out)
    }

    fn checked(&self) -> Option<(usize, String)> {
        self.checked.get().map(|n| (n, "callers".to_string()))
    }

    fn hint(&self) -> &'static str {
        "A rewrite that deletes or renames a script leaves its callers pointed at nothing — \
law-a-rename-repoints-no-reader. Repoint every caller this names (grep the old name for \
any this fence missed) in the SAME branch that deletes or renames the script: a Rust string literal, a systemd unit's @PLACEHOLDER@ target, or a spira/watchers row. Pre-existing debt goes in spira-lint/script-callers-allow \
(shrink-only, by file) with the fix named in a comment, not silently — never add an entry \
for a file this change itself breaks."
    }
}


pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(ScriptCallers::default()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, tracked: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), tracked.iter().copied(), std::iter::empty::<&str>());
        ScriptCallers::default().check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test_id(tw9h380)]
    #[test]
    fn a_production_default_naming_a_deleted_script_is_caught() {
        let t = TempDir::new("sc-rust");
        // Assembled so this test file's own source carries no literal spawn.
        let planted = format!("{}{}", "no-such-scrip", "t.sh");
        t.write(
            "batcher-cut/src/main.rs",
            &format!("fn default_forge() -> PathBuf {{ PathBuf::from(\"{planted}\") }}\n"),
        );
        let got = run(&t, &["batcher-cut/src/main.rs"]).unwrap();
        assert_eq!(got, vec![format!("script-callers: batcher-cut/src/main.rs:1: names spira/{planted}, which is not in the tree")]);
    }

    #[test_id(t02mjzk)]
    #[test]
    fn a_command_new_spawn_naming_a_deleted_script_is_caught() {
        // The real production hit this rule's own first run turned up: spira-world's
        // `world stop`/`drain --deadline` still spawned the retired slay.sh (sp-6onps left
        // one caller unrepointed; the suite-side cp/stub pattern this rule's own tests
        // exercise elsewhere is exactly why no suite caught it — none is wired to it yet).
        let t = TempDir::new("sc-rust-spawn");
        let planted = concat!("no-such-slay", ".sh");
        t.write("spira-world/src/bin/world.rs", &format!("fn f() {{ Command::new(\"{planted}\"); }}\n"));
        let got = run(&t, &["spira-world/src/bin/world.rs"]).unwrap();
        assert_eq!(got, vec![format!("script-callers: spira-world/src/bin/world.rs:1: names spira/{planted}, which is not in the tree")]);
    }

    #[test_id(tkz3kuh)]
    #[test]
    fn test_code_literals_are_not_scanned() {
        let t = TempDir::new("sc-rust-test");
        let planted = concat!("no-such-fixture", ".sh");
        t.write(
            "queue/src/lib.rs",
            &format!("pub fn f() {{}}\n#[cfg(test)]\nmod tests {{\n    fn t() {{ let _ = PathBuf::from(\"{planted}\"); }}\n}}\n"),
        );
        t.write("queue/tests/it.rs", &format!("fn t() {{ let _ = Path::new(\"{planted}\"); }}\n"));
        assert!(run(&t, &["queue/src/lib.rs", "queue/tests/it.rs"]).unwrap().is_empty());
    }

    #[test_id(t3w0z0p)]
    #[test]
    fn a_comment_mentioning_the_construct_is_not_a_literal() {
        let t = TempDir::new("sc-rust-comment");
        let planted = concat!("no-such-thing", ".sh");
        t.write("gate/src/x.rs", &format!("// Command::new(\"{planted}\") was retired\nfn f() {{}}\n")); // comment, not code
        assert!(run(&t, &["gate/src/x.rs"]).unwrap().is_empty());
    }

    #[test_id(t8p16yl)]
    #[test]
    fn a_log_message_or_usage_string_is_not_a_literal() {
        // Exactly what flooded the first draft of this rule: a log line or --help text
        // naming the script's pre-rewrite name for an operator's benefit, not a path.
        let t = TempDir::new("sc-rust-log");
        t.write(
            "dwarf/src/escape.rs",
            "fn f(rune: &str) -> String { format!(\"escape.sh {rune}: nothing ready\") }\nconst USAGE: &str = \"usage: queue.sh submit\";\n",
        );
        assert!(run(&t, &["dwarf/src/escape.rs"]).unwrap().is_empty());
    }

    #[test_id(ttj15x5)]
    #[test]
    fn a_rust_file_that_writes_the_script_before_naming_it_is_exempt() {
        let t = TempDir::new("sc-rust-creates");
        let planted = concat!("rendered-helper", ".sh");
        t.write(
            "install/src/lib.rs",
            &format!("fn render() {{ fs::write(dir.join(\"{planted}\"), body).unwrap(); }}\nfn run() {{ Command::new(\"{planted}\"); }}\n"),
        );
        assert!(run(&t, &["install/src/lib.rs"]).unwrap().is_empty());
    }

    // ── unit templates & the watchers manifest ──

    #[test_id(t0ft5ei)]
    #[test]
    fn a_unit_s_placeholder_target_naming_a_deleted_script_is_caught() {
        let t = TempDir::new("sc-unit");
        t.write(
            "systemd/spira-planted.service",
            "[Service]\nExecStart=@SPIRA_HOME@/gone.sh check\n",
        );
        let got = run(&t, &["systemd/spira-planted.service"]).unwrap();
        assert_eq!(got, vec!["script-callers: systemd/spira-planted.service:2: names spira/gone.sh, which is not in the tree"]);
    }

    #[test_id(tnuhy62)]
    #[test]
    fn a_unit_s_comment_is_not_scanned() {
        let t = TempDir::new("sc-unit-comment");
        t.write("systemd/spira-ok.service", "# Documentation=file://@SPIRA_HOME@/gone.sh\n[Service]\nExecStart=sentinel\n");
        assert!(run(&t, &["systemd/spira-ok.service"]).unwrap().is_empty());
    }

    #[test_id(t59aj2v)]
    #[test]
    fn a_watchers_row_naming_a_deleted_bare_script_is_caught() {
        let t = TempDir::new("sc-watchers");
        t.write("spira/watchers", "# header comment, not data\nplanted|daemon|planted-watch.sh watch|planted-watch.sh health\n");
        let got = run(&t, &["spira/watchers"]).unwrap();
        assert_eq!(
            got,
            vec!["script-callers: spira/watchers:2: names spira/planted-watch.sh, which is not in the tree"]
        );
    }

    #[test_id(toyszvg)]
    #[test]
    fn an_unknown_placeholder_is_not_guessed_at() {
        let t = TempDir::new("sc-unknown-ph");
        t.write("systemd/spira-x.service", "[Service]\nExecStart=@SOME_FUTURE_VAR@/x.sh\n");
        assert!(run(&t, &["systemd/spira-x.service"]).unwrap().is_empty());
    }

    // ── scope, refusal, positive control ──

    #[test_id(tst81gx)]
    #[test]
    fn empty_scope_refuses() {
        let t = TempDir::new("sc-empty");
        assert_eq!(run(&t, &["README"]), Err(LintError::EmptyScope));
    }

    #[test_id(thcnwxd)]
    #[test]
    fn a_clean_tree_reports_checked() {
        let t = TempDir::new("sc-checked");
        t.write("spira/watchers", "# header comment, not data\n");
        let tree = Tree::from_paths(t.path(), ["spira/watchers"], std::iter::empty::<&str>());
        let r = ScriptCallers::default();
        assert_eq!(r.check(&tree), Ok(vec![]));
        assert_eq!(r.checked(), Some((1, "callers".to_string())));
    }

    #[test_id(ti9h962)]
    #[test]
    fn a_listed_file_is_silenced_and_a_stale_entry_is_refused() {
        let t = TempDir::new("sc-allow");
        t.write("spira/watchers", "planted|daemon|planted-watch.sh watch|planted-watch.sh health\n");
        t.write(ALLOW_FILE, "spira/watchers\n");
        assert!(run(&t, &["spira/watchers"]).unwrap().is_empty());
        t.write("spira/watchers", "# fixed\n");
        assert_eq!(
            run(&t, &["spira/watchers"]).unwrap(),
            vec!["script-callers: spira-lint/script-callers-allow: lists spira/watchers, which no longer needs an exception — remove the line (the list only shrinks)"]
        );
    }
}
