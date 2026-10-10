//! `config-fence` — nothing outside spira-config/ names spira.toml or repo-map, parses TOML
//! config, or writes the config path. Contract: DESIGN.md.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::{rust, shell};
use crate::{allow_lines, pathspec_match, scope, Entry, Finding, LintError, Rule, Tree};

pub struct ConfigFence;

const NAME: &str = "config-fence";
const SCOPE: &[&str] = &["spira/*.sh", "spira/gate-suites", "spira/chamber/*.fayth", "spira/chamber/*.md", "*.rs"];
/// This rule's own source names the very strings it hunts.
const OWN_SOURCE: &str = "spira-lint/src/rules/config_fence.rs";
const CONFIG_VARS: &[&str] = &["SPIRA_TOML", "SPIRA_REPO_MAP"];

/// `spira/config-fence-allow`: exact repo-relative paths.
pub struct ConfigAllow(BTreeSet<String>);

impl ConfigAllow {
    pub fn parse(text: &str) -> ConfigAllow {
        ConfigAllow(allow_lines(text).into_iter().collect())
    }
    pub fn covers(&self, path: &str) -> bool {
        self.0.contains(path)
    }
}

/// The violation kinds found in one file, in report order.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Kinds {
    pub name: bool,
    pub parse: bool,
    pub write: bool,
}

impl Kinds {
    fn any(&self) -> bool {
        self.name || self.parse || self.write
    }
    fn render(&self) -> String {
        [(self.name, "name"), (self.parse, "parse"), (self.write, "write")]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, k)| *k)
            .collect::<Vec<_>>()
            .join(",")
    }
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

/// Scan one file. `path` decides which language the parse/write checks read it as.
pub fn scan(path: &str, content: &[u8]) -> Kinds {
    static NAME_RE: OnceLock<Regex> = OnceLock::new();
    static TOMLLIB: OnceLock<Regex> = OnceLock::new();
    let mut k = Kinds {
        name: re(&NAME_RE, r"spira\.toml|repo-map").is_match(content),
        ..Kinds::default()
    };
    if path.ends_with(".rs") {
        let cl = rust::classify(content);
        k.parse = k.name && rust_uses_toml_crate(&cl);
        k.write = rust_writes_config(&cl, k.name);
    } else {
        k.parse = k.name
            && re(&TOMLLIB, r"(?m)(?:^|[^A-Za-z0-9_.])tomllib(?:[^A-Za-z0-9_]|$)").is_match(content);
        if path.ends_with(".sh") {
            k.write = shell_writes_config(content);
        }
    }
    k
}

/// A path into the toml / toml_edit / basic_toml crate, in code.
fn rust_uses_toml_crate(cl: &rust::Classified) -> bool {
    static CRATE: OnceLock<Regex> = OnceLock::new();
    re(&CRATE, r"(?:^|[^A-Za-z0-9_:])(?:toml|toml_edit|basic_toml)::").is_match(&cl.code_only())
}

/// `fs::write`/`File::create`/`File::create_new` whose first argument, or
/// `fs::rename`/`fs::copy` whose second argument, is the config: it mentions `spira_toml` /
/// `repo_map` (the env var or a binding named for it), or — in a file that names the
/// config — `toml` at all. A TOML write in a file that never mentions the config writes some
/// other TOML.
fn rust_writes_config(cl: &rust::Classified, named: bool) -> bool {
    static CALL: OnceLock<Regex> = OnceLock::new();
    let code = cl.code_only();
    let text = cl.without_comments();
    let call = re(&CALL, r"(?:^|[^A-Za-z0-9_])(fs::write|File::create_new|File::create|fs::rename|fs::copy)\s*\(");
    for c in call.captures_iter(&code) {
        let dest = match &c[1] {
            b"fs::rename" | b"fs::copy" => 1,
            _ => 0,
        };
        let open = c.get(0).unwrap().end(); // just past '('
        if let Some(arg) = nth_arg(&code, &text, open, dest) {
            let a = arg.to_ascii_lowercase();
            let has = |n: &[u8]| a.windows(n.len()).any(|w| w == n);
            if has(b"spira_toml") || has(b"repo_map") || (named && has(b"toml")) {
                return true;
            }
        }
    }
    false
}

