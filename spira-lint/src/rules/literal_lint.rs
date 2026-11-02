//! `literal-lint` — refuse configured-name literals outside the files that declare them.
//! Contract: DESIGN.md. Ported from `spira/literal-lint.sh` (deleted).


use std::process::Command;
use regex::bytes::Regex;

use crate::{lines, trim_lead, Entry, Finding, LintError, Rule, Tree};

pub struct LiteralLint;

const NAME: &str = "literal-lint";
/// This rule's own source names every fallback literal; it must never flag itself.
const OWN_SOURCE: &str = "spira-lint/src/rules/literal_lint.rs";
const MARKER: &str = "literal-ok";

/// The shipped defaults, used when `spira/schema.sh` cannot be read at all — never empty
/// (law-absence-needs-a-positive-control: an empty pattern list makes every tree "clean").
const FALLBACK: &[&str] =
    &["needs-operator", "needs-ryan", "awaiting-ci", "mimir-sweep", "mimir-remedy", "review-finding", "world-stop"];

/// A name specific enough to guard: lowercase, digits and hyphens only, and at least one
/// hyphen (a compound token) — short single-word names are excluded because they appear
/// legitimately as English prose.
fn is_compound(v: &str) -> bool {
    !v.is_empty() && v.contains('-') && v.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// `schema.sh names`, then `name <k>` and `default <k>` for each — BOTH values, because a
/// literal is wrong whether it matches the configured value or the shipped default, and
/// which of the two `schema_name` returns depends on whether the environment carries an
/// operator override (law-schema-over-code: ask the declaration, not a copy of it).
/// `None` for the warning when the shipped defaults had to stand in.
pub fn configured_names(schema: &std::path::Path) -> (Vec<String>, Option<String>) {
    configured_names_env(schema, &[])
}

/// [`configured_names`], with `extra_env` applied to every `schema.sh` invocation on top of
/// the inherited process environment — a way for a test to simulate an operator override
/// (`SPIRA_ASK_LABEL`, set by conf.sh in production) without mutating the real process
/// environment, which every thread in the test binary shares and races on.
fn configured_names_env(schema: &std::path::Path, extra_env: &[(&str, &str)]) -> (Vec<String>, Option<String>) {
    let executable = std::fs::metadata(schema).map(|m| {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o111 != 0
    }).unwrap_or(false);
    if executable {
        if let Some(names) = run_schema(schema, &["names"], extra_env) {
            let keys: Vec<String> = names.lines().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect();
            let mut set = std::collections::BTreeSet::new();
            for k in &keys {
                for cmd in ["name", "default"] {
                    if let Some(v) = run_schema(schema, &[cmd, k], extra_env) {
                        let v = v.trim();
                        if is_compound(v) {
                            set.insert(v.to_string());
                        }
                    }
                }
            }
            if !set.is_empty() {
                return (set.into_iter().collect(), None);
            }
        }
    }
    (
        FALLBACK.iter().map(|s| s.to_string()).collect(),
        Some(format!("literal-lint: WARNING — could not read names from {}; using shipped defaults", schema.display())),
    )
}

/// What `schema.sh` would otherwise read from the box rather than the tree (sp-g9f3t):
/// conf.sh's config file search (explicit SPIRA_CONF/SPIRA_TOML, else XDG — the operator's
/// own config file) and the runes its staleness check compares against it. Pinned to a path
/// that does not exist, so the names are the tree's declarations plus any override the
/// environment carries — the same on every box, whatever its config file or its mtimes.
/// (Unpinned, the same tree flipped between 0 and 5 findings on the gate on mtimes alone.)
/// SPIRA_TOML is pinned to the TREE's own complete fixture, never the box's file: under one
/// source of config (per Ryan 2026-10-05) a missing spec is a refusal, so a path that does not
/// exist would make every schema read fail and fall back to the shipped defaults.
const BOX_CONFIG_PINS: &[(&str, &str)] = &[
    ("SPIRA_CONF", "/nonexistent/spira-lint/conf"),
    ("SPIRA_RUNES_DIR", "/nonexistent/spira-lint/runes"),
];
/// The tree's complete fixture, relative to its `spira/schema.sh`.
const TREE_FIXTURE: &str = "../spira/testlib/complete.toml";

fn run_schema(schema: &std::path::Path, args: &[&str], extra_env: &[(&str, &str)]) -> Option<String> {
    // batch-job: this runs whatever its caller names, as long as that takes
    let mut cmd = Command::new(schema);
    cmd.args(args);
    for (k, v) in BOX_CONFIG_PINS {
        cmd.env(k, v);
    }
    if let Some(dir) = schema.parent() {
        cmd.env("SPIRA_TOML", dir.join(TREE_FIXTURE));
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn names_re(names: &[String]) -> Regex {
    let alt = names.iter().map(|n| regex::escape(n)).collect::<Vec<_>>().join("|");
    Regex::new(&alt).expect("escaped alternation is always valid")
}

/// True when `line` (or the line before it) carries the escape marker.
fn marked(ls: &[&[u8]], i: usize) -> bool {
    let has = |l: &[u8]| l.windows(MARKER.len()).any(|w| w == MARKER.as_bytes());
    has(ls[i]) || (i > 0 && has(ls[i - 1]))
}

/// An identity label's env-with-literal-fallback can never be excused: a unit does not
/// export the key, so the literal silently becomes the value.
fn ask_fallback(line: &[u8]) -> bool {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"ASK_LABEL[^;]*(unwrap_or|\bor\(|:-)").unwrap()).is_match(line)
}

fn is_comment(line: &[u8]) -> bool {
    let stripped = {
        let start = line.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(line.len());
        &line[start..]
    };
    stripped.starts_with(b"#") || stripped.starts_with(b"//")
}

/// Hits in one file: (1-based line, line text with leading whitespace stripped).
pub fn scan(content: &[u8], names: &[String]) -> Vec<(usize, String)> {
    if names.is_empty() {
        return Vec::new();
    }
    let re = names_re(names);
    if !re.is_match(content) {
        return Vec::new();
    }
    let ls = lines(content);
    let mut out = Vec::new();
    for (i, l) in ls.iter().enumerate() {
        if is_comment(l) || (marked(&ls, i) && !ask_fallback(l)) {
            continue;
        }
        if re.is_match(l) {
            out.push((i + 1, trim_lead(l)));
        }
    }
    out
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn exempt(path: &str) -> bool {
    let b = basename(path);
    path == OWN_SOURCE
        || b == "schema.sh"
        || b == "conf.sh"
        // The complete config fixture IS a declaration of every key's value, labels included.
        || path.starts_with("spira-config/tests/fixtures/")
        || path == "spira/testlib/complete.toml"
        // sp-g3uwp: spira/conf.d/<KEY> is conf.sh's own former defaults block, split one
        // file per key — the canonical place a label's default literal is declared, exactly
        // as conf.sh itself already was exempt for. conf.d.keys.generated.sh and
        // conf.d.defaults.generated.sh are conf-gen.sh's build output (never committed, but
        // exempted too in case a scratch tree carries them) for the same reason.
        || path.contains("/conf.d/")
        || b == "conf.d.keys.generated.sh"
        || b == "conf.d.defaults.generated.sh"
        || path.ends_with(".md")
        || (b.starts_with("test-") && b.ends_with(".sh"))
        || path.ends_with(".json")
}

/// Text: no NUL byte and at least one non-newline byte (`grep -qI .`); a compiled binary
/// carries no line to annotate and its source is linted where it lives (PR 331:
/// `bin/queue-watch` carried a default label as a string constant).
fn is_text(content: &[u8]) -> bool {
    !content.contains(&0) && content.iter().any(|&b| b != b'\n')
}

impl Rule for LiteralLint {
    fn name(&self) -> &'static str {
        NAME
    }

    /// The INDEX, not the worktree: what the next commit ships is what matters. Untracked
    /// files are not scanned (matching `binary-path-fence`'s convention, and the bash
    /// original's `git ls-files` with no `--others`).
    fn applies_to(&self, e: &Entry) -> bool {
        e.tracked
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = crate::scope(tree, self)?;
        let (names, warning) = configured_names(&tree.root.join("spira/schema.sh"));
        if let Some(w) = &warning {
            eprintln!("{w}");
        }
        let mut out = Vec::new();
        for e in files {
            if exempt(&e.path) {
                continue;
            }
            let Some(content) = tree.content(e) else { continue };
            if !is_text(content) {
                continue;
            }
            for (line, text) in scan(content, &names) {
                out.push(Finding { rule: NAME, path: e.path.clone(), line: Some(line), message: text });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "The names declared in schema.sh must be read through their accessors everywhere \
else — bash spira/schema.sh name <key>, or $SPIRA_*_LABEL after conf.sh is sourced. A literal \
that bypasses these can disagree when an operator changes the default. If the use is \
genuinely correct (a test fixture, a fallback), mark it `# literal-ok: <why>` on the line or \
the one above it."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(LiteralLint),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn literal_ok_never_excuses_an_ask_label_fallback() {
        let names = vec!["needs-operator".to_string()];
        let bad = scan(b"x = var(\"SPIRA_ASK_LABEL\").unwrap_or_else(|| \"needs-operator\".into()); // literal-ok: no\n", &names);
        assert_eq!(bad.len(), 1);
        let shell = scan(b"a=\"${SPIRA_ASK_LABEL:-needs-operator}\" # literal-ok: no\n", &names);
        assert_eq!(shell.len(), 1);
        let fine = scan(b"(\"SPIRA_ASK_LABEL\", \"needs-operator\"), // literal-ok: table\n", &names);
        assert!(fine.is_empty());
    }

    #[test]
    fn escape_hatch_on_the_line_or_the_line_above() {
        let names = vec!["needs-operator".to_string()];
        let on_line = scan(b"SOME=\"${X:-needs-operator}\"  # literal-ok: standing it down\n", &names);
        assert!(on_line.is_empty());
        let above = scan(b"# literal-ok: the line below is special\nSOME=\"${X:-needs-operator}\"\n", &names);
        assert!(above.is_empty());
        let neither = scan(b"SOME=\"${X:-needs-operator}\"\n", &names);
        assert_eq!(neither.len(), 1);
    }

    #[test]
    fn pure_comment_lines_in_shell_and_rust_are_not_flagged() {
        let names = vec!["needs-operator".to_string()];
        assert!(scan(b"# default label is needs-operator\n", &names).is_empty());
        assert!(scan(b"// default label is needs-operator\n", &names).is_empty());
    }

    #[test]
    fn is_compound_excludes_short_single_words() {
        assert!(is_compound("needs-operator"));
        assert!(!is_compound("plan"));
        assert!(!is_compound("Plan-Foo"));
        assert!(!is_compound(""));
    }

    #[test]
    fn configured_names_falls_back_when_schema_is_missing() {
        let (names, warning) = configured_names(std::path::Path::new("/nonexistent/schema.sh"));
        assert!(warning.is_some());
        for f in FALLBACK {
            assert!(names.iter().any(|n| n == f), "{names:?} missing {f}");
        }
    }
}
