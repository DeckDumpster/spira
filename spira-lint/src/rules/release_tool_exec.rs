//! `release-tool-exec` — a release-shipped tool is exec'd through
//! `spira_config::release_env::tool`, never by bare name: a unit or shell whose PATH lacks the
//! release dies with exit 127. Contract: DESIGN.md.

use std::collections::BTreeSet;

use regex::bytes::Regex;

use crate::lex::rust;
use crate::rust_test::{shape, whole_test_files, Shape};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct ReleaseToolExec;

const NAME: &str = "release-tool-exec";
const ALLOW_FILE: &str = "spira-lint/release-tool-exec-allow";
const EXTRA_TOOLS: &[&str] = &["bd"];
const NOT_TOOLS: &[&str] = &["spira", "gate"];

impl Rule for ReleaseToolExec {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path.ends_with(".rs")
            && !e.path.starts_with("testkit/")
            && !e.path.starts_with("target/")
            && !e.path.starts_with("spira-config/src/release_env.rs")
            && !e.path.starts_with("spira-lint/")
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = crate::scope(tree, self)?;
        let tools = tool_names(tree);
        let alt = tools.iter().map(|t| regex::escape(t)).collect::<Vec<_>>().join("|");
        let re = &build(&alt);
        let shapes: Vec<(&Entry, Shape)> = files.iter().filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();
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
        let mut offenders = BTreeSet::new();
        for (e, s) in &shapes {
            if whole.contains(&e.path) || e.path.contains("/tests/") || e.path.ends_with("/tests.rs") {
                continue;
            }
            let Some(src) = tree.content(e) else { continue };
            let cl = rust::classify(src);
            let text = cl.without_comments();
            for m in re.find_iter(&text) {
                if s.covers(m.start()) {
                    continue;
                }
                offenders.insert(e.path.clone());
                if allow.iter().any(|(_, p)| p == &e.path) {
                    continue;
                }
                out.push(Finding {
                    rule: NAME,
                    path: e.path.clone(),
                    line: Some(cl.line_of(m.start())),
                    message: format!("execs a release tool by bare name ({}) — use spira_config::release_env::tool(\"…\")", String::from_utf8_lossy(m.as_bytes()).split_whitespace().collect::<Vec<_>>().join(" ").chars().take(70).collect::<String>()),
                });
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
        "A tool exec'd by bare name fails with exit 127 where the release is not on PATH. Name the \
program with spira_config::release_env::tool(\"<name>\") (also inside `timeout` argument lists). \
spira-lint/release-tool-exec-allow only shrinks."
    }
}

fn build(alt: &str) -> Regex {
    let t = format!(r#""(?:{alt})""#);
    Regex::new(&format!(
        r#"Command::new\({t}\)|Command::new\("timeout"\)\s*\.args\(\[[^,\]\n]*,\s*{t}|Command::new\("timeout"\)\s*\.args\(\w+\)\s*\.args\(\[{t}|unwrap_or_else\(\|\| {t}\.to_string\(\)\)|"\d+"(?:\.to_string\(\))?\s*,\s*{t}|\.arg\("\d+"\)\s*\.arg\({t}\)|PathBuf::from\({t}\)|\bbin:\s*{t}|\b\w*run\w*\(\s*&?\[\s*{t}\s*[,\]]|\bargv\s*=\s*&?\[\s*{t}"#
    ))
    .expect("static regex")
}

/// Every `[[bin]]` the workspace builds (what the release ships in `bin/`), plus [`EXTRA_TOOLS`].
fn tool_names(tree: &Tree) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = EXTRA_TOOLS.iter().map(|s| s.to_string()).collect();
    for e in &tree.entries {
        let p = e.path.as_str();
        let comps: Vec<&str> = p.split('/').collect();
        match comps.as_slice() {
            [krate, "src", "main.rs"] => {
                names.insert(krate.to_string());
            }
            [.., "src", "bin", f] if f.ends_with(".rs") => {
                names.insert(f.trim_end_matches(".rs").to_string());
            }
            [.., "Cargo.toml"] => {
                let toml = tree.read_text(p);
                let mut in_bin = false;
                for l in toml.lines() {
                    let l = l.trim();
                    if l.starts_with('[') {
                        in_bin = l == "[[bin]]";
                    } else if in_bin && l.starts_with("name") {
                        if let Some(n) = l.split('"').nth(1) {
                            names.insert(n.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for n in NOT_TOOLS {
        names.remove(*n);
    }
    names
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(ReleaseToolExec),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Vec<String> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        ReleaseToolExec.check(&tree).unwrap().iter().map(|f| f.to_string()).collect()
    }

    #[test]
    fn every_bare_shape_is_caught_and_the_routed_and_test_ones_pass() {
        let t = TempDir::new("rte");
        t.write(ALLOW_FILE, "");
        t.write("spira-lc/src/main.rs", "fn main() {}\n");
        t.write("queue/src/main.rs", "fn main() {}\n");
        t.write("a/src/lib.rs", "fn f() { Command::new(\"queue\").status(); }\n");
        t.write("b/src/lib.rs", "fn f() { Command::new(\"timeout\").arg(\"30\").arg(\"spira-lc\").status(); }\n");
        t.write("c/src/lib.rs", "fn f() { let a = vec![\"30\".to_string(), \"spira-lc\".into()]; }\n");
        t.write("d/src/lib.rs", "fn f() { S { spira_lc: PathBuf::from(\"spira-lc\"), dir: PathBuf::from(\"spira\") }; }\n");
        t.write("e/src/lib.rs", "fn f() { Command::new(\"timeout\").args([\"5\", \"queue\"]); }\n");
        t.write("h/src/lib.rs", "fn f() { Command::new(\"timeout\").arg(\"5\").arg(&gh).args([\"release\", \"list\"]); }\n");
        t.write("i/src/lib.rs", "fn f() { host.run(&[\"queue\", \"set\"]); }\n");
        t.write("j/src/lib.rs", "fn f() { x.run(&[\"git\", \"queue\"]); y(&[\"queue\", \"b\"]); }\n");
        t.write("g/src/lib.rs", "fn f() { Command::new(tool(\"queue\")); Command::new(\"git\"); }\n// Command::new(\"queue\")\n#[cfg(test)]\nmod t { fn g() { Command::new(\"queue\"); } }\n");
        let got = run(&t, &["spira-lc/src/main.rs", "queue/src/main.rs", "a/src/lib.rs", "b/src/lib.rs", "c/src/lib.rs", "d/src/lib.rs", "e/src/lib.rs", "g/src/lib.rs", "h/src/lib.rs", "i/src/lib.rs"]);
        let files: Vec<&str> = got.iter().map(|g| g.split(": ").nth(1).unwrap().split(':').next().unwrap()).collect();
        assert_eq!(files, ["a/src/lib.rs", "b/src/lib.rs", "c/src/lib.rs", "d/src/lib.rs", "e/src/lib.rs", "i/src/lib.rs"], "{got:?}");
    }

    #[test]
    fn a_listed_exception_passes_and_a_stale_one_is_refused() {
        let t = TempDir::new("rte-allow");
        t.write("queue/src/main.rs", "fn main() {}\n");
        t.write("a/src/lib.rs", "fn f() { Command::new(\"queue\"); }\n");
        t.write("b/src/lib.rs", "fn f() {}\n");
        t.write(ALLOW_FILE, "a/src/lib.rs\nb/src/lib.rs\n");
        let got = run(&t, &["queue/src/main.rs", "a/src/lib.rs", "b/src/lib.rs"]);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("lists b/src/lib.rs"));
    }
}
