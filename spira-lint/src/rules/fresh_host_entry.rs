//! `fresh-host-entry` — the scripts an operator runs on a host with nothing installed
//! resolve everything from their own release tree. Contract: DESIGN.md.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use regex::Regex;

use crate::lex::shell::{self, Command};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct FreshHostEntry;

const NAME: &str = "fresh-host-entry";
const ENTRY: &[&str] =
    &["spira/configure.sh", "spira/install.sh", "spira/ready.sh", "spira/uninstall.sh", "spira/owned.sh"];
const PATH_SETUP_ONLY: &[&str] = &["spira/conf.sh"];

fn is_shell(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path);
    path.ends_with(".sh") && !path.starts_with("target/") && !base.starts_with("test-") && !path.contains("/tests/")
}

fn func_defs(src: &str) -> BTreeSet<String> {
    static DEF: OnceLock<Regex> = OnceLock::new();
    let re = DEF.get_or_init(|| {
        Regex::new(r"(?m)^[ \t]*(?:function[ \t]+)?([A-Za-z_][A-Za-z0-9_]*)[ \t]*\(\)").expect("static regex")
    });
    re.captures_iter(src).map(|c| c[1].to_string()).collect()
}

fn function_lines(src: &str) -> BTreeSet<usize> {
    let mut out = BTreeSet::new();
    let lines: Vec<&str> = src.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        if l.trim_end().ends_with("() {") || (l.contains("() {") && !l.trim_end().ends_with('}')) {
            let indent = &l[..l.len() - l.trim_start().len()];
            let close = format!("{indent}}}");
            let end = (i + 1..lines.len()).find(|&k| lines[k].trim_end() == close).unwrap_or(i);
            out.extend(i + 1..=end + 1);
            i = end;
        } else if l.contains("() {") {
            out.insert(i + 1);
        }
        i += 1;
    }
    out
}

fn release_binaries(tree: &Tree) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for e in &tree.entries {
        let p = e.path.as_str();
        let parts: Vec<&str> = p.split('/').collect();
        if parts.len() == 3 && parts[1] == "src" && parts[2] == "main.rs" {
            out.insert(parts[0].to_string());
        }
        if parts.len() == 4 && parts[1] == "src" && parts[2] == "bin" {
            if let Some(s) = parts[3].strip_suffix(".rs") {
                out.insert(s.replace('_', "-"));
            }
        }
        if parts.len() == 2 && parts[1] == "Cargo.toml" {
            for l in tree.read_text(p).lines() {
                if let Some(v) = l.trim().strip_prefix("name = \"") {
                    out.insert(v.trim_end_matches('"').to_string());
                }
            }
        }
    }
    out.retain(|n| n.contains('-'));
    out
}

fn head(c: &Command) -> Option<&shell::Word> {
    let (_, argv) = c.split_env();
    argv.first()
}

fn source_target(c: &Command) -> Option<String> {
    let (_, argv) = c.split_env();
    let h = argv.first()?.raw.as_str();
    if h != "." && h != "source" {
        return None;
    }
    let last = argv.get(1)?.unquoted();
    let base = last.rsplit('/').next()?.to_string();
    base.ends_with(".sh").then_some(base)
}

fn sets_up_path(c: &Command) -> bool {
    let (env, argv) = c.split_env();
    let assigns = env.iter().chain(argv.iter().take_while(|w| w.assignment_name().is_some()));
    if assigns.into_iter().any(|w| w.assignment_name() == Some("PATH") && w.raw.contains("bin")) {
        return true;
    }
    source_target(c).as_deref() == Some("conf.sh")
}