/// The `n`th top-level argument of the call whose `(` ends at `open`: brackets are balanced
/// on `code` (literals blanked), the argument's text is taken from `text`.
fn nth_arg<'t>(code: &[u8], text: &'t [u8], open: usize, n: usize) -> Option<&'t [u8]> {
    let mut depth = 0usize;
    let mut idx = 0usize;
    let mut start = open;
    for i in open..code.len() {
        match code[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth == 0 => {
                return (idx == n).then(|| &text[start..i]);
            }
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                if idx == n {
                    return Some(&text[start..i]);
                }
                idx += 1;
                start = i + 1;
            }
            _ => {}
        }
    }
    None
}

/// A shell command that writes a word expanding `$SPIRA_TOML` / `$SPIRA_REPO_MAP`.
fn shell_writes_config(content: &[u8]) -> bool {
    let expands = |w: &shell::Word| w.vars.iter().any(|v| CONFIG_VARS.contains(&v.as_str()));
    for cmd in shell::parse(content) {
        let redirect_write = cmd.redirects.iter().any(|r| {
            matches!(r.op.as_str(), ">" | ">>" | ">|" | "<>" | "&>" | "&>>") && r.target.as_ref().is_some_and(expands)
        });
        if redirect_write {
            return true;
        }
        let (_, argv) = cmd.split_env();
        let Some(head) = argv.first() else { continue };
        let args = &argv[1..];
        let written = match head.unquoted().rsplit('/').next().unwrap_or("") {
            "sed" => {
                args.iter().any(|a| {
                    let r = a.unquoted();
                    r.starts_with("--in-place") || (r.starts_with('-') && !r.starts_with("--") && r[1..].starts_with(|c: char| c.is_ascii_alphabetic()) && r[1..].chars().take_while(|c| c.is_ascii_alphabetic()).any(|c| c == 'i'))
                }) && args.iter().any(expands)
            }
            "tee" => args.iter().any(expands),
            "cp" | "mv" | "install" | "ln" => args.last().is_some_and(expands),
            _ => false,
        };
        if written {
            return true;
        }
    }
    false
}

