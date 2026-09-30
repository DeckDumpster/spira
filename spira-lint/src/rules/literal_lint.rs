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
    &["needs-operator", "needs-ryan", "awaiting-ci", "maechen-sweep", "maechen-remedy", "review-finding", "world-stop"];

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
/// own spira.toml) and the chamber its staleness check compares against it. Pinned to a path
/// that does not exist, so the names are the tree's declarations plus any override the
/// environment carries — the same on every box, whatever its config file or its mtimes.
/// (Unpinned, the same tree flipped between 0 and 5 findings on the gate on mtimes alone.)
const BOX_CONFIG_PINS: &[(&str, &str)] = &[
    ("SPIRA_CONF", "/nonexistent/spira-lint/spira.conf"),
    ("SPIRA_TOML", "/nonexistent/spira-lint/spira.toml"),
    ("SPIRA_CHAMBER", "/nonexistent/spira-lint/chamber"),
];

fn run_schema(schema: &std::path::Path, args: &[&str], extra_env: &[(&str, &str)]) -> Option<String> {
    let mut cmd = Command::new(schema);
    cmd.args(args);
    for (k, v) in BOX_CONFIG_PINS {
        cmd.env(k, v);
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
        if is_comment(l) || marked(&ls, i) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_git(t.path()).unwrap();
        LiteralLint.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn empty_index_refuses_then_a_planted_literal_is_seen_red() {
        let t = TempDir::new("lit");
        t.git_init();
        assert_eq!(run(&t), Err(LintError::EmptyScope));

        t.write("spira/helper.sh", "#!/usr/bin/env bash\necho ok\n");
        t.write("spira/planted.sh", "#!/usr/bin/env bash\nSOME=\"${SPIRA_CI_LABEL:-awaiting-ci}\"\n");
        t.git(&["add", "."]);
        let got = run(&t).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("spira/planted.sh:2"), "{got:?}");
        assert!(got[0].contains("awaiting-ci"));

        t.remove("spira/planted.sh");
        t.git(&["add", "-A"]);
        assert!(run(&t).unwrap().is_empty());
    }

    #[test]
    fn exemptions_are_exact_and_scoped() {
        let t = TempDir::new("lit-exempt");
        t.git_init();
        t.write("spira/planted.sh", "#!/usr/bin/env bash\nSOME=\"${SPIRA_CI_LABEL:-awaiting-ci}\"\n");
        t.write("spira/schema.sh", "schema_name() { printf %s \"${SPIRA_CI_LABEL:-awaiting-ci}\"; }\n");
        t.write("spira/conf.sh", "SPIRA_CI_LABEL=\"${SPIRA_CI_LABEL:-awaiting-ci}\"\n");
        t.write("spira/test-ci.sh", "#!/usr/bin/env bash\nDB_LABEL=awaiting-ci\n");
        t.write("spira/labels.json", "{\"label\":\"awaiting-ci\"}\n");
        t.write("docs/notes.md", "the default label is awaiting-ci\n");
        t.git(&["add", "."]);
        let got = run(&t).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("spira/planted.sh"));
    }

    #[test]
    fn binary_files_are_not_scanned() {
        let t = TempDir::new("lit-bin");
        t.git_init();
        t.write("spira/helper.sh", "echo ok\n");
        std::fs::write(t.path().join("bin_tool"), b"\x7fELF\0label=awaiting-ci\0\0").unwrap();
        t.git(&["add", "."]);
        assert!(run(&t).unwrap().is_empty());
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

    #[test]
    fn configured_names_reads_both_name_and_default_from_a_real_schema_sh() {
        let t = TempDir::new("lit-schema");
        t.write(
            "schema.sh",
            "#!/usr/bin/env bash\ncase \"$1\" in\n  names) echo ask ;;\n  name) [ \"$2\" = ask ] && printf '%s' \"${SPIRA_ASK_LABEL:-needs-operator}\" ;;\n  default) [ \"$2\" = ask ] && printf '%s' needs-operator ;;\nesac\n",
        );
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(t.path().join("schema.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let (names, warning) = configured_names(&t.path().join("schema.sh"));
        assert!(warning.is_none(), "{warning:?}");
        assert!(names.contains(&"needs-operator".to_string()));

        // An exported override must ALSO appear, so a literal like the shipped default is
        // still refused under an aeon's environment that overrides SPIRA_ASK_LABEL. Passed
        // as an explicit extra_env rather than a real env::set_var: the test binary is
        // multi-threaded, and every thread shares (and races on) the real process
        // environment.
        let (names2, _) =
            configured_names_env(&t.path().join("schema.sh"), &[("SPIRA_ASK_LABEL", "needs-ryan-lit-test")]);
        assert!(names2.contains(&"needs-operator".to_string()), "{names2:?}");
        assert!(names2.contains(&"needs-ryan-lit-test".to_string()), "{names2:?}");
    }

    /// sp-g9f3t, the 2026-09-30 base-red: the real schema.sh sources conf.sh, which reads the
    /// box's own spira.toml (HOME/XDG, or SPIRA_CONF/SPIRA_TOML) — or not, depending on file
    /// mtimes. A config file on the box is not an input to a tree's verdict: the same tree
    /// must give the same names on any box. This fixture reads the box's config the way
    /// conf.sh does; the rule must not see its `needs-box` label. An explicit environment
    /// override still counts.
    #[test]
    fn the_boxs_config_file_is_not_an_input() {
        let t = TempDir::new("lit-box");
        testkit::write_exe(
            t.path().join("schema.sh"),
            "#!/usr/bin/env bash\ncfg=\"${SPIRA_TOML-${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.toml}\"\nask=\n[ -f \"$cfg\" ] && ask=\"$(sed -n 's/^ask_label = \"\\(.*\\)\"$/\\1/p' \"$cfg\")\"\ncase \"$1\" in\n  names) echo ask ;;\n  name) printf '%s' \"${SPIRA_ASK_LABEL:-${ask:-needs-operator}}\" ;;\n  default) printf '%s' needs-operator ;;\nesac\n",
        );
        t.write("home/.config/spira/spira.toml", "[spira]\nask_label = \"needs-box\"\n");
        let home = t.path().join("home");
        let home = home.to_str().unwrap();
        let schema = t.path().join("schema.sh");
        // Positive control: the fixture does read the box's config when nothing pins it.
        let out = std::process::Command::new(&schema)
            .args(["name", "ask"])
            .env("HOME", home)
            .env_remove("SPIRA_TOML")
            .env_remove("SPIRA_ASK_LABEL")
            .env_remove("XDG_CONFIG_HOME")
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "needs-box");
        let (names, warning) = configured_names_env(&schema, &[("HOME", home)]);
        assert!(warning.is_none(), "{warning:?}");
        assert!(!names.contains(&"needs-box".to_string()), "the box's config reached the rule: {names:?}");
        assert!(names.contains(&"needs-operator".to_string()), "{names:?}");
        let (names, _) = configured_names_env(&schema, &[("HOME", home), ("SPIRA_ASK_LABEL", "needs-env")]);
        assert!(names.contains(&"needs-env".to_string()), "{names:?}");
    }
}
