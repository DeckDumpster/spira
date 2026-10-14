//! Whose words are whose. Each repo's identity terms are installation knowledge, declared on its
//! project page in the brain (`<wiki>/<identity_projects>/<repo>/identity.md`); the harness ships only this
//! reader. A batch member that adds another registered repo's term is sent back.
//!
//! ```text
//! ## Identity terms          words that belong to this repo and to no other
//! - term
//! ## Vendored paths          path prefixes in this repo exempt from the scan
//! - vendor/
//! ## Allowed foreign terms   another repo's terms this repo may use
//! - term
//! ```

use std::collections::BTreeMap;
use std::path::Path;

pub const PAGE: &str = "identity.md";
const TERMS: &str = "identity terms";
const VENDORED: &str = "vendored paths";
const ALLOWED: &str = "allowed foreign terms";
const SHOWN: usize = 10;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Page {
    pub terms: Vec<String>,
    pub vendored: Vec<String>,
    pub allowed: Vec<String>,
}

pub fn parse(text: &str) -> Page {
    let mut page = Page::default();
    let mut into: Option<&mut Vec<String>> = None;
    for line in text.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            into = match h.trim().to_ascii_lowercase().as_str() {
                TERMS => Some(&mut page.terms),
                VENDORED => Some(&mut page.vendored),
                ALLOWED => Some(&mut page.allowed),
                _ => None,
            };
        } else if let (Some(v), Some(item)) = (into.as_deref_mut(), line.trim().strip_prefix("- ")) {
            let item = item.trim().trim_matches('`').trim();
            if !item.is_empty() {
                v.push(item.to_string());
            }
        }
    }
    page
}

/// A registered repo with no page declares no terms; an unset wiki or pages directory is a
/// refusal naming the key.
pub fn load(wiki: &str, rel: &str, repos: &[String]) -> Result<BTreeMap<String, Page>, String> {
    if wiki.is_empty() {
        return Err("SPIRA_WIKI is empty: the brain path is where each repo's identity terms are declared; set SPIRA_WIKI through the config CLI — refusing".into());
    }
    if rel.is_empty() || Path::new(rel).is_absolute() || rel.split('/').any(|c| c == "..") {
        return Err(format!("SPIRA_IDENTITY_PROJECTS {rel:?} must be a directory path relative to SPIRA_WIKI; set it through the config CLI — refusing"));
    }
    let projects = Path::new(wiki).join(rel);
    if !projects.is_dir() {
        return Err(format!("SPIRA_IDENTITY_PROJECTS {rel:?} is not a directory under SPIRA_WIKI {wiki:?} — refusing"));
    }
    let mut out = BTreeMap::new();
    for r in repos {
        let p = projects.join(r).join(PAGE);
        match std::fs::read_to_string(&p) {
            Ok(t) => out.insert(r.clone(), parse(&t)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => out.insert(r.clone(), Page::default()),
            Err(e) => return Err(format!("{}: {e}", p.display())),
        };
    }
    Ok(out)
}

fn owner<'a>(own: &str, pages: &'a BTreeMap<String, Page>, hay: &str) -> Option<(&'a str, &'a str)> {
    let hay = hay.to_ascii_lowercase();
    let mine = pages.get(own);
    pages.iter().filter(|(r, _)| r.as_str() != own).find_map(|(r, p)| {
        p.terms
            .iter()
            .filter(|t| !mine.is_some_and(|m| m.terms.iter().chain(&m.allowed).any(|x| x.eq_ignore_ascii_case(t))))
            .find(|t| hay.contains(&t.to_ascii_lowercase()))
            .map(|t| (r.as_str(), t.as_str()))
    })
}