fn closure(tree: &Tree, from: &Entry, shell_files: &[&Entry]) -> BTreeSet<String> {
    let mut defs = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut todo = vec![from.path.clone()];
    while let Some(p) = todo.pop() {
        if !seen.insert(p.clone()) {
            continue;
        }
        let Some(e) = shell_files.iter().find(|e| e.path == p) else { continue };
        let Some(src) = tree.content(e) else { continue };
        defs.extend(func_defs(&String::from_utf8_lossy(src)));
        let dir = p.rsplit_once('/').map_or("", |(d, _)| d);
        for c in shell::parse(src) {
            if let Some(base) = source_target(&c) {
                let same = format!("{dir}/{base}");
                let found = shell_files
                    .iter()
                    .find(|e| e.path == same)
                    .or_else(|| shell_files.iter().find(|e| e.path.ends_with(&format!("/{base}"))));
                if let Some(f) = found {
                    todo.push(f.path.clone());
                }
            }
        }
    }
    defs
}

impl Rule for FreshHostEntry {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        None
    }

    fn applies_to(&self, e: &Entry) -> bool {
        ENTRY.contains(&e.path.as_str()) || PATH_SETUP_ONLY.contains(&e.path.as_str())
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        static DEFAULTED: OnceLock<Regex> = OnceLock::new();
        let defaulted = DEFAULTED
            .get_or_init(|| Regex::new(r"\$\{SPIRA_(?:TOML|RELEASES):?-[^}]").expect("static regex"));
        let files = crate::scope(tree, self)?;
        let bins = release_binaries(tree);
        let shell_files: Vec<&Entry> = tree.entries.iter().filter(|e| is_shell(&e.path)).collect();
        let mut global_defs: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for e in &shell_files {
            if let Some(src) = tree.content(e) {
                for d in func_defs(&String::from_utf8_lossy(src)) {
                    global_defs.entry(d).or_default().insert(e.path.clone());
                }
            }
        }
        let mut out = Vec::new();
        for e in files {
            let Some(src) = tree.content(e) else { continue };
            let cmds = shell::parse(src);
            let setup_line = cmds
                .iter()
                .filter(|c| sets_up_path(c))
                .filter_map(|c| c.words.first().map(|w| w.line))
                .min()
                .unwrap_or(usize::MAX);
            let in_fn = function_lines(&String::from_utf8_lossy(src));
            let entry_point = ENTRY.contains(&e.path.as_str());
            let defs = if entry_point { closure(tree, e, &shell_files) } else { BTreeSet::new() };
            for c in &cmds {
                for w in &c.words {
                    if entry_point && w.vars.iter().any(|v| v == "SPIRA_TOML" || v == "SPIRA_RELEASES") && !defaulted.is_match(&w.raw) {
                        out.push(Finding {
                            rule: NAME,
                            path: e.path.clone(),
                            line: Some(w.line),
                            message: format!("{} reads SPIRA_TOML/SPIRA_RELEASES with no default — a fresh host has neither exported; give it the default the config writer and the script's own release resolve to (${{VAR:-…}})", w.raw),
                        });
                    }
                }
                let Some(h) = head(c) else { continue };
                let name = h.raw.as_str();
                let ready = h.line >= setup_line || (in_fn.contains(&h.line) && setup_line != usize::MAX);
                if bins.contains(name) && !ready {
                    out.push(Finding {
                        rule: NAME,
                        path: e.path.clone(),
                        line: Some(h.line),
                        message: format!("{name} is called by bare name before this script puts its own release bin/ on PATH (or sources conf.sh, which does) — a fresh host has no PATH to find it"),
                    });
                }
                if entry_point && global_defs.contains_key(name) && !defs.contains(name) {
                    out.push(Finding {
                        rule: NAME,
                        path: e.path.clone(),
                        line: Some(h.line),
                        message: format!("{name} is a shell function defined in {} but not in this file or any file it sources — source the file that defines it", global_defs[name].iter().cloned().collect::<Vec<_>>().join(", ")),
                    });
                }
            }
        }
        out.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "Operator entry points run before anything is installed: find binaries in the script's own \
release (dirname \"$0\"/../bin on PATH, or conf.sh sourced first), source every file that defines a \
function the script calls, and give SPIRA_TOML / SPIRA_RELEASES the default the configure step writes."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(FreshHostEntry)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(files: &[(&str, &str)]) -> Vec<String> {
        let t = TempDir::new("fhe");
        t.write("spira-config/src/main.rs", "fn main() {}\n");
        t.write("spira/conf.sh", "spira_unit() { :; }\nPATH=\"$d/../bin:$PATH\"\nspira-config resolve\n");
        for (p, c) in files {
            t.write(p, c);
        }
        let mut paths: Vec<&str> = files.iter().map(|(p, _)| *p).collect();
        paths.extend(["spira-config/src/main.rs", "spira/conf.sh"]);
        let tree = Tree::from_paths(t.path(), paths, std::iter::empty::<&str>());
        FreshHostEntry.check(&tree).unwrap().iter().map(|f| f.to_string()).collect()
    }

    #[test]
    fn a_bare_release_binary_before_path_is_set_up_is_refused() {
        let got = run(&[("spira/configure.sh", "#!/bin/bash\nspira-config set a b\n")]);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("spira/configure.sh:2") && got[0].contains("bare name"));
    }

    #[test]
    fn the_release_tree_path_setup_or_conf_sh_clears_it() {
        let fixed = "HERE=x\nif ! command -v spira-config; then\n    PATH=\"$HERE/../bin:$PATH\"\nfi\nspira-config set a b\n";
        assert!(run(&[("spira/configure.sh", fixed)]).is_empty());
        let sourced = ". \"$HERE/conf.sh\" || exit 1\nspira-config validate\n";
        assert!(run(&[("spira/owned.sh", sourced)]).is_empty());
    }

    #[test]
    fn a_function_from_a_file_that_is_not_sourced_is_refused_and_a_sourced_one_passes() {
        let got = run(&[("spira/ready.sh", "#!/bin/bash\nx=\"$(spira_unit sentinel timer)\"\n")]);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("spira_unit") && got[0].contains("spira/conf.sh"));
        let ok = run(&[("spira/ready.sh", ". \"$HERE/conf.sh\"\nx=\"$(spira_unit sentinel timer)\"\n")]);
        assert!(ok.is_empty(), "{ok:?}");
        let local = run(&[("spira/ready.sh", "spira_unit() { :; }\nspira_unit a\n")]);
        assert!(local.is_empty(), "{local:?}");
    }

    #[test]
    fn a_function_reached_through_a_nested_source_passes() {
        let got = run(&[
            ("spira/helper.sh", ". \"$D/conf.sh\"\n"),
            ("spira/uninstall.sh", ". \"$HERE/helper.sh\"\nspira_unit a b\n"),
        ]);
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn an_undefaulted_read_of_the_toml_or_releases_path_is_refused() {
        let bad = ". \"$HERE/conf.sh\"\ncat \"$SPIRA_TOML\"\nls \"${SPIRA_RELEASES}\"/current\n";
        let got = run(&[("spira/owned.sh", bad)]);
        assert_eq!(got.len(), 2, "{got:?}");
        let ok = ". \"$HERE/conf.sh\"\ncat \"${SPIRA_TOML:-$HOME/.config/spira/cfg}\"\nSPIRA_TOML=\"$x\"\n";
        assert!(run(&[("spira/owned.sh", ok)]).is_empty());
    }

    #[test]
    fn a_call_inside_a_function_is_judged_by_the_script_setting_up_path_anywhere() {
        let late = "w() {\n  spira-config set a b\n}\nPATH=\"$H/../bin:$PATH\"\nw\n";
        assert!(run(&[("spira/configure.sh", late)]).is_empty());
        let never = "w() {\n  spira-config set a b\n}\nw\n";
        assert_eq!(run(&[("spira/configure.sh", never)]).len(), 1);
    }

    #[test]
    fn a_comment_or_an_external_command_is_not_a_finding() {
        let src = "# spira-config set; spira_unit; $SPIRA_TOML\n. \"$H/conf.sh\"\ngit status\n";
        assert!(run(&[("spira/ready.sh", src)]).is_empty());
    }
}
