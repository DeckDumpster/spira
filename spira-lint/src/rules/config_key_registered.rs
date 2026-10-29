//! `config-key-registered` — production code names a `SPIRA_…` variable only if it is a
//! registered config key (`spira/conf.d/<KEY>`) or is listed, per file, as environment-only
//! (law-a-binary-resolves-the-config-it-reads). Contract: DESIGN.md.

use crate::SyncCell as Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::{rust, shell};
use crate::rust_test::{shape, whole_test_files, Shape};
use crate::{pathspec_match, registered_keys, Entry, Finding, LintError, Rule, Tree, CONF_D};

pub struct ConfigKeyRegistered {
    checked: Cell<Option<usize>>,
}

const NAME: &str = "config-key-registered";
const ALLOW: &str = "spira-lint/config-key-registered-allow";
const OWN_SOURCE: &str = "spira-lint/src/rules/config_key_registered.rs";
const SHELL_SCOPE: &[&str] = &["spira/*.sh", "systemd/*"];

struct Hit {
    name: String,
    path: String,
    line: usize,
}

fn name_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^SPIRA_[A-Z0-9_]+$").expect("static regex"))
}

fn unit_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\$\{?(SPIRA_[A-Z0-9_]+)").expect("static regex"))
}

fn is_rust(e: &Entry) -> bool {
    e.path.ends_with(".rs") && !e.path.starts_with("target/") && e.path != OWN_SOURCE
}

fn is_shell_scope(e: &Entry) -> bool {
    SHELL_SCOPE.iter().any(|p| pathspec_match(p, &e.path))
        && !(e.path.starts_with("spira/test-") || e.path.starts_with("spira/testlib/"))
}

/// The content of a string literal's bytes (`"x"`, `b"x"`, `r#"x"#`), quotes and prefix removed.
fn literal_body(lit: &[u8]) -> &[u8] {
    let start = lit.iter().position(|&b| b == b'"').map_or(lit.len(), |i| i + 1);
    let end = lit.iter().rposition(|&b| b == b'"').unwrap_or(0);
    if end < start {
        &[]
    } else {
        &lit[start..end]
    }
}

/// Every string literal in `src` that is exactly a `SPIRA_…` name, outside test code.
fn rust_names(src: &[u8], s: &Shape, whole_file_is_test: bool) -> Vec<(usize, String)> {
    if whole_file_is_test {
        return Vec::new();
    }
    let cl = rust::classify(src);
    let mut out = Vec::new();
    let mut i = 0;
    while i < src.len() {
        if cl.class[i] != rust::Class::Literal {
            i += 1;
            continue;
        }
        let start = i;
        while i < src.len() && cl.class[i] == rust::Class::Literal {
            i += 1;
        }
        let body = literal_body(&src[start..i]);
        if name_re().is_match(body) && !s.covers(start) {
            out.push((cl.line_of(start), String::from_utf8_lossy(body).into_owned()));
        }
    }
    out
}

fn shell_names(src: &[u8]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for cmd in shell::parse(src) {
        let redirect_targets = cmd.redirects.iter().filter_map(|r| r.target.as_ref());
        for w in cmd.words.iter().chain(redirect_targets) {
            out.extend(w.vars.iter().filter(|v| v.starts_with("SPIRA_")).map(|v| (w.line, v.clone())));
        }
    }
    out
}

fn unit_names(src: &[u8]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, line) in src.split(|&b| b == b'\n').enumerate() {
        if line.trim_ascii_start().starts_with(b"#") {
            continue;
        }
        out.extend(unit_re().captures_iter(line).map(|c| (i + 1, String::from_utf8_lossy(&c[1]).into_owned())));
    }
    out
}

/// Every `SPIRA_…` name production code mentions, registered or not.
fn names(tree: &Tree) -> Result<Vec<Hit>, LintError> {
    let rust_files: Vec<&Entry> = tree.entries.iter().filter(|e| is_rust(e)).collect();
    let shapes: Vec<(&Entry, Shape)> =
        rust_files.iter().filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();
    let whole = whole_test_files(tree, &shapes);
    let mut out = Vec::new();
    for (e, s) in &shapes {
        let src = tree.content(e).unwrap_or_default();
        for (line, name) in rust_names(src, s, whole.contains(&e.path)) {
            out.push(Hit { name, path: e.path.clone(), line });
        }
    }
    for e in tree.entries.iter().filter(|e| is_shell_scope(e)) {
        let Some(src) = tree.content(e) else { continue };
        let found = if e.path.ends_with(".sh") { shell_names(src) } else { unit_names(src) };
        out.extend(found.into_iter().map(|(line, name)| Hit { name, path: e.path.clone(), line }));
    }
    if out.is_empty() && rust_files.is_empty() {
        return Err(LintError::EmptyScope);
    }
    Ok(out)
}

fn registry(tree: &Tree) -> Result<BTreeSet<String>, LintError> {
    let keys = registered_keys(tree);
    if keys.is_empty() {
        return Err(LintError::Refused(format!("{CONF_D} lists no registered keys — nothing to check against")));
    }
    Ok(keys)
}

