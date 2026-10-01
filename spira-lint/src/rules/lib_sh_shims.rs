//! `lib-sh-shims` — every function left in `spira/lib.sh` and `spira/conf.sh` is a
//! one-line shim onto a binary (or the `log`/`die` prelude). Written for the wave-4
//! close-out audit (sp-hlng2, "wave 4.37"): the family-by-family peel
//! (`wave4-decomposition.md`) only stays "lib.sh is shims only" if nothing quietly grows
//! real logic back into the file every earlier bead emptied.
//!
//! **Intent.** A function's body counts as a SHIM when every statement in it is set-up
//! (a `local`/plain assignment, or env-threading ahead of a call) or delegation (one call
//! to a Spira binary, or to another function already defined earlier in the same file —
//! calling a sibling shim is still a shim). It counts as a REAL BODY — and needs a named
//! allow-list entry — when it contains any of:
//!
//!   - a loop (`while` / `for` / `until`);
//!   - an `if` that carries an `else` or `elif` branch (a bare `if ... ; then ... fi`
//!     guard, with no else, is treated as set-up — the shape `lc_release_bead` and
//!     `_counter_events_query` both use to turn a delegate's failure into a plain
//!     return);
//!   - an inline interpreter script (`python3 -c`, `perl -e`);
//!   - two or more invocations of an external command that is neither a known delegate
//!     binary nor a sibling function defined in the same file — in practice, raw `git`
//!     plumbing repeated more than once (`content_landed`'s ancestor-then-merge-tree
//!     proof is the motivating case).
//!
//! This is a heuristic over the file's own very consistent style — `name() {` at column
//! 0, the body indented, a lone `}` at column 0 closing it — not a general bash parser.
//! It will not catch everything a determined rewrite could hide from it; it exists to
//! catch regrowth, not to replace the audit that first classified every function.
//!
//! **Allow list.** `spira-lint/lib-sh-shims-allow`: `<file>:<function>` pairs, one per
//! line, each commented with why. An entry naming a function that no longer exists in
//! that file is refused (shrink by removing the line, not by leaving it to rot).

use std::collections::BTreeSet;

use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct LibShShims;

const NAME: &str = "lib-sh-shims";
const ALLOW_FILE: &str = "spira-lint/lib-sh-shims-allow";
const FILES: &[&str] = &["spira/lib.sh", "spira/conf.sh"];

/// Spira's own binaries and the handful of bash-builtin/text-tool names a thin shim may
/// use for arg-prep or output-marshalling without that counting as "real logic". `read`
/// covers the `IFS=$'\t' read -r a b <<<"$row"` destructuring idiom; `sed`/`tr`/`head`
/// cover a shim parsing one line of a delegate's own output into a legacy global.
const DELEGATES: &[&str] = &[
    "aeon", "sentinel", "strand", "spira-claim", "spira-lc", "landing-pass", "sending", "queue",
    "queue-helpers", "gh-intake", "maechen-trigger", "rule", "census", "spira-config", "bead", "bdq",
    "bd", "ghq", "tsd-write", "rebase-stale", "suite-select", "command",
];
const TRIVIAL: &[&str] = &[
    "local", "declare", "readonly", "return", "exit", "printf", "shift", "unset", "true", "false",
    "mktemp", "rm", "cat", "sed", "tr", "head", "basename", "dirname", "readlink", "cd", "pwd", "set",
    "trap", "echo", "read", "export",
];
/// Bash's own reserved words: never "the command", so never foreign. `case`/`esac` and a
/// bare `in` cover a (possibly multi-line) case statement's frame; a pattern LABEL such as
/// `prod)` or `*)` is recognised separately, by `ends_with(')')`, below — a case body's own
/// command on the same line as its label is not re-examined (a known gap: no function in
/// this file puts a non-trivial call there today).
const KEYWORDS: &[&str] =
    &["if", "then", "elif", "else", "fi", "do", "done", "case", "esac", "in", "select", "function", "{", "}"];

/// One `name() {` ... lone `}` region: the function's name and its body's lines (between
/// the header and the closer, exclusive of both).
struct Func {
    name: String,
    body: String,
}

