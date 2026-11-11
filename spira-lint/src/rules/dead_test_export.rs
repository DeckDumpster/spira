//! `dead-test-export` — a suite, testlib or fixture assigns a `SPIRA_*` variable that no
//! process reads. Contract: DESIGN.md.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::{rust, shell};
use crate::rust_test::{shape, whole_test_files};
use crate::{registered_keys, Entry, Finding, LintError, Rule, Tree};

pub struct DeadTestExport;

const NAME: &str = "dead-test-export";
const ALLOW: &str = "spira-lint/dead-test-export-allow";

fn is_test_side(path: &str) -> bool {
    path.ends_with(".sh")
        && (path.starts_with("spira/test-") || path.starts_with("spira/testlib"))
}

fn is_generated(path: &str) -> bool {
    path.contains(".generated.") || path.starts_with("spira/conf.d/")
}

fn spira_name(s: &str) -> bool {
    s.strip_prefix("SPIRA_").is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'))
}

/// `(line, NAME)` of every `SPIRA_*` assignment that is a command's own environment or an
/// `export`/`declare -x` argument. A `tl_config NAME=…` argument and a `case` pattern are not.
fn assignments(src: &[u8]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for cmd in shell::parse(src) {
        let (env, rest) = cmd.split_env();
        let exported = rest.first().is_some_and(|w| matches!(w.raw.as_str(), "export" | "declare" | "typeset"));
        let args = if exported { &rest[1..] } else { &rest[..0] };
        for w in env.iter().chain(args) {
            if let Some(n) = w.assignment_name().filter(|n| spira_name(n) && !w.raw.ends_with("=*")) {
                out.push((w.line, n.to_string()));
            }
        }
    }
    out
}

fn shell_read_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\$\{?!?(SPIRA_[A-Z0-9_]+)").expect("static regex"))
}

/// `export X="${X:-0}"` reads X only to assign it back, so it is not a reader of X.
fn self_assign_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^\s*(?:export\s+)?(SPIRA_[A-Z0-9_]+)=").expect("static regex"))
}

fn env_call_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"(?:^|[^A-Za-z0-9_])[A-Za-z0-9_:]*env[A-Za-z0-9_]*\s*\(\s*"(SPIRA_[A-Z0-9_]+)"\s*\)"#)
            .expect("static regex")
    })
}

fn literal_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#""(SPIRA_[A-Z0-9_]+)""#).expect("static regex"))
}

/// Every name some process reads: `$NAME` in a non-generated shell file (not the registry's own defaults) or unit, an
/// environment call in non-test Rust, and — for a name that is not a config key, which
/// `cfg()` resolves without the environment — any string literal naming it there.
fn readers(tree: &Tree, keys: &BTreeSet<String>) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let rs: Vec<&Entry> = tree
        .entries
        .iter()
        .filter(|e| e.path.ends_with(".rs") && !e.path.starts_with("target/"))
        .filter(|e| tree.content(e).is_some_and(|c| c.windows(6).any(|w| w == b"SPIRA_")))
        .collect();
    let shapes: Vec<_> = rs.iter().filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();
    let whole = whole_test_files(tree, &shapes);
    for (e, s) in &shapes {
        let Some(src) = tree.content(e) else { continue };
        if whole.contains(&e.path) {
            continue;
        }
        let text = rust::classify(src).without_comments();
        let offset_of = |m: &regex::bytes::Match| s.covers(m.start());
        for c in env_call_re().captures_iter(&text) {
            if !offset_of(&c.get(0).unwrap()) {
                seen.insert(String::from_utf8_lossy(&c[1]).into_owned());
            }
        }
        for c in literal_re().captures_iter(&text) {
            let n = String::from_utf8_lossy(&c[1]).into_owned();
            if !keys.contains(&n) && !offset_of(&c.get(0).unwrap()) {
                seen.insert(n);
            }
        }
    }
    for e in tree.entries.iter().filter(|e| !e.path.ends_with(".rs") && !is_generated(&e.path)) {
        let in_scope = ["spira/", "systemd/", "hooks/"].iter().any(|d| e.path.starts_with(d))
            && !(e.path.ends_with(".md") || e.path.ends_with(".toml"));
        let Some(src) = tree.content(e).filter(|_| in_scope) else { continue };
        for line in src.split(|&b| b == b'\n') {
            if line.trim_ascii_start().starts_with(b"#") {
                continue;
            }
            let own = self_assign_re().captures(line).map(|c| c[1].to_vec());
            seen.extend(
                shell_read_re()
                    .captures_iter(line)
                    .filter(|c| own.as_deref() != Some(&c[1]))
                    .map(|c| String::from_utf8_lossy(&c[1]).into_owned()),
            );
        }
    }
    seen
}