/// The unregistered hits, sorted by path and line.
fn unregistered(tree: &Tree) -> Result<(Vec<Hit>, usize), LintError> {
    let keys = registry(tree)?;
    let all = names(tree)?;
    let n = all.len();
    let mut bad: Vec<Hit> = all.into_iter().filter(|h| !keys.contains(&h.name)).collect();
    bad.sort_by(|a, b| (&a.path, a.line, &a.name).cmp(&(&b.path, b.line, &b.name)));
    Ok((bad, n))
}

/// The allow file's text for the tree as it stands: what `--emit-allow` prints.
pub fn render_allow(tree: &Tree) -> Result<String, LintError> {
    let pairs: BTreeSet<(String, String)> =
        unregistered(tree)?.0.into_iter().map(|h| (h.name, h.path)).collect();
    let mut s = String::from(
        "# config-key-registered: environment-only variables production code names, as\n\
# `<NAME> <path>` per file that reads the name. Generated: spira-lint --only config-key-registered --emit-allow.\n\
#\n\
# SHRINK-ONLY. This lists variables that are NOT config (the environment is their only source);\n\
# a config key is registered in spira/conf.d and read through spira_config::process::cfg.\n\
# Never add a line for new code.\n",
    );
    for (name, path) in pairs {
        s.push_str(&format!("{name} {path}\n"));
    }
    Ok(s)
}

fn parse_allow(text: &str, out: &mut Vec<Finding>) -> BTreeMap<(String, String), usize> {
    let mut table = BTreeMap::new();
    for (i, l) in text.lines().enumerate() {
        let t = l.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = t.split_whitespace().collect();
        if let [name, path] = f[..] {
            table.insert((name.to_string(), path.to_string()), i + 1);
        } else {
            out.push(Finding {
                rule: NAME,
                path: ALLOW.to_string(),
                line: Some(i + 1),
                message: format!("malformed line {t:?} — expected `<NAME> <path>`"),
            });
        }
    }
    table
}

impl ConfigKeyRegistered {
    pub fn new() -> Self {
        ConfigKeyRegistered { checked: Cell::new(None) }
    }
}

impl Default for ConfigKeyRegistered {
    fn default() -> Self {
        Self::new()
    }
}

