//! Untracked or modified paths in a production checkout, answered in-process and held on the
//! worktree digest, so the sentinel pass and the cockpit probe run git only when the tree moved.

#[derive(Debug, PartialEq, Eq)]
pub enum Probe {
    Clean,
    Drift(String),
    Unreadable,
}

pub fn classify(code: Option<i32>, out: &str, repo: &str) -> Probe {
    match code {
        Some(0) if out.trim().is_empty() => Probe::Clean,
        Some(0) => Probe::Drift(out.lines().map(|l| format!("DIRTY {l} ({repo})\n")).collect()),
        _ => Probe::Unreadable,
    }
}

pub fn checkout(repo: &str) -> Probe {
    let path = std::path::Path::new(repo);
    if !path.join(".git").exists() {
        return Probe::Unreadable;
    }
    let read = || -> Result<String, String> {
        let o = spira_config::bounded::bounded("git")
            .args(["--no-optional-locks", "-C", repo, "status", "--porcelain"])
            .output()
            .map_err(|e| format!("git status: {e}"))?;
        let out = String::from_utf8_lossy(&o.stdout).into_owned();
        match classify(o.status.code(), &out, repo) {
            Probe::Unreadable => Err(format!("git status exited {:?}", o.status.code())),
            _ => Ok(out),
        }
    };
    let held = match spira_config::git_refs::worktree_digest(path) {
        Some(digest) => spira_config::git_refs::held(path, "drift-status", &digest.to_string(), read),
        None => read(),
    };
    match held {
        Ok(out) => classify(Some(0), &out, repo),
        Err(_) => Probe::Unreadable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tb2febk)]
    #[test]
    fn git_status_output_is_clean_drift_or_unreadable() {
        assert!(matches!(classify(Some(0), "", "repo"), Probe::Clean));
        assert!(matches!(classify(Some(0), "?? x\n", "repo"), Probe::Drift(s) if s == "DIRTY ?? x (repo)\n"));
        for code in [Some(128), Some(124), None] {
            assert!(matches!(classify(code, "", "repo"), Probe::Unreadable));
        }
    }

    #[test_id(tbl7ijn)]
    #[test]
    fn a_directory_with_no_git_is_unreadable() {
        let d = testkit::TempDir::new("no-git");
        assert!(matches!(checkout(&d.path().display().to_string()), Probe::Unreadable));
    }
}
