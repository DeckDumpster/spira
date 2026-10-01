//! `script-callers` — a caller that invokes, sources or names a `.sh`/`.py` script absent
//! from the tree fails the branch that deleted or renamed it, not the next one to touch it
//! (sp-9y0gf). Contract: DESIGN.md.
//!
//! Widened to Rust string literals, and to unit templates and the watchers manifest, after
//! sp-yv4b3 re-violated the day sp-9y0gf itself was filed: queue-watch, batcher-cut and
//! czar-pass each defaulted `SPIRA_FORGE` to the retired `forge.sh`, and nothing at the gate
//! had read a Rust source for the name of a script.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use regex::Regex;

use crate::lex::rust::{self, Class};
use crate::lex::shell::{self, Command, Word};
use crate::rules::payload_argv;
use crate::rust_test::{shape, whole_test_files, Shape};
use crate::{direct_child, Entry, Finding, LintError, Rule, Tree};

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

/// Interpreter/loader words after which the NEXT word is the thing actually run.
const INTERP: &[&str] = &["bash", "sh", "python3", "python", "source", "."];
/// Commands (by basename) whose last-ish argument is a file they write — the "the suite
/// made this script itself" exemption.
const CREATORS: &[&str] = &["cp", "install", "ln", "tee", "write_exe"];
/// Pure pass-through placeholders `env -i … "${@}" "$REAL" args` leaves in front of the
/// real command; skipped before reading argv[0].
const PASSTHROUGH: &[&str] = &["$@", "${@}", "$*", "${*}"];

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

/// `$SH/x.sh`, `${HERE}/sub/x.py`, `$SPIRA_HOME/x.sh` — anywhere in a word's raw text.
/// Capture 1 is the suffix after the variable and its slash.
fn sh_path_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    re(&RE, r"\$\{?(?:SH|HERE|SPIRA_HOME)\}?/([A-Za-z0-9_./-]+\.(?:sh|py))")
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
/// (`"escape.sh {fayth}: …"`) and a `queue.sh …` usage string are not, and a literal-text
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

/// The basename (no directory) of a word's unquoted text.
fn basename_of(w: &Word) -> String {
    let u = w.unquoted();
    u.rsplit('/').next().unwrap_or(&u).to_string()
}

/// `spira/test-*.sh` directly in `spira/`.
fn is_suite(path: &str) -> bool {
    direct_child(path, "spira").is_some_and(|b| b.starts_with("test-") && b.ends_with(".sh"))
}

fn is_unit(path: &str) -> bool {
    path.starts_with("systemd/") && (path.ends_with(".service") || path.ends_with(".timer"))
}

const WATCHERS: &str = "spira/watchers";

// ───────────────────────────── suites: shell callers ─────────────────────────────

/// `NAME=value` assignments in `cmds` whose value is a bare (no `/`) literal `*.sh`/`*.py`
/// name — the suite's own `X=foo.sh` idiom, read so a later bare `"$X"` resolves to `foo.sh`.
fn script_assignments(cmds: &[Command]) -> BTreeMap<String, String> {
    static BARE: OnceLock<Regex> = OnceLock::new();
    let bare = re(&BARE, r"^[A-Za-z0-9_.-]+\.(?:sh|py)$");
    let mut m = BTreeMap::new();
    for cmd in cmds {
        let (env, _) = cmd.split_env();
        for w in env {
            let Some(name) = w.assignment_name() else { continue };
            if !w.vars.is_empty() {
                continue; // the value itself expands something — not a literal
            }
            let Some(eq) = w.raw.find('=') else { continue };
            let value = w.raw[eq + 1..].trim_matches(['"', '\'']);
            if bare.is_match(value) {
                m.insert(name.to_string(), value.to_string());
            }
        }
    }
    m
}

/// Resolve one word to the script path it names, if it names one at all: a direct
/// `$SH/$HERE/$SPIRA_HOME` path, or a bare variable this file assigned a bare script name.
fn resolve_word(w: &Word, assigns: &BTreeMap<String, String>) -> Option<String> {
    if let Some(c) = sh_path_re().captures(&w.raw) {
        return Some(normalize_path(&format!("spira/{}", &c[1])));
    }
    if w.vars.len() == 1 {
        let name = &w.vars[0];
        let u = w.unquoted();
        if u == format!("${name}") || u == format!("${{{name}}}") {
            if let Some(script) = assigns.get(name) {
                return Some(normalize_path(&format!("spira/{script}")));
            }
        }
    }
    None
}