/// The added lines and added paths of a `git diff -U0` that carry another repo's term, as
/// `path:line: 'term' belongs to <repo>` lines; None when the diff is clean.
pub fn gap(own: &str, pages: &BTreeMap<String, Page>, dir: &Path, diff: &str) -> Option<String> {
    let exempt: &[String] = pages.get(own).map_or(&[], |p| &p.vendored);
    let (mut path, mut line, mut skip) = (String::new(), 0u64, true);
    let mut hits: Vec<String> = Vec::new();
    for l in diff.lines() {
        if let Some(p) = l.strip_prefix("+++ ") {
            path = p.strip_prefix("b/").unwrap_or(p).to_string();
            skip = p == "/dev/null" || exempt.iter().any(|v| path.starts_with(v.as_str()));
            if !skip {
                if let Some((repo, term)) = owner(own, pages, &path) {
                    hits.push(format!("{path}: path: '{term}' belongs to {repo}"));
                }
            }
        } else if let Some(h) = l.strip_prefix("@@ ") {
            line = h.split_whitespace().find_map(|w| w.strip_prefix('+')).and_then(|w| w.split(',').next()).and_then(|n| n.parse().ok()).unwrap_or(0);
        } else if let Some(added) = l.strip_prefix('+') {
            if !skip {
                if let Some((repo, term)) = owner(own, pages, added) {
                    hits.push(format!("{path}:{line}: '{term}' belongs to {repo}"));
                }
            }
            line += 1;
        }
    }
    if hits.is_empty() {
        return None;
    }
    let more = hits.len().saturating_sub(SHOWN);
    hits.truncate(SHOWN);
    let tail = if more > 0 { format!("\n… and {more} more") } else { String::new() };
    Some(format!(
        "adds another repo's identity terms:\n{}{tail}\noverride: list the term under \"## Allowed foreign terms\" in {}",
        hits.join("\n"),
        dir.join(own).join(PAGE).display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pages() -> BTreeMap<String, Page> {
        let mut m = BTreeMap::new();
        m.insert("alpha".into(), parse("## Identity terms\n- Zorblax\n- `quuxify`\n## Vendored paths\n- kit/\n"));
        m.insert("beta".into(), parse("## Identity terms\n- Wibblet\n## Vendored paths\n- kit/\n## Allowed foreign terms\n- Zorblax\n"));
        m
    }

    const ADD: &str = "diff --git a/web/app.ts b/web/app.ts\n--- a/web/app.ts\n+++ b/web/app.ts\n@@ -3,0 +4,2 @@\n+const a = 1;\n+const card = 'wibblet';\n";

    #[test]
    fn a_term_another_repo_owns_fails_naming_it_and_the_override() {
        let g = gap("alpha", &pages(), Path::new("/brain/projects"), ADD).expect("a hit");
        assert!(g.contains("web/app.ts:5: 'Wibblet' belongs to beta"), "{g}");
        assert!(g.contains("/brain/projects/alpha/identity.md"), "{g}");
    }

    #[test]
    fn the_owning_repo_and_one_that_allows_the_term_pass() {
        assert_eq!(gap("beta", &pages(), Path::new("/brain/projects"), ADD), None);
        let zorb = ADD.replace("wibblet", "ZORBLAX");
        assert_eq!(gap("beta", &pages(), Path::new("/brain/projects"), &zorb), None);
        assert!(gap("alpha", &pages(), Path::new("/brain/projects"), &zorb).is_none(), "alpha owns Zorblax");
    }

    #[test]
    fn a_vendored_path_is_exempt_and_an_added_path_is_scanned() {
        let kit = ADD.replace("web/app.ts", "kit/app.ts");
        assert_eq!(gap("alpha", &pages(), Path::new("/brain/projects"), &kit), None);
        let named = "--- /dev/null\n+++ b/web/wibblet.ts\n@@ -0,0 +1 @@\n+ok\n";
        assert!(gap("alpha", &pages(), Path::new("/brain/projects"), named).unwrap().contains("web/wibblet.ts: path: 'Wibblet' belongs to beta"));
    }

    #[test]
    fn removed_lines_and_clean_additions_pass() {
        let d = "--- a/f\n+++ b/f\n@@ -1 +1 @@\n-wibblet\n+fine\n";
        assert_eq!(gap("alpha", &pages(), Path::new("/brain/projects"), d), None);
    }

    #[test]
    fn an_unset_wiki_is_a_refusal_naming_the_key() {
        assert!(load("", "wiki/projects", &["alpha".into()]).unwrap_err().contains("SPIRA_WIKI"));
        let d = testkit::TempDir::new("sift-identity");
        let w = d.path().to_str().unwrap();
        assert!(load(w, "", &["alpha".into()]).unwrap_err().contains("SPIRA_IDENTITY_PROJECTS"));
        assert!(load(w, "../x", &["alpha".into()]).unwrap_err().contains("SPIRA_IDENTITY_PROJECTS"));
        assert!(load(w, "wiki/projects", &["alpha".into()]).unwrap_err().contains("SPIRA_IDENTITY_PROJECTS"));
    }

    #[test]
    fn pages_are_read_from_the_brain_and_a_repo_without_one_declares_nothing() {
        let d = testkit::TempDir::new("sift-identity");
        let dir = d.path().join("wiki/projects/alpha");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(PAGE), "## Identity terms\n- Zorblax\n").unwrap();
        let p = load(d.path().to_str().unwrap(), "wiki/projects", &["alpha".into(), "beta".into()]).unwrap();
        assert_eq!(p["alpha"].terms, ["Zorblax"]);
        assert_eq!(p["beta"], Page::default());
    }
}
