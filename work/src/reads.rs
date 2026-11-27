//! The submit fence for a removal the active release still reads: the round that judges a branch
//! is run by that release, so what it reads leaves the tree only after a release stops reading it.

use spira_config::recipe;
use std::path::Path;

/// The tree paths this binary's release declares it reads; `work` ships with that release.
pub const DECLARED: [&str; 9] = union();

const fn union<const N: usize>() -> [&'static str; N] {
    let mut out = [""; N];
    let (mut len, mut i) = (0, 0);
    while i < recipe::PATHS.len() + recipe::TAG_INPUTS.len() {
        let path = if i < recipe::PATHS.len() { recipe::PATHS[i] } else { recipe::TAG_INPUTS[i - recipe::PATHS.len()] };
        let (mut j, mut seen) = (0, false);
        while j < len {
            seen |= same(out[j].as_bytes(), path.as_bytes());
            j += 1;
        }
        if !seen {
            out[len] = path;
            len += 1;
        }
        i += 1;
    }
    assert!(len == N, "DECLARED's length is not the number of distinct declared paths");
    out
}

const fn same(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Judges the branch at `root` against `land` for the active release at commit `release`.
pub fn gate(root: &Path, land: &str, release: &str) -> Result<(), String> {
    let out = spira_config::bounded::bounded("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--name-only", "--no-renames", "--diff-filter=D", &format!("{land}...HEAD")])
        .output()
        .ok()
        .filter(|o| o.status.success());
    refusal(out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()), &DECLARED, release)?;
    ::gate::fixture::against_rev(root, release, &format!("the active release {release}"))
}

/// `deleted`: the paths the branch deletes against its landing ref, `None` when they cannot be read.
pub fn refusal(deleted: Option<String>, declared: &[&str], release: &str) -> Result<(), String> {
    let Some(deleted) = deleted else { return Err("cannot read the branch's diff against its landing ref to look for removals the active release reads".into()) };
    let bad: Vec<&str> = deleted.lines().filter(|p| declared.contains(p)).collect();
    if bad.is_empty() {
        return Ok(());
    }
    Err(format!("the branch deletes path(s) the active release {release} reads: {}", bad.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deleted(paths: &str) -> Option<String> {
        Some(paths.to_string())
    }

    #[test_id(tjfwsmj)]
    #[test]
    fn deleting_a_declared_input_is_refused_naming_it_and_the_release_only() {
        let e = refusal(deleted("flake.lock\ndocs/x.md\n"), &DECLARED, "abc").unwrap_err();
        assert!(e.contains("flake.lock") && e.contains("abc") && !e.contains("docs/x.md"), "{e}");
    }

    #[test_id(tn4zdaq)]
    #[test]
    fn a_path_the_release_no_longer_declares_may_be_deleted() {
        let without: Vec<&str> = DECLARED.iter().copied().filter(|p| *p != "flake.lock").collect();
        assert_eq!(refusal(deleted("flake.lock\ndocs/x.md\n"), &without, "abc"), Ok(()));
    }

    #[test_id(te6o43d)]
    #[test]
    fn an_undeclared_deletion_passes_and_an_unreadable_diff_refuses() {
        assert_eq!(refusal(deleted("docs/x.md\n"), &DECLARED, "abc"), Ok(()));
        assert!(refusal(None, &DECLARED, "abc").is_err());
    }

    #[test_id(tnhzy59)]
    #[test]
    fn declared_is_exactly_the_recipe_and_tag_inputs_once_each() {
        let want: std::collections::BTreeSet<&str> = recipe::PATHS.iter().chain(&recipe::TAG_INPUTS).copied().collect();
        let got: std::collections::BTreeSet<&str> = DECLARED.iter().copied().collect();
        assert_eq!(got, want);
        assert_eq!(DECLARED.len(), want.len(), "a path is listed twice");
    }
}
