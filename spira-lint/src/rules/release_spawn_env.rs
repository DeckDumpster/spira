//! `release-spawn-env` — a release binary that spawns `bash` or a `*.sh` script hands the
//! child its own release first on PATH (`spira_config::release_env`), or the script dies
//! reaching `conf.sh` from a bare shell. Contract: DESIGN.md.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust;
use crate::rust_test::{shape, whole_test_files, Shape};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct ReleaseSpawnEnv;

const NAME: &str = "release-spawn-env";
const ALLOW_FILE: &str = "spira-lint/release-spawn-env-allow";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

impl Rule for ReleaseSpawnEnv {
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
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        static SPAWN: OnceLock<Regex> = OnceLock::new();
        let files = crate::scope(tree, self)?;
        let shapes: Vec<(&Entry, Shape)> =
            files.iter().filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();
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
        let spawn = re(&SPAWN, r#"Command::new\([^)\n]*?(?:"(?:bash|sh)"|\.sh")"#);
        let mut out = Vec::new();
        let mut offenders = BTreeSet::new();
        for (e, s) in &shapes {
            if whole.contains(&e.path) || e.path.contains("/tests/") || e.path.ends_with("/tests.rs") {
                continue;
            }
            let Some(src) = tree.content(e) else { continue };
            let cl = rust::classify(src);
            let text = cl.without_comments();
            if text.windows(b"child_path_env".len()).any(|w| w == b"child_path_env") {
                continue;
            }
            for m in spawn.find_iter(&text) {
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
                    message: "spawns bash or a *.sh script without spira_config::release_env::child_path_env — add `.envs(child_path_env_for_process())`".into(),
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
        "A script spawned from a bare shell reaches conf.sh without spira-config on PATH and dies. \
Route the Command through spira_config::release_env::child_path_env_for_process (own release \
first on PATH, SPIRA_RELEASE set). spira-lint/release-spawn-env-allow only shrinks."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Vec<String> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        ReleaseSpawnEnv.check(&tree).unwrap().iter().map(|f| f.to_string()).collect()
    }

    #[test]
    fn a_planted_bare_spawn_is_caught_and_the_routed_one_passes() {
        let t = TempDir::new("rse");
        t.write(ALLOW_FILE, "");
        t.write("a/src/main.rs", "fn f() { let _ = std::process::Command::new(\"incident.sh\").status(); }\n");
        t.write("b/src/main.rs", "fn f() { let _ = Command::new(home.join(\"x.sh\")).status(); }\n");
        t.write("c/src/main.rs", "fn f() { Command::new(\"bash\").envs(child_path_env_for_process()).status(); }\n");
        t.write("d/src/main.rs", "fn f() { Command::new(\"git\").status(); }\n// Command::new(\"bash\")\n#[cfg(test)]\nmod t { fn g() { Command::new(\"bash\"); } }\n");
        let got = run(&t, &["a/src/main.rs", "b/src/main.rs", "c/src/main.rs", "d/src/main.rs"]);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].starts_with("release-spawn-env: a/src/main.rs:1:"));
        assert!(got[1].starts_with("release-spawn-env: b/src/main.rs:1:"));
    }

    #[test]
    fn a_listed_exception_passes_and_a_stale_one_is_refused() {
        let t = TempDir::new("rse-allow");
        t.write("a/src/main.rs", "fn f() { Command::new(\"bash\"); }\n");
        t.write("b/src/main.rs", "fn f() {}\n");
        t.write(ALLOW_FILE, "a/src/main.rs\nb/src/main.rs\n");
        let got = run(&t, &["a/src/main.rs", "b/src/main.rs"]);
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("lists b/src/main.rs"));
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(ReleaseSpawnEnv),
    ]
}