/// The resolved basename a word names, by the same rules as [`resolve_word`] but falling
/// back to the word's own literal basename — for recognising the suite's own creation of
/// the very file it goes on to invoke.
fn named_basename(w: &Word, assigns: &BTreeMap<String, String>) -> String {
    if let Some(p) = resolve_word(w, assigns) {
        return p.rsplit('/').next().unwrap_or(&p).to_string();
    }
    basename_of(w)
}

/// The word(s) one command actually invokes: argv[0] after unwrapping `env`/`command`/
/// `exec`/`nohup` and any leading pass-through placeholder, plus argv[1] when argv[0] is an
/// interpreter/loader (`bash`, `source`, `.`, …) — the word interpreted, not the word run.
fn command_targets(cmd: &Command) -> Vec<&Word> {
    let (_, argv) = payload_argv::resolve(cmd);
    let mut argv: &[&Word] = &argv;
    while argv.first().is_some_and(|w| PASSTHROUGH.contains(&w.unquoted().as_str())) {
        argv = &argv[1..];
    }
    let Some(head) = argv.first() else { return Vec::new() };
    let mut out = vec![*head];
    if INTERP.contains(&basename_of(head).as_str()) {
        if let Some(next) = argv.get(1) {
            out.push(*next);
        }
    }
    out
}

/// Does this file create `basename` itself before (or after — order is not load-bearing;
/// a suite that writes it unconditionally earlier still counts) invoking it? `cat …>`,
/// `printf …>`, any redirect, `cp`/`install`/`ln`/`tee`'s destination, or a call to a
/// `write_exe` helper — the shapes the deliver-brief names.
fn creates(cmds: &[Command], basename: &str, assigns: &BTreeMap<String, String>) -> bool {
    let names_it = |w: &Word| named_basename(w, assigns) == basename;
    for cmd in cmds {
        for r in &cmd.redirects {
            if matches!(r.op.as_str(), ">" | ">>" | ">|" | "<>" | "&>" | "&>>") {
                if r.target.as_ref().is_some_and(names_it) {
                    return true;
                }
            }
        }
        let (_, argv) = cmd.split_env();
        let Some(head) = argv.first() else { continue };
        if CREATORS.contains(&basename_of(head).as_str()) && argv[1..].iter().any(|w| names_it(w)) {
            return true;
        }
    }
    false
}

