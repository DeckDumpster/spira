//! The base-suite cache (sp-kqger). Every branch-red gate used to re-run the branch's whole
//! suite selection on the landing base to decide whose fault the red was — 275-337s, paid
//! again by the next gate on the same base tree, since many branches iterate against the
//! same local/main tip. `engine.rs` no longer mirrors that selection at all (DESIGN.md
//! "Composition"'s base trial is fences-only for a suites composition); each of the branch's
//! red suites is judged here instead, or by a targeted rerun that pays for only what the
//! cache could not answer.
//!
//! `${SPIRA_VERDICTS:-<run>/verdicts}/base-suites/<repo>/<base-tree>/<suite>`. The base tree
//! is the directory itself: a landing ref that moves reads an empty namespace under its new
//! tree, never a stale answer under the old one, by construction (no TTL is needed for that
//! axis). `harness` and `image` are recorded in the file and checked on every read, so a gate
//! binary upgrade or a rebuilt suite container invalidates every entry it did not itself
//! write, even though the base tree has not moved.
//!
//! FAIL CLOSED: [`fresh`] returns `None` — a miss, meaning "run it" — for anything it cannot
//! parse into an exact match, never a guessed verdict. [`path`] returns `None` for anything
//! that is not one safe path component in each position, the same discipline `cert.rs` and
//! `base_rerun_cmd`'s own suite-name filter already hold to.

use std::path::{Path, PathBuf};

/// `<verdicts>/base-suites/<repo>/<tree>/<suite>`; None when any component is not one safe
/// path segment on its own — never a guess at a path a hostile suite name, or a base tree
/// that did not resolve, could escape.
pub fn path(verdicts: &Path, repo: &str, tree: &str, suite: &str) -> Option<PathBuf> {
    (crate::cert::is_repo_name(repo)
        && crate::cert::is_object_id(tree)
        && crate::compose::is_suite_name(suite))
    .then(|| verdicts.join("base-suites").join(repo).join(tree).join(suite))
}

/// The file a fresh judgement writes: `pass`, and the harness and image it was judged under.
pub fn render(pass: bool, harness_h: &str, image: &str, when: &str, at: u64) -> String {
    format!(
        "verdict={}\nharness={harness_h}\nimage={image}\nwhen={when}\nat={at}\n",
        if pass { "PASS" } else { "FAIL" }
    )
}

/// `Some(true)` — a cached PASS; `Some(false)` — a cached FAIL, still naming the suite to the
/// caller (the suite that failed is `path`'s own argument, not anything this function reads
/// back); `None` — a miss: unreadable, unparseable, or recorded under a different harness or
/// testenv image than the trial in hand is asking about. An empty `harness_h` or `image`
/// (the caller could not resolve one) never matches anything, including another empty one —
/// a cache that cannot be checked is the same as a cache that was never asked.
pub fn fresh(entry: &str, harness_h: &str, image: &str) -> Option<bool> {
    if harness_h.is_empty() || image.is_empty() {
        return None;
    }
    let mut verdict = None;
    let mut harness = None;
    let mut img = None;
    for line in entry.lines() {
        let (k, v) = line.split_once('=').unwrap_or((line, ""));
        match k {
            "verdict" => verdict = Some(v),
            "harness" => harness = Some(v),
            "image" => img = Some(v),
            _ => {}
        }
    }
    if harness != Some(harness_h) || img != Some(image) {
        return None;
    }
    match verdict {
        Some("PASS") => Some(true),
        Some("FAIL") => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_entry_matches_on_harness_and_image_exactly() {
        let e = render(true, "h1", "img1", "2026-09-30T00:00:00Z", 100);
        assert_eq!(fresh(&e, "h1", "img1"), Some(true));
        assert_eq!(fresh(&e, "h2", "img1"), None, "a different harness is a miss");
        assert_eq!(fresh(&e, "h1", "img2"), None, "a different image is a miss");
    }

    #[test]
    fn a_red_verdict_round_trips_and_still_names_nothing_itself() {
        // The suite name is never in the file; the caller already knows it from `path`.
        let e = render(false, "h", "img", "w", 1);
        assert_eq!(fresh(&e, "h", "img"), Some(false));
        assert!(!e.contains("suite="));
    }

    #[test]
    fn unreadable_or_unrecognised_content_is_a_miss_never_a_guess() {
        assert_eq!(fresh("", "h", "img"), None, "an empty (unreadable) file");
        assert_eq!(
            fresh("verdict=MAYBE\nharness=h\nimage=img\n", "h", "img"),
            None,
            "an unrecognised verdict word"
        );
        assert_eq!(
            fresh("harness=h\nimage=img\n", "h", "img"),
            None,
            "no verdict line at all"
        );
    }

    #[test]
    fn an_unresolved_key_component_never_matches_even_itself() {
        // The caller could not resolve a harness or image; it must never use an old entry
        // that also happens to be recorded with an empty value for the same reason.
        let e = render(true, "", "", "w", 1);
        assert_eq!(fresh(&e, "", ""), None);
    }

    #[test]
    fn hostile_or_malformed_names_never_become_a_path() {
        let v = Path::new("/v");
        let t = "0".repeat(40);
        assert_eq!(
            path(v, "spira", &t, "test-a.sh"),
            Some(PathBuf::from(format!("/v/base-suites/spira/{t}/test-a.sh")))
        );
        assert!(path(v, "../x", &t, "test-a.sh").is_none(), "repo escapes");
        assert!(path(v, "spira", "not-a-tree", "test-a.sh").is_none(), "not an object id");
        assert!(path(v, "spira", &t, "x;rm -rf /.sh").is_none(), "not a suite name");
    }
}
