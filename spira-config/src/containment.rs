//! `spira_containment_check` (`spira/lib.sh`), ported — see wave4-decomposition.md (c) #5. A
//! non-`prod` instance (a fixture, a scratch second instance) is confined to
//! `SPIRA_WORKSPACES` and may name no checkout with a real (network) remote, so a test
//! instance can never accidentally reach across to an operator's real repository. `prod`
//! itself, or an unset instance, is unconstrained — the map exists for the box that runs more
//! than one instance side by side.
//!
//! Folded into [`crate::resolve::resolve`] itself (not a function a caller must remember to
//! call): "when the repo registry moves to spira-config, the check must move into resolve, so
//! that it still fires for every process" (wave4-decomposition.md (c) #5).

use std::collections::BTreeSet;
use std::path::Path;

/// One row of the repo-map: `name|path|...` (trailing columns are the repo registry's own
/// business, not this check's — only `name` and `path` are read here, matching
/// `spira_containment_check`'s own `while IFS='|' read -r name path rest` destructuring).
fn parse_rows(text: &str) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let mut parts = line.splitn(3, '|');
        let name = parts.next().unwrap_or("").trim().to_string();
        let path = parts.next().unwrap_or("").trim().to_string();
        if name.is_empty() || name.starts_with('#') || path.is_empty() {
            continue;
        }
        rows.push((name, path));
    }
    rows
}

/// `_spira_remote_is_real` (lib.sh): a URL is "real" (network-reachable, and so forbidden on
/// a confined instance) unless it is a local filesystem path — absolute, or `file://`.
pub fn remote_is_real(url: &str) -> bool {
    if url.is_empty() {
        return false;
    }
    !(url.starts_with('/') || url.starts_with("file:///"))
}

/// The canonical form of `path` if it resolves (matches bash's `cd "$path" && pwd -P`),
/// falling back to the literal string when it does not exist yet — the same fallback
/// `spira_containment_check` takes for an entry that names a path not yet checked out.
fn canonical_or_literal(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.to_string())
}

fn is_under(path: &str, root: &str) -> bool {
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return true; // no workspaces root configured — nothing to confine against
    }
    path == root || path.starts_with(&format!("{root}/"))
}