fn check_suite(tree: &Tree, e: &Entry, content: &[u8]) -> Vec<Finding> {
    let cmds = shell::parse(content);
    let assigns = script_assignments(&cmds);
    let mut out = Vec::new();
    let mut reported: BTreeSet<(usize, String)> = BTreeSet::new();
    for cmd in &cmds {
        for w in command_targets(cmd) {
            let Some(path) = resolve_word(w, &assigns) else { continue };
            if tree.entry(&path).is_some() {
                continue;
            }
            let basename = path.rsplit('/').next().unwrap_or(&path);
            if creates(&cmds, basename, &assigns) {
                continue;
            }
            if reported.insert((w.line, path.clone())) {
                out.push(Finding {
                    rule: NAME,
                    path: e.path.clone(),
                    line: Some(w.line),
                    message: format!("invokes {path}, which is not in the tree and is not written by this suite"),
                });
            }
        }
    }
    out
}

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
        is_suite(&e.path)
            || (e.path.ends_with(".rs") && !e.path.starts_with("target/"))
            || is_unit(&e.path)
            || e.path == WATCHERS
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        self.checked.set(None);
        let files = crate::scope(tree, self)?;
        let allow: Vec<String> = crate::allow_lines(&tree.read_text(ALLOW_FILE));
        let mut raw = Vec::new();
        let mut checked = 0usize;

        for e in files.iter().filter(|e| is_suite(&e.path)) {
            let Some(c) = tree.content(e) else { continue };
            checked += 1;
            raw.extend(check_suite(tree, e, c));
        }

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
any this fence missed) in the SAME branch that deletes or renames the script: a bare shell \
word ($SH/$HERE/$SPIRA_HOME + path, or a bare variable assigned a bare *.sh/*.py name), a \
Rust string literal, a systemd unit's @PLACEHOLDER@ target, or a spira/watchers row. A \
suite that creates the file itself first (cat/printf/tee/cp/install/ln/write_exe writing \
that exact basename) is exempt. Pre-existing debt goes in spira-lint/script-callers-allow \
(shrink-only, by file) with the fix named in a comment, not silently — never add an entry \
for a file this change itself breaks."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, tracked: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), tracked.iter().copied(), std::iter::empty::<&str>());
        ScriptCallers::default().check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    // ── suites: the positive control (sp-missing-scan.py's verdict.sh case) ──

    #[test]
    fn a_suite_calling_a_deleted_script_via_sh_is_caught_then_clears() {
        let t = TempDir::new("sc-sh");
        t.write("spira/test-planted.sh", "#!/usr/bin/env bash\nSH=\"$(cd \"$(dirname \"$0\")\" && pwd)\"\nbash \"$SH/verdict.sh\" --quiet\n");
        let got = run(&t, &["spira/test-planted.sh"]).unwrap();
        assert_eq!(got, vec!["script-callers: spira/test-planted.sh:3: invokes spira/verdict.sh, which is not in the tree and is not written by this suite"]);
        t.write("spira/verdict.sh", "true\n");
        assert!(run(&t, &["spira/test-planted.sh", "spira/verdict.sh"]).unwrap().is_empty());
    }

    #[test]
    fn a_bare_name_variable_assigned_a_deleted_script_is_caught() {
        // sp-yv4b3's shape, read by a suite: WORLD=world.sh then "$WORLD" start, wrapped in
        // env -i and a pass-through "${@}", the way test-mail-deliver.sh's real bug reads.
        let t = TempDir::new("sc-bare");
        t.write(
            "spira/test-bare.sh",
            "#!/usr/bin/env bash\nWORLD=world.sh\nenv -i HOME=/h PATH=\"$PATH\" \"${@}\" \"$WORLD\" start\n",
        );
        let got = run(&t, &["spira/test-bare.sh"]).unwrap();
        assert_eq!(got, vec!["script-callers: spira/test-bare.sh:3: invokes spira/world.sh, which is not in the tree and is not written by this suite"]);
    }

    #[test]
    fn a_suite_that_writes_the_script_itself_is_exempt() {
        // test-world-drain-deadline.sh's real shape: SH repointed at a sandbox, the suite
        // prints the fake aeon.sh into it itself, then runs it.
        let t = TempDir::new("sc-creates");
        t.write(
            "spira/test-sandboxed.sh",
            "#!/usr/bin/env bash\nSH=\"$TMP/spira\"\nprintf '#!/bin/sh\\nsleep 1\\n' > \"$SH/aeon.sh\"\nbash \"$SH/aeon.sh\" &\n",
        );
        assert!(run(&t, &["spira/test-sandboxed.sh"]).unwrap().is_empty());
    }

    #[test]
    fn source_and_dot_are_interpreters_too() {
        let t = TempDir::new("sc-source");
        t.write("spira/test-src.sh", "#!/usr/bin/env bash\nHERE=\"$(dirname \"$0\")\"\n. \"$HERE/gone.sh\"\n");
        let got = run(&t, &["spira/test-src.sh"]).unwrap();
        assert_eq!(got, vec!["script-callers: spira/test-src.sh:3: invokes spira/gone.sh, which is not in the tree and is not written by this suite"]);
    }

    #[test]
    fn an_existing_target_is_clean() {
        let t = TempDir::new("sc-ok");
        t.write("spira/test-ok.sh", "#!/usr/bin/env bash\nSPIRA_HOME=/x\nbash \"$SPIRA_HOME/lib.sh\"\n");
        t.write("spira/lib.sh", "true\n");
        assert!(run(&t, &["spira/test-ok.sh", "spira/lib.sh"]).unwrap().is_empty());
    }

    #[test]
    fn here_dot_dot_names_a_repo_root_sibling_not_a_literal_dot_dot_path() {
        // `$HERE/../install.sh` from a suite in spira/ names the repo-root install.sh —
        // collapsing ".." is what tells those two apart (test-install-*.sh's real shape).
        let t = TempDir::new("sc-dotdot");
        t.write("spira/test-install-x.sh", "#!/usr/bin/env bash\nHERE=\"$(dirname \"$0\")\"\n. \"$HERE/../install.sh\"\n");
        t.write("install.sh", "true\n");
        assert!(run(&t, &["spira/test-install-x.sh", "install.sh"]).unwrap().is_empty());
        t.write("spira/test-install-y.sh", "#!/usr/bin/env bash\nHERE=\"$(dirname \"$0\")\"\n. \"$HERE/../systemd/unit-ensure.sh\"\n");
        let got = run(&t, &["spira/test-install-x.sh", "install.sh", "spira/test-install-y.sh"]).unwrap();
        assert_eq!(got, vec!["script-callers: spira/test-install-y.sh:3: invokes systemd/unit-ensure.sh, which is not in the tree and is not written by this suite"]);
    }

    // ── Rust string literals ──

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

    #[test]
    fn a_comment_mentioning_the_construct_is_not_a_literal() {
        let t = TempDir::new("sc-rust-comment");
        let planted = concat!("no-such-thing", ".sh");
        t.write("gate/src/x.rs", &format!("// Command::new(\"{planted}\") was retired\nfn f() {{}}\n")); // comment, not code
        assert!(run(&t, &["gate/src/x.rs"]).unwrap().is_empty());
    }

    #[test]
    fn a_log_message_or_usage_string_is_not_a_literal() {
        // Exactly what flooded the first draft of this rule: a log line or --help text
        // naming the script's pre-rewrite name for an operator's benefit, not a path.
        let t = TempDir::new("sc-rust-log");
        t.write(
            "aeon/src/escape.rs",
            "fn f(fayth: &str) -> String { format!(\"escape.sh {fayth}: nothing ready\") }\nconst USAGE: &str = \"usage: queue.sh submit\";\n",
        );
        assert!(run(&t, &["aeon/src/escape.rs"]).unwrap().is_empty());
    }

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

    #[test]
    fn a_unit_s_comment_is_not_scanned() {
        let t = TempDir::new("sc-unit-comment");
        t.write("systemd/spira-ok.service", "# Documentation=file://@SPIRA_HOME@/gone.sh\n[Service]\nExecStart=sentinel\n");
        assert!(run(&t, &["systemd/spira-ok.service"]).unwrap().is_empty());
    }

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

    #[test]
    fn an_unknown_placeholder_is_not_guessed_at() {
        let t = TempDir::new("sc-unknown-ph");
        t.write("systemd/spira-x.service", "[Service]\nExecStart=@SOME_FUTURE_VAR@/x.sh\n");
        assert!(run(&t, &["systemd/spira-x.service"]).unwrap().is_empty());
    }

    // ── scope, refusal, positive control ──

    #[test]
    fn empty_scope_refuses() {
        let t = TempDir::new("sc-empty");
        assert_eq!(run(&t, &["README"]), Err(LintError::EmptyScope));
    }

    #[test]
    fn a_clean_tree_reports_checked() {
        let t = TempDir::new("sc-checked");
        t.write("spira/test-clean.sh", "#!/usr/bin/env bash\necho ok\n");
        let tree = Tree::from_paths(t.path(), ["spira/test-clean.sh"], std::iter::empty::<&str>());
        let r = ScriptCallers::default();
        assert_eq!(r.check(&tree), Ok(vec![]));
        assert_eq!(r.checked(), Some((1, "callers".to_string())));
    }

    #[test]
    fn a_listed_file_is_silenced_and_a_stale_entry_is_refused() {
        let t = TempDir::new("sc-allow");
        t.write("spira/test-planted.sh", "#!/usr/bin/env bash\nSH=\"$(dirname \"$0\")\"\nbash \"$SH/gone.sh\"\n");
        t.write(ALLOW_FILE, "spira/test-planted.sh\n");
        assert!(run(&t, &["spira/test-planted.sh"]).unwrap().is_empty());
        t.write("spira/test-planted.sh", "#!/usr/bin/env bash\necho fixed\n");
        assert_eq!(
            run(&t, &["spira/test-planted.sh"]).unwrap(),
            vec!["script-callers: spira-lint/script-callers-allow: lists spira/test-planted.sh, which no longer needs an exception — remove the line (the list only shrinks)"]
        );
    }
}