impl Rule for ConfigKeyRegistered {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        is_rust(e) || is_shell_scope(e)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        self.checked.set(None);
        crate::scope(tree, self)?;
        let (bad, n) = unregistered(tree)?;
        let mut out = Vec::new();
        let table = parse_allow(&tree.read_text(ALLOW), &mut out);
        let mut used = BTreeSet::new();
        for h in bad {
            let pair = (h.name.clone(), h.path.clone());
            if table.contains_key(&pair) {
                used.insert(pair);
                continue;
            }
            out.push(Finding {
                rule: NAME,
                path: h.path,
                line: Some(h.line),
                message: format!(
                    "{} is neither a registered config key ({CONF_D}) nor listed as environment-only for this file",
                    h.name
                ),
            });
        }
        for ((name, path), line) in &table {
            if !used.contains(&(name.clone(), path.clone())) {
                out.push(Finding {
                    rule: NAME,
                    path: ALLOW.to_string(),
                    line: Some(*line),
                    message: format!("lists {name} for {path}, which no longer names it unregistered — remove the line (the list only shrinks)"),
                });
            }
        }
        self.checked.set(Some(n));
        Ok(out)
    }

    fn checked(&self) -> Option<(usize, String)> {
        self.checked.get().map(|n| (n, "names".to_string()))
    }

    fn hint(&self) -> &'static str {
        "A SPIRA_ name production code reads as config belongs in spira/conf.d and is read through \
spira_config::process::cfg (law-a-binary-resolves-the-config-it-reads); a lookup of an unregistered \
key fails at runtime however it is spelled. A variable that is environment only is listed as \
`<NAME> <path>` in spira-lint/config-key-registered-allow, which only shrinks."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(ConfigKeyRegistered::new())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run_with(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        ConfigKeyRegistered::new().check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    fn fixture(allow: &str) -> TempDir {
        let t = TempDir::new("ckr");
        t.write("spira/conf.d/SPIRA_HOME_REPO", "TYPE=string\n");
        t.write(ALLOW, allow);
        t
    }

    const REG: &str = "spira/conf.d/SPIRA_HOME_REPO";

    #[test]
    fn the_sp_6gsvrm_settings_file_is_caught_at_the_read_and_not_at_the_test_stub() {
        let t = fixture("");
        t.write("spira-runtime/src/settings.rs", include_str!("../../tests/fixtures/sp6gsvrm_settings.txt"));
        let got = run_with(&t, &[REG, ALLOW, "spira-runtime/src/settings.rs"]).unwrap();
        assert!(got.iter().any(|f| f.starts_with("config-key-registered: spira-runtime/src/settings.rs:48: SPIRA_REPO ")), "{got:?}");
        assert!(!got.iter().any(|f| f.contains("settings.rs:98:")), "{got:?}");
    }

    #[test]
    fn shell_names_are_caught_in_code_and_not_in_comments() {
        let t = fixture("");
        t.write(
            "spira/x.sh",
            "#!/bin/sh\n# echo \"$SPIRA_IN_COMMENT\"\necho \"$SPIRA_NOT_A_KEY\"\necho \"${SPIRA_NOT_A_KEY:-x}\" > \"$SPIRA_REDIR\"\necho \"$SPIRA_HOME_REPO\"\n",
        );
        let got = run_with(&t, &[REG, ALLOW, "spira/x.sh"]).unwrap();
        let want = [
            "config-key-registered: spira/x.sh:3: SPIRA_NOT_A_KEY ",
            "config-key-registered: spira/x.sh:4: SPIRA_NOT_A_KEY ",
            "config-key-registered: spira/x.sh:4: SPIRA_REDIR ",
        ];
        assert_eq!(got.len(), 3, "{got:?}");
        for (g, w) in got.iter().zip(want) {
            assert!(g.starts_with(w), "{g} vs {w}");
        }
    }

    #[test]
    fn test_scripts_are_out_of_scope_and_unit_files_are_read_outside_comments() {
        let t = fixture("");
        t.write("spira/test-x.sh", "echo \"$SPIRA_NOT_A_KEY\"\n");
        t.write("spira/testlib/y.sh", "echo \"$SPIRA_NOT_A_KEY\"\n");
        t.write("systemd/a.service", "# $SPIRA_IN_COMMENT\nExecStart=/bin/x ${SPIRA_UNIT_KEY}\nEnvironment=SPIRA_X=1\n");
        let got = run_with(&t, &[REG, ALLOW, "spira/test-x.sh", "spira/testlib/y.sh", "systemd/a.service"]).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].starts_with("config-key-registered: systemd/a.service:2: SPIRA_UNIT_KEY "), "{got:?}");
    }

    #[test]
    fn a_rust_literal_is_a_name_and_a_comment_or_fragment_is_not() {
        let t = fixture("");
        t.write(
            "a/src/main.rs",
            "// \"SPIRA_NOT_A_KEY\"\nfn f() { let k = \"SPIRA_NOT_A_KEY\"; let _ = \"SPIRA_RUN=1\"; let _ = \"SPIRA_HOME_REPO\"; let _ = r#\"SPIRA_RAW\"#; }\n",
        );
        let got = run_with(&t, &[REG, ALLOW, "a/src/main.rs"]).unwrap();
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].contains("a/src/main.rs:2: SPIRA_NOT_A_KEY"), "{got:?}");
        assert!(got[1].contains("a/src/main.rs:2: SPIRA_RAW"), "{got:?}");
    }

    #[test]
    fn a_name_is_allowed_only_in_the_file_it_is_listed_for_and_a_stale_line_is_a_finding() {
        let t = fixture("SPIRA_ENV_ONLY a/src/main.rs\nSPIRA_ENV_ONLY gone/src/lib.rs\nbroken\n");
        t.write("a/src/main.rs", "fn f() { let _ = \"SPIRA_ENV_ONLY\"; }\n");
        t.write("b/src/main.rs", "fn f() { let _ = \"SPIRA_ENV_ONLY\"; }\n");
        let got = run_with(&t, &[REG, ALLOW, "a/src/main.rs", "b/src/main.rs"]).unwrap();
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(got.iter().any(|f| f.contains("malformed line")), "{got:?}");
        assert!(got.iter().any(|f| f.contains("b/src/main.rs:1: SPIRA_ENV_ONLY")), "{got:?}");
        assert!(got.iter().any(|f| f.contains(&format!("{ALLOW}:2:")) && f.contains("remove the line")), "{got:?}");
    }

    #[test]
    fn an_empty_registry_is_a_refusal_and_a_clean_tree_reports_how_much_it_checked() {
        let t = TempDir::new("ckr-noreg");
        t.write("a/src/main.rs", "fn f() {}\n");
        assert!(run_with(&t, &["a/src/main.rs"]).is_err());

        let t = fixture("");
        t.write("a/src/main.rs", "fn f() { let _ = \"SPIRA_HOME_REPO\"; }\n");
        let tree = Tree::from_paths(t.path(), [REG, ALLOW, "a/src/main.rs"], std::iter::empty::<&str>());
        let r = ConfigKeyRegistered::new();
        assert!(r.check(&tree).unwrap().is_empty());
        assert_eq!(r.checked(), Some((1, "names".to_string())));
    }

    #[test]
    fn render_allow_round_trips() {
        let t = fixture("");
        t.write("a/src/main.rs", "fn f() { let _ = \"SPIRA_ENV_ONLY\"; }\n");
        let tree = Tree::from_paths(t.path(), [REG, ALLOW, "a/src/main.rs"], std::iter::empty::<&str>());
        let text = render_allow(&tree).unwrap();
        assert!(text.contains("\nSPIRA_ENV_ONLY a/src/main.rs\n"), "{text}");
        t.write(ALLOW, &text);
        assert!(ConfigKeyRegistered::new().check(&tree).unwrap().is_empty());
    }
}
