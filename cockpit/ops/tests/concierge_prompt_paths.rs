//! Every command path the Concierge prompt gives under `{{COCKPIT}}/` or `{{SPIRA_HOME}}/`
//! must name a file the release ships.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const COCKPIT: &str = "{{COCKPIT}}/";
const SPIRA_HOME: &str = "{{SPIRA_HOME}}/";
const BIN_PREFIX: &str = "../bin/";

fn repo_root() -> PathBuf {
    testkit::manifest_dir().join("../..")
}

/// What `release build` puts in `bin/`: every declared `[[bin]]` plus the compat aliases.
fn release_bins(root: &Path) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = release::workspace::expected_bins(root).expect("workspace bins").into_iter().collect();
    names.extend(release::compat::read(root).into_iter().map(|(_, alias)| alias));
    names
}

fn tokens(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for prefix in [COCKPIT, SPIRA_HOME] {
        let mut rest = line;
        while let Some(at) = rest.find(prefix) {
            let tail = &rest[at..];
            let end = tail.find(|c: char| c.is_whitespace() || matches!(c, '`' | '"' | '|' | ')')).unwrap_or(tail.len());
            out.push(tail[..end].trim_end_matches(['.', ',', ';', ':']));
            rest = &tail[end.max(prefix.len())..];
        }
    }
    out
}

fn resolves(token: &str, root: &Path, bins: &BTreeSet<String>) -> bool {
    if let Some(rel) = token.strip_prefix(COCKPIT) {
        return root.join("cockpit").join(rel).exists();
    }
    let Some(rel) = token.strip_prefix(SPIRA_HOME) else { return false };
    match rel.strip_prefix(BIN_PREFIX) {
        Some(name) => bins.contains(name),
        None => root.join("spira").join(rel).exists(),
    }
}

fn unresolved(prompt: &str, root: &Path) -> Vec<String> {
    let bins = release_bins(root);
    let mut bad = Vec::new();
    for (i, line) in prompt.lines().enumerate() {
        for t in tokens(line) {
            if !resolves(t, root, &bins) {
                bad.push(format!("concierge.md:{}: {t} names nothing the release ships", i + 1));
            }
        }
    }
    bad
}

#[test]
fn concierge_prompt_names_only_paths_the_release_ships() {
    let root = repo_root();
    let prompt = std::fs::read_to_string(root.join("spira/runes/concierge.md")).expect("read concierge.md");
    let bad = unresolved(&prompt, &root);
    assert!(bad.is_empty(), "\n{}\n", bad.join("\n"));
}

#[test]
fn the_check_resolves_a_shipped_bin_and_refuses_a_missing_cockpit_script() {
    let root = repo_root();
    assert!(unresolved("{{SPIRA_HOME}}/../bin/reply x", &root).is_empty());
    assert!(unresolved("{{COCKPIT}}/panel-run.sh", &root).is_empty());
    assert!(unresolved("{{SPIRA_HOME}}/capacity.sh", &root).is_empty());
    assert!(unresolved("{{SPIRA_HOME}}/../bin/world.sh status", &root).is_empty());
    assert_eq!(
        unresolved("ok\n{{COCKPIT}}/no-such.sh", &root),
        vec!["concierge.md:2: {{COCKPIT}}/no-such.sh names nothing the release ships".to_string()]
    );
}
