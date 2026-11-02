//! `inventory` — refuse to ship one operator's infrastructure. Contract: DESIGN.md.
//! Ported from `spira/inventory.sh` (deleted).

use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct Inventory;

const NAME: &str = "inventory";
const DENY_FILE: &str = "spira/inventory-deny";
/// This rule's own source spells out every pattern it hunts; it and the deny file it reads
/// (whose content IS the deny list) must never flag themselves.
const OWN_SOURCE: &str = "spira-lint/src/rules/inventory.rs";

/// The structural patterns, before the operator's own `inventory-deny` additions.
pub const PATTERNS: &[&str] =
    &[r"/home/[a-z][a-z0-9_.-]*/", r"/Users/[A-Za-z][A-Za-z0-9_.-]*/", r"/workspaces/", r"\(per [A-Z][a-z]+, [0-9]{4}-[0-9]{2}-[0-9]{2}"];

const EMAIL_PATTERN: &str = r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}";

/// Reserved for documentation (RFC 2606 / RFC 6761), the `git@host` SSH remote form, a
/// systemd template instance (`unit@instance.service`, which has the shape of an address but
/// no mail domain ends this way), and this harness's own synthetic author.
const EMAIL_EXEMPT: &str = concat!(
    r"^git@|@example\.(com|net|org|invalid)|@[A-Za-z0-9_.-]+\.invalid$|",
    r"@(example|test|invalid|localhost)$|@spira\.local|",
    r"@[A-Za-z0-9_.-]*\.(service|timer|socket|target|path|mount|slice|scope|swap|device)$"
);

fn is_own(path: &str) -> bool {
    path == OWN_SOURCE || path == DENY_FILE
}

/// The release's vendored third-party binaries (`vendor/bin/**`: the aerc build-tarball.sh
/// builds into a release tree, never tracked in git). Their bytes are someone else's — an
/// upstream's test addresses and maintainers' mail — not this harness's history, and no
/// edit here could change them; the fast tier scans a release tree, where they sit.
const VENDORED: &str = "vendor/bin/";

fn is_vendored(path: &str) -> bool {
    path.starts_with(VENDORED)
}

/// The deny file's lines, `#`-comment stripped, blank lines dropped — extra ERE fragments
/// the operator adds to the structural patterns.
pub fn deny_fragments(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn pattern_re(deny: &[String]) -> Result<Regex, String> {
    let mut alts: Vec<String> = PATTERNS.iter().map(|s| s.to_string()).collect();
    alts.extend(deny.iter().cloned());
    Regex::new(&alts.join("|")).map_err(|e| e.to_string())
}

fn email_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(EMAIL_PATTERN).expect("static regex"))
}

fn email_exempt_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(EMAIL_EXEMPT).expect("static regex"))
}

/// Every offending token in `content`, sorted and deduplicated — `inventory.sh`'s `scan()`.
pub fn scan(content: &[u8], pat: &Regex) -> Vec<String> {
    let mut hits: std::collections::BTreeSet<Vec<u8>> = std::collections::BTreeSet::new();
    for m in pat.find_iter(content) {
        hits.insert(m.as_bytes().to_vec());
    }
    for m in email_re().find_iter(content) {
        if !email_exempt_re().is_match(m.as_bytes()) {
            hits.insert(m.as_bytes().to_vec());
        }
    }
    hits.into_iter().map(|b| String::from_utf8_lossy(&b).into_owned()).collect()
}

impl Rule for Inventory {
    fn name(&self) -> &'static str {
        NAME
    }

    /// Every tracked-or-untracked-not-ignored file (the whole walk) except its own pattern
    /// files — the exemption is applied here rather than narrowing the scope, so an empty
    /// repository (not merely an all-exempt one) is what refuses.
    fn applies_to(&self, _e: &Entry) -> bool {
        true
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = crate::scope(tree, self)?;
        let deny = deny_fragments(&tree.read_text(DENY_FILE));
        // A malformed deny-list entry must refuse, not silently disable every pattern check:
        // the bash fence's `grep -E` on a bad regex errored to stderr but the pipeline still
        // read as "no hits", an undetected false-negative wide open.
        let pat = pattern_re(&deny)
            .map_err(|reason| LintError::BadAllow { file: DENY_FILE.to_string(), line: 0, reason })?;
        let mut out = Vec::new();
        for e in files {
            if is_own(&e.path) || is_vendored(&e.path) {
                continue;
            }
            let Some(content) = tree.content(e) else { continue };
            let hits = scan(content, &pat);
            if hits.is_empty() {
                continue;
            }
            out.push(Finding { rule: NAME, path: e.path.clone(), line: None, message: hits.join(", ") });
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "The files above name one operator's infrastructure. This repository is meant to be \
cloned: move the case history — the path, the host, the person and the date — to wherever \
your own notes live, and cite it from there. A genuinely generic token can be added to \
spira/inventory-deny as an extended regex, or exempted structurally if the pattern itself is wrong."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(Inventory),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn scan_is_sorted_and_deduplicated() {
        let pat = pattern_re(&[]).unwrap();
        let hits = scan(b"/home/a/x and /home/a/x again, plus /workspaces/\n", &pat);
        assert_eq!(hits, vec!["/home/a/".to_string(), "/workspaces/".to_string()]);
    }
}
