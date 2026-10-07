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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_git(t.path()).unwrap();
        Inventory.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn empty_index_refuses_then_a_home_path_is_seen_red_and_withdrawn() {
        let t = TempDir::new("inv");
        t.git_init();
        assert_eq!(run(&t), Err(LintError::EmptyScope), "empty index refuses to report clean");

        t.write("spira/helper.sh", "#!/usr/bin/env bash\necho ok\n");
        t.git(&["add", "."]);
        assert!(run(&t).unwrap().is_empty(), "clean tree");

        t.write("spira/planted.sh", "# config at /home/testuser/config.conf\n");
        t.git(&["add", "."]);
        let got = run(&t).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("spira/planted.sh"), "{got:?}");
        assert!(got[0].contains("/home/testuser/"), "{got:?}");

        t.remove("spira/planted.sh");
        t.git(&["add", "-A"]);
        assert!(run(&t).unwrap().is_empty(), "clean after withdrawal");
    }

    #[test]
    fn untracked_violation_is_caught_sp_hm2vw() {
        let t = TempDir::new("inv-untracked");
        t.git_init();
        t.write("spira/helper.sh", "ok\n");
        t.git(&["add", "."]);
        t.write("spira/untracked.sh", "# config at /home/testuser/config.conf\n");
        let got = run(&t).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("spira/untracked.sh"));
    }

    #[test]
    fn workspaces_and_provenance_and_email_are_each_caught() {
        let t = TempDir::new("inv-shapes");
        t.git_init();
        t.write("spira/helper.sh", "ok\n");
        t.write("spira/ws.sh", "# repo lives at /workspaces/myproject\n");
        t.write("spira/prov.sh", "# (per Alice, 2025-09-01) changed this\n");
        t.write("spira/mail.sh", "# contact: user@realcompany.io\n");
        t.git(&["add", "."]);
        let got = run(&t).unwrap();
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(got.iter().any(|l| l.contains("spira/ws.sh") && l.contains("/workspaces/")));
        assert!(got.iter().any(|l| l.contains("spira/prov.sh") && l.contains("per Alice")));
        assert!(got.iter().any(|l| l.contains("spira/mail.sh") && l.contains("user@realcompany.io")));
    }

    #[test]
    fn exempt_email_forms_are_not_flagged() {
        let t = TempDir::new("inv-exempt-mail");
        t.git_init();
        t.write(
            "spira/remote.sh",
            "remote: git@github.com:org/repo.git\n# author: user@example.com\n# admin: user@example.org\n# unit: watcher@watcher.service\n",
        );
        t.git(&["add", "."]);
        assert!(run(&t).unwrap().is_empty());
    }

    #[test]
    fn a_vendored_binary_is_never_flagged_but_the_same_bytes_elsewhere_are() {
        let t = TempDir::new("inv-vendor");
        t.git_init();
        t.write("vendor/bin/aerc", "maintainer: aerc-devel@lists.sr.ht\n");
        t.write("vendor/binary-notes.txt", "maintainer: aerc-devel@lists.sr.ht\n");
        t.git(&["add", "-f", "."]);
        let got = run(&t).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("vendor/binary-notes.txt"), "{got:?}");
    }

    #[test]
    fn own_source_and_deny_file_are_never_flagged_but_a_real_offender_beside_them_still_is() {
        let t = TempDir::new("inv-own");
        t.git_init();
        t.write("spira/helper.sh", "ok\n");
        t.write(OWN_SOURCE, "// PATTERNS include /home/[a-z] and /workspaces/\n");
        t.write(DENY_FILE, "# /home/exemptuser/path — in deny list, not scanned\nacme\\.internal\n");
        t.write("spira/offender.sh", "# /home/baduser/secrets\n");
        t.git(&["add", "."]);
        let got = run(&t).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("spira/offender.sh"));

        // The deny-file addition ("acme\.internal") is picked up as an extra pattern.
        t.write("spira/acme.sh", "internal host: acme.internal\n");
        t.git(&["add", "."]);
        let got = run(&t).unwrap();
        assert!(got.iter().any(|l| l.contains("spira/acme.sh") && l.contains("acme.internal")), "{got:?}");
    }

    #[test]
    fn scan_is_sorted_and_deduplicated() {
        let pat = pattern_re(&[]).unwrap();
        let hits = scan(b"/home/a/x and /home/a/x again, plus /workspaces/\n", &pat);
        assert_eq!(hits, vec!["/home/a/".to_string(), "/workspaces/".to_string()]);
    }

    #[test]
    fn a_malformed_deny_entry_refuses_rather_than_silently_matching_nothing() {
        let t = TempDir::new("inv-bad-deny");
        t.git_init();
        t.write("spira/helper.sh", "ok\n");
        t.write(DENY_FILE, "acme[.internal\n"); // unbalanced bracket: invalid regex
        t.git(&["add", "."]);
        assert!(matches!(run(&t), Err(LintError::BadAllow { .. })));
    }
}
