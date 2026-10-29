//! `config-door` — spira-config is the only door onto the config file: no other crate resolves its
//! path or parses it. Contract: DESIGN.md. `spira/test-spira-config-only-door.sh` runs this rule.

use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust;
use crate::rust_test::{shape, whole_test_files, Shape};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct ConfigDoor;

const NAME: &str = "config-door";
const DOOR: &str = "spira-config/src/";
const OWN_SOURCE: &str = "spira-lint/src/rules/config_door.rs";

/// Files whose `toml::from_str` parses a document that is not the config.
const EXEMPT_TOML_FROM_STR: &[&str] = &[
    "desired-state/src/store.rs",
    "reconciler-flow/src/io.rs",
    "spira-lint/src/rules/lockfile_lint.rs",
    "sim/src/actors.rs",
    "sim/src/fit.rs",
    "spira/sim/src/drive.rs",
];

const USE_INSTEAD: &str = "use spira_config::process::spec() (the path) or spira_config::load (the parse)";

fn parse_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"toml::from_str").expect("static regex"))
}

fn path_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r#"\.join\("spira\.toml"\)|Path::new\("spira\.toml"\)|read_to_string\("spira\.toml"\)|File::open\("spira\.toml"\)|env::var(_os)?\("SPIRA_TOML"\)"#,
        )
        .expect("static regex")
    })
}

/// Byte offset and message of every production-code offence in `src`, `path` being its place in the tree.
fn scan(path: &str, src: &[u8], s: &Shape) -> Vec<(usize, &'static str)> {
    let text = rust::classify(src).without_comments();
    let mut out = Vec::new();
    if !EXEMPT_TOML_FROM_STR.contains(&path) {
        for m in parse_re().find_iter(&text).filter(|m| !s.covers(m.start())) {
            out.push((m.start(), "calls toml::from_str outside spira-config"));
        }
    }
    for m in path_re().find_iter(&text).filter(|m| !s.covers(m.start())) {
        out.push((m.start(), "resolves the config path outside spira-config"));
    }
    out.sort_by_key(|(at, _)| *at);
    out
}

impl Rule for ConfigDoor {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path.ends_with(".rs")
            && e.path.contains("/src/")
            && !e.path.starts_with("target/")
            && !e.path.starts_with(DOOR)
            && e.path != OWN_SOURCE
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = crate::scope(tree, self)?;
        let shapes: Vec<(&Entry, Shape)> =
            files.iter().filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();
        let whole = whole_test_files(tree, &shapes);
        let mut out = Vec::new();
        for (e, s) in &shapes {
            if whole.contains(&e.path) || s.inner_test {
                continue;
            }
            let Some(src) = tree.content(e) else { continue };
            let cl = rust::classify(src);
            for (at, what) in scan(&e.path, src, s) {
                out.push(Finding {
                    rule: NAME,
                    path: e.path.clone(),
                    line: Some(cl.line_of(at)),
                    message: format!("{what} — {USE_INSTEAD}"),
                });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "the config is read only through spira-config: spira_config::process::spec() resolves its path, \
spira_config::load parses it. A parse of a different document belongs in EXEMPT_TOML_FROM_STR \
(spira-lint/src/rules/config_door.rs) with the reason."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(ConfigDoor)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    // `@` stands for the dot of the config file's name, which config-fence refuses to see written out.
    fn run(files: &[(&str, &str)]) -> Vec<String> {
        let t = TempDir::new("cdoor");
        for (p, c) in files {
            t.write(p, &c.replace('@', "."));
        }
        let tree = Tree::from_paths(t.path(), files.iter().map(|(p, _)| *p), std::iter::empty::<&str>());
        ConfigDoor.check(&tree).unwrap().iter().map(|f| f.to_string()).collect()
    }

    #[test]
    fn a_planted_env_read_of_spira_toml_names_file_line_and_entry_point() {
        let got = run(&[
            ("spira-config/src/lib.rs", "pub fn f() { let _ = std::env::var(\"SPIRA_TOML\"); }\n"),
            ("work/src/main.rs", "fn main() {\n    let _ = std::env::var(\"SPIRA_TOML\");\n}\n"),
        ]);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].starts_with("config-door: work/src/main.rs:2: resolves the config path"), "{got:?}");
        assert!(got[0].contains("spira_config::process::spec()") && got[0].contains("spira_config::load"), "{got:?}");
    }

    #[test]
    fn a_planted_second_parser_and_path_join_are_found() {
        let got = run(&[(
            "offender/src/main.rs",
            "fn main() {\n    let t = std::fs::read_to_string(\"spira@toml\").unwrap();\n    let _: toml::Value = toml::from_str(&t).unwrap();\n}\n",
        )]);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].contains(":2:") && got[1].contains(":3: calls toml::from_str"), "{got:?}");
    }

    #[test]
    fn exempt_documents_comments_and_test_code_are_clean() {
        let got = run(&[
            ("sim/src/actors.rs", "fn f() { let _: A = toml::from_str(&t).unwrap(); }\n"),
            ("a/src/lib.rs", "// env::var(\"SPIRA_TOML\")\n#[cfg(test)]\nmod tests {\n    fn t() { let _ = p.join(\"spira@toml\"); }\n}\n"),
            ("gated/src/main.rs", "#[cfg(test)]\nmod tests;\nfn main() {}\n"),
            ("gated/src/tests.rs", "fn t() { let _ = std::env::var(\"SPIRA_TOML\"); }\n"),
        ]);
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn an_exempt_file_still_may_not_resolve_the_path() {
        let got = run(&[("sim/src/actors.rs", "fn f() { let _ = p.join(\"spira@toml\"); }\n")]);
        assert_eq!(got.len(), 1, "{got:?}");
    }
}