/// Every `(fetch)` remote URL git reports for the checkout at `path`, or an empty vec if
/// `path` is not a git checkout at all (`git -C path rev-parse --git-dir` failing is not an
/// error here — most registered repos are plain directories at the fixture tier).
fn fetch_remotes(path: &str) -> Vec<String> {
    let is_repo = std::process::Command::new("git")
        .args(["-C", path, "rev-parse", "--git-dir"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !is_repo {
        return Vec::new();
    }
    let out = std::process::Command::new("git")
        .args(["-C", path, "remote", "-v"])
        .output();
    let Ok(out) = out else { return Vec::new() };
    out.stdout
        .split(|&b| b == b'\n')
        .filter_map(|line| std::str::from_utf8(line).ok())
        .filter(|line| line.contains("(fetch)"))
        .filter_map(|line| line.split_whitespace().nth(1))
        .map(str::to_string)
        .collect()
}

/// `spira_containment_check` itself. `instance` is `SPIRA_INSTANCE` as resolved (never empty
/// by the time this runs — `resolve` defaults it to `prod`). `workspaces` is the resolved
/// `SPIRA_WORKSPACES`. `repo_map_text` is the content of the resolved `SPIRA_REPO_MAP` file,
/// or `None` if it does not exist (checked by the caller, which already has the path) — "no
/// map to check" is not a violation.
///
/// Returns every violation found (not just the first — an operator fixing one should not have
/// to re-run to find the next), or `Ok(())`. The bash original exits the whole process; this
/// port returns the decision instead and leaves exiting to the caller, so a library consumer
/// (a future in-process `resolve()` caller) can choose how to fail rather than this function
/// taking the process down under it.
pub fn check(instance: &str, workspaces: &str, repo_map_text: Option<&str>) -> Result<(), Vec<String>> {
    if instance.is_empty() || instance == "prod" {
        return Ok(());
    }
    let Some(text) = repo_map_text else { return Ok(()) };

    let mut violations = Vec::new();
    // De-duplicate identical messages across rows is unnecessary (bash doesn't either) — a
    // distinct violation per registered checkout is exactly the granularity an operator wants.
    for (name, path) in parse_rows(text) {
        if !workspaces.is_empty() {
            let resolved = canonical_or_literal(&path);
            if !is_under(&resolved, workspaces) {
                violations.push(format!(
                    "spira: containment: instance {instance} is confined to {workspaces} — {name} ({path}) is outside it"
                ));
            }
        }
        for url in fetch_remotes(&path) {
            if remote_is_real(&url) {
                violations.push(format!(
                    "spira: containment: instance {instance} may not have a real remote — {name} ({path}) has {url}"
                ));
                break; // one reported remote per checkout, same as the bash `break`
            }
        }
    }
    let violations = violations_sorted_unique(violations);
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

/// Stable order for test assertions and for a human reading the refusal — bash's own loop
/// order (map-file order) is preserved by NOT sorting; this only drops exact duplicates,
/// which cannot occur from distinct rows but keeps the function honest if the map ever
/// repeats a checkout.
fn violations_sorted_unique(v: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    v.into_iter().filter(|m| seen.insert(m.clone())).collect()
}

/// Reads `SPIRA_REPO_MAP` at `path`, returning `None` when it does not exist — "no map to
/// check" is not a violation, matching `spira_containment_check`'s own `[ -f "$SPIRA_REPO_MAP"
/// ] || return 0`.
pub fn read_repo_map(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prod_or_unset_instance_is_always_allowed() {
        assert_eq!(check("prod", "/ws", Some("x|/outside\n")), Ok(()));
        assert_eq!(check("", "/ws", Some("x|/outside\n")), Ok(()));
    }

    #[test]
    fn no_map_is_not_a_violation() {
        assert_eq!(check("test", "/ws", None), Ok(()));
    }

    #[test]
    fn comments_and_blanks_are_skipped() {
        assert_eq!(check("test", "/ws", Some("# a comment\n\nx|/ws/ok\n")), Ok(()));
    }

    #[test]
    fn path_inside_workspaces_is_fine() {
        let dir = testkit::TempDir::new("spira-config-containment-inside");
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let map = format!("x|{}\n", repo.display());
        assert_eq!(check("test", dir.path().to_str().unwrap(), Some(&map)), Ok(()));
    }

    #[test]
    fn path_outside_workspaces_is_a_violation() {
        let dir = testkit::TempDir::new("spira-config-containment-outside");
        let ws = dir.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let outside = dir.join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        let map = format!("x|{}\n", outside.display());
        let err = check("test", ws.to_str().unwrap(), Some(&map)).unwrap_err();
        assert_eq!(err.len(), 1);
        assert!(err[0].contains("is confined to"), "{err:?}");
        assert!(err[0].contains("x ("), "{err:?}");
    }

    #[test]
    fn a_nonexistent_path_is_compared_literally() {
        // The entry names a checkout not made yet — fall back to the literal string rather
        // than failing the canonicalisation outright.
        let out = check("test", "/ws", Some("x|/ws/not-made-yet\n"));
        assert_eq!(out, Ok(()));
        let out2 = check("test", "/ws", Some("x|/elsewhere/not-made-yet\n"));
        assert!(out2.is_err());
    }

    #[test]
    fn a_real_remote_is_a_violation_and_a_local_remote_is_not() {
        let dir = testkit::TempDir::new("spira-config-containment-remote");
        let ws = dir.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let repo = ws.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["remote", "add", "origin", "https://example.invalid/x.git"]);
        let map = format!("x|{}\n", repo.display());
        let err = check("test", ws.to_str().unwrap(), Some(&map)).unwrap_err();
        assert!(err.iter().any(|m| m.contains("may not have a real remote")), "{err:?}");

        let repo2 = ws.join("repo2");
        std::fs::create_dir_all(&repo2).unwrap();
        run_git(&repo2, &["init", "-q"]);
        run_git(&repo2, &["remote", "add", "origin", "/some/local/path"]);
        let map2 = format!("y|{}\n", repo2.display());
        assert_eq!(check("test", ws.to_str().unwrap(), Some(&map2)), Ok(()));
    }

    #[test]
    fn remote_is_real_matches_lib_sh() {
        assert!(!remote_is_real(""));
        assert!(!remote_is_real("/abs/path"));
        assert!(!remote_is_real("file:///abs/path"));
        assert!(remote_is_real("https://example.invalid/x.git"));
        assert!(remote_is_real("git@example.invalid:x/y.git"));
        assert!(remote_is_real("ssh://example.invalid/x.git"));
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed in {dir:?}");
    }
}
