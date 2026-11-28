//! `lc-serve-no-spawn` — spira-lc starts a process only through `spira-lc/src/proc.rs`, whose
//! door refuses inside lc-serve. Contract: DESIGN.md. No allow file and no override marker.

use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust;
use crate::rust_test::{shape, whole_test_files, Shape};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct LcServeNoSpawn;

const NAME: &str = "lc-serve-no-spawn";
const DOOR: &str = "spira-lc/src/proc.rs";

fn in_scope(path: &str) -> bool {
    path.starts_with("spira-lc/src/") && path.ends_with(".rs") && path != DOOR
}

fn start_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"\bCommand::new\b|\bstd::process::(?:Command\b|\{[^}]*\bCommand\b)|\bbounded::(?:bounded(?:_for)?\b|\{)|\bSysProc\b").expect("static regex")
    })
}

/// The line and text of every process start in one source, outside the test regions.
pub fn scan(src: &[u8], in_test: &dyn Fn(usize) -> bool) -> Vec<(usize, String)> {
    let cl = rust::classify(src);
    let code = cl.code_only();
    let mut out: Vec<(usize, String)> = Vec::new();
    for m in start_re().find_iter(&code) {
        let line = cl.line_of(m.start());
        if !in_test(m.start()) && !out.iter().any(|(l, _)| *l == line) {
            out.push((line, String::from_utf8_lossy(&code[m.start()..m.end()]).into_owned()));
        }
    }
    out
}

impl Rule for LcServeNoSpawn {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        in_scope(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files: Vec<&Entry> = tree.entries.iter().filter(|e| in_scope(&e.path)).collect();
        let shapes: Vec<(&Entry, Shape)> = files.iter().filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();
        let whole = whole_test_files(tree, &shapes);
        let mut out = Vec::new();
        for (e, s) in &shapes {
            if whole.contains(&e.path) {
                continue;
            }
            let Some(src) = tree.content(e) else { continue };
            for (line, what) in scan(src, &|at| s.regions.iter().any(|(a, b)| *a <= at && at < *b)) {
                out.push(Finding {
                    rule: NAME,
                    path: e.path.clone(),
                    line: Some(line),
                    message: format!("`{what}` starts a process outside {DOOR}; build it with `proc::command`, `proc::bounded`, `proc::bounded_for` or `proc::sys`"),
                });
            }
        }
        out.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "lc-serve starts no child process: every start in spira-lc goes through spira-lc/src/proc.rs, which refuses \
while serving. Build the command with `proc::command`, `proc::bounded` or `proc::bounded_for`, and a process port with `proc::sys`; there is no \
allow file and no marker."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(LcServeNoSpawn)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Vec<String> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        LcServeNoSpawn.check(&tree).unwrap().iter().map(|f| f.to_string()).collect()
    }

    #[test_id(t1ai8wi)]
    #[test]
    fn a_planted_start_in_spira_lc_is_found_and_named() {
        let t = TempDir::new("lcns-planted");
        t.write("spira-lc/src/x.rs", "fn f() {\n    std::process::Command::new(\"git\");\n}\n");
        let got = run(&t, &["spira-lc/src/x.rs"]);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("x.rs") && got[0].contains(":2") && got[0].contains("proc::command"), "{got:?}");
    }

    #[test_id(t75h3uj)]
    #[test]
    fn every_form_of_start_is_found() {
        let t = TempDir::new("lcns-forms");
        t.write(
            "spira-lc/src/x.rs",
            "use std::process::{Command, Stdio};\nfn a() { spira_config::bounded::bounded(\"git\"); }\nfn b() { bounded::bounded_for(\"git\", \"1\"); }\nuse spira_config::bounded::{bounded, bounded_for};\nfn c() { Command::new(\"x\"); }\n",
        );
        assert_eq!(run(&t, &["spira-lc/src/x.rs"]).len(), 5);
    }

    #[test_id(tzlv2dl)]
    #[test]
    fn a_system_process_port_built_in_spira_lc_is_found() {
        let t = TempDir::new("lcns-port");
        t.write("spira-lc/src/x.rs", "use runtime::ports::SysProc;\nfn f() { let p = Arc::new(runtime::ports::SysProc); }\n#[cfg(test)]\nmod tests {\n    fn g() { SysProc; }\n}\n");
        let got = run(&t, &["spira-lc/src/x.rs"]);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].contains(":1") && got[1].contains(":2") && got[1].contains("proc::sys"), "{got:?}");
    }

    #[test_id(tevwz6r)]
    #[test]
    fn test_code_the_door_comments_and_strings_pass() {
        let t = TempDir::new("lcns-exempt");
        let planted = "std::process::Command::new(\"git\");";
        t.write("spira-lc/src/x.rs", &format!("fn f() {{}}\n#[cfg(test)]\nmod tests {{\n    fn g() {{ {planted} }}\n}}\n// {planted}\nconst S: &str = \"Command::new(git)\";\nfn h() {{ proc::command(\"git\"); proc::bounded(\"git\"); crate::facts::bounded(\"x\"); }}\n"));
        t.write("spira-lc/src/proc.rs", &format!("fn door() {{ {planted} }}\n"));
        t.write("spira-lc/src/serve/tests/mod.rs", &format!("fn g() {{ {planted} }}\n"));
        t.write("spira-lc/src/serve.rs", "#[cfg(test)]\nmod tests;\n");
        assert_eq!(run(&t, &["spira-lc/src/x.rs", "spira-lc/src/proc.rs", "spira-lc/src/serve/tests/mod.rs", "spira-lc/src/serve.rs"]), Vec::<String>::new());
    }

    #[test_id(thwnk18)]
    #[test]
    fn a_start_outside_spira_lc_is_not_this_rules_business() {
        let t = TempDir::new("lcns-scope");
        t.write("other/src/x.rs", "fn f() { std::process::Command::new(\"git\"); }\n");
        assert!(run(&t, &["other/src/x.rs"]).is_empty());
    }

    #[test_id(tihzpbv)]
    #[test]
    fn the_door_itself_would_be_a_finding_were_it_in_scope() {
        let hits = scan(b"fn door() { Command::new(p) }\n", &|_| false);
        assert_eq!(hits.len(), 1);
        assert!(!in_scope(DOOR) && in_scope("spira-lc/src/bd.rs"));
    }

    fn rust_files_under(root: &std::path::Path, rel: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut dirs = vec![rel.to_string()];
        while let Some(d) = dirs.pop() {
            for e in std::fs::read_dir(root.join(&d)).unwrap() {
                let e = e.unwrap();
                let p = format!("{d}/{}", e.file_name().to_string_lossy());
                if e.file_type().unwrap().is_dir() {
                    dirs.push(p);
                } else if p.ends_with(".rs") {
                    out.push(p);
                }
            }
        }
        out
    }

    #[test_id(t04qtpc)]
    #[test]
    fn the_real_spira_lc_tree_has_no_start_outside_the_door() {
        let manifest = testkit::manifest_dir();
        let root = manifest.parent().unwrap();
        let tree = Tree::from_paths(root, rust_files_under(root, "spira-lc/src"), std::iter::empty::<String>());
        assert!(tree.entries.iter().any(|e| e.path == DOOR), "the walk must reach spira-lc for its silence to mean anything");
        let found = LcServeNoSpawn.check(&tree).unwrap();
        assert!(found.is_empty(), "{:?}", found.iter().map(|f| f.to_string()).collect::<Vec<_>>());
    }
}
