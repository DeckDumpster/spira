//! `doors-coverage` — a tool whose usage block says it is the only path for something has a row
//! in `spira/doors.toml`, so the hook can point a session at it. Contract: DESIGN.md.

use std::sync::OnceLock;

use regex::Regex;

use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct DoorsCoverage;

const NAME: &str = "doors-coverage";
const ALLOW_FILE: &str = "spira-lint/doors-coverage-allow";
const REGISTRY: &str = "spira/doors.toml";
const HEADER_LINES: usize = 40;

fn claims_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"the ONE\b|(?i:only path|chokepoint)").expect("static regex"))
}

fn in_scope(path: &str) -> bool {
    let direct = |dir: &str| crate::direct_child(path, dir);
    match (direct("bin"), direct("spira")) {
        (Some(_), _) => true,
        (_, Some(name)) => name.ends_with(".sh") && !name.starts_with("test-"),
        _ => false,
    }
}

fn header(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines()
        .enumerate()
        .skip_while(|(_, l)| l.starts_with("#!"))
        .take_while(|(_, l)| l.starts_with('#'))
        .take(HEADER_LINES)
        .map(|(i, l)| (i + 1, l))
}

fn tool_name(path: &str) -> &str {
    let base = path.rsplit('/').next().unwrap_or(path);
    base.strip_suffix(".sh").unwrap_or(base)
}

/// Command names the registry's rows are the doors of, and the files their usage is read from.
fn covered(registry: &str) -> Result<(Vec<String>, Vec<String>), LintError> {
    let doc: toml::Value = registry.parse().map_err(|e| LintError::Refused(format!("{REGISTRY}: {e}")))?;
    let (mut names, mut files) = (Vec::new(), Vec::new());
    for r in doc.get("resource").and_then(|r| r.as_array()).into_iter().flatten() {
        let words = |v: Option<&toml::Value>| -> Vec<String> {
            match v {
                Some(toml::Value::String(s)) => vec![s.clone()],
                Some(toml::Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
                _ => Vec::new(),
            }
        };
        for w in words(r.get("door")).into_iter().chain(words(r.get("door_commands"))) {
            if let Some(first) = w.split_whitespace().next() {
                names.push(tool_name(first).to_string());
            }
        }
        files.extend(words(r.get("usage_in")));
    }
    Ok((names, files))
}

impl Rule for DoorsCoverage {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        in_scope(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let (names, files) = covered(&tree.read_text(REGISTRY))?;
        let allow = crate::allow_lines(&tree.read_text(ALLOW_FILE));
        let scope = crate::scope(tree, self)?;
        let mut out = Vec::new();
        for e in &scope {
            let Some(c) = tree.content(e) else { continue };
            let text = String::from_utf8_lossy(c);
            let Some((line, claim)) = header(&text).find(|(_, l)| claims_re().is_match(l)) else { continue };
            let has_row = names.iter().any(|n| n == tool_name(&e.path)) || files.contains(&e.path);
            if !has_row && !allow.iter().any(|a| a.trim() == e.path) {
                out.push(Finding { rule: NAME, path: e.path.clone(), line: Some(line), message: format!("says it is the only path ({}) and has no row in {REGISTRY}", claim.trim_start_matches('#').trim()) });
            }
        }
        for (n, a) in allow.iter().enumerate() {
            if !tree.entries.iter().any(|e| e.path == a.trim() && tree.root.join(a.trim()).is_file()) {
                out.push(Finding { rule: NAME, path: ALLOW_FILE.to_string(), line: Some(n + 1), message: format!("lists {}, which no longer exists — remove the line (the list only shrinks)", a.trim()) });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "Add a [[resource]] row to spira/doors.toml naming the resource the tool guards and how a \
command is recognised as touching it, so a session that reaches for the resource another way is \
shown this tool. spira-lint/doors-coverage-allow only shrinks."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(DoorsCoverage)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const ROW: &str = "[[resource]]\nname = \"x\"\nwrites_only = true\ndoor = \"guarded.sh\"\nusage = \"u\"\nwhy = \"w\"\n[resource.touched_by]\npaths = [\"*/x\"]\n";

    fn run(t: &TempDir, files: &[&str]) -> Vec<String> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        DoorsCoverage.check(&tree).unwrap().iter().map(|f| f.to_string()).collect()
    }

    #[test]
    fn a_tool_claiming_to_be_the_only_path_without_a_row_is_flagged_and_one_with_a_row_is_not() {
        let t = TempDir::new("dc");
        t.write(REGISTRY, ROW);
        t.write("spira/orphan.sh", "#!/usr/bin/env bash\n# orphan.sh — the ONE way to move the thing.\nset -u\n");
        t.write("spira/guarded.sh", "#!/usr/bin/env bash\n# guarded.sh — the only path for x.\n");
        t.write("spira/plain.sh", "#!/usr/bin/env bash\n# plain.sh — does a thing.\n");
        t.write("spira/body.sh", "#!/usr/bin/env bash\n# body.sh — does a thing.\nset -u\n# the ONE in a later comment is not a usage block\n");
        t.write("spira/test-suite.sh", "#!/usr/bin/env bash\n# the ONE\n");
        let got = run(&t, &["spira/orphan.sh", "spira/guarded.sh", "spira/plain.sh", "spira/body.sh", "spira/test-suite.sh"]);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].starts_with("doors-coverage: spira/orphan.sh:2:"), "{got:?}");
    }

    #[test]
    fn a_listed_tool_is_allowed_and_a_stale_entry_is_refused() {
        let t = TempDir::new("dc-allow");
        t.write(REGISTRY, ROW);
        t.write("spira/old.sh", "# chokepoint for y\n");
        t.write(ALLOW_FILE, "# header\nspira/old.sh\nspira/gone.sh\n");
        assert_eq!(run(&t, &["spira/old.sh"]), vec!["doors-coverage: spira-lint/doors-coverage-allow:2: lists spira/gone.sh, which no longer exists — remove the line (the list only shrinks)"]);
    }
}
