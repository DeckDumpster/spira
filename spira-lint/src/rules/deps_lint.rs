//! `deps-lint` — every external program the harness invokes is declared in `spira/deps.toml`.
//! Ported from `spira/deps-lint.sh`. Contract: DESIGN.md.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;
use serde::Deserialize;

use crate::{direct_child, Entry, Finding, LintError, Rule, Tree};

pub struct DepsLint;

const NAME: &str = "deps-lint";
pub const MANIFEST: &str = "spira/deps.toml";

/// Standard system utilities and shell functions probed with `command -v` that are not
/// harness dependencies. A standard utility is added here, not to the manifest.
pub const SYSTEM_ALLOW: &[&str] = &[
    "bash", "sh", "dash", "env", "true", "false", "sort", "cut", "awk", "gawk", "sed", "grep", "find", "cat",
    "echo", "printf", "date", "kill", "sleep", "wait", "read", "test", "mkdir", "rmdir", "rm", "mv", "cp", "ln", "df",
    "stat", "sha256sum", "wc", "tr", "head", "tail", "tee", "diff", "patch", "timeout", "curl", "gcc", "nc", "ps",
    "dirname", "basename",
    "setsid", "pgrep", "fuser", "script", "systemctl", "systemd-run", "stty",
    "nodejs", // an alternate name for node on some platforms
    "gate_meter", "yield_note", "fayth_names", // shell functions, not programs
];

#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    dep: Vec<Dep>,
}

#[derive(Deserialize)]
struct Dep {
    name: String,
}

/// The declared program names. An unreadable, malformed or empty manifest is a refusal.
pub fn declared(text: Option<String>) -> Result<BTreeSet<String>, LintError> {
    let bad = |reason: String| LintError::BadAllow { file: MANIFEST.into(), line: 0, reason };
    let text = text.ok_or_else(|| bad("not found — refusing to report clean".into()))?;
    let m: Manifest = toml::from_str(&text).map_err(|e| bad(format!("does not parse: {e}")))?;
    let names: BTreeSet<String> = m.dep.into_iter().map(|d| d.name).collect();
    if names.is_empty() {
        return Err(bad("declares no [[dep]] — refusing to report clean".into()));
    }
    Ok(names)
}

fn cv_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"command\s+-v\s+([a-z][a-z0-9_.-]*[a-z0-9_-])").expect("static regex"))
}

fn rust_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"Command::new\("([a-z][a-z0-9_-]+)"\)"#).expect("static regex"))
}

/// Programs probed with `command -v` in shell text: (line, program). Comment lines are
/// skipped, and so is a probe that sits inside an open quote on its line (odd count of `"`
/// or `'` before it) — a message naming the idiom, not a probe.
pub fn shell_probes(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        for c in cv_re().captures_iter(line) {
            let before = &line[..c.get(0).unwrap().start()];
            if before.matches('"').count() % 2 == 1 || before.matches('\'').count() % 2 == 1 {
                continue;
            }
            // A spira/ script (`<x>.sh`, `<x>.py`) is the release's own, found on the
            // launcher's PATH (sp-gypjk) — not an external program to declare.
            if c[1].ends_with(".sh") || c[1].ends_with(".py") {
                continue;
            }
            out.push((i + 1, c[1].to_string()));
        }
    }
    out
}

/// Programs spawned by a literal `Command::new("…")` in Rust text: (line, program).
pub fn rust_spawns(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        for c in rust_re().captures_iter(line) {
            out.push((i + 1, c[1].to_string()));
        }
    }
    out
}

