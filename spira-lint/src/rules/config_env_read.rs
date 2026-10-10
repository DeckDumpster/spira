//! `config-env-read` — no binary reads a registered config key (`spira/conf.d`) straight
//! from the environment; it resolves it through `spira_config::process::cfg`
//! (law-a-binary-resolves-the-config-it-reads). Contract: DESIGN.md.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust;
use crate::rust_test::{shape, whole_test_files, Shape};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct ConfigEnvRead;

const NAME: &str = "config-env-read";
const EXCEPTIONS: &str = "spira-lint/config-env-read-exceptions";
const OWN_SOURCE: &str = "spira-lint/src/rules/config_env_read.rs";
const CONF_D: &str = "spira/conf.d/";

fn registered_keys(tree: &Tree) -> BTreeSet<String> {
    tree.entries
        .iter()
        .filter_map(|e| e.path.strip_prefix(CONF_D))
        .filter(|k| !k.contains('/') && !k.is_empty())
        .map(str::to_string)
        .collect()
}

fn call_re() -> &'static Regex {
    static CALL: OnceLock<Regex> = OnceLock::new();
    CALL.get_or_init(|| {
        Regex::new(r#"(?:^|[^A-Za-z0-9_:])(?:(?:std::)?env::var(?:_os)?|nonempty_env)\s*\(\s*"([A-Za-z0-9_]+)"\s*\)"#)
            .expect("static regex")
    })
}

/// Byte offset and key of every raw read of a registered key in `src`.
fn scan(src: &[u8], keys: &BTreeSet<String>) -> Vec<(usize, String)> {
    let cl = rust::classify(src);
    let text = cl.without_comments();
    call_re()
        .captures_iter(&text)
        .filter_map(|c| {
            let key = String::from_utf8_lossy(&c[1]).into_owned();
            keys.contains(&key).then(|| (c.get(0).unwrap().start(), key))
        })
        .collect()
}

impl Rule for ConfigEnvRead {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(EXCEPTIONS)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path.ends_with(".rs") && !e.path.starts_with("target/")
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = crate::scope(tree, self)?;
        let keys = registered_keys(tree);
        if keys.is_empty() {
            return Err(LintError::Refused(format!("{CONF_D} lists no registered keys — nothing to check against")));
        }
        let shapes: Vec<(&Entry, Shape)> =
            files.iter().filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();
        let whole = whole_test_files(tree, &shapes);

        let mut out = Vec::new();
        let mut table: Vec<(usize, String)> = Vec::new();
        for (i, l) in tree.read_text(EXCEPTIONS).lines().enumerate() {
            let t = l.trim();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            let mut parts = t.splitn(2, char::is_whitespace);
            let path = parts.next().unwrap_or_default().to_string();
            if parts.next().map_or(true, |r| r.trim().is_empty()) {
                out.push(Finding {
                    rule: NAME,
                    path: EXCEPTIONS.to_string(),
                    line: Some(i + 1),
                    message: format!("{path} carries no reason — an exception names why it cannot resolve through config"),
                });
            }
            table.push((i + 1, path));
        }

        let mut offenders = BTreeSet::new();
        for (e, s) in &shapes {
            if e.path == OWN_SOURCE {
                continue;
            }
            let Some(src) = tree.content(e) else { continue };
            let cl = rust::classify(src);
            let file_is_test = whole.contains(&e.path);
            let hits: Vec<(usize, String)> =
                scan(src, &keys).into_iter().filter(|(at, _)| !(file_is_test || s.covers(*at))).collect();
            if hits.is_empty() {
                continue;
            }
            offenders.insert(e.path.clone());
            if table.iter().any(|(_, p)| p == &e.path) {
                continue;
            }
            for (at, key) in hits {
                out.push(Finding {
                    rule: NAME,
                    path: e.path.clone(),
                    line: Some(cl.line_of(at)),
                    message: format!("{key} is a registered config key read from the environment — resolve it through spira_config::process::cfg"),
                });
            }
        }
        for (line, p) in &table {
            if !offenders.contains(p) {
                out.push(Finding {
                    rule: NAME,
                    path: EXCEPTIONS.to_string(),
                    line: Some(*line),
                    message: format!("lists {p}, which reads no registered key from the environment — remove the line (the table only shrinks)"),
                });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "Resolve a registered key through spira_config::process::cfg (law-a-binary-resolves-the-config-it-reads); \
a read that cannot (bootstrap, a hermetic stage that must not see config) is listed in \
spira-lint/config-env-read-exceptions with its reason. The table only shrinks."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        ConfigEnvRead.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    fn fixture(name: &str, exceptions: &str) -> TempDir {
        let t = TempDir::new(name);
        t.write("spira/conf.d/SPIRA_RUN", "TYPE=string\n");
        t.write(EXCEPTIONS, exceptions);
        t
    }

    const FILES: [&str; 3] = ["spira/conf.d/SPIRA_RUN", "a/src/main.rs", EXCEPTIONS];

    #[test]
    fn a_planted_raw_read_of_a_registered_key_is_found() {
        let t = fixture("cer-red", "");
        t.write(
            "a/src/main.rs",
            "fn run() -> Option<String> {\n    std::env::var_os(\"SPIRA_RUN\").map(|v| v.to_string_lossy().into_owned())\n}\n\
             fn b() -> String { nonempty_env(\"SPIRA_RUN\") }\n",
        );
        let got = run(&t, &FILES).unwrap();
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].starts_with("config-env-read: a/src/main.rs:2: SPIRA_RUN"), "{got:?}");
        assert!(got[1].contains(":4:"), "{got:?}");
    }

    #[test]
    fn an_unregistered_key_a_comment_and_test_code_are_clean() {
        let t = fixture("cer-green", "");
        t.write(
            "a/src/main.rs",
            "// env::var(\"SPIRA_RUN\")\nfn f() { let _ = std::env::var(\"HOME\"); let _ = env::var(\"SPIRA_BD\"); }\n\
             #[cfg(test)]\nmod tests {\n    #[test]\n    fn t() { let _ = std::env::var(\"SPIRA_RUN\"); }\n}\n",
        );
        assert!(run(&t, &FILES).unwrap().is_empty());
    }

    #[test]
    fn a_listed_file_passes_and_a_stale_or_reasonless_line_is_refused() {
        let t = fixture("cer-table", "a/src/main.rs bootstrap: runs before config exists\nb/src/lib.rs\nc/src/lib.rs hermetic\n");
        t.write("a/src/main.rs", "fn f() { let _ = std::env::var(\"SPIRA_RUN\"); }\n");
        t.write("b/src/lib.rs", "fn f() { let _ = std::env::var(\"SPIRA_RUN\"); }\n");
        t.write("c/src/lib.rs", "fn f() {}\n");
        let got = run(&t, &["spira/conf.d/SPIRA_RUN", "a/src/main.rs", "b/src/lib.rs", "c/src/lib.rs", EXCEPTIONS]).unwrap();
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].contains(":2: b/src/lib.rs carries no reason"), "{got:?}");
        assert!(got[1].contains(":3: lists c/src/lib.rs"), "{got:?}");
    }

    #[test]
    fn no_registry_is_a_refusal_not_a_pass() {
        let t = TempDir::new("cer-noreg");
        t.write("a/src/main.rs", "fn f() {}\n");
        assert!(run(&t, &["a/src/main.rs"]).is_err());
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(ConfigEnvRead),
    ]
}