fn parse_allow(text: &str) -> BTreeMap<(String, String), usize> {
    text.lines()
        .enumerate()
        .filter_map(|(i, l)| {
            let t = l.trim();
            let f: Vec<&str> = t.split_whitespace().collect();
            (!t.starts_with('#')).then_some(())?;
            match f[..] {
                [name, path, ..] => Some(((name.to_string(), path.to_string()), i + 1)),
                _ => None,
            }
        })
        .collect()
}

impl Rule for DeadTestExport {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        is_test_side(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = crate::scope(tree, self)?;
        let keys = registered_keys(tree);
        let read = readers(tree, &keys);
        let allow = parse_allow(&tree.read_text(ALLOW));
        let mut used = BTreeSet::new();
        let mut out = Vec::new();
        for e in files {
            let Some(src) = tree.content(e) else { continue };
            for (line, name) in assignments(src) {
                if read.contains(&name) {
                    continue;
                }
                let key = (name.clone(), e.path.clone());
                if allow.contains_key(&key) {
                    used.insert(key);
                    continue;
                }
                let why = if keys.contains(&name) {
                    "a config key is resolved by spira-config, which never reads the environment — set it with tl_config"
                } else {
                    "nothing reads it, so the test runs on the default"
                };
                out.push(Finding {
                    rule: NAME,
                    path: e.path.clone(),
                    line: Some(line),
                    message: format!("{name} is assigned but never read: {why}"),
                });
            }
        }
        for ((name, path), line) in &allow {
            if !used.contains(&(name.clone(), path.clone())) {
                out.push(Finding {
                    rule: NAME,
                    path: ALLOW.to_string(),
                    line: Some(*line),
                    message: format!("lists {name} in {path}, which no longer assigns an unread variable — remove the line"),
                });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "Delete the assignment, or make the read real: set a config key with tl_config, and read an \
environment-only variable in the code under test. spira-lint/dead-test-export-allow only shrinks."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(DeadTestExport)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Vec<String> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        DeadTestExport.check(&tree).unwrap().iter().map(|f| f.to_string()).collect()
    }

    fn base(name: &str) -> TempDir {
        let t = TempDir::new(name);
        t.write("spira/conf.d/SPIRA_CALL_JITTER", "TYPE=u64\n");
        t.write(ALLOW, "");
        t.write("spira/conf.d.defaults.generated.sh", ": \"${SPIRA_CALL_JITTER:=20}\"\n");
        t.write("dwarf/src/seam.rs", "fn f() { cfg(\"SPIRA_CALL_JITTER\"); std::env::var(\"SPIRA_READ_BY_RUST\"); }\n");
        t
    }

    #[test]
    fn an_export_of_a_config_key_only_spira_config_reads_is_refused() {
        let t = base("dte-key");
        t.write("spira/testlib.sh", "#!/bin/bash\nexport SPIRA_CALL_JITTER=0\n");
        let got = run(&t, &["spira/testlib.sh", "dwarf/src/seam.rs", "spira/conf.d/SPIRA_CALL_JITTER", "spira/conf.d.defaults.generated.sh"]);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("spira/testlib.sh:2") && got[0].contains("SPIRA_CALL_JITTER"), "{got:?}");
    }

    #[test]
    fn exports_something_reads_and_tl_config_arguments_pass() {
        let t = base("dte-ok");
        t.write(
            "spira/test-x.sh",
            "export SPIRA_READ_BY_RUST=1\nSPIRA_SH=1 cmd\ntl_config SPIRA_NOBODY=1\necho \"$SPIRA_SH\"\n",
        );
        let got = run(&t, &["spira/test-x.sh", "dwarf/src/seam.rs", "spira/conf.d/SPIRA_CALL_JITTER"]);
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn a_self_defaulting_export_is_not_its_own_reader() {
        let t = base("dte-self");
        t.write("spira/testlib.sh", "export SPIRA_CALL_JITTER=\"${SPIRA_CALL_JITTER:-0}\"\n");
        let got = run(&t, &["spira/testlib.sh", "spira/conf.d/SPIRA_CALL_JITTER"]);
        assert_eq!(got.len(), 1, "{got:?}");
    }

    #[test]
    fn a_stale_allow_line_is_refused() {
        let t = base("dte-allow");
        t.write(ALLOW, "SPIRA_GONE spira/test-x.sh\n");
        t.write("spira/test-x.sh", "echo hi\n");
        let got = run(&t, &["spira/test-x.sh", "spira/conf.d/SPIRA_CALL_JITTER"]);
        assert_eq!(got.len(), 1, "{got:?}");
    }
}