impl Rule for ConfigFence {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some("spira/config-fence-allow")
    }

    fn applies_to(&self, e: &Entry) -> bool {
        !e.path.starts_with("spira-config/") && SCOPE.iter().any(|p| pathspec_match(p, &e.path))
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = scope(tree, self)?;
        let allow = ConfigAllow::parse(&tree.read_text(self.allow_file().unwrap()));
        let mut out = Vec::new();
        for e in files {
            if e.path == OWN_SOURCE || allow.covers(&e.path) {
                continue;
            }
            let Some(content) = tree.content(e) else { continue };
            let k = scan(&e.path, content);
            if k.any() {
                out.push(Finding { rule: NAME, path: e.path.clone(), line: None, message: k.render() });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "Only spira-config (the crate and its CLI) may name, parse or write spira.toml or repo-map; \
call it instead of opening the file yourself. A file caught mid-cutover belongs in \
spira/config-fence-allow — shrink-only: nothing is added for a newly written file."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, tracked: &[&str], untracked: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), tracked.iter().copied(), untracked.iter().copied());
        ConfigFence.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    // ── the bash suite's cases ─────────────────────────────────────────────────────────

    #[test]
    fn name_is_refused_then_clean_when_withdrawn() {
        let t = TempDir::new("cf-name");
        t.write("spira/config-fence-allow", "");
        t.write("spira/planted.sh", "#!/bin/sh\n# see spira.toml for the defaults\n");
        t.write("spira/other.sh", "#!/bin/sh\necho ok\n");
        let got = run(&t, &["spira/planted.sh", "spira/other.sh"], &[]).unwrap();
        assert_eq!(got, vec!["config-fence: spira/planted.sh: name"]);
        t.remove("spira/planted.sh");
        assert!(run(&t, &["spira/planted.sh", "spira/other.sh"], &[]).unwrap().is_empty());
    }

    #[test]
    fn parse_counts_only_beside_a_name() {
        let named = b"#!/bin/sh\n# reads spira.toml\npython3 -c \"import tomllib\"\n";
        assert_eq!(scan("spira/p.sh", named).render(), "name,parse");
        let unrelated = b"#!/bin/sh\npython3 -c \"import tomllib\"\n";
        assert!(!scan("spira/u.sh", unrelated).any(), "tomllib with no config reference is not this defect");
    }

    #[test]
    fn write_through_the_resolved_variable() {
        let k = scan("spira/w.sh", b"#!/bin/sh\nprintf \"[spira]\\n\" > \"$SPIRA_TOML\"\n");
        assert_eq!(k.render(), "write");
    }

    #[test]
    fn allow_listed_file_is_skipped() {
        let t = TempDir::new("cf-allow");
        t.write("spira/legacy.sh", "#!/bin/sh\n# spira.toml\n");
        t.write("spira/config-fence-allow", "");
        assert_eq!(run(&t, &["spira/legacy.sh"], &[]).unwrap().len(), 1);
        t.write("spira/config-fence-allow", "# comment\n\nspira/legacy.sh\n");
        assert!(run(&t, &["spira/legacy.sh"], &[]).unwrap().is_empty());
    }

    #[test]
    fn spira_config_is_exempt_by_directory() {
        let t = TempDir::new("cf-exempt");
        t.write("spira-config/src/main.rs", "fn main() { let _ = toml::from_str::<toml::Value>(\"x=1\"); } // spira.toml\n");
        t.write("spira/ok.sh", "echo\n");
        assert!(run(&t, &["spira-config/src/main.rs", "spira/ok.sh"], &[]).unwrap().is_empty());
    }

    // ── scope and walk ────────────────────────────────────────────────────────────────

    #[test]
    fn untracked_files_are_scanned_and_scope_is_pathspec() {
        let t = TempDir::new("cf-scope");
        t.write("spira/new.sh", "# repo-map\n");
        t.write("spira/chamber/x.fayth", "repo-map\n");
        t.write("docs/readme.txt", "spira.toml\n");
        t.write("broker/src/deep/x.rs", "// spira.toml\n");
        let got = run(&t, &["spira/chamber/x.fayth", "docs/readme.txt", "broker/src/deep/x.rs"], &["spira/new.sh"]).unwrap();
        assert_eq!(
            got,
            vec![
                "config-fence: broker/src/deep/x.rs: name",
                "config-fence: spira/chamber/x.fayth: name",
                "config-fence: spira/new.sh: name",
            ]
        );
    }

    #[test]
    fn empty_scope_refuses() {
        let t = TempDir::new("cf-empty");
        assert_eq!(run(&t, &["README"], &[]), Err(LintError::EmptyScope));
    }

    // ── parse: the construct, not the text ───────────────────────────────────────────

    #[test]
    fn rust_parse_is_a_crate_path_in_code() {
        let hit = b"// spira.toml\nfn f(s: &str) { let v: toml::Value = s.parse().unwrap(); }\n";
        assert_eq!(scan("a.rs", hit).render(), "name,parse");
        let de = b"const P: &str = \"spira.toml\"; fn f(s:&str){ toml::de::from_str::<X>(s); }";
        assert!(scan("a.rs", de).parse, "toml::de:: was missed by the bash regex");
        let in_comment = b"// spira.toml: never call toml::from_str here\nfn f() {}\n";
        assert!(!scan("a.rs", in_comment).parse, "a comment naming the parser is not a parse");
        let in_string = b"fn f() { println!(\"spira.toml via toml::from_str\"); }";
        assert!(!scan("a.rs", in_string).parse);
        let submodule = b"// repo-map\nuse spira_config::toml::Loaded;\n";
        assert!(!scan("a.rs", submodule).parse, "a module named toml inside spira_config is not the crate");
    }

    // ── write: Rust ──────────────────────────────────────────────────────────────────

    #[test]
    fn rust_write_inspects_the_destination_argument() {
        // Every case names the config somewhere (a comment), so only the destination decides.
        let w = |body: &str| scan("a.rs", format!("// spira.toml\n{body}").as_bytes()).write;
        assert!(w("fn f(){ fs::write(&toml_path, body).unwrap(); }"));
        assert!(w("fn f(){ std::fs::File::create(cfg_dir().join(\"x.TOML\")); }"));
        assert!(w("fn f(){ fs::write(\n    dir().join(\"x.toml\"),\n    b); }"), "bash's [^)]* stopped at dir( and never saw the path");
        assert!(w("fn f(){ fs::rename(&tmp, &toml_path); }"));
        assert!(!w("fn f(){ fs::write(&out, toml_text); }"), "toml in the CONTENT argument is not a write of the config path");
        assert!(!w("fn f(){ fs::rename(&toml_tmp, &out); }"), "source, not destination");
        assert!(!w("// fs::write(&toml_path, b)\nfn f(){}"), "a comment is not a call");
    }

    #[test]
    fn rust_write_of_other_toml_is_not_the_config() {
        let other = b"fn t(){ fs::write(dir.join(\"a.toml\"), \"area = 1\").unwrap(); }";
        assert!(!scan("a.rs", other).any(), "a test-plan catalogue a.toml is not the config");
        let env = b"fn t(){ fs::write(std::env::var(\"SPIRA_TOML\").unwrap(), b).unwrap(); }";
        assert_eq!(scan("a.rs", env).render(), "write", "the config's env var needs no name to count");
        assert!(scan("a.rs", b"fn t(){ File::create(&repo_map_path); }").write);
    }

    // ── write: shell ─────────────────────────────────────────────────────────────────

    #[test]
    fn shell_write_is_the_construct_and_the_exact_variable() {
        let w = |s: &str| scan("spira/x.sh", s.as_bytes()).write;
        assert!(w("printf x >> ${SPIRA_REPO_MAP}\n"));
        assert!(w("sed -i 's/a/b/' \"$SPIRA_TOML\"\n"));
        assert!(w("sed -Ei.bak 's/a/b/' \"$SPIRA_TOML\"\n"));
        assert!(w("render | tee -a \"$SPIRA_TOML\" >/dev/null\n"));
        assert!(w("mv \"$tmp\" \"$SPIRA_TOML\"\n"), "missed by the bash line regex");
        assert!(w("x=\"$(cat f > \"$SPIRA_TOML\")\"\n"), "inside a substitution");
        assert!(!w("cat \"$SPIRA_TOML\" > \"$out\"\n"), "reading it is not writing it");
        assert!(!w("printf x > \"$SPIRA_TOML_TMP\"\n"), "a different variable (bash matched the prefix)");
        assert!(!w("mv \"$SPIRA_TOML\" \"$backup\"\n"), "source, not destination");
        assert!(!w("sed -n '1p' \"$SPIRA_TOML\"\n"), "sed without -i reads");
        assert!(!w("# printf x > \"$SPIRA_TOML\"\n"), "a comment");
        assert!(!w("cat <<'EOF'\nprintf x > \"$SPIRA_TOML\"\nEOF\n"), "heredoc body is data");
    }

    #[test]
    fn prose_gets_no_write_check() {
        assert!(!scan("spira/chamber/a.md", b"run: printf x > \"$SPIRA_TOML\"\n").write);
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(ConfigFence),
    ]
}