/// Every top-level function in `text`, in file order. Matches this file family's own
/// convention: the header is `name() {` alone on a line (column 0), and the first
/// following line that is exactly `}` (no leading whitespace) closes it.
fn functions_in(text: &str) -> Vec<Func> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some((name, rest)) = header_open(lines[i]) {
            // A one-liner (`log() { printf '...'; }`) closes on the same line — the
            // common shape for this file's `log`/`die`/`ghq`/`watch_unit_name`. Anything
            // else (a header followed by nothing, or by a trailing `# comment`) opens a
            // multi-line body, closed by a lone `}` at column 0 — this file's own
            // convention, verified across every function in it.
            if rest.trim_end().ends_with('}') {
                let body = rest.trim_end().trim_end_matches('}').to_string();
                out.push(Func { name, body });
                i += 1;
                continue;
            }
            let start = i + 1;
            let mut j = start;
            while j < lines.len() && lines[j] != "}" {
                j += 1;
            }
            out.push(Func { name, body: lines[start..j].join("\n") });
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// `(name, rest-of-line-after-the-opener)` when `line` opens a function: `name() {`,
/// starting at column 0 (this file's own convention), with nothing of its own name before
/// the `(`.
fn header_open(line: &str) -> Option<(String, &str)> {
    let idx = line.find("() {")?;
    let name = &line[..idx];
    (!name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        .then(|| (name.to_string(), &line[idx + 4..]))
}

/// Every function name defined in either file — calling one of these is delegation, not
/// real logic, however much work the callee itself does.
fn all_names(texts: &[(&str, &str)]) -> BTreeSet<String> {
    texts.iter().flat_map(|(_, t)| functions_in(t)).map(|f| f.name).collect()
}

/// The head word of one simple command, after stripping leading `NAME=value` env-threading
/// words and an optional leading `command`.
fn head_word(seg: &str) -> Option<String> {
    let mut words = seg.split_whitespace();
    let mut w = words.next()?;
    loop {
        let bytes = w.as_bytes();
        let is_assign = bytes.first().is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
            && w.contains('=')
            && w.split('=').next().is_some_and(|n| n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
        if !is_assign {
            break;
        }
        w = words.next()?;
    }
    if w == "command" {
        w = words.next()?;
    }
    Some(w.trim_matches(|c: char| "'\"".contains(c)).to_string())
}

/// Split `line` into simple-command segments on top-level `;`, `&&`, `||` and `|`
/// (parentheses/braces/quotes are not tracked — good enough for this file's own style,
/// where none of those operators appear inside a quoted string on the same line as
/// another command).
fn segments(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ';' => {
                out.push(std::mem::take(&mut cur));
            }
            '&' if chars.peek() == Some(&'&') => {
                chars.next();
                out.push(std::mem::take(&mut cur));
            }
            '|' if chars.peek() == Some(&'|') => {
                chars.next();
                out.push(std::mem::take(&mut cur));
            }
            '|' => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// Command substitutions `$(...)` nested in `s`, balanced-paren extracted (one level of
/// nested `(...)` inside is tolerated; good enough for this file's own style).
fn command_subs(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'$' && bytes[i + 1] == b'(' {
            let mut depth = 1;
            let mut j = i + 2;
            while j < bytes.len() && depth > 0 {
                match bytes[j] {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            out.push(s[i + 2..j.saturating_sub(1)].to_string());
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

struct Shape {
    has_loop: bool,
    has_if_else: bool,
    has_inline_script: bool,
    foreign_calls: usize,
}

fn shape(body: &str, known: &BTreeSet<String>) -> Shape {
    let mut has_loop = false;
    let mut has_if_else = false;
    let mut has_inline_script = false;
    let mut foreign_calls = 0usize;

    let is_known_head = |w: &str| {
        !w.is_empty()
            && (DELEGATES.contains(&w)
                || TRIVIAL.contains(&w)
                || KEYWORDS.contains(&w)
                || known.contains(w)
                || w.starts_with('[')
                || w.ends_with(')'))
    };

    let mut count_line = |line: &str| {
        for seg in segments(line) {
            let seg = seg.trim();
            if seg.is_empty() {
                continue;
            }
            // Command substitutions anywhere in this segment (an assignment's RHS, or a
            // bare one) contribute their own inner commands too.
            for inner in command_subs(seg) {
                if let Some(w) = head_word(&inner) {
                    if !is_known_head(&w) {
                        foreign_calls += 1;
                    }
                }
            }
            if seg.contains("python3") && seg.contains("-c") {
                has_inline_script = true;
            }
            if seg.contains("perl") && seg.contains("-e") {
                has_inline_script = true;
            }
            // The segment's own head, when it is not itself an assignment (an assignment
            // was already credited via command_subs above, if its RHS called out).
            if let Some(w) = head_word(seg) {
                let is_assign = seg.split_whitespace().next().is_some_and(|f| {
                    f.split('=').next().is_some_and(|n| {
                        !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    }) && f.contains('=')
                });
                if !is_assign && !is_known_head(&w) && !w.is_empty() {
                    foreign_calls += 1;
                }
            }
        }
    };

    for raw in body.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with("while ") || line.starts_with("for ") || line.starts_with("until ") {
            has_loop = true;
        }
        if line == "else" || line.starts_with("else ") || line.starts_with("elif ") {
            has_if_else = true;
        }
        count_line(line);
    }
    Shape { has_loop, has_if_else, has_inline_script, foreign_calls }
}

/// Real body (needs an allow-list entry) vs. shim, for one function.
fn is_real(body: &str, known: &BTreeSet<String>) -> bool {
    let s = shape(body, known);
    s.has_loop || s.has_if_else || s.has_inline_script || s.foreign_calls >= 2
}

impl Rule for LibShShims {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        FILES.contains(&e.path.as_str())
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = crate::scope(tree, self)?;
        let texts: Vec<(&str, String)> = files
            .iter()
            .filter_map(|e| tree.content(e).map(|c| (e.path.as_str(), String::from_utf8_lossy(c).into_owned())))
            .collect();
        let borrowed: Vec<(&str, &str)> = texts.iter().map(|(p, t)| (*p, t.as_str())).collect();
        let known = all_names(&borrowed);

        let allow_lines: Vec<(usize, String)> = tree
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
        for (path, text) in &borrowed {
            let funcs = functions_in(text);
            let names: BTreeSet<&str> = funcs.iter().map(|f| f.name.as_str()).collect();
            for f in &funcs {
                if !is_real(&f.body, &known) {
                    continue;
                }
                let key = format!("{path}:{}", f.name);
                if !allow_lines.iter().any(|(_, a)| a == &key) {
                    out.push(Finding {
                        rule: NAME,
                        path: path.to_string(),
                        line: None,
                        message: format!(
                            "{}: real-bodied function with no allow-list entry — port it, retire it, or add '{key}' to {ALLOW_FILE} with why",
                            f.name
                        ),
                    });
                }
            }
            for (line, entry) in &allow_lines {
                let Some(fname) = entry.strip_prefix(&format!("{path}:")) else { continue };
                if !names.contains(fname) {
                    out.push(Finding {
                        rule: NAME,
                        path: ALLOW_FILE.to_string(),
                        line: Some(*line),
                        message: format!("lists {entry}, which no longer defines a function in {path} — remove the line"),
                    });
                }
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "lib.sh/conf.sh hold shims (or log/die) only. A real body needs a port, a retirement, or a \
reasoned line in spira-lint/lib-sh-shims-allow naming what still blocks it (usually groups 5-7 of \
the remaining-bash-inventory, or conf.sh's own bootstrap paradox)."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        LibShShims.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_one_line_shim_onto_a_delegate_is_clean() {
        let t = TempDir::new("shim");
        t.write("spira/lib.sh", "repo_root() {\n    _spira_config_repo root \"${1:-}\"\n}\n");
        t.write(ALLOW_FILE, "");
        assert!(run(&t, &["spira/lib.sh"]).unwrap().is_empty());
    }

    #[test]
    fn a_bare_if_guard_with_no_else_is_still_a_shim() {
        let t = TempDir::new("guard");
        t.write(
            "spira/lib.sh",
            "requeues_of() {\n    local id=\"$1\" out\n    if out=\"$(command spira-claim count-events \"$id\" --db \"$SPIRA_DB\")\"; then\n        printf '%s' \"$out\"\n        return 0\n    fi\n    printf '?'\n    return 1\n}\n",
        );
        t.write(ALLOW_FILE, "");
        assert!(run(&t, &["spira/lib.sh"]).unwrap().is_empty());
    }

    #[test]
    fn calling_a_sibling_shim_is_still_delegation() {
        let t = TempDir::new("sibling");
        t.write(
            "spira/lib.sh",
            "spira_home_repo() {\n    _spira_config_repo home-repo\n}\nbead_repo() {\n    local id=\"$1\" name\n    name=\"$(bdq state \"$id\" repo)\"\n    printf '%s' \"${name:-$(spira_home_repo)}\"\n}\n",
        );
        t.write(ALLOW_FILE, "");
        assert!(run(&t, &["spira/lib.sh"]).unwrap().is_empty());
    }

    /// The positive control: a planted real-bodied function — two raw `git` calls chained
    /// on a condition, exactly `content_landed`'s own shape — must fail.
    #[test]
    fn a_planted_real_bodied_function_is_caught() {
        let t = TempDir::new("planted");
        t.write(
            "spira/lib.sh",
            "content_landed() {\n    local repo=\"$1\" br=\"$2\" base=\"$3\"\n    git -C \"$repo\" merge-base --is-ancestor \"$br\" \"$base\" && return 0\n    git -C \"$repo\" rev-parse \"$base\" >/dev/null\n}\n",
        );
        t.write(ALLOW_FILE, "");
        let got = run(&t, &["spira/lib.sh"]).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("content_landed"), "{got:?}");
        assert!(got[0].contains("no allow-list entry"), "{got:?}");
    }

    #[test]
    fn a_loop_or_an_if_else_is_real_even_with_no_foreign_call() {
        let t = TempDir::new("loopy");
        t.write(
            "spira/lib.sh",
            "spira_status_seam() {\n    local sid sst src=\"$1\"\n    while IFS=$'\\t' read -r sid sst; do\n        true\n    done < \"$src\"\n}\n",
        );
        t.write(ALLOW_FILE, "");
        let got = run(&t, &["spira/lib.sh"]).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("spira_status_seam"), "{got:?}");
    }

    #[test]
    fn an_inline_python_script_is_real() {
        let t = TempDir::new("py");
        t.write(
            "spira/lib.sh",
            "spira_bead_status() {\n    bdjson show \"$1\" | python3 -c 'import sys; print(sys.stdin.read())'\n}\n",
        );
        t.write(ALLOW_FILE, "");
        let got = run(&t, &["spira/lib.sh"]).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("spira_bead_status"), "{got:?}");
    }

    #[test]
    fn a_listed_entry_passes_and_a_stale_one_is_refused() {
        let t = TempDir::new("allow");
        t.write(
            "spira/lib.sh",
            "host_cores() {\n    local n\n    n=\"$(getconf _NPROCESSORS_ONLN)\"\n    git -C / status\n    git -C / log\n}\n",
        );
        t.write(ALLOW_FILE, "# known exception\nspira/lib.sh:host_cores\nspira/lib.sh:gone_function\n");
        let got = run(&t, &["spira/lib.sh"]).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("gone_function") && got[0].contains("no longer defines"), "{got:?}");
    }

    #[test]
    fn nested_and_non_lib_files_are_out_of_scope() {
        let t = TempDir::new("scope");
        t.write("spira/sub/lib.sh", "content_landed() {\n    git a\n    git b\n}\n");
        t.write("spira/other.sh", "content_landed() {\n    git a\n    git b\n}\n");
        assert_eq!(run(&t, &["spira/sub/lib.sh", "spira/other.sh"]), Err(LintError::EmptyScope));
    }
}