impl Rule for DepsLint {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(MANIFEST)
    }

    /// `spira/*.sh` directly in spira/, and every `*.rs`.
    fn applies_to(&self, e: &Entry) -> bool {
        e.path.ends_with(".rs") || direct_child(&e.path, "spira").is_some_and(|b| b.ends_with(".sh"))
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let known = declared(tree.text_of(MANIFEST))?;
        let ok = |p: &str| known.contains(p) || SYSTEM_ALLOW.contains(&p);
        let mut out = Vec::new();
        for e in crate::scope(tree, self)? {
            let Some(c) = tree.content(e) else { continue };
            let text = String::from_utf8_lossy(c);
            let (kind, hits) = if e.path.ends_with(".rs") {
                ("Command::new", rust_spawns(&text))
            } else {
                ("command -v", shell_probes(&text))
            };
            for (line, prog) in hits.into_iter().filter(|(_, p)| !ok(p)) {
                out.push(Finding {
                    rule: NAME,
                    path: e.path.clone(),
                    line: Some(line),
                    message: format!("{prog}: {kind} of a program {MANIFEST} does not declare"),
                });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "Every external program is declared in spira/deps.toml (doctor.sh reads it); a standard \
system utility goes in deps-lint's SYSTEM_ALLOW instead."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const DEPS: &str = "[[dep]]\nname = \"bd\"\ntier = \"runtime\"\n\n[[dep]]\nname = \"git\"\n";
    // Assembled so this source carries no literal spawn of an undeclared program.
    const PLANT: &str = concat!("spira-no-such", "-prog-xyz");

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        DepsLint.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_planted_shell_probe_is_caught_then_clears() {
        let t = TempDir::new("deps-sh");
        t.write(MANIFEST, DEPS);
        t.write("spira/planted.sh", &format!("#!/bin/sh\ncommand -v {PLANT} >/dev/null\ncommand -v bd\ncommand -v sed\n"));
        let got = run(&t, &[MANIFEST, "spira/planted.sh"]).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].starts_with(&format!("deps-lint: spira/planted.sh:2: {PLANT}:")), "{got:?}");
        t.write("spira/planted.sh", "#!/bin/sh\ncommand -v bd\n");
        assert!(run(&t, &[MANIFEST, "spira/planted.sh"]).unwrap().is_empty());
    }

    #[test]
    fn a_planted_rust_spawn_is_caught_then_clears() {
        let t = TempDir::new("deps-rs");
        t.write(MANIFEST, DEPS);
        t.write("src/main.rs", &format!("fn main() {{ let _ = std::process::Command::new(\"{PLANT}\"); }}\n"));
        let got = run(&t, &[MANIFEST, "src/main.rs"]).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].starts_with(&format!("deps-lint: src/main.rs:1: {PLANT}:")), "{got:?}");
        t.write("src/main.rs", "fn main() { let _ = std::process::Command::new(\"git\"); }\n");
        assert!(run(&t, &[MANIFEST, "src/main.rs"]).unwrap().is_empty());
    }

    #[test]
    fn a_spira_script_probe_is_not_an_external_program() {
        assert!(shell_probes("if command -v land-build-ensure.sh >/dev/null; then :; fi\ncommand -v x.py\n").is_empty());
        assert_eq!(shell_probes("command -v spira-config >/dev/null\n"), vec![(1, "spira-config".to_string())]);
    }

    #[test]
    fn comments_and_quoted_probes_are_not_probes() {
        let text = format!("# command -v {PLANT}\necho \"try command -v {PLANT}\"\n");
        assert!(shell_probes(&text).is_empty());
        assert_eq!(shell_probes(&format!("x && command -v {PLANT}\n")), vec![(1, PLANT.to_string())]);
    }

    #[test]
    fn nested_shell_is_out_of_scope() {
        let t = TempDir::new("deps-nest");
        t.write(MANIFEST, DEPS);
        t.write("spira/hooks/x.sh", &format!("command -v {PLANT}\n"));
        t.write("spira/y.sh", "true\n");
        assert!(run(&t, &[MANIFEST, "spira/hooks/x.sh", "spira/y.sh"]).unwrap().is_empty());
    }

    #[test]
    fn a_missing_or_empty_manifest_refuses_to_report_clean() {
        let t = TempDir::new("deps-none");
        t.write("spira/y.sh", "true\n");
        assert!(matches!(run(&t, &["spira/y.sh"]), Err(LintError::BadAllow { .. })));
        t.write(MANIFEST, "# nothing\n");
        assert!(matches!(run(&t, &[MANIFEST, "spira/y.sh"]), Err(LintError::BadAllow { .. })));
        t.write(MANIFEST, "[[dep]\n");
        assert!(matches!(run(&t, &[MANIFEST, "spira/y.sh"]), Err(LintError::BadAllow { .. })));
    }
}
