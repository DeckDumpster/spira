//! The per-repo identity registry: terms that belong to exactly one registered repo, and the
//! guard that refuses a bead for one repo carrying another's.
//!
//! File format, one row per repo: `repo | term, term, ...`; `#` comments and blank lines
//! skipped. A repo with no row declares no identity and is exempt (the shared kit).

use std::collections::BTreeMap;

pub const OVERRIDE_ENV: &str = "BEAD_CROSS_REPO_CONSIDERED";

pub fn parse(text: &str) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let Some((repo, terms)) = t.split_once('|') else { continue };
        let repo = repo.trim();
        if repo.is_empty() {
            continue;
        }
        let terms = terms.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
        out.entry(repo.to_string()).or_default().extend(terms);
    }
    out
}

fn contains_term(haystack: &str, term: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    haystack.match_indices(term).any(|(i, m)| {
        let before = haystack[..i].chars().next_back().is_none_or(|c| !word(c));
        let after = haystack[i + m.len()..].chars().next().is_none_or(|c| !word(c));
        before && after
    })
}

/// Every `(term, owning repo)` another repo claims that `text` carries. A term the filing
/// repo also declares is its own and never counts.
pub fn foreign_terms(
    registry: &BTreeMap<String, Vec<String>>,
    repo: &str,
    text: &str,
) -> Vec<(String, String)> {
    let text = text.to_lowercase();
    let own = registry.get(repo);
    let mut hits = Vec::new();
    for (owner, terms) in registry {
        if owner == repo {
            continue;
        }
        for term in terms {
            if own.is_some_and(|o| o.contains(term)) {
                continue;
            }
            if contains_term(&text, term) {
                hits.push((term.clone(), owner.clone()));
            }
        }
    }
    hits.sort();
    hits.dedup();
    hits
}

pub fn refusal(repo: &str, hits: &[(String, String)]) -> String {
    let mut m = format!("bead: refusing: this bead is for repo:{repo} but carries another repo's identity terms:\n");
    for (term, owner) in hits {
        m.push_str(&format!("  {term:?} belongs to {owner}\n"));
    }
    m.push_str(&format!(
        "A repo is defined in its own terms; cross-repo work routes through the shared kit. If this genuinely spans repos, set {OVERRIDE_ENV}=\"<why>\" and the reason is recorded on the bead."
    ));
    m
}

pub fn override_reason(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

pub fn recorded_line(reason: &str) -> String {
    format!("Cross-repo considered: {reason}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> BTreeMap<String, Vec<String>> {
        parse("# c\ndex | Pokédex, Pokémon, pd-\nstar | dumpstar\nkit-less |\n")
    }

    #[test]
    fn foreign_term_is_named_with_its_owner() {
        let h = foreign_terms(&reg(), "star", "extend tests with Pokédex terms");
        assert_eq!(h, vec![("pokédex".to_string(), "dex".to_string())]);
    }

    #[test]
    fn own_terms_and_unregistered_names_pass() {
        assert!(foreign_terms(&reg(), "dex", "Pokémon page").is_empty());
        assert!(foreign_terms(&reg(), "star", "route through dumpster-kit").is_empty());
    }

    #[test]
    fn substring_inside_a_word_is_not_a_hit() {
        assert!(foreign_terms(&reg(), "star", "repokémonic").is_empty());
    }

    #[test]
    fn shared_term_is_not_foreign() {
        let r = parse("a | shared, only-a\nb | shared\n");
        assert!(foreign_terms(&r, "b", "shared").is_empty());
        assert_eq!(foreign_terms(&r, "b", "only-a").len(), 1);
    }

    #[test]
    fn blank_override_is_no_override() {
        assert_eq!(override_reason(Some("  ".into())), None);
        assert_eq!(override_reason(Some(" why ".into())), Some("why".into()));
    }
}
